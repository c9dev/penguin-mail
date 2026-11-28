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
        email_addresses: vec![EmailAddress {
            name: None,
            address: Some("ann@example.com".into()),
        }],
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
    h.fake.with(|s| {
        s.contact_folders.push(ContactFolder {
            id: "work".into(),
            display_name: "Work".into(),
        })
    });
    h.fake.put_contact(
        "work",
        GraphContact {
            id: "c2".into(),
            display_name: Some("Bo".into()),
            ..GraphContact::default()
        },
    );
    let contacts = h.sync.services().contacts.clone().unwrap();
    let (people, _, _) = read_whole(&contacts).await;
    let ann = people.iter().find(|p| p.resource == "c1").unwrap();
    assert_eq!(ann.name.as_deref(), Some("Ann Lee"));
    assert_eq!(ann.emails, ["ann@example.com"]);
    assert_eq!(
        (ann.organization.as_deref(), ann.phone.as_deref()),
        (Some("Contoso"), Some("+351 900 000 000"))
    );
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
    assert!(
        contacts
            .contact_photo("graph:contact/c1")
            .await
            .unwrap()
            .is_empty()
    );
    h.fake
        .with(|s| s.photos.insert("c1".into(), b"jpeg".to_vec()));
    assert_eq!(
        contacts.contact_photo("graph:contact/c1").await.unwrap(),
        b"jpeg"
    );
    assert!(
        matches!(
            contacts
                .contact_photo("https://people.googleapis.com/x")
                .await,
            Err(BackendError::NotFound)
        ),
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
        .update_contact(
            &made.resource,
            &ContactFields {
                organization: Some("Fabrikam".into()),
                ..ContactFields::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(changed.organization.as_deref(), Some("Fabrikam"));
    assert_eq!(
        changed.emails,
        ["cy@example.com"],
        "a change touches only what it names"
    );
    assert_eq!(
        changed.name.as_deref(),
        Some("Cy"),
        "Outlook would name the contact after its company"
    );
}

#[tokio::test]
async fn a_delta_link_graph_refuses_reads_the_book_whole_once() {
    let h = outlook().await;
    h.fake.put_contact("contacts-1", ann());
    let dir = tempfile::tempdir().unwrap();
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    let book = ContactBook::new(
        Arc::new(Connected(connected)),
        h.db.clone(),
        dir.path().join("photos"),
    );
    book.refresh(h.account_id).await.unwrap();
    h.fake.put_contact(
        "contacts-1",
        GraphContact {
            id: "c2".into(),
            display_name: Some("Bo".into()),
            ..GraphContact::default()
        },
    );
    h.fake.expire_links();
    book.refresh(h.account_id).await.unwrap();
    let stored = h.db.read(address_book::list).await.unwrap();
    let mut ids: Vec<_> = stored.iter().map(|c| c.resource.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["c1", "c2"]);
}

#[tokio::test]
async fn editing_the_numbers_keeps_a_home_number_at_home() {
    let h = outlook().await;
    h.fake.put_contact(
        "contacts-1",
        GraphContact {
            home_phones: vec!["+351 210 000 000".into()],
            ..ann()
        },
    );
    let contacts = h.sync.services().contacts.clone().unwrap();
    let changed = contacts
        .update_contact(
            "c1",
            &ContactFields {
                phones: Some(vec![
                    "+351 900 000 000".into(),
                    "+351 210 000 000".into(),
                    "+351 220 000 000".into(),
                ]),
                ..ContactFields::default()
            },
        )
        .await
        .unwrap();
    let held = h.fake.with(|s| s.contacts.get("c1").unwrap().1.clone());
    assert_eq!(held.home_phones, ["+351 210 000 000"]);
    assert_eq!(held.business_phones, ["+351 220 000 000"]);
    assert_eq!(changed.details.phones.len(), 3);
}

#[tokio::test]
async fn every_contact_field_goes_to_graph_and_comes_back() {
    use mailrs_domain::contact::{Changes, Field};
    let h = outlook().await;
    let contacts = h.sync.services().contacts.clone().unwrap();
    let address = |street: &str, city: &str| -> [String; 7] {
        [
            "Box 7".into(),
            "Floor 2".into(),
            street.into(),
            city.into(),
            "LX".into(),
            "1000-001".into(),
            "Portugal".into(),
        ]
    };
    let made = contacts
        .create_contact(&ContactFields {
            name: Some("Priya Raman".into()),
            emails: Some(vec!["priya@fernwood.example".into()]),
            photo: Some(b"\x89PNG-bytes".to_vec()),
            details: Changes {
                birthday: Some("1990-04-12".into()),
                notes: Some("Met in March".into()),
                urls: Some(vec!["https://one.example".into(), "https://two.example".into()]),
                groups: Some(vec!["Friends".into()]),
                dates: Some(vec![Field {
                    label: "Anniversary".into(),
                    value: "2015-06-20".into(),
                }]),
                custom: Some(vec![Field {
                    label: "Badge".into(),
                    value: "42".into(),
                }]),
                addresses: Some(vec![
                    address("1 Main Street", "Lisbon"),
                    address("2 Side Street", "Porto"),
                    address("3 Back Street", "Faro"),
                    address("4 Far Street", "Braga"),
                ]),
            },
            ..ContactFields::default()
        })
        .await
        .unwrap();
    assert_eq!(h.fake.with(|s| s.photos.get(&made.resource).cloned()), Some(b"\x89PNG-bytes".to_vec()));
    let (people, _, _) = read_whole(&contacts).await;
    let read = people.iter().find(|p| p.resource == made.resource).unwrap();
    let d = &read.details;
    assert_eq!(d.birthday, "1990-04-12");
    assert_eq!(d.notes, "Met in March");
    assert_eq!(d.urls, ["https://one.example", "https://two.example"]);
    assert_eq!(d.groups, ["Friends"]);
    assert_eq!(d.dates[0].value, "2015-06-20");
    assert_eq!(d.custom[0].label, "Badge");
    assert_eq!(d.addresses.len(), 4, "three in Graph's slots and one in the extras");
    assert_eq!(d.addresses[0][0], "Box 7", "the PO box rides in the extras");
    assert_eq!(d.addresses[2][3], "Faro");
    assert_eq!(d.addresses[3][3], "Braga");
    // An edit of one field keeps the others, extras included.
    let edited = contacts
        .update_contact(
            &made.resource,
            &ContactFields {
                details: Changes {
                    custom: Some(vec![]),
                    ..Changes::default()
                },
                ..ContactFields::default()
            },
        )
        .await
        .unwrap();
    assert!(edited.details.custom.is_empty());
    assert_eq!(edited.details.dates[0].label, "Anniversary");
    assert_eq!(edited.details.urls.len(), 2);
    assert_eq!(edited.details.addresses.len(), 4);
    contacts.delete_contact(&made.resource, None).await.unwrap();
    let (after, _, _) = read_whole(&contacts).await;
    assert!(after.iter().all(|p| p.resource != made.resource));
}
