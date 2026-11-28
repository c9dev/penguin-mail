//! What Contacts writes: saving, deleting and putting back a contact in
//! its account. The demo keeps contacts in the store alone.

use std::rc::Rc;

use anyhow::{Result, anyhow};
use gtk::glib;
use mailrs_domain::contact::Changes;
use mailrs_domain::translate::gettext;
use mailrs_gmail::ContactFields;
use mailrs_store::address_book::{self, Contact, Link};
use mailrs_sync::Permitted;

use crate::app::App;

/// Every field of `contact`, to write it again after a delete.
pub fn fields_of(contact: &Contact) -> ContactFields {
    let d = &contact.details;
    let phones = if d.phones.is_empty() {
        contact.phone.iter().cloned().collect()
    } else {
        d.phones.clone()
    };
    ContactFields {
        name: contact.name.clone(),
        organization: contact.organization.clone(),
        emails: Some(contact.emails.clone()),
        phones: Some(phones),
        details: Changes {
            birthday: Some(d.birthday.clone()),
            notes: Some(d.notes.clone()),
            addresses: Some(d.addresses.clone()),
            urls: Some(d.urls.clone()),
            groups: Some(d.groups.clone()),
            dates: Some(d.dates.clone()),
            custom: Some(d.custom.clone()),
        },
        ..Default::default()
    }
}

/// Writes a new contact into `account`, or the changes to `original`, and
/// answers the contact as saved. `demo` is the contact as the demo store
/// should hold it afterwards.
pub async fn save(
    app: &Rc<App>,
    account: mailrs_domain::AccountId,
    original: Option<Contact>,
    mut fields: ContactFields,
    mut demo: Contact,
) -> Result<Contact> {
    if app.core.demo {
        if let Some(bytes) = fields.photo {
            demo.photo_file = None;
            demo.photo_url = None;
            if !bytes.is_empty() {
                let file = format!("demo-{}.png", glib::uuid_string_random());
                let directory = app.core.contacts().photo_dir().to_owned();
                let path = directory.join(&file);
                app.core
                    .call(async move {
                        std::fs::create_dir_all(directory)?;
                        std::fs::write(path, bytes)
                    })
                    .await?;
                demo.photo_url = Some(file.clone());
                demo.photo_file = Some(file);
            }
        }
        let saved = demo.clone();
        app.core
            .write(move |c| address_book::save(c, &[demo]))
            .await?;
        return Ok(saved);
    }
    let book = app.core.contacts();
    if original.is_none() {
        fields.creation_key = Some(glib::uuid_string_random().to_string());
        if fields.address_book.is_none() {
            let books = {
                let service = app.core.contacts();
                app.core
                    .call(async move { service.address_books(account).await })
                    .await?
            };
            // A contact put back after a delete goes to the book that held it.
            let held = books
                .iter()
                .find(|(id, _)| !id.is_empty() && path_of(&demo.resource).starts_with(path_of(id)));
            fields.address_book = held.or(books.first()).map(|(id, _)| id.clone());
        }
    }
    let answer = app
        .core
        .call(async move {
            match original {
                None => book.create(account, &fields, true).await,
                Some(old) => book.update(account, &old.resource, &fields, true).await,
            }
        })
        .await?;
    match answer {
        Permitted::Done(saved) => Ok(saved),
        Permitted::NeedsPermission => Err(anyhow!(gettext(
            "Grant contacts permission in Preferences, then try again."
        ))),
    }
}

/// The path of a CardDAV href, which a server may give whole or from `/`.
fn path_of(href: &str) -> &str {
    href.split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|at| &rest[at..]))
        .unwrap_or(href)
}

/// Deletes `contact` from its account.
pub async fn delete(app: &Rc<App>, contact: Contact) -> Result<()> {
    if app.core.demo {
        app.core
            .write(move |c| address_book::forget(c, contact.account_id, &[contact.resource]))
            .await?;
        return Ok(());
    }
    let book = app.core.contacts();
    app.core
        .call(async move {
            book.delete(
                contact.account_id,
                contact.resource,
                (!contact.details.revision.is_empty()).then_some(contact.details.revision),
            )
            .await
        })
        .await?;
    Ok(())
}

/// The photo bytes of `contact`, read before a delete removes the file.
pub fn photo_of(app: &Rc<App>, contact: &Contact) -> Option<Vec<u8>> {
    let file = contact.photo_file.as_ref()?;
    std::fs::read(app.core.contacts().photo_dir().join(file)).ok()
}

/// What Undo needs to put a deleted contact back: its photo, read before
/// the delete removes the file, and the links the delete takes with it.
pub struct Gone {
    pub contact: Contact,
    pub photo: Option<Vec<u8>>,
    pub links: Vec<Link>,
}

/// Reads what [`restore`] needs, before `contact` is deleted.
pub fn gone(app: &Rc<App>, contact: Contact, links: &[Link]) -> Gone {
    let key = (contact.account_id, contact.resource.clone());
    Gone {
        photo: photo_of(app, &contact),
        links: links
            .iter()
            .filter(|l| l.a == key || l.b == key)
            .cloned()
            .collect(),
        contact,
    }
}

/// Puts a deleted contact back, and its links on the copy the server made.
pub async fn restore(app: &Rc<App>, gone: Gone) -> Result<()> {
    let Gone {
        contact,
        photo,
        links,
    } = gone;
    let old = (contact.account_id, contact.resource.clone());
    let saved = if app.core.demo {
        let saved = contact.clone();
        app.core
            .write(move |c| address_book::save(c, &[contact]))
            .await?;
        saved
    } else {
        let mut fields = fields_of(&contact);
        fields.photo = photo;
        let account = contact.account_id;
        save(app, account, None, fields, contact).await?
    };
    let new = (saved.account_id, saved.resource);
    let links: Vec<Link> = links
        .into_iter()
        .map(|mut l| {
            for side in [&mut l.a, &mut l.b] {
                if *side == old {
                    *side = new.clone();
                }
            }
            l
        })
        .collect();
    if !links.is_empty() {
        app.core
            .write(move |c| links.iter().try_for_each(|l| address_book::link(c, l)))
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_whole_href_and_a_path_compare_by_path() {
        assert_eq!(
            super::path_of("https://dav.example/u/work/c1.vcf"),
            "/u/work/c1.vcf"
        );
        assert_eq!(super::path_of("/u/work/"), "/u/work/");
        assert!(
            super::path_of("https://dav.example/u/work/c1.vcf")
                .starts_with(super::path_of("/u/work/"))
        );
    }
}
