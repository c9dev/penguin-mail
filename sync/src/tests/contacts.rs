use std::collections::HashMap;
use std::sync::Arc;

use mailrs_gmail::{GmailError, Person};
use mailrs_store::address_book;

use super::{Connected, Harness, harness};
use crate::contacts::{ContactBook, REFRESH_AFTER, Refreshed};
use crate::settings::Permitted;

struct Book {
    book: ContactBook<Connected>,
    _dir: tempfile::TempDir,
}

fn book(h: &Harness) -> Book {
    let dir = tempfile::tempdir().unwrap();
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    Book {
        book: ContactBook::new(
            Arc::new(Connected(connected)),
            h.db.clone(),
            dir.path().join("photos"),
        ),
        _dir: dir,
    }
}

fn person(resource: &str, name: &str, emails: &[&str], photo: Option<&str>) -> Person {
    Person {
        resource: resource.into(),
        name: Some(name.into()),
        emails: emails.iter().map(|e| e.to_string()).collect(),
        photo_url: photo.map(str::to_string),
        organization: Some("Fernwood".into()),
        phone: None,
    }
}

#[tokio::test]
async fn a_refresh_walks_every_page_and_downloads_each_photo_once() {
    let h = harness().await;
    let b = book(&h);
    // The fake pages two at a time, so four contacts take two calls.
    h.fake.with(|s| {
        s.contacts = vec![
            person(
                "people/c1",
                "Mara Okafor",
                &["mara@example.org"],
                Some("p/mara"),
            ),
            person("people/c2", "Theo Lang", &["theo@example.org"], None),
            person(
                "people/c3",
                "Priya Raman",
                &["priya@fernwood.example"],
                None,
            ),
            person(
                "people/c4",
                "Jonas Weber",
                &["jonas@fernwood.example"],
                None,
            ),
        ];
        s.photos.insert("p/mara".into(), b"jpeg".to_vec());
    });

    let refreshed = b.book.refresh(h.account_id).await.unwrap();
    assert_eq!(
        refreshed,
        Permitted::Done(Refreshed {
            contacts: 4,
            photos: 1,
            needs_permission: Vec::new(),
        })
    );
    let mara = b.book.card("MARA@example.org").await.unwrap().unwrap();
    assert_eq!(mara.contact.name.as_deref(), Some("Mara Okafor"));
    assert_eq!(mara.contact.organization.as_deref(), Some("Fernwood"));
    let photo = mara.photo.expect("the photo landed on disk");
    assert_eq!(std::fs::read(&photo).unwrap(), b"jpeg");

    // The second refresh hands back the sync token, which asks for
    // changes alone, and touches no photo.
    let again = b.book.refresh(h.account_id).await.unwrap();
    assert_eq!(
        again,
        Permitted::Done(Refreshed {
            contacts: 0,
            photos: 0,
            needs_permission: Vec::new(),
        })
    );
    assert_eq!(
        h.fake.with(|s| s.usage.calls_to("people.connections.list")),
        3,
        "two pages, then one call with the sync token"
    );
    assert_eq!(std::fs::read(&photo).unwrap(), b"jpeg");
}

#[tokio::test]
async fn contacts_wait_for_the_permission() {
    let h = harness().await;
    let b = book(&h);
    h.fake.fail_next(GmailError::MissingScope);
    assert_eq!(
        b.book.refresh(h.account_id).await.unwrap(),
        Permitted::NeedsPermission
    );
}

/// One account without the permission used to stop the refresh, so every
/// account after it went unread. The harness has one account, so it goes
/// in twice, and Google refuses only the first read.
#[tokio::test]
async fn an_account_without_the_permission_does_not_hold_up_the_rest() {
    let h = harness().await;
    let b = book(&h);
    h.fake.with(|s| {
        s.contacts = vec![person(
            "people/c1",
            "Mara Okafor",
            &["mara@example.org"],
            None,
        )]
    });
    h.fake.fail_next(GmailError::MissingScope);
    let read = b
        .book
        .refresh_stale(&[h.account_id, h.account_id], crate::now_millis())
        .await
        .unwrap();
    assert_eq!(read.needs_permission, vec![h.account_id]);
    assert_eq!(read.contacts, 1);
}

#[tokio::test]
async fn an_expired_sync_token_reads_the_whole_address_book_again() {
    let h = harness().await;
    let b = book(&h);
    h.fake.with(|s| {
        s.contacts = vec![person(
            "people/c1",
            "Mara Okafor",
            &["mara@example.org"],
            None,
        )]
    });
    b.book.refresh(h.account_id).await.unwrap();

    h.fake.fail_next(GmailError::ExpiredSyncToken);
    h.fake.with(|s| {
        s.contacts = vec![person(
            "people/c9",
            "Theo Lang",
            &["theo@example.org"],
            None,
        )]
    });
    let refreshed = b.book.refresh(h.account_id).await.unwrap();
    assert_eq!(
        refreshed,
        Permitted::Done(Refreshed {
            contacts: 1,
            photos: 0,
            needs_permission: Vec::new(),
        })
    );
    // Reading it all again replaces what the stale token would have kept.
    assert!(b.book.card("mara@example.org").await.unwrap().is_none());
    assert!(b.book.card("theo@example.org").await.unwrap().is_some());
}

/// Reading the whole address book again after the token expired can fail
/// part way. The book keeps what it held rather than the pages that made
/// it through.
#[tokio::test]
async fn a_read_again_that_fails_part_way_keeps_the_address_book() {
    let h = harness().await;
    let b = book(&h);
    let three = vec![
        person("people/c1", "Mara Okafor", &["mara@example.org"], None),
        person("people/c2", "Theo Lang", &["theo@example.org"], None),
        person("people/c3", "Priya Raman", &["priya@example.org"], None),
    ];
    h.fake.with(|s| s.contacts = three);
    b.book.refresh(h.account_id).await.unwrap();

    // The stored token has expired; the first page of the new read comes
    // back and the second fails.
    h.fake.fail_next(GmailError::ExpiredSyncToken);
    let mut expired = h.fake.hold("people.connections.list");
    let refreshing = b.book.refresh(h.account_id);
    let failing = async {
        expired.entered().await;
        let mut first = h.fake.hold("people.connections.list");
        expired.release();
        first.entered().await;
        let mut second = h.fake.hold("people.connections.list");
        first.release();
        second.entered().await;
        h.fake.fail_next(GmailError::Network("down".into()));
        second.release();
    };
    let (refreshed, ()) = tokio::join!(refreshing, failing);

    assert!(refreshed.is_err(), "{refreshed:?}");
    for email in ["mara@example.org", "theo@example.org", "priya@example.org"] {
        assert!(
            b.book.card(email).await.unwrap().is_some(),
            "{email} is still in the book"
        );
    }
}

#[tokio::test]
async fn a_fresh_address_book_is_left_alone() {
    let h = harness().await;
    let b = book(&h);
    h.fake.with(|s| {
        s.contacts = vec![person(
            "people/c1",
            "Mara Okafor",
            &["mara@example.org"],
            None,
        )]
    });
    b.book.refresh(h.account_id).await.unwrap();
    let calls = h.fake.with(|s| s.usage.calls_to("people.connections.list"));

    let now = crate::now_millis();
    b.book.refresh_stale(&[h.account_id], now).await.unwrap();
    assert_eq!(
        h.fake.with(|s| s.usage.calls_to("people.connections.list")),
        calls,
        "a book read minutes ago is not read again"
    );

    b.book
        .refresh_stale(&[h.account_id], now + REFRESH_AFTER + 1)
        .await
        .unwrap();
    assert!(h.fake.with(|s| s.usage.calls_to("people.connections.list")) > calls);
}

#[tokio::test]
async fn turning_contacts_off_leaves_nothing_behind() {
    let h = harness().await;
    let b = book(&h);
    h.fake.with(|s| {
        s.contacts = vec![person(
            "people/c1",
            "Mara Okafor",
            &["mara@example.org"],
            Some("p/mara"),
        )];
        s.photos.insert("p/mara".into(), b"jpeg".to_vec());
    });
    b.book.refresh(h.account_id).await.unwrap();
    let photo = b
        .book
        .card("mara@example.org")
        .await
        .unwrap()
        .unwrap()
        .photo
        .unwrap();

    b.book.forget(h.account_id).await.unwrap();
    assert!(!photo.exists());
    assert!(b.book.card("mara@example.org").await.unwrap().is_none());
    let left = h.db.read(address_book::list).await.unwrap();
    assert!(left.is_empty());
}
