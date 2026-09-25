//! Folders and smart mailboxes of an account whose server reads no Gmail
//! syntax list from the store's copy of the mail, not a server search.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::{Account, AccountId, AccountState, Folder, Provider};

use super::{Connected, imap_harness};
use crate::fake::raw_message;
use crate::mailbox::{Loaded, Mailbox, Mailboxes, Scope, View};

/// The subjects `folder` lists for `account_id`, newest first.
async fn subjects(
    lists: &Mailboxes<Connected>,
    scope: &Scope,
    view: &View,
    account_id: AccountId,
    folder: Folder,
) -> Vec<String> {
    let mailbox = Mailbox::Folder {
        account_id: Some(account_id),
        folder,
    };
    let listing = lists
        .list(&mailbox, scope, view, Loaded::default())
        .await
        .unwrap();
    assert!(listing.notices.is_empty(), "{:?}", listing.notices);
    listing.rows.into_iter().map(|row| row.subject).collect()
}

#[tokio::test]
async fn an_imap_folder_lists_from_the_store_without_a_server_search() {
    let h = imap_harness().await;
    let now = crate::now_millis();
    h.imap
        .deliver("INBOX", raw_message("hello", "Hello", now, None), now);
    let earlier = now - 60_000;
    h.imap
        .deliver("Archive", raw_message("budget", "Budget", earlier, None), earlier);

    h.bootstrap().await;
    // Sync itself may search while it fills the window; only the listings
    // below must not.
    let searched = h.imap.calls_to("search");

    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    let lists = Mailboxes::new(Arc::new(Connected(connected)), h.db.clone());
    let scope = Scope::over([Account {
        id: h.account_id,
        email: "me@example.com".into(),
        state: AccountState::Ok,
        provider: Provider::Imap,
        provider_name: Some("Fastmail".into()),
    }]);
    let view = View {
        now,
        ..View::default()
    };

    assert_eq!(
        subjects(&lists, &scope, &view, h.account_id, Folder::Archive).await,
        ["Budget"]
    );
    assert_eq!(
        subjects(&lists, &scope, &view, h.account_id, Folder::AllMail).await,
        ["Hello", "Budget"]
    );
    assert_eq!(
        h.imap.calls_to("search"),
        searched,
        "a folder listing asked the server to search"
    );
}
