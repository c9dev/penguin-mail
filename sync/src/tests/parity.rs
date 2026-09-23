//! Rows of the Gmail parity table in the neutral core spec that no other
//! sync test held. They pin what the code does before the services move
//! under it, and they must keep passing after.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::Address;
use mailrs_gmail::{CONTACTS_WRITE_SCOPE, ContactFields, SendAs};
use mailrs_store::address_book;

use super::{Connected, Harness, harness};
use crate::contacts::ContactBook;
use crate::fake::meta;
use crate::settings::Permitted;
use crate::{SendAsAddress, now_millis};

const DAY: i64 = 24 * 60 * 60 * 1000;

fn book(h: &Harness, dir: &tempfile::TempDir) -> ContactBook<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    ContactBook::new(
        Arc::new(Connected(connected)),
        h.db.clone(),
        dir.path().join("photos"),
    )
}

fn settings(h: &Harness) -> crate::AccountSettings<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    crate::AccountSettings::new(Arc::new(Connected(connected)), h.db.clone())
}

fn alias(email: &str, name: &str, signature: &str, status: &str) -> SendAs {
    SendAs {
        send_as_email: email.into(),
        display_name: name.into(),
        is_default: false,
        is_primary: false,
        signature: signature.into(),
        verification_status: Some(status.into()),
    }
}

#[tokio::test]
async fn a_contact_is_created_then_changed_and_kept() {
    let h = harness().await;
    let dir = tempfile::tempdir().unwrap();
    let book = book(&h, &dir);
    let made = book
        .create(
            h.account_id,
            &ContactFields {
                name: Some("Ana Lopes".into()),
                emails: Some(vec!["ana@example.com".into()]),
                ..ContactFields::default()
            },
            true,
        )
        .await
        .unwrap()
        .done()
        .expect("the fake grants every permission");
    assert_eq!(made.name.as_deref(), Some("Ana Lopes"));

    let changed = book
        .update(
            h.account_id,
            &made.resource,
            &ContactFields {
                organization: Some("Fernwood".into()),
                ..ContactFields::default()
            },
            true,
        )
        .await
        .unwrap()
        .done()
        .expect("the fake grants every permission");
    assert_eq!(changed.name.as_deref(), Some("Ana Lopes"));
    assert_eq!(changed.organization.as_deref(), Some("Fernwood"));

    let stored = h.db.read(address_book::list).await.unwrap();
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].resource, made.resource);
    assert_eq!(stored[0].organization.as_deref(), Some("Fernwood"));
    assert_eq!(stored[0].emails, ["ana@example.com"]);
}

#[tokio::test]
async fn creating_a_contact_waits_for_the_permission() {
    let h = harness().await;
    let dir = tempfile::tempdir().unwrap();
    h.fake.withhold(CONTACTS_WRITE_SCOPE);
    let made = book(&h, &dir)
        .create(
            h.account_id,
            &ContactFields {
                name: Some("Ana Lopes".into()),
                ..ContactFields::default()
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(made, Permitted::NeedsPermission);
    assert!(h.fake.with(|s| s.contacts.is_empty()));
}

#[tokio::test]
async fn send_as_lists_confirmed_addresses_with_their_names_and_signatures() {
    let h = harness().await;
    h.fake.with(|s| {
        s.send_as.push(alias(
            "hello@fernwood.example",
            "Fernwood",
            "<p>Fernwood Studio<br>hello@fernwood.example</p>",
            "accepted",
        ));
        s.send_as
            .push(alias("old@fernwood.example", "Old", "", "pending"));
    });
    let addresses = settings(&h).send_as(h.account_id).await.unwrap();
    assert_eq!(
        addresses,
        vec![
            SendAsAddress {
                email: "me@example.com".into(),
                name: Some("Me".into()),
                signature: String::new(),
                default: true,
            },
            SendAsAddress {
                email: "hello@fernwood.example".into(),
                name: Some("Fernwood".into()),
                signature: "Fernwood Studio\nhello@fernwood.example".into(),
                default: false,
            },
        ],
        "an alias nobody confirmed is left out, since Gmail would refuse it"
    );
}

#[tokio::test]
async fn a_search_reaches_gmail_as_typed() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("old", "t1", now - 400 * DAY, &["Label_1"]));
    h.fake.seed(meta("new", "t2", now, &["INBOX"]));
    h.fake.with(|s| {
        let old = s.messages.get_mut("old").expect("seeded");
        old.has_attachments = true;
        old.from = Some(Address {
            name: None,
            email: "bo@example.org".into(),
        });
    });
    let found = h
        .sync
        .search_ids("from:bo@example.org has:attachment older_than:90d", 10)
        .await
        .unwrap();
    let ids: Vec<&str> = found.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["old"]);
}
