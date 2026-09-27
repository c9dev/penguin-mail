//! A whole conversation moved on a folder account carries only the
//! messages in the folder the person moved it from.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::{MailSet, Role, Target};

use super::days_ago;
use crate::fake::{FakeImap, raw_message};
use crate::tests::{Connected, ImapHarness, fake_settings, imap_harness_on};
use crate::{History, MailAction, MailActions, MovedFrom, TriageAction};

/// A conversation on a folder account: Ann's message in Work, her
/// follow-up filed alone in Travel, and the person's reply in Sent. Gives
/// the harness, the thread, and the stored id of each message by folder.
async fn work_and_travel() -> (ImapHarness, String, HashMap<&'static str, String>) {
    let imap = FakeImap::new();
    imap.add_mailbox("Work", None);
    imap.add_mailbox("Travel", None);
    imap.deliver_flagged(
        "Work",
        &raw_message("w", "Kites", days_ago(3), None),
        &[],
        days_ago(3),
    );
    imap.deliver_flagged(
        "Travel",
        &raw_message("t", "Re: Kites", days_ago(2), Some("w")),
        &[],
        days_ago(2),
    );
    imap.deliver_flagged(
        "Sent",
        &raw_message("s", "Re: Kites", days_ago(1), Some("w")),
        &["\\Seen"],
        days_ago(1),
    );
    let h = imap_harness_on(imap, fake_settings()).await;
    h.bootstrap().await;
    h.sync.follow_mailbox("Work").await.unwrap();
    h.sync.follow_mailbox("Travel").await.unwrap();
    h.sync.incremental().await.unwrap();
    h.drain();
    let mut ids = HashMap::new();
    for id in h.ids().await {
        let folder = id.split('/').next().unwrap_or_default().to_string();
        let key = match folder.as_str() {
            "Work" => "work",
            "Travel" => "travel",
            "Sent" => "sent",
            _ => continue,
        };
        ids.insert(key, id);
    }
    assert_eq!(ids.len(), 3, "all three stored: {:?}", h.ids().await);
    let thread = h.thread_of(&ids["work"]).await.expect("threaded");
    for key in ["travel", "sent"] {
        assert_eq!(
            h.thread_of(&ids[key]).await.as_deref(),
            Some(thread.as_str()),
            "{key} threads with Ann's first message"
        );
    }
    (h, thread, ids)
}

fn actions(h: &ImapHarness) -> MailActions<Connected> {
    MailActions::new(
        Arc::new(Connected(HashMap::from([(
            h.account_id,
            Arc::clone(&h.sync),
        )]))),
        h.db.clone(),
        crate::OneClick::Fake(Arc::default()),
    )
}

/// The mailboxes the store holds message `id` in.
async fn places(h: &ImapHarness, id: &str) -> Vec<String> {
    h.stored(id).await.expect("stored").held.mailboxes
}

/// The server's mailbox for message `id` now, read from its remote ref.
async fn on_server(h: &ImapHarness, id: &str) -> String {
    let location = h.location(id).await.expect("a remote ref");
    location.split('/').next().unwrap_or_default().to_string()
}

async fn run(h: &ImapHarness, thread: &str, action: MailAction, from: MovedFrom) {
    let target = Target::thread(h.account_id, thread);
    let outcome = actions(h)
        .run_from(&[target], action, History::Record, &from)
        .await;
    assert!(outcome.failed.is_empty(), "{outcome:?}");
}

#[tokio::test]
async fn archiving_from_work_leaves_the_message_in_travel_alone() {
    let (h, thread, ids) = work_and_travel().await;
    let from = MovedFrom::one(h.account_id, MailSet::Mailbox("Work".into()));

    run(&h, &thread, MailAction::Triage(TriageAction::Archive), from).await;

    assert_eq!(places(&h, &ids["work"]).await, ["Archive"]);
    assert_eq!(on_server(&h, &ids["work"]).await, "Archive");
    assert_eq!(
        places(&h, &ids["travel"]).await,
        ["Travel"],
        "Travel keeps its message"
    );
    assert_eq!(on_server(&h, &ids["travel"]).await, "Travel");
    assert_eq!(places(&h, &ids["sent"]).await, ["Sent"]);
}

/// A search result has no one folder behind it, so the roles decide as
/// before: Sent keeps the reply and every ordinary folder's message goes.
#[tokio::test]
async fn archiving_from_a_search_keeps_the_role_rule() {
    let (h, thread, ids) = work_and_travel().await;

    run(
        &h,
        &thread,
        MailAction::Triage(TriageAction::Archive),
        MovedFrom::nowhere(),
    )
    .await;

    assert_eq!(places(&h, &ids["work"]).await, ["Archive"]);
    assert_eq!(places(&h, &ids["travel"]).await, ["Archive"]);
    assert_eq!(on_server(&h, &ids["travel"]).await, "Archive");
    assert_eq!(
        places(&h, &ids["sent"]).await,
        ["Sent"],
        "the reply stays in Sent"
    );
}

/// Remind Me from Work archives the Work message alone, and the reminder
/// coming due brings back what it archived, not the Travel message.
#[tokio::test]
async fn a_reminder_moves_only_what_it_set_aside() {
    let (h, thread, ids) = work_and_travel().await;
    let from = MovedFrom::one(h.account_id, MailSet::Mailbox("Work".into()));
    let actions = actions(&h);
    let target = Target::thread(h.account_id, &thread);
    let at = crate::now_millis() + 60_000;
    let set = actions
        .run_from(&[target], MailAction::Remind { at }, History::Record, &from)
        .await;
    assert!(set.failed.is_empty(), "{set:?}");
    assert_eq!(places(&h, &ids["work"]).await, ["Archive"]);
    assert_eq!(places(&h, &ids["travel"]).await, ["Travel"]);

    let back = actions.return_due(at + 1).await.unwrap();

    assert_eq!(back.len(), 1);
    assert_eq!(places(&h, &ids["work"]).await, ["INBOX"]);
    assert_eq!(
        places(&h, &ids["travel"]).await,
        ["Travel"],
        "Travel keeps its message"
    );
    assert_eq!(on_server(&h, &ids["travel"]).await, "Travel");
}

/// Taking mail out of Travel moves what sits in Travel, whatever list the
/// conversation was picked from.
#[tokio::test]
async fn taking_mail_out_of_a_folder_moves_only_what_sits_in_it() {
    let (h, thread, ids) = work_and_travel().await;
    let remove = TriageAction::Relabel {
        add: vec![],
        remove: vec![MailSet::Mailbox("Travel".into())],
    };

    run(
        &h,
        &thread,
        MailAction::Triage(remove),
        MovedFrom::nowhere(),
    )
    .await;

    assert_eq!(places(&h, &ids["travel"]).await, ["Archive"]);
    assert_eq!(
        places(&h, &ids["work"]).await,
        ["Work"],
        "Work keeps its message"
    );
    assert_eq!(on_server(&h, &ids["work"]).await, "Work");
}

/// The unified Inbox names the Inbox of each account, one place in each,
/// so archiving there leaves a message filed in a folder where it is.
#[tokio::test]
async fn archiving_from_every_inbox_takes_only_the_inbox_message() {
    let (h, thread, ids) = work_and_travel().await;
    h.imap.deliver_flagged(
        "INBOX",
        &raw_message("i", "Re: Kites", days_ago(0), Some("w")),
        &[],
        days_ago(0),
    );
    h.sync.incremental().await.unwrap();
    let inbox = h
        .ids()
        .await
        .into_iter()
        .find(|id| id.starts_with("INBOX/"))
        .expect("the Inbox message is stored");
    assert_eq!(h.thread_of(&inbox).await.as_deref(), Some(thread.as_str()));

    let every_inbox = MovedFrom::every(MailSet::Role(Role::Inbox));
    run(
        &h,
        &thread,
        MailAction::Triage(TriageAction::Archive),
        every_inbox,
    )
    .await;

    assert_eq!(places(&h, &inbox).await, ["Archive"]);
    assert_eq!(places(&h, &ids["work"]).await, ["Work"]);
    assert_eq!(places(&h, &ids["travel"]).await, ["Travel"]);
}

/// Calling a reminder off from the Remind Me list, which shows mail from
/// every account and folder, brings back what Remind Me archived.
#[tokio::test]
async fn calling_a_reminder_off_brings_back_only_what_it_set_aside() {
    let (h, thread, ids) = work_and_travel().await;
    let from = MovedFrom::one(h.account_id, MailSet::Mailbox("Work".into()));
    let at = crate::now_millis() + 60_000;
    run(&h, &thread, MailAction::Remind { at }, from).await;

    run(
        &h,
        &thread,
        MailAction::CancelReminder,
        MovedFrom::nowhere(),
    )
    .await;

    assert_eq!(places(&h, &ids["work"]).await, ["INBOX"]);
    assert_eq!(
        places(&h, &ids["travel"]).await,
        ["Travel"],
        "Travel keeps its message"
    );
}

/// Mute from Work archives the Work message alone, and Unmute, from the
/// Muted list that shows mail from anywhere, brings back what Mute
/// archived.
#[tokio::test]
async fn unmuting_brings_back_only_what_mute_set_aside() {
    let (h, thread, ids) = work_and_travel().await;
    let from = MovedFrom::one(h.account_id, MailSet::Mailbox("Work".into()));
    run(&h, &thread, MailAction::Mute { muted: true }, from).await;
    assert_eq!(places(&h, &ids["work"]).await, ["Archive"]);

    run(
        &h,
        &thread,
        MailAction::Mute { muted: false },
        MovedFrom::nowhere(),
    )
    .await;

    assert_eq!(places(&h, &ids["work"]).await, ["INBOX"]);
    assert_eq!(
        places(&h, &ids["travel"]).await,
        ["Travel"],
        "Travel keeps its message"
    );
}
