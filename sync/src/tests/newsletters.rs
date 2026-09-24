//! The newsletters list: what the store already knows, and the one fetch
//! that fills in mail stored before the unsubscribe headers were.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::{Address, MessageMeta};
use mailrs_gmail::labels;
use mailrs_store::messages;

use super::{Connected, Harness, harness};
use crate::fake::meta;
use crate::{Newsletters, now_millis};

const SHOP: &str = "<https://shop.example/u/9>";
const WEEKLY: &str = "<mailto:leave@weekly.example>";

/// A message from `email` in Promotions, carrying `header`.
fn from(email: &str, id: &str, thread: &str, header: &str) -> MessageMeta {
    MessageMeta {
        from: Some(Address {
            name: Some("News".into()),
            email: email.into(),
        }),
        list_unsubscribe: Some(header.into()),
        one_click: header.starts_with("<https"),
        ..meta(
            id,
            thread,
            now_millis(),
            &[labels::INBOX, labels::CATEGORY_PROMOTIONS],
        )
    }
}

fn newsletters(h: &Harness) -> Newsletters<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    Newsletters::new(Arc::new(Connected(connected)), h.db.clone())
}

/// Forgets what the store holds about a message's unsubscribe headers, so
/// it reads as a row stored before those headers were fetched.
async fn forget_header(h: &Harness, message_id: &str) {
    let (account_id, message_id) = (h.account_id, message_id.to_string());
    h.db.write(move |c| messages::set_unsubscribe(c, account_id, &message_id, None, false))
        .await
        .unwrap();
}

#[tokio::test]
async fn mail_stored_before_the_headers_costs_one_fetch_per_sender() {
    let h = harness().await;
    for (id, thread) in [("s1", "ts1"), ("s2", "ts2"), ("s3", "ts3")] {
        h.fake.seed(from("news@shop.example", id, thread, SHOP));
    }
    for (id, thread) in [("w1", "tw1"), ("w2", "tw2"), ("w3", "tw3")] {
        h.fake
            .seed(from("letter@weekly.example", id, thread, WEEKLY));
    }
    h.bootstrap_all().await;
    for id in ["s1", "s2", "s3"] {
        forget_header(&h, id).await;
    }
    h.fake.reset_usage();

    let found = newsletters(&h).list(h.account_id).await.unwrap();

    assert_eq!(
        h.fake.usage().calls_to("users.messages.get"),
        1,
        "only the sender with nothing stored, and only their newest message"
    );
    let shop = found
        .iter()
        .find(|s| s.email == "news@shop.example")
        .expect("{found:?}");
    assert_eq!(shop.messages, 3);
    assert_eq!(shop.header.as_deref(), Some(SHOP), "the fetch filled it in");
    assert!(shop.one_click);
    let weekly = found
        .iter()
        .find(|s| s.email == "letter@weekly.example")
        .expect("{found:?}");
    assert_eq!(weekly.header.as_deref(), Some(WEEKLY));
    assert!(!weekly.one_click);

    h.fake.reset_usage();
    let again = newsletters(&h).list(h.account_id).await.unwrap();
    assert_eq!(
        h.fake.usage().calls_to("users.messages.get"),
        0,
        "the header was kept, so the second listing asks Gmail nothing"
    );
    assert_eq!(again, found);
}

#[tokio::test]
async fn a_sender_with_too_little_mail_is_listed_without_a_fetch() {
    let h = harness().await;
    for (id, thread) in [("s1", "ts1"), ("s2", "ts2")] {
        h.fake.seed(from("news@shop.example", id, thread, SHOP));
    }
    h.bootstrap_all().await;
    for id in ["s1", "s2"] {
        forget_header(&h, id).await;
    }
    h.fake.reset_usage();

    let found = newsletters(&h).list(h.account_id).await.unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].messages, 2);
    assert_eq!(found[0].header, None);
    assert_eq!(h.fake.usage().calls, 0, "two messages are not a list yet");
}
