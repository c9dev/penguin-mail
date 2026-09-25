use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::query::{Query, Term};
use mailrs_domain::{Folder, MailSet, Role};
use mailrs_store::accounts;

use super::{days_ago, message};
use crate::mailbox::{Loaded, Mailbox, Mailboxes, Scope, View};
use crate::tests::{Connected, imap_harness};
use crate::{BackendError, MailBackend, SearchQuery};

fn ids(refs: &[crate::RemoteRef]) -> Vec<&str> {
    refs.iter().map(|r| r.id.as_str()).collect()
}

#[tokio::test]
async fn a_search_past_the_window_asks_the_server() {
    let h = imap_harness().await;
    h.imap.deliver_flagged(
        "Archive",
        &message("old", "Kites of 2025", ""),
        &["\\Seen"],
        days_ago(400),
    );
    h.imap
        .deliver_flagged("INBOX", &message("new", "Moss", ""), &[], days_ago(1));
    h.bootstrap().await;

    let searched = h
        .sync
        .search_tree(&Query::Term(Term::Subject("kites".into())), 10)
        .await
        .unwrap();

    assert!(!searched.store_only);
    assert_eq!(ids(&searched.refs), ["Archive/1006/1"]);
}

#[tokio::test]
async fn a_name_beyond_ascii_reaches_the_server_in_one_mailbox() {
    let h = imap_harness().await;
    let raw = b"From: =?UTF-8?Q?Jos=C3=A9?= <jose@example.com>\r\nTo: me@example.com\r\n\
                Subject: Kites\r\nMessage-ID: <jose@example.com>\r\n\r\nHi.\r\n";
    h.imap
        .deliver_flagged("Sent", raw, &["\\Seen"], days_ago(400));
    h.imap.deliver_flagged("INBOX", raw, &[], days_ago(400));
    h.bootstrap().await;
    let query = Query::And(vec![
        Query::Term(Term::In(MailSet::Role(Role::Sent))),
        Query::Term(Term::From("José".into())),
    ]);

    let searched = h.sync.search_tree(&query, 10).await.unwrap();

    assert_eq!(ids(&searched.refs), ["Sent/1002/1"]);
}

#[tokio::test]
async fn a_mailbox_named_by_its_name_is_searched_alone() {
    let h = imap_harness().await;
    h.imap.add_mailbox("Receipts", None);
    h.imap.deliver_flagged(
        "Receipts",
        &message("r", "Kites receipt", ""),
        &["\\Seen"],
        days_ago(400),
    );
    h.imap.deliver_flagged(
        "Archive",
        &message("k", "Kites", ""),
        &["\\Seen"],
        days_ago(400),
    );
    h.bootstrap().await;
    let query = Query::And(vec![
        Query::Term(Term::MailboxNamed("receipts".into())),
        Query::Term(Term::Subject("kites".into())),
    ]);

    let searched = h.sync.search_tree(&query, 10).await.unwrap();

    assert!(!searched.store_only);
    assert_eq!(ids(&searched.refs), ["Receipts/1007/1"]);
}

#[tokio::test]
async fn a_query_imap_cannot_say_is_answered_from_the_store_alone() {
    let h = imap_harness().await;
    h.bootstrap().await;
    let query = Query::And(vec![
        Query::Term(Term::From("ann".into())),
        Query::Term(Term::HasAttachment),
    ]);

    let searched = h.sync.search_tree(&query, 10).await.unwrap();

    assert!(searched.store_only);
    assert!(searched.refs.is_empty());
}

#[tokio::test]
async fn a_search_typed_for_gmail_never_reaches_an_imap_server() {
    let h = imap_harness().await;
    let typed = SearchQuery::Native("from:ann has:attachment".into());
    assert!(matches!(
        h.sync.services().mail.search(&typed, 10).await,
        Err(BackendError::Unsupported)
    ));
}

#[tokio::test]
async fn a_typed_search_finds_mail_on_this_computer_and_past_the_window() {
    let h = imap_harness().await;
    h.imap.deliver_flagged(
        "INBOX",
        &message("new", "Kites in May", ""),
        &[],
        days_ago(1),
    );
    h.imap.deliver_flagged(
        "Archive",
        &message("old", "Kites of 2025", ""),
        &["\\Seen"],
        days_ago(400),
    );
    h.bootstrap().await;
    let typed = SearchQuery::Native("from:ann subject:kites".into());

    let found = h.sync.search_listing(&typed, 10).await.unwrap();

    assert!(!found.store_only);
    // The stored message first; the server's hit for it is the same id
    // and shows once.
    assert_eq!(ids(&found.refs), ["INBOX/1001/1", "Archive/1006/1"]);
}

#[tokio::test]
async fn a_typed_label_searches_the_mailbox_it_spells() {
    let h = imap_harness().await;
    h.imap.add_mailbox("Work/Clients", None);
    h.imap.deliver_flagged(
        "Work/Clients",
        &message("w", "Kites contract", ""),
        &["\\Seen"],
        days_ago(400),
    );
    h.imap.deliver_flagged(
        "Archive",
        &message("k", "Kites", ""),
        &["\\Seen"],
        days_ago(400),
    );
    h.bootstrap().await;
    h.sync.refresh_labels().await.unwrap();
    // The search box suggests a label in Gmail's spelling.
    let typed = SearchQuery::Native("label:work-clients subject:kites".into());

    let found = h.sync.search_listing(&typed, 10).await.unwrap();

    assert_eq!(ids(&found.refs), ["Work/Clients/1007/1"]);
}

#[tokio::test]
async fn a_typed_search_the_server_cannot_run_lists_from_this_computer_and_says_so() {
    let h = imap_harness().await;
    h.imap
        .deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(1));
    h.bootstrap().await;
    let signed_in = h.db.read(accounts::list_accounts).await.unwrap();
    let lists = Mailboxes::new(
        Arc::new(Connected(HashMap::from([(
            h.account_id,
            Arc::clone(&h.sync),
        )]))),
        h.db.clone(),
    );
    // IMAP has no key for an attachment, so the server cannot say this.
    let search = Mailbox::Search {
        query: "kites OR has:attachment".into(),
        account_id: None,
    };
    let view = View {
        now: crate::now_millis(),
        ..View::default()
    };

    let listing = lists
        .list(&search, &Scope::over(signed_in), &view, Loaded::nothing())
        .await
        .unwrap();

    assert_eq!(listing.rows.len(), 1, "the stored message matches");
    assert_eq!(
        listing.notices,
        ["Results for me@example.com come from the mail on this computer alone"]
    );
}

/// Mailboxes over the harness's one account, and the scope that signs it
/// in.
async fn listings(h: &crate::tests::ImapHarness) -> (Mailboxes<Connected>, Scope) {
    let signed_in = h.db.read(accounts::list_accounts).await.unwrap();
    let lists = Mailboxes::new(
        Arc::new(Connected(HashMap::from([(
            h.account_id,
            Arc::clone(&h.sync),
        )]))),
        h.db.clone(),
    );
    (lists, Scope::over(signed_in))
}

/// IMAP has no key for "in no Inbox, Sent, Drafts, Junk or Trash", so the
/// store lists the folder for the mail it holds rather than the listing
/// asking the server again and again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folder_the_server_cannot_search_lists_from_this_computer() {
    let h = imap_harness().await;
    h.imap.add_mailbox("Work", None);
    h.imap
        .deliver_flagged("Work", &message("w", "Plans", ""), &["\\Seen"], days_ago(1));
    h.bootstrap().await;
    h.sync.follow_mailbox("Work").await.unwrap();
    let (lists, scope) = listings(&h).await;
    let archive = Mailbox::Folder {
        account_id: Some(h.account_id),
        folder: Folder::Archive,
    };
    let view = View {
        now: crate::now_millis(),
        ..View::default()
    };

    let listing = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        lists.list(&archive, &scope, &view, Loaded::nothing()),
    )
    .await
    .expect("the listing ends")
    .unwrap();

    assert_eq!(listing.rows.len(), 1);
}

/// An account whose search fails is listed once, with a notice, and not
/// asked again and again for the rest of the page.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_whose_listing_fails_is_asked_once() {
    let h = imap_harness().await;
    h.imap.deliver_flagged(
        "Archive",
        &message("old", "Kites of 2025", ""),
        &["\\Seen"],
        days_ago(400),
    );
    h.bootstrap().await;
    for _ in 0..50 {
        h.imap
            .fail_on("headers", mailrs_imap::ImapError::Network("reset".into()));
    }
    let (lists, scope) = listings(&h).await;
    let search = Mailbox::Search {
        query: "kites".into(),
        account_id: None,
    };
    let view = View {
        now: crate::now_millis(),
        ..View::default()
    };

    let listing = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        lists.list(&search, &scope, &view, Loaded::nothing()),
    )
    .await
    .expect("the listing ends")
    .unwrap();

    assert_eq!(h.imap.calls_to("headers"), 1, "{:?}", h.imap.calls());
    assert!(listing.rows.is_empty());
    assert_eq!(listing.notices.len(), 1, "{:?}", listing.notices);
}
