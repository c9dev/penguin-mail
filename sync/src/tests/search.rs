//! The Gmail search language `FakeGmail` answers.

use mailrs_domain::MessageMeta;

use crate::api::GmailApi;
use crate::fake::{FakeGmail, meta};
use crate::now_millis;

const DAY: i64 = 24 * 60 * 60 * 1000;

/// A mailbox with one message per corner of the search language.
async fn mailbox() -> FakeGmail {
    let fake = FakeGmail::new();
    let now = now_millis();
    fake.with(|s| s.page_size = 100);
    fake.seed(meta("inbox", "t1", now, &["INBOX", "UNREAD"]));
    fake.seed(meta("archived", "t2", now - DAY, &["Label_1"]));
    fake.seed(meta("junk", "t3", now - 2 * DAY, &["SPAM"]));
    fake.seed(meta("binned", "t4", now - 3 * DAY, &["TRASH"]));
    fake.seed(meta("ancient", "t5", now - 400 * DAY, &["Label_1"]));
    fake.with(|s| {
        let big = s.messages.get_mut("archived").expect("seeded");
        big.size = 4 * 1024 * 1024;
        big.has_attachments = true;
        let other = s.messages.get_mut("ancient").expect("seeded");
        other.subject = "Lunch on Thursday".into();
        other.from = Some(mailrs_domain::Address {
            name: Some("Bo".into()),
            email: "bo@example.org".into(),
        });
    });
    fake
}

async fn found(fake: &FakeGmail, query: &str) -> Vec<String> {
    fake.list_messages(query, None)
        .await
        .expect("the search runs")
        .messages
        .into_iter()
        .map(|m| m.id)
        .collect()
}

#[tokio::test]
async fn spam_and_trash_stay_out_until_a_search_asks_for_them() {
    let fake = mailbox().await;
    assert_eq!(found(&fake, "").await, ["inbox", "archived", "ancient"]);
    assert_eq!(found(&fake, "in:spam").await, ["junk"]);
    assert_eq!(found(&fake, "in:trash").await, ["binned"]);
    assert_eq!(
        found(&fake, "-in:spam -in:trash").await,
        ["inbox", "archived", "ancient"]
    );
    assert_eq!(found(&fake, "in:anywhere").await.len(), 5);
}

#[tokio::test]
async fn the_archive_search_finds_received_mail_outside_the_inbox() {
    let fake = mailbox().await;
    fake.seed(meta("mine", "t6", now_millis(), &["SENT"]));
    assert_eq!(
        found(&fake, mailrs_domain::Folder::Archive.query()).await,
        ["archived", "ancient"]
    );
}

#[tokio::test]
async fn braces_hold_any_one_term_and_a_dash_holds_none() {
    let fake = mailbox().await;
    // The window query: recent mail, plus everything in the inbox.
    assert_eq!(
        found(&fake, "{newer_than:30d in:inbox}").await,
        ["inbox", "archived"]
    );
    assert_eq!(found(&fake, "-is:unread").await, ["archived", "ancient"]);
    assert_eq!(found(&fake, "is:unread").await, ["inbox"]);
}

#[tokio::test]
async fn a_term_names_a_field_a_label_a_size_or_an_age() {
    let fake = mailbox().await;
    assert_eq!(found(&fake, "from:bo@example.org").await, ["ancient"]);
    assert_eq!(found(&fake, "subject:lunch").await, ["ancient"]);
    assert_eq!(found(&fake, "\"lunch on thursday\"").await, ["ancient"]);
    assert_eq!(found(&fake, "label:label_1").await, ["archived", "ancient"]);
    assert_eq!(found(&fake, "has:attachment").await, ["archived"]);
    assert_eq!(found(&fake, "larger:1M").await, ["archived"]);
    assert_eq!(found(&fake, "older_than:90d").await, ["ancient"]);
    // A term the fake does not know matches its value as a word.
    assert!(found(&fake, "after:2020/01/01").await.is_empty());
}

#[tokio::test]
async fn a_listing_pages_the_newest_first() {
    let fake = mailbox().await;
    fake.with(|s| s.page_size = 2);
    let first = fake.list_messages("", None).await.unwrap();
    let ids: Vec<&str> = first.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["inbox", "archived"]);
    let second = fake
        .list_messages("", first.next_page_token.as_deref())
        .await
        .unwrap();
    assert_eq!(second.messages.len(), 1);
    assert!(second.next_page_token.is_none());
}

#[tokio::test]
async fn a_message_reads_back_as_the_mail_it_stands_for() {
    let fake = mailbox().await;
    let stored: MessageMeta = fake.message_metadata("inbox").await.unwrap();
    assert_eq!(stored.thread_id, "t1");
    let raw = String::from_utf8(fake.raw_message("inbox").await.unwrap()).unwrap();
    assert!(raw.contains("From: ann@example.com"), "{raw}");
    assert!(raw.contains("Subject: Subject inbox"), "{raw}");
}
