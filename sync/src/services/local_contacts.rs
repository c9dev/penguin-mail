//! An address book kept on this computer, for an account whose server has
//! none. The store holds the cards; this service answers the same calls
//! the other address books do, so Contacts lists, edits and deletes them
//! the same way. The server never sees them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mailrs_domain::AccountId;
use mailrs_domain::translate::gettext;
use mailrs_gmail::{ConnectionsPage, ContactFields, Person};
use mailrs_store::{Db, address_book};

use super::ContactsService;
use crate::BackendError;

#[derive(Clone)]
pub struct LocalContacts {
    db: Db,
    account_id: AccountId,
    /// Photos saved but not yet written to the photo directory, by the
    /// address `contact_photo` is asked for. A contact's photo reaches
    /// disk the way a downloaded one does, in the book's refresh.
    photos: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl LocalContacts {
    pub fn new(db: Db, account_id: AccountId) -> LocalContacts {
        LocalContacts {
            db,
            account_id,
            photos: Arc::default(),
        }
    }

    /// Puts what `fields` names onto `person`.
    fn apply(&self, person: &mut Person, fields: &ContactFields) {
        if let Some(name) = &fields.name {
            person.name = (!name.is_empty()).then(|| name.clone());
        }
        if let Some(emails) = &fields.emails {
            person.emails = emails.clone();
        }
        if let Some(organization) = &fields.organization {
            person.organization = (!organization.is_empty()).then(|| organization.clone());
        }
        if let Some(phones) = &fields.phones {
            person.phone = phones.first().cloned();
            person.details.phones = phones.clone();
        }
        let d = &mut person.details;
        let changes = &fields.details;
        if let Some(v) = &changes.birthday {
            d.birthday = v.clone();
        }
        if let Some(v) = &changes.notes {
            d.notes = v.clone();
        }
        if let Some(v) = &changes.addresses {
            d.addresses = v.clone();
        }
        if let Some(v) = &changes.urls {
            d.urls = v.clone();
        }
        if let Some(v) = &changes.groups {
            d.groups = v.clone();
        }
        if let Some(v) = &changes.dates {
            d.dates = v.clone();
        }
        if let Some(v) = &changes.custom {
            d.custom = v.clone();
        }
        if let Some(bytes) = &fields.photo {
            person.photo_url = None;
            if !bytes.is_empty() {
                let key = format!("local-photo-{:016x}", rand::random::<u64>());
                if let Ok(mut held) = self.photos.lock() {
                    held.insert(key.clone(), bytes.clone());
                }
                person.photo_url = Some(key);
            }
        }
    }
}

fn person_of(contact: address_book::Contact) -> Person {
    Person {
        resource: contact.resource,
        details: contact.details,
        name: contact.name,
        emails: contact.emails,
        photo_url: contact.photo_url,
        organization: contact.organization,
        phone: contact.phone,
    }
}

fn stored(err: mailrs_store::StoreError) -> BackendError {
    BackendError::Refused(err.to_string())
}

impl ContactsService for LocalContacts {
    async fn address_books(&self) -> Result<Vec<(String, String)>, BackendError> {
        Ok(vec![("local".into(), gettext("Contacts"))])
    }

    fn edits_details(&self) -> bool {
        true
    }

    async fn delete_contact(
        &self,
        _resource: &str,
        _revision: Option<&str>,
    ) -> Result<(), BackendError> {
        // The book on this computer is the only copy, and the caller
        // forgets the stored card once this answers.
        Ok(())
    }

    async fn connections(
        &self,
        _page_token: Option<&str>,
        _sync_token: Option<&str>,
    ) -> Result<ConnectionsPage, BackendError> {
        let account_id = self.account_id;
        let held = self
            .db
            .read(address_book::list)
            .await
            .map_err(stored)?;
        Ok(ConnectionsPage {
            people: held
                .into_iter()
                .filter(|c| c.account_id == account_id)
                .map(person_of)
                .collect(),
            ..ConnectionsPage::default()
        })
    }

    async fn contact_photo(&self, url: &str) -> Result<Vec<u8>, BackendError> {
        self.photos
            .lock()
            .ok()
            .and_then(|mut held| held.remove(url))
            .ok_or(BackendError::Unsupported)
    }

    async fn create_contact(&self, fields: &ContactFields) -> Result<Person, BackendError> {
        let mut person = Person {
            resource: format!("local/{:016x}", rand::random::<u64>()),
            ..Person::default()
        };
        person.details.uid = person.resource.clone();
        self.apply(&mut person, fields);
        Ok(person)
    }

    async fn update_contact(
        &self,
        resource: &str,
        fields: &ContactFields,
    ) -> Result<Person, BackendError> {
        let (account_id, key) = (self.account_id, resource.to_string());
        let held = self
            .db
            .read(move |c| address_book::one(c, account_id, &key))
            .await
            .map_err(stored)?
            .ok_or(BackendError::Unsupported)?;
        let mut person = person_of(held);
        self.apply(&mut person, fields);
        Ok(person)
    }
}

#[cfg(test)]
mod tests {
    use mailrs_domain::contact::Changes;
    use mailrs_store::{Db, accounts};

    use super::*;

    async fn book() -> (LocalContacts, Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        let id = db
            .write(|c| accounts::insert_account(c, "dana@home.example", 0))
            .await
            .unwrap();
        (LocalContacts::new(db.clone(), id), db, dir)
    }

    #[tokio::test]
    async fn a_local_book_creates_edits_and_lists_every_field() {
        let (book, db, _dir) = book().await;
        let created = book
            .create_contact(&ContactFields {
                name: Some("Owen Mercer".into()),
                emails: Some(vec!["owen@example.com".into()]),
                phones: Some(vec!["+1 555 0100".into()]),
                organization: Some("Maple".into()),
                photo: Some(vec![1, 2, 3]),
                details: Changes {
                    birthday: Some("1990-04-12".into()),
                    notes: Some("Met in March".into()),
                    urls: Some(vec!["https://example.org".into()]),
                    groups: Some(vec!["Friends".into()]),
                    custom: Some(vec![mailrs_domain::contact::Field {
                        label: "Badge".into(),
                        value: "42".into(),
                    }]),
                    ..Changes::default()
                },
                ..ContactFields::default()
            })
            .await
            .unwrap();
        assert!(created.resource.starts_with("local/"));
        assert_eq!(created.details.groups, ["Friends"]);
        let url = created.photo_url.clone().unwrap();
        assert_eq!(book.contact_photo(&url).await.unwrap(), [1, 2, 3]);
        assert!(
            book.contact_photo(&url).await.is_err(),
            "the bytes leave once they are on disk"
        );

        // The caller stores what a create answers; a later read lists it.
        let account_id = book.account_id;
        let stored = address_book::Contact {
            account_id,
            resource: created.resource.clone(),
            details: created.details.clone(),
            name: created.name.clone(),
            emails: created.emails.clone(),
            organization: created.organization.clone(),
            phone: created.phone.clone(),
            photo_url: None,
            photo_file: None,
        };
        db.write(move |c| address_book::save(c, &[stored]))
            .await
            .unwrap();
        let edited = book
            .update_contact(
                &created.resource,
                &ContactFields {
                    name: Some("Owen M.".into()),
                    details: Changes {
                        groups: Some(Vec::new()),
                        ..Changes::default()
                    },
                    ..ContactFields::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(edited.name.as_deref(), Some("Owen M."));
        assert!(edited.details.groups.is_empty());
        assert_eq!(edited.details.birthday, "1990-04-12", "untouched fields stay");
        let page = book.connections(None, None).await.unwrap();
        assert_eq!(page.people.len(), 1);
        assert_eq!(page.people[0].emails, ["owen@example.com"]);
    }
}
