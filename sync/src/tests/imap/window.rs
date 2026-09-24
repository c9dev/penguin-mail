use super::{adapter, days_ago, fill, message};
use crate::MailBackend;
use crate::fake::FakeImap;
use crate::tests::imap_harness;

#[tokio::test]
async fn the_window_holds_the_inbox_and_recent_mail_under_the_locations_it_was_met_at() {
    let h = imap_harness().await;
    h.imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(400));
    h.imap.deliver_flagged("INBOX", &message("b", "Moss", ""), &["\\Seen"], days_ago(1));
    h.imap.deliver_flagged("Archive", &message("c", "Old news", ""), &["\\Seen"], days_ago(400));
    h.imap.deliver_flagged("Archive", &message("d", "Fresh news", ""), &["\\Seen"], days_ago(2));

    h.bootstrap().await;

    assert_eq!(h.ids().await, ["Archive/1006/2", "INBOX/1001/1", "INBOX/1001/2"]);
    assert_eq!(h.location("Archive/1006/2").await.as_deref(), Some("Archive/1006/2"));
}

#[tokio::test]
async fn a_long_inbox_loads_a_page_at_a_time_down_by_uid() {
    let h = imap_harness().await;
    for n in 0..150 {
        h.imap.deliver_flagged("INBOX", &message(&format!("m{n}"), "Note", ""), &["\\Seen"], days_ago(1));
    }
    h.imap.deliver_flagged("Sent", &message("s", "Sent note", ""), &["\\Seen"], days_ago(1));

    h.sync.bootstrap().await.unwrap();
    let first = h.ids().await;
    assert_eq!(first.len(), 100);
    assert!(first.contains(&"INBOX/1001/150".to_string()), "the newest come first");
    while h.sync.backfill_step().await.unwrap() {}

    assert_eq!(h.ids().await.len(), 151);
    assert!(h.ids().await.contains(&"Sent/1002/1".to_string()));
}

#[tokio::test]
async fn a_reply_joins_the_thread_of_the_message_it_names() {
    let h = imap_harness().await;
    h.imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(3));
    h.imap.deliver_flagged(
        "INBOX",
        &message("b", "Re: Kites", "In-Reply-To: <a@example.com>\r\nReferences: <a@example.com>\r\n"),
        &[],
        days_ago(2),
    );
    h.imap.deliver_flagged(
        "Sent",
        &message(
            "c",
            "Re: Kites",
            "In-Reply-To: <b@example.com>\r\nReferences: <a@example.com> <b@example.com>\r\n",
        ),
        &["\\Seen"],
        days_ago(1),
    );
    h.imap.deliver_flagged("INBOX", &message("d", "Moss", ""), &[], days_ago(1));

    h.bootstrap().await;

    let thread = h.thread_of("INBOX/1001/1").await;
    assert!(thread.is_some());
    assert_eq!(h.thread_of("INBOX/1001/2").await, thread);
    assert_eq!(h.thread_of("Sent/1002/1").await, thread, "a reply filed in Sent joins too");
    assert_ne!(h.thread_of("INBOX/1001/3").await, thread);
}

#[tokio::test]
async fn flags_arrive_as_keywords_and_the_size_as_the_server_counts_it() {
    let h = imap_harness().await;
    let raw = message("a", "Kites", "");
    h.imap.deliver_flagged("INBOX", &raw, &["\\Seen", "\\Flagged", "$Forwarded", "\\Recent"], days_ago(1));

    h.bootstrap().await;

    let stored = h.stored("INBOX/1001/1").await.expect("stored");
    let mut keywords = stored.held.keywords.clone();
    keywords.sort();
    assert_eq!(keywords, ["$flagged", "$forwarded", "$seen"]);
    assert!(!stored.is_unread() && stored.is_flagged());
    assert_eq!(stored.size, raw.len() as i64);
}

#[tokio::test]
async fn a_message_marked_deleted_is_not_in_the_window() {
    let h = imap_harness().await;
    h.imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &["\\Deleted"], days_ago(1));
    h.bootstrap().await;
    assert!(h.ids().await.is_empty());
}

#[tokio::test]
async fn opening_a_thread_asks_the_server_for_each_of_its_messages_again() {
    let h = imap_harness().await;
    h.imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(2));
    h.imap.deliver_flagged(
        "INBOX",
        &message("b", "Re: Kites", "In-Reply-To: <a@example.com>\r\n"),
        &[],
        days_ago(1),
    );
    h.bootstrap().await;
    let thread = h.thread_of("INBOX/1001/1").await.expect("threaded");
    h.imap.set_flags("INBOX", 2, &["\\Flagged"]);
    h.imap.remote_expunge("INBOX", 1);

    h.sync.ensure_thread(&thread).await.unwrap();

    assert_eq!(h.ids().await, ["INBOX/1001/2"]);
    assert!(h.stored("INBOX/1001/2").await.unwrap().is_flagged());
    assert_eq!(h.thread_of("INBOX/1001/2").await, Some(thread));
}

/// Each backfill page searches down from the UID the last page reached,
/// in a range near the page's size, so loading a long Inbox costs about
/// what listing it once does, not a whole-Inbox search per page.
#[tokio::test]
async fn a_backfill_page_searches_only_below_where_the_last_one_stopped() {
    let imap = FakeImap::new();
    fill(&imap, "INBOX", 3_000);
    let (imap, adapter) = adapter(imap);

    let mut listed = Vec::new();
    let mut cursor = None;
    loop {
        let page = adapter.backfill(30, cursor.as_deref()).await.unwrap();
        listed.extend(page.refs.into_iter().map(|r| r.id));
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    let answered = imap.with(|s| s.answered);
    eprintln!("a backfill of 3,000 messages answered {answered} UIDs");
    assert_eq!(listed.len(), 3_000);
    assert_eq!(listed.first().map(String::as_str), Some("INBOX/1001/3000"));
    assert_eq!(listed.last().map(String::as_str), Some("INBOX/1001/1"));
    assert!(answered <= 5 * 3_000, "the searches answered {answered} UIDs");
}
