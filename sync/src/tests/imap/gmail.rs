//! Gmail over IMAP against `FakeImap::gmail()`, through the engine: ids
//! are the hex of X-GM-MSGID, labels come from X-GM-LABELS, and a message
//! that moves between All Mail and Trash keeps its id.

use std::sync::Arc;

use mailrs_domain::Target;

use super::{days_ago, message};
use crate::fake::{FakeImap, FakeSmtp};
use crate::services::{GmailImap, Imap, ImapSettings};
use crate::tests::{ImapHarness, gmail_imap_harness_on};
use crate::{MailBackend, MailOp, SearchQuery, TriageAction};

const ALL_MAIL: &str = "[Gmail]/All Mail";
const TRASH: &str = "[Gmail]/Trash";

fn gmail_settings() -> ImapSettings {
    ImapSettings {
        address: "me@example.com".into(),
        provider_name: "Gmail".into(),
        files_sent_mail: true,
        window_days: crate::DEFAULT_WINDOW_DAYS,
    }
}

async fn gmail_on(imap: FakeImap) -> ImapHarness {
    gmail_imap_harness_on(imap).await
}

fn hex(msgid: u64) -> String {
    format!("{msgid:x}")
}

/// The Gmail adapter alone on `imap`.
fn adapter_on(imap: FakeImap) -> (Arc<FakeImap>, GmailImap<FakeImap, FakeSmtp>) {
    let imap = Arc::new(imap);
    let adapter = GmailImap::over(Imap::new(
        Arc::clone(&imap),
        Arc::new(FakeSmtp::default()),
        gmail_settings(),
    ));
    (imap, adapter)
}

async fn mailboxes_of(h: &ImapHarness, id: &str) -> Vec<String> {
    let mut held = h.stored(id).await.expect("stored").held.mailboxes;
    held.sort();
    held
}

#[tokio::test]
async fn a_message_with_three_labels_is_stored_once_with_three_labels() {
    let imap = FakeImap::gmail();
    let id = imap.deliver_gmail(
        &["\\Inbox", "Work", "Travel"],
        message("a", "Kites", ""),
        days_ago(1),
    );
    let h = gmail_on(imap).await;
    h.bootstrap().await;

    assert_eq!(h.ids().await, [hex(id)]);
    assert_eq!(
        mailboxes_of(&h, &hex(id)).await,
        ["INBOX", "Travel", "Work"]
    );
}

#[tokio::test]
async fn ids_and_threads_are_the_hex_of_gmails_numbers() {
    let imap = FakeImap::gmail();
    let first = imap.deliver_gmail(&["\\Inbox"], message("a", "Kites", ""), days_ago(2));
    let reply = imap.deliver_gmail_reply(
        first,
        &["\\Inbox"],
        message("b", "Re: Kites", "In-Reply-To: <a@example.com>\r\n"),
        days_ago(1),
    );
    let h = gmail_on(imap).await;
    h.bootstrap().await;

    let mut want = vec![hex(first), hex(reply)];
    want.sort();
    assert_eq!(h.ids().await, want);
    assert_eq!(
        h.stored(&hex(reply)).await.unwrap().thread_id,
        hex(first),
        "a reply shares its thread's id"
    );
}

#[tokio::test]
async fn a_message_trashed_elsewhere_keeps_its_id_and_moves_to_trash() {
    let imap = FakeImap::gmail();
    let id = imap.deliver_gmail(&["\\Inbox"], message("a", "Kites", ""), days_ago(1));
    let h = gmail_on(imap).await;
    h.bootstrap().await;
    h.sync.incremental().await.unwrap();
    let bodies = h.imap.calls_to("body");

    h.imap.remote_trash(id);
    h.sync.incremental().await.unwrap();

    assert_eq!(
        h.ids().await,
        [hex(id)],
        "one message, never deleted and fetched again"
    );
    assert_eq!(mailboxes_of(&h, &hex(id)).await, ["TRASH"]);
    let at = h.location(&hex(id)).await.expect("it has a place");
    assert!(at.starts_with(TRASH), "{at}");
    assert_eq!(h.imap.calls_to("body"), bodies);
}

#[tokio::test]
async fn a_label_added_elsewhere_reaches_the_store_at_the_next_look() {
    let imap = FakeImap::gmail();
    let id = imap.deliver_gmail(&["\\Inbox"], message("a", "Kites", ""), days_ago(1));
    let h = gmail_on(imap).await;
    h.bootstrap().await;
    h.sync.incremental().await.unwrap();

    h.imap.remote_label(id, "Work", true);
    h.sync.incremental().await.unwrap();

    assert_eq!(mailboxes_of(&h, &hex(id)).await, ["INBOX", "Work"]);
}

#[tokio::test]
async fn a_nested_accented_label_round_trips() {
    let imap = FakeImap::gmail();
    let id = imap.deliver_gmail(
        &["\\Inbox", "Família/Férias 2026"],
        message("a", "Kites", ""),
        days_ago(1),
    );
    let (_, adapter) = adapter_on(imap);
    let listed = adapter.mailboxes().await.unwrap();
    let names: Vec<&str> = listed.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"Família/Férias 2026"), "{names:?}");
    assert_eq!(
        listed
            .iter()
            .filter(|m| m.name == "Família/Férias 2026")
            .count(),
        1,
        "listed once, by its full name"
    );

    let imap = FakeImap::gmail();
    imap.deliver_gmail(
        &["\\Inbox", "Família/Férias 2026"],
        message("a", "Kites", ""),
        days_ago(1),
    );
    let h = gmail_on(imap).await;
    h.bootstrap().await;
    let held = mailboxes_of(&h, &hex(id)).await;
    assert!(
        held.contains(&"Família/Férias 2026".to_string()),
        "{held:?}"
    );
}

#[tokio::test]
async fn the_window_is_the_inbox_and_the_last_days() {
    let imap = FakeImap::gmail();
    let old_inbox = imap.deliver_gmail(&["\\Inbox"], message("a", "Old inbox", ""), days_ago(400));
    let recent = imap.deliver_gmail(&["Work"], message("b", "Recent", ""), days_ago(1));
    let old_work = imap.deliver_gmail(&["Work"], message("c", "Old work", ""), days_ago(400));
    let (_, adapter) = adapter_on(imap);

    let mut ids: Vec<String> = adapter
        .window_ids(30, None)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    ids.sort();

    let mut want = vec![hex(old_inbox), hex(recent)];
    want.sort();
    assert_eq!(ids, want);
    assert!(!ids.contains(&hex(old_work)));
}

#[tokio::test]
async fn a_category_search_files_the_inbox_into_tabs() {
    let imap = FakeImap::gmail();
    let social = imap.deliver_gmail(&["\\Inbox"], message("a", "Friends", ""), days_ago(1));
    let personal = imap.deliver_gmail(&["\\Inbox"], message("b", "Mum", ""), days_ago(1));
    imap.set_category(social, Some("social"));
    let h = gmail_on(imap).await;
    h.bootstrap().await;

    let held = |id: u64| {
        let h = &h;
        async move { h.stored(&hex(id)).await.unwrap().held.categories }
    };
    assert_eq!(held(social).await, ["CATEGORY_SOCIAL"]);
    assert_eq!(held(personal).await, ["CATEGORY_PERSONAL"]);
}

#[tokio::test]
async fn native_search_returns_ids_the_store_holds() {
    let imap = FakeImap::gmail();
    let wanted = imap.deliver_gmail(&["\\Inbox"], message("a", "Invoice", ""), days_ago(1));
    imap.deliver_gmail(&["\\Inbox"], message("b", "Kites", ""), days_ago(1));
    let h = gmail_on(imap).await;
    h.bootstrap().await;

    let found = h
        .sync
        .search_ids(&SearchQuery::Native("invoice".into()), 10)
        .await
        .unwrap();

    assert_eq!(
        found.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        [hex(wanted)]
    );
    assert!(h.stored(&hex(wanted)).await.is_some());
}

#[tokio::test]
async fn a_new_uidvalidity_on_all_mail_downloads_no_body() {
    let imap = FakeImap::gmail();
    let id = imap.deliver_gmail(&["\\Inbox"], message("a", "Kites", ""), days_ago(1));
    let h = gmail_on(imap).await;
    h.bootstrap().await;
    h.sync.incremental().await.unwrap();
    let bodies = h.imap.calls_to("body");

    h.imap.reset_uidvalidity(ALL_MAIL);
    h.sync.incremental().await.unwrap();

    assert_eq!(h.ids().await, [hex(id)]);
    assert!(h.location(&hex(id)).await.is_some());
    assert_eq!(h.imap.calls_to("body"), bodies);
}

// ---- Writes ------------------------------------------------------------------

async fn one_inbox_message() -> (ImapHarness, u64, String) {
    let imap = FakeImap::gmail();
    let id = imap.deliver_gmail(&["\\Inbox"], message("a", "Kites", ""), days_ago(1));
    let h = gmail_on(imap).await;
    h.bootstrap().await;
    h.sync.incremental().await.unwrap();
    let thread = h.thread_of(&hex(id)).await.expect("threaded");
    (h, id, thread)
}

fn labels_on(h: &ImapHarness, id: u64) -> Vec<String> {
    h.imap.with(|s| {
        s.find_msgid(id)
            .map(|(_, _, labels)| labels.into_iter().collect())
            .unwrap_or_default()
    })
}

fn mailbox_of(h: &ImapHarness, id: u64) -> String {
    h.imap
        .with(|s| s.find_msgid(id).map(|(m, _, _)| m).unwrap_or_default())
}

#[tokio::test]
async fn archive_removes_the_inbox_label_in_all_mail() {
    let (h, id, thread) = one_inbox_message().await;

    h.sync
        .triage_thread(&thread, &TriageAction::Archive)
        .await
        .unwrap();

    assert!(!labels_on(&h, id).contains(&"\\Inbox".to_string()));
    assert_eq!(mailbox_of(&h, id), ALL_MAIL);
}

#[tokio::test]
async fn trash_moves_to_trash_and_keeps_the_id() {
    let (h, id, thread) = one_inbox_message().await;

    h.sync
        .triage_thread(&thread, &TriageAction::Trash)
        .await
        .unwrap();

    assert_eq!(mailbox_of(&h, id), TRASH);
    assert_eq!(h.ids().await, [hex(id)]);
    let at = h.location(&hex(id)).await.unwrap();
    assert!(at.starts_with(TRASH), "{at}");
}

#[tokio::test]
async fn restore_moves_back_to_the_inbox() {
    let (h, id, thread) = one_inbox_message().await;
    h.sync
        .triage_thread(&thread, &TriageAction::Trash)
        .await
        .unwrap();

    h.sync
        .triage_thread(&thread, &TriageAction::Untrash)
        .await
        .unwrap();

    assert_eq!(mailbox_of(&h, id), ALL_MAIL);
    assert!(labels_on(&h, id).contains(&"\\Inbox".to_string()));
}

#[tokio::test]
async fn a_move_to_a_label_out_of_trash_keeps_the_label_and_skips_the_inbox() {
    let (h, id, thread) = one_inbox_message().await;
    h.sync
        .triage_thread(&thread, &TriageAction::Trash)
        .await
        .unwrap();

    let to_work = TriageAction::Relabel {
        add: vec![mailrs_domain::MailSet::Mailbox("Work".into())],
        remove: vec![mailrs_domain::MailSet::Role(mailrs_domain::Role::Trash)],
    };
    h.sync.triage_thread(&thread, &to_work).await.unwrap();

    assert_eq!(mailbox_of(&h, id), ALL_MAIL);
    let labels = labels_on(&h, id);
    assert!(labels.contains(&"Work".to_string()), "{labels:?}");
    assert!(!labels.contains(&"\\Inbox".to_string()), "{labels:?}");
}

#[tokio::test]
async fn spam_and_not_spam_move_between_mailboxes() {
    let (h, id, thread) = one_inbox_message().await;

    h.sync
        .triage_thread(&thread, &TriageAction::Junk)
        .await
        .unwrap();
    assert_eq!(mailbox_of(&h, id), "[Gmail]/Spam");

    h.sync
        .triage_thread(&thread, &TriageAction::NotJunk)
        .await
        .unwrap();
    assert_eq!(mailbox_of(&h, id), ALL_MAIL);
}

#[tokio::test]
async fn star_and_read_set_flags_in_all_mail() {
    let (h, id, thread) = one_inbox_message().await;

    h.sync
        .triage_thread(&thread, &TriageAction::Star)
        .await
        .unwrap();
    h.sync
        .triage_thread(&thread, &TriageAction::MarkRead)
        .await
        .unwrap();

    let flags = h.imap.with(|s| {
        let (mailbox, uid, _) = s.find_msgid(id).unwrap();
        s.mailbox_mut(&mailbox).messages[&uid].flags.clone()
    });
    assert!(
        flags.contains("\\Flagged") && flags.contains("\\Seen"),
        "{flags:?}"
    );
}

#[tokio::test]
async fn add_and_remove_a_user_label() {
    let (h, id, _) = one_inbox_message().await;
    let name = hex(id);

    let thread = h.thread_of(&name).await.unwrap();
    h.sync
        .triage_thread(&thread, &TriageAction::AddLabel("Work".into()))
        .await
        .unwrap();
    assert!(labels_on(&h, id).contains(&"Work".to_string()));

    h.sync
        .triage_thread(&thread, &TriageAction::RemoveLabel("Work".into()))
        .await
        .unwrap();
    assert!(!labels_on(&h, id).contains(&"Work".to_string()));
}

#[tokio::test]
async fn delete_forever_expunges_only_in_trash() {
    let (h, id, thread) = one_inbox_message().await;
    h.sync
        .triage_thread(&thread, &TriageAction::Trash)
        .await
        .unwrap();

    let erased = h
        .sync
        .erase_all(&[Target::thread(h.account_id, &thread)])
        .await
        .unwrap();
    assert!(erased[0].is_ok());

    assert_eq!(h.imap.with(|s| s.find_msgid(id)), None);
    let calls = h.imap.calls();
    assert!(
        calls
            .iter()
            .all(|c| !c.starts_with(&format!("expunge {ALL_MAIL}"))),
        "nothing is expunged in All Mail: {calls:?}"
    );
}

#[tokio::test]
async fn a_category_change_is_refused_and_named() {
    let imap = FakeImap::gmail();
    let id = imap.deliver_gmail(&["\\Inbox"], message("a", "Kites", ""), days_ago(1));
    let (_, adapter) = adapter_on(imap);
    adapter.mailboxes().await.unwrap();

    let refused = adapter
        .apply(
            &[hex(id)],
            &[MailOp::SetCategory {
                category: "CATEGORY_SOCIAL".into(),
                on: true,
            }],
        )
        .await
        .unwrap_err();

    assert!(matches!(refused.error, crate::BackendError::Unsupported));
    assert_eq!(refused.taken, 0);
}

#[tokio::test]
async fn a_draft_saved_then_replaced_leaves_one_draft_and_no_archived_copy() {
    let (h, _, _) = one_inbox_message().await;
    let first = h
        .sync
        .save_draft(message("d", "Draft", ""), None, None)
        .await
        .unwrap();
    let second = h
        .sync
        .save_draft(
            message("d2", "Draft 2", ""),
            None,
            Some(first.draft_id.clone()),
        )
        .await
        .unwrap();

    assert_ne!(first.draft_id, second.draft_id);
    let drafts_in_all_mail: Vec<String> = h
        .imap
        .messages_in(ALL_MAIL)
        .into_iter()
        .filter(|raw| raw.contains("Subject: Draft"))
        .collect();
    assert_eq!(drafts_in_all_mail.len(), 1, "{drafts_in_all_mail:?}");
    assert!(drafts_in_all_mail[0].contains("Subject: Draft 2"));
    assert!(
        h.imap
            .messages_in(TRASH)
            .iter()
            .all(|raw| !raw.contains("Subject: Draft\r\n")),
        "the old draft is expunged from Trash"
    );
}

#[tokio::test]
async fn sending_appends_nothing() {
    let (h, _, _) = one_inbox_message().await;
    let before = h.imap.calls_to("append");

    let raw = b"From: me@example.com\r\nTo: ann@example.com\r\nSubject: Hi\r\nMessage-ID: <s@example.com>\r\n\r\nHi.\r\n";
    h.sync.send(raw.to_vec(), None, None).await.unwrap();

    assert_eq!(h.smtp.sent().len(), 1);
    assert_eq!(h.imap.calls_to("append"), before);
}

#[tokio::test]
async fn a_gmail_account_offers_no_label_colours() {
    let imap = FakeImap::gmail();
    let h = gmail_on(imap).await;
    assert!(h.sync.services().offers().labels);
    assert!(!h.sync.services().offers().label_colors);
}
