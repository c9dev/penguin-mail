use std::collections::HashMap;
use std::sync::Arc;

use mailrs_gmail::{ContactFields, Person};
use mailrs_graph::{ContactFolder, EmailAddress, GraphContact};
use mailrs_store::address_book;

use super::outlook;
use crate::contacts::ContactBook;
use crate::tests::Connected;
use crate::{AnyContacts, BackendError, ContactsService};

fn ann() -> GraphContact {
    GraphContact {
        id: "c1".into(),
        display_name: Some("Ann Lee".into()),
        email_addresses: vec![EmailAddress { name: None, address: Some("ann@example.com".into()) }],
        company_name: Some("Contoso".into()),
        mobile_phone: Some("+351 900 000 000".into()),
        ..GraphContact::default()
    }
}

async fn read_whole(contacts: &AnyContacts) -> (Vec<Person>, Vec<String>, String) {
    let (mut people, mut deleted, mut page) = (Vec::new(), Vec::new(), None::<String>);
    loop {
        let got = contacts.connections(page.as_deref(), None).await.unwrap();
        people.extend(got.people);
        deleted.extend(got.deleted);
        match (got.next_page_token, got.next_sync_token) {
            (Some(next), _) => page = Some(next),
            (None, Some(sync)) => return (people, deleted, sync),
            (None, None) => panic!("a contacts page with neither token"),
        }
    }
}

#[tokio::test]
async fn contacts_from_every_folder_come_as_people() {
    let h = outlook().await;
    h.fake.put_contact("contacts-1", ann());
    h.fake.with(|s| s.contact_folders.push(ContactFolder { id: "work".into(), display_name: "Work".into() }));
    h.fake.put_contact("work", GraphContact { id: "c2".into(), display_name: Some("Bo".into()), ..GraphContact::default() });
    let contacts = h.sync.services().contacts.clone().unwrap();
    let (people, _, _) = read_whole(&contacts).await;
    let ann = people.iter().find(|p| p.resource == "c1").unwrap();
    assert_eq!(ann.name.as_deref(), Some("Ann Lee"));
    assert_eq!(ann.emails, ["ann@example.com"]);
    assert_eq!((ann.organization.as_deref(), ann.phone.as_deref()), (Some("Contoso"), Some("+351 900 000 000")));
    assert_eq!(ann.photo_url.as_deref(), Some("graph:contact/c1"));
    assert!(people.iter().any(|p| p.resource == "c2"));
}

#[tokio::test]
async fn a_second_read_brings_only_what_changed() {
    let h = outlook().await;
    h.fake.put_contact("contacts-1", ann());
    let contacts = h.sync.services().contacts.clone().unwrap();
    let (_, _, sync) = read_whole(&contacts).await;
    h.fake.remove_contact("c1");
    let got = contacts.connections(None, Some(&sync)).await.unwrap();
    assert_eq!(got.deleted, ["c1"]);
    assert!(got.people.is_empty());
}

#[tokio::test]
async fn a_contact_without_a_photo_answers_empty() {
    let h = outlook().await;
    h.fake.put_contact("contacts-1", ann());
    let contacts = h.sync.services().contacts.clone().unwrap();
    assert!(contacts.contact_photo("graph:contact/c1").await.unwrap().is_empty());
    h.fake.with(|s| s.photos.insert("c1".into(), b"jpeg".to_vec()));
    assert_eq!(contacts.contact_photo("graph:contact/c1").await.unwrap(), b"jpeg");
    assert!(
        matches!(contacts.contact_photo("https://people.googleapis.com/x").await, Err(BackendError::NotFound)),
        "only its own names"
    );
}

#[tokio::test]
async fn adding_and_changing_a_contact_write_graphs_fields() {
    let h = outlook().await;
    let contacts = h.sync.services().contacts.clone().unwrap();
    let made = contacts
        .create_contact(&ContactFields {
            name: Some("Cy".into()),
            emails: Some(vec!["cy@example.com".into()]),
            ..ContactFields::default()
        })
        .await
        .unwrap();
    assert_eq!(made.emails, ["cy@example.com"]);
    let changed = contacts
        .update_contact(&made.resource, &ContactFields { organization: Some("Fabrikam".into()), ..ContactFields::default() })
        .await
        .unwrap();
    assert_eq!(changed.organization.as_deref(), Some("Fabrikam"));
    assert_eq!(changed.emails, ["cy@example.com"], "a change touches only what it names");
}

#[tokio::test]
async fn a_delta_link_graph_refuses_reads_the_book_whole_once() {
    let h = outlook().await;
    h.fake.put_contact("contacts-1", ann());
    let dir = tempfile::tempdir().unwrap();
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    let book = ContactBook::new(Arc::new(Connected(connected)), h.db.clone(), dir.path().join("photos"));
    book.refresh(h.account_id).await.unwrap();
    h.fake.put_contact("contacts-1", GraphContact { id: "c2".into(), display_name: Some("Bo".into()), ..GraphContact::default() });
    h.fake.expire_links();
    book.refresh(h.account_id).await.unwrap();
    let stored = h.db.read(address_book::list).await.unwrap();
    let mut ids: Vec<_> = stored.iter().map(|c| c.resource.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["c1", "c2"]);
}
