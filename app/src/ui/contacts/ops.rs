//! What Contacts writes: saving, deleting and putting back a contact in
//! its account. The demo keeps contacts in the store alone.

use std::rc::Rc;

use anyhow::{Result, anyhow};
use gtk::glib;
use mailrs_domain::contact::Changes;
use mailrs_domain::translate::gettext;
use mailrs_gmail::ContactFields;
use mailrs_store::address_book::{self, Contact};
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

/// Writes a new contact into `account`, or the changes to `original`.
/// `demo` is the contact as the demo store should hold it afterwards.
pub async fn save(
    app: &Rc<App>,
    account: mailrs_domain::AccountId,
    original: Option<Contact>,
    mut fields: ContactFields,
    mut demo: Contact,
) -> Result<()> {
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
        app.core
            .write(move |c| address_book::save(c, &[demo]))
            .await?;
        return Ok(());
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
            fields.address_book = books.first().map(|(id, _)| id.clone());
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
        Permitted::Done(_) => Ok(()),
        Permitted::NeedsPermission => Err(anyhow!(gettext(
            "Grant contacts permission in Preferences, then try again."
        ))),
    }
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

/// Puts a deleted contact back with the photo `photo_of` read.
pub async fn restore(app: &Rc<App>, contact: Contact, photo: Option<Vec<u8>>) -> Result<()> {
    if app.core.demo {
        app.core
            .write(move |c| address_book::save(c, &[contact]))
            .await?;
        return Ok(());
    }
    let mut fields = fields_of(&contact);
    fields.photo = photo;
    let account = contact.account_id;
    save(app, account, None, fields, contact).await
}
