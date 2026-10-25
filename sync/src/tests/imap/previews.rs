//! List previews read bodies without opening or marking their messages.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::{MessageBody, Protection, Target};
use mailrs_store::{bodies, messages, threads};

use super::{days_ago, message};
use crate::Mailboxes;
use crate::fake::FakeImap;
use crate::tests::{Connected, ImapHarness, gmail_imap_harness_on, imap_harness};

fn lists(h: &ImapHarness) -> Mailboxes<Connected> {
    Mailboxes::new(
        Arc::new(Connected(HashMap::from([(
            h.account_id,
            Arc::clone(&h.sync),
        )]))),
        h.db.clone(),
    )
}

async fn target(h: &ImapHarness, id: &str) -> Target {
    Target::thread(h.account_id, h.thread_of(id).await.unwrap())
}

#[tokio::test]
async fn a_preview_is_kept_without_marking_read_and_survives_header_refresh() {
    let h = imap_harness().await;
    h.imap
        .deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(1));
    h.bootstrap().await;
    let target = target(&h, "INBOX/1001/1").await;
    let lists = lists(&h);
    assert_eq!(
        lists.preview(target.clone()).await.unwrap().as_deref(),
        Some("Hi.")
    );
    let calls = h.imap.calls();
    assert!(h.stored("INBOX/1001/1").await.unwrap().is_unread());
    assert!(!calls.iter().any(|call| call.starts_with("store ")));
    assert_eq!(
        lists.preview(target.clone()).await.unwrap().as_deref(),
        Some("Hi.")
    );
    assert_eq!(h.imap.calls(), calls, "a saved preview needs no server");

    let mut meta = h.stored("INBOX/1001/1").await.unwrap();
    meta.snippet.clear();
    h.db.write(move |c| {
        messages::apply(
            c,
            meta.account_id,
            &[messages::Change::UpsertLocal {
                meta: Box::new(meta),
                generation: 2,
                links: Default::default(),
            }],
        )
    })
    .await
    .unwrap();
    assert_eq!(h.stored("INBOX/1001/1").await.unwrap().snippet, "Hi.");
    let row =
        h.db.read(move |c| threads::get_thread(c, target.account_id, &target.thread_id))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.snippet, "Hi.");
    assert!(row.unread);
}

#[tokio::test]
async fn conversations_preview_the_latest_message_but_message_rows_keep_their_own() {
    let h = imap_harness().await;
    let first = String::from_utf8(message("a", "Kites", ""))
        .unwrap()
        .replace("Hi.", "First body.");
    let second = String::from_utf8(message(
        "b",
        "Re: Kites",
        "In-Reply-To: <a@example.com>\r\nReferences: <a@example.com>\r\n",
    ))
    .unwrap()
    .replace("Hi.", "Second body.");
    h.imap
        .deliver_flagged("INBOX", first.as_bytes(), &[], days_ago(1));
    h.imap
        .deliver_flagged("INBOX", second.as_bytes(), &[], days_ago(1));
    h.bootstrap().await;
    let target = target(&h, "INBOX/1001/1").await;
    assert_eq!(
        h.thread_of("INBOX/1001/2").await.as_deref(),
        Some(target.thread_id.as_str())
    );
    let lists = lists(&h);
    assert_eq!(
        lists.preview(target.clone()).await.unwrap().as_deref(),
        Some("Second body.")
    );
    assert_eq!(
        h.imap.calls_to("body"),
        1,
        "only the latest body is fetched"
    );
    let single = Target {
        message_id: Some("INBOX/1001/1".into()),
        ..target.clone()
    };
    assert_eq!(
        lists.preview(single).await.unwrap().as_deref(),
        Some("First body.")
    );
    let row =
        h.db.read(move |c| threads::get_thread(c, target.account_id, &target.thread_id))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        row.snippet, "Second body.",
        "an earlier message never replaces the conversation preview"
    );
}

#[tokio::test]
async fn cached_html_is_used_without_a_fetch_and_encrypted_text_is_not_saved() {
    let h = imap_harness().await;
    h.imap
        .deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(1));
    h.imap
        .deliver_flagged("INBOX", &message("b", "Secret", ""), &[], days_ago(1));
    h.bootstrap().await;
    let account = h.account_id;
    h.db.write(move |c| {
        bodies::put_body(c, account, "INBOX/1001/1", &MessageBody {
            html: Some("<style>hidden</style><p>Olá &amp; ahoj</p><img src=\"https://example.org/pixel\">".into()),
            ..MessageBody::default()
        }, 0)?;
        bodies::put_body(c, account, "INBOX/1001/2", &MessageBody {
            text: Some("Private decrypted content".into()),
            protection: Some(Protection::Encrypted),
            ..MessageBody::default()
        }, 0)
    }).await.unwrap();
    let calls = h.imap.calls();
    let lists = lists(&h);
    assert_eq!(
        lists
            .preview(target(&h, "INBOX/1001/1").await)
            .await
            .unwrap()
            .as_deref(),
        Some("Olá & ahoj")
    );
    assert_eq!(
        lists
            .preview(target(&h, "INBOX/1001/2").await)
            .await
            .unwrap()
            .as_deref(),
        Some("")
    );
    assert_eq!(h.imap.calls(), calls);
    assert!(h.stored("INBOX/1001/2").await.unwrap().snippet.is_empty());
}

#[tokio::test]
async fn a_gone_target_does_not_fetch_and_provider_previews_can_still_be_cleared() {
    let h = imap_harness().await;
    let lists = lists(&h);
    assert_eq!(
        lists
            .preview(Target::thread(h.account_id, "gone"))
            .await
            .unwrap(),
        None
    );
    assert!(h.imap.calls().is_empty());
    h.imap
        .deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(1));
    h.bootstrap().await;
    let target = target(&h, "INBOX/1001/1").await;
    lists.preview(target).await.unwrap();
    let mut meta = h.stored("INBOX/1001/1").await.unwrap();
    meta.snippet.clear();
    h.db.write(move |c| {
        messages::apply(
            c,
            meta.account_id,
            &[messages::Change::Upsert {
                meta: Box::new(meta),
                generation: 1,
            }],
        )
    })
    .await
    .unwrap();
    assert!(h.stored("INBOX/1001/1").await.unwrap().snippet.is_empty());
}

#[tokio::test]
async fn grouped_search_and_folder_previews_use_the_matching_message() {
    let h = imap_harness().await;
    let first = String::from_utf8(message("a", "Kites", ""))
        .unwrap()
        .replace("Hi.", "Matching body.");
    let second = String::from_utf8(message(
        "b",
        "Re: Kites",
        "In-Reply-To: <a@example.com>\r\nReferences: <a@example.com>\r\n",
    ))
    .unwrap()
    .replace("Hi.", "Newer unrelated body.");
    h.imap
        .deliver_flagged("INBOX", first.as_bytes(), &[], days_ago(2));
    h.imap
        .deliver_flagged("INBOX", second.as_bytes(), &[], days_ago(1));
    h.bootstrap().await;
    let hit = h.stored("INBOX/1001/1").await.unwrap();
    let grouped = crate::summarize_search(vec![hit], true).pop().unwrap();
    let action = Target::from_row(&grouped);
    assert!(
        action.message_id.is_none(),
        "actions still cover the whole conversation"
    );
    let preview = Target::for_preview(&grouped);
    assert_eq!(preview.message_id.as_deref(), Some("INBOX/1001/1"));
    assert_eq!(
        lists(&h).preview(preview).await.unwrap().as_deref(),
        Some("Matching body.")
    );
    assert_eq!(h.imap.calls_to("body"), 1);
    assert!(h.stored("INBOX/1001/2").await.unwrap().snippet.is_empty());
    let stored =
        h.db.read(move |c| threads::get_thread(c, action.account_id, &action.thread_id))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(stored.preview_message_id.as_deref(), Some("INBOX/1001/2"));
    assert!(
        stored.snippet.is_empty(),
        "the inbox conversation still previews its newest message"
    );
}

#[tokio::test]
async fn gmail_over_imap_previews_a_row_from_its_body() {
    let imap = FakeImap::gmail();
    let id = format!(
        "{:x}",
        imap.deliver_gmail(&["\\Inbox"], message("a", "Kites", ""), days_ago(1))
    );
    let h = gmail_imap_harness_on(imap).await;
    h.bootstrap().await;
    let target = target(&h, &id).await;
    assert_eq!(
        lists(&h).preview(target).await.unwrap().as_deref(),
        Some("Hi.")
    );
    assert_eq!(h.stored(&id).await.unwrap().snippet, "Hi.");
}
