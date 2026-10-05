//! A reply to a muted conversation stays out of the Inbox. Gmail's own
//! filter files it there; on Outlook, IMAP and POP3 the engine files it
//! as the feed brings it in, before anything announces it.

use mailrs_domain::mailbox::keyword::{MUTED, SEEN};
use mailrs_domain::{ChangeEvent, MailSet, RemoveSetting, Role, Target};

use super::imap::days_ago;
use super::microsoft::outlook;
use super::pop3::pop3_harness;
use super::{harness, imap_harness};
use crate::fake::{FakeMail, FakePop3, meta, pop3_mail, raw_message};
use crate::{TriageAction, now_millis};

/// Whether `events` hold a new-mail event naming `id`.
fn announced(events: &[ChangeEvent], id: &str) -> bool {
    events.iter().any(|e| {
        matches!(e, ChangeEvent::NewMail { message_ids, .. } if message_ids.iter().any(|m| m == id))
    })
}

#[tokio::test]
async fn an_outlook_reply_to_a_muted_thread_goes_to_the_archive_read_and_unannounced() {
    let h = outlook().await;
    let (inbox, archive) = (h.fake.folder_id("inbox"), h.fake.folder_id("archive"));
    let kites = FakeMail { at: now_millis(), conversation: Some("kites"), ..FakeMail::default() };
    h.fake.deliver(&inbox, kites.clone());
    h.bootstrap_all().await;
    let thread = [Target::thread(h.account_id, "kites")];
    h.sync.triage_all(&thread, &TriageAction::Mute, None).await.unwrap();
    h.drain();

    let reply = h.fake.deliver(&inbox, kites.clone());
    h.look().await;

    assert_eq!(h.fake.with(|s| s.messages[&reply].folder.clone()), archive, "moved on the server");
    assert_eq!(h.fake.with(|s| s.messages[&reply].message.is_read), Some(true), "read on the server");
    let held = h.held(&reply).await;
    assert_eq!(held.mailboxes, std::slice::from_ref(&archive));
    assert!(held.keywords.contains(&SEEN.to_string()) && held.keywords.contains(&MUTED.to_string()), "{held:?}");
    assert!(!announced(&h.drain(), &reply), "no notification");

    h.sync.triage_all(&thread, &TriageAction::Unmute, None).await.unwrap();
    h.drain();
    let later = h.fake.deliver(&inbox, kites);
    h.look().await;

    assert_eq!(h.fake.with(|s| s.messages[&later].folder.clone()), inbox, "unmuted, so the Inbox keeps it");
    assert!(announced(&h.drain(), &later));
}

#[tokio::test]
async fn an_imap_reply_to_a_muted_thread_goes_to_the_archive_read_and_unannounced() {
    let h = imap_harness().await;
    h.imap.deliver_flagged("INBOX", &raw_message("w", "Kites", days_ago(2), None), &[], days_ago(2));
    h.bootstrap().await;
    let first = h.ids().await.pop().expect("stored");
    let thread = [Target::thread(h.account_id, h.thread_of(&first).await.expect("threaded"))];
    h.sync.triage_all(&thread, &TriageAction::Mute, None).await.unwrap();
    h.drain();

    h.imap.deliver_flagged("INBOX", &raw_message("r", "Re: Kites", days_ago(0), Some("w")), &[], days_ago(0));
    h.sync.incremental().await.unwrap();

    let reply = h
        .ids()
        .await
        .into_iter()
        .find(|id| id != &first)
        .expect("the reply is stored");
    let stored = h.stored(&reply).await.expect("stored");
    assert_eq!(stored.held.mailboxes, ["Archive"], "{stored:?}");
    assert!(!stored.is_unread());
    assert!(h.location(&reply).await.expect("a remote ref").starts_with("Archive/"), "moved on the server");
    assert!(!announced(&h.drain(), &reply), "no notification");

    h.sync.triage_all(&thread, &TriageAction::Unmute, None).await.unwrap();
    h.drain();
    h.imap.deliver_flagged("INBOX", &raw_message("l", "Re: Kites", now_millis(), Some("w")), &[], now_millis());
    h.sync.incremental().await.unwrap();

    let events = h.drain();
    let later = events
        .iter()
        .find_map(|e| match e {
            ChangeEvent::NewMail { message_ids, .. } => message_ids.first().cloned(),
            _ => None,
        })
        .expect("unmuted, so the reply is announced");
    assert_eq!(h.stored(&later).await.expect("stored").held.mailboxes, ["INBOX"]);
}

/// A reply to the first test message, which `pop3_mail(1)` names
/// `<m1@example.org>`.
fn pop3_reply(n: u32) -> Vec<u8> {
    format!(
        "From: Ana <ana@example.org>\r\nTo: me@example.org\r\nSubject: Re: Hello 1\r\n\
         Message-ID: <r{n}@example.org>\r\nIn-Reply-To: <m1@example.org>\r\n\
         References: <m1@example.org>\r\nDate: Mon, 4 Jan 2021 10:00:00 +0000\r\n\r\nReply {n}\r\n"
    )
    .into_bytes()
}

#[tokio::test]
async fn a_pop3_reply_to_a_muted_thread_goes_to_the_archive_read_and_unannounced() {
    let h = pop3_harness(FakePop3::default().with_message("u1", &pop3_mail(1)), RemoveSetting::Never).await;
    h.sync.pop3_check().await.unwrap();
    let thread = [Target::thread(h.account_id, h.thread_of("pop3/u1").await)];
    h.sync.triage_all(&thread, &TriageAction::Mute, None).await.unwrap();
    std::iter::from_fn(|| h.events.try_recv().ok()).for_each(drop);

    h.fake.add("u2", &pop3_reply(2));
    h.sync.pop3_check().await.unwrap();

    assert_eq!(h.thread_of("pop3/u2").await, thread[0].thread_id, "the reply threads with the first");
    assert!(h.ids_in(MailSet::Role(Role::Inbox)).await.is_empty(), "the Inbox stays empty");
    assert_eq!(h.ids_in(MailSet::Role(Role::Archive)).await, ["pop3/u1", "pop3/u2"]);
    assert!(!h.ids_in(MailSet::Unseen).await.contains(&"pop3/u2".to_string()), "the reply is read");
    let events: Vec<ChangeEvent> = std::iter::from_fn(|| h.events.try_recv().ok()).collect();
    assert!(!announced(&events, "pop3/u2"), "no notification");

    h.sync.triage_all(&thread, &TriageAction::Unmute, None).await.unwrap();
    std::iter::from_fn(|| h.events.try_recv().ok()).for_each(drop);
    h.fake.add("u3", &pop3_reply(3));
    h.sync.pop3_check().await.unwrap();

    assert!(h.ids_in(MailSet::Role(Role::Inbox)).await.contains(&"pop3/u3".to_string()));
    let events: Vec<ChangeEvent> = std::iter::from_fn(|| h.events.try_recv().ok()).collect();
    assert!(announced(&events, "pop3/u3"));
}

/// Gmail's own filter files a muted thread's reply, as the fake's does,
/// and leaves it unread. The engine adds nothing to that.
#[tokio::test]
async fn gmail_keeps_its_own_filing_of_a_muted_threads_reply() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "ta", now - 1000, &["INBOX"]));
    h.bootstrap_all().await;
    h.sync.triage_all(&[Target::thread(h.account_id, "ta")], &TriageAction::Mute, None).await.unwrap();
    h.drain();

    h.fake.deliver(meta("b", "ta", now, &["INBOX", "UNREAD"]));
    h.sync.incremental().await.unwrap();

    assert_eq!(h.labels_of("b").await, ["MUTE", "UNREAD"], "Gmail's filing, nothing more");
    assert!(!announced(&h.drain(), "b"), "no notification");
}

/// Gmail brings a muted thread back to the Inbox when a reply is
/// addressed to the person alone. That is Gmail's call: the engine moves
/// nothing, and still raises no notification.
#[tokio::test]
async fn a_muted_reply_gmail_leaves_in_the_inbox_stays_there_unannounced() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "ta", now - 1000, &["INBOX"]));
    h.bootstrap_all().await;
    h.sync.triage_all(&[Target::thread(h.account_id, "ta")], &TriageAction::Mute, None).await.unwrap();
    h.drain();

    h.fake.deliver(meta("b", "ta", now, &["INBOX", "UNREAD"]));
    h.fake.remote_relabel("b", &["INBOX"], &[]);
    h.sync.incremental().await.unwrap();

    assert_eq!(h.labels_of("b").await, ["INBOX", "MUTE", "UNREAD"]);
    assert!(!announced(&h.drain(), "b"), "no notification");
}
