use std::sync::Arc;

use mailrs_domain::{ChangeEvent, MailSet, Role};
use mailrs_imap::SpecialUse;
use mailrs_store::mailboxes;

use crate::fake::{FakeImap, FakeSmtp};
use crate::tests::imap_harness;
use crate::{
    AccountServices, IdentityService, MailBackend, MailCapabilities, Offers, SendAsAddress,
};

#[test]
fn an_imap_account_files_mail_in_folders_and_threads_it_here() {
    let services =
        AccountServices::fake_imap(Arc::new(FakeImap::new()), Arc::new(FakeSmtp::default()));
    assert_eq!(
        services.capabilities(),
        MailCapabilities {
            fetches_raw_together: true,
            labels: false,
            server_threads: false,
            files_sent_mail: false,
            categories: false,
            delete_forever: true,
            batch_limit: 500,
            // Until the Inbox is selected, only the system flags count as
            // stored.
            keywords: &["$seen", "$flagged", "$answered", "$draft"],
            native_search: false,
            paged_mailboxes: true,
            renames: true,
            restates: false,
            tags: false,
            focus: false,
            local_mailboxes: false,
            files_muted_replies: false,
        }
    );
}

#[test]
fn an_imap_account_offers_mail_alone() {
    let services =
        AccountServices::fake_imap(Arc::new(FakeImap::new()), Arc::new(FakeSmtp::default()));
    assert_eq!(
        services.offers(),
        Offers {
            labels: false,
            categories: false,
            delete_forever: true,
            calendar: false,
            contacts: false,
            rules: false,
            auto_reply: false,
            auto_reply_needs_sign_in: false,
            auto_reply_subject: false,
            auto_reply_contacts_only: false,
            search: true,
            tags: false,
            focused: false,
            event_files: false,
            moves_events: false,
            calendar_list: false,
            subscriptions: false,
            quiet_changes: false,
            out_of_office: false,
            focus_time: false,
            declines: false,
            masked_email: false,
            label_colors: false,
        }
    );
}

#[tokio::test]
async fn the_account_sends_as_its_own_address() {
    let h = imap_harness().await;
    assert_eq!(
        h.sync.services().identities.identities().await.unwrap(),
        [SendAsAddress {
            email: "me@example.com".into(),
            name: None,
            signature: String::new(),
            default: true,
        }]
    );
}

#[tokio::test]
async fn listing_the_mailboxes_stores_their_roles() {
    let h = imap_harness().await;
    h.sync.refresh_labels().await.unwrap();
    let account_id = h.account_id;
    let listed =
        h.db.read(move |c| mailboxes::listed(c, account_id))
            .await
            .unwrap();
    let sent = listed
        .iter()
        .find(|m| m.id == "Sent")
        .expect("Sent is listed");
    assert_eq!(sent.role, Some(Role::Sent));
    let mail = &h.sync.services().mail;
    assert_eq!(mail.mailbox_for(Role::Archive).as_deref(), Some("Archive"));
    assert_eq!(mail.set_of("Sent"), MailSet::Role(Role::Sent));
    assert_eq!(mail.set_of("Receipts"), MailSet::Mailbox("Receipts".into()));
    assert!(mail.made_by_person("Receipts"));
    assert!(!mail.made_by_person("Sent"));
    assert!(
        h.drain()
            .iter()
            .any(|e| matches!(e, ChangeEvent::LabelsChanged { .. })),
        "the sidebar hears of the new mailboxes"
    );
}

#[tokio::test]
async fn a_flagged_view_stands_for_the_flagged_keyword() {
    let h = imap_harness().await;
    h.imap.add_mailbox("Starred", Some(SpecialUse::Flagged));
    h.sync.refresh_labels().await.unwrap();
    assert_eq!(h.sync.services().mail.set_of("Starred"), MailSet::flagged());
}

/// Counting the Inbox's messages selects it, and that SELECT's
/// PERMANENTFLAGS say which keywords the server stores, as any other
/// SELECT of the Inbox does.
#[tokio::test]
async fn counting_the_inbox_learns_which_keywords_the_server_stores() {
    let (_imap, adapter) = super::adapter(FakeImap::new());
    assert!(!adapter.capabilities().keywords.contains(&"$muted"));

    adapter.mailbox_threads("INBOX").await.unwrap();

    assert!(adapter.capabilities().keywords.contains(&"$muted"));
}

/// On a folder server a folder's id is its name, so dropping "bugs" into
/// "Personal" renames it on the server, and its place goes to the new id.
#[tokio::test]
async fn a_folder_dropped_into_another_is_renamed_there_and_placed_by_its_new_name() {
    let h = imap_harness().await;
    h.bootstrap().await;
    h.imap.add_mailbox("Personal", None);
    h.imap.add_mailbox("Personal/bills", None);
    h.imap.add_mailbox("bugs", None);
    h.sync.refresh_labels().await.unwrap();

    h.sync
        .place_label(
            "bugs",
            "Personal/bugs",
            &["bugs".into(), "Personal/bills".into()],
        )
        .await
        .unwrap();

    let account_id = h.account_id;
    let order =
        h.db.read(move |c| mailrs_store::labels::positions(c, account_id))
            .await
            .unwrap();
    assert_eq!(order.get("Personal/bugs"), Some(&0));
    assert_eq!(order.get("Personal/bills"), Some(&1));
    assert_eq!(order.get("bugs"), None);
}

#[tokio::test(start_paused = true)]
async fn unread_counts_keep_completed_folders_when_one_status_times_out() {
    use crate::MailBackend;
    use mailrs_domain::{MailSet, Role};
    let (imap, adapter) = super::adapter(crate::fake::FakeImap::new());
    super::fill(&imap, "INBOX", 3);
    let hold = imap.hold_next_unread();
    let counting = tokio::spawn(async move { adapter.unread_counts().await.unwrap() });
    hold.reached().await;
    let counts = tokio::time::timeout(std::time::Duration::from_secs(11), counting)
        .await
        .expect("one STATUS cannot hold the count forever")
        .unwrap();
    assert_eq!(counts[&MailSet::Role(Role::Inbox)], 3);
    assert!(!counts.contains_key(&MailSet::Role(Role::Archive)));
}

/// The mailboxes stored for the account, by id, with whether each is
/// hidden.
async fn hidden(h: &crate::tests::ImapHarness) -> Vec<(String, bool)> {
    let account_id = h.account_id;
    h.db.read(move |c| mailboxes::listed(c, account_id))
        .await
        .unwrap()
        .into_iter()
        .map(|m| (m.id, m.hidden))
        .collect()
}

#[tokio::test]
async fn a_folder_nobody_subscribes_to_is_hidden_while_its_tree_holds() {
    let h = imap_harness().await;
    for name in ["Lists", "Lists/Rust", "Old", "Old/2019"] {
        h.imap.add_mailbox(name, None);
    }
    h.imap.with(|s| {
        s.unsubscribed
            .extend(["Lists", "Old", "Old/2019", "Sent"].map(String::from));
    });

    h.sync.refresh_labels().await.unwrap();

    let hidden = hidden(&h).await;
    let of = |id: &str| hidden.iter().find(|(m, _)| m == id).map(|(_, h)| *h);
    assert_eq!(of("Lists/Rust"), Some(false), "subscribed");
    assert_eq!(of("Lists"), Some(false), "above a folder that shows");
    assert_eq!(of("Old"), Some(true));
    assert_eq!(of("Old/2019"), Some(true));
    assert_eq!(of("Sent"), Some(false), "the app files mail into it");
    assert_eq!(of("INBOX"), Some(false));
}

#[tokio::test]
async fn a_folder_made_here_is_subscribed_and_shows() {
    let h = imap_harness().await;
    h.sync.refresh_labels().await.unwrap();

    h.sync.create_label("Receipts/2026").await.unwrap();

    assert!(
        h.imap
            .calls()
            .contains(&"subscribe Receipts/2026".to_string())
    );
    let hidden = hidden(&h).await;
    assert!(
        hidden.contains(&("Receipts/2026".into(), false)),
        "{hidden:?}"
    );
}

/// The fake leaves subscriptions behind on RENAME, as many servers do.
#[tokio::test]
async fn a_renamed_folder_keeps_its_subscription_and_its_children_theirs() {
    let h = imap_harness().await;
    for name in ["Work", "Work/Plans", "Work/Old"] {
        h.imap.add_mailbox(name, None);
    }
    h.imap.with(|s| {
        s.unsubscribed.insert("Work/Old".into());
    });
    h.sync.refresh_labels().await.unwrap();

    h.sync.rename_label("Work", "Job").await.unwrap();

    let hidden = hidden(&h).await;
    assert!(hidden.contains(&("Job".into(), false)), "{hidden:?}");
    assert!(hidden.contains(&("Job/Plans".into(), false)), "{hidden:?}");
    assert!(hidden.contains(&("Job/Old".into(), true)), "{hidden:?}");
    let unsubscribed = h.imap.with(|s| s.unsubscribed.clone());
    assert!(unsubscribed.contains("Work") && unsubscribed.contains("Work/Plans"));
}

#[tokio::test]
async fn a_deleted_folder_takes_its_subscription_along() {
    let h = imap_harness().await;
    h.imap.add_mailbox("Spare", None);
    h.sync.refresh_labels().await.unwrap();

    h.sync.delete_label("Spare").await.unwrap();

    assert!(h.imap.calls().contains(&"unsubscribe Spare".to_string()));
}
