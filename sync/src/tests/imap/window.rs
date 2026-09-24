use super::{days_ago, message};
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
