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
            search: true,
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
