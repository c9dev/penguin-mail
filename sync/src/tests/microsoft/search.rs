//! Search results join stored mail and open through the same account.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::{Account, AccountState, Provider};
use mailrs_store::messages;

use super::{Outlook, outlook};
use crate::fake::FakeMail;
use crate::tests::Connected;
use crate::{Loaded, Mailbox, Mailboxes, Scope, SearchQuery, View, now_millis};

const DAY: i64 = 86_400_000;

async fn searched_mail(rest_ids: bool) -> (Outlook, Vec<String>) {
    let h = outlook().await;
    let archive = h.fake.folder_id("archive");
    let ids = [5, 100]
        .into_iter()
        .map(|days| {
            h.fake.deliver(
                &archive,
                FakeMail {
                    at: now_millis() - days * DAY,
                    ..FakeMail::default()
                },
            )
        })
        .collect::<Vec<_>>();
    h.bootstrap_all().await;
    assert!(h.stored(&ids[0]).await);
    assert!(!h.stored(&ids[1]).await);
    h.fake.with(|s| s.search_rest_ids = rest_ids);
    (h, ids)
}

#[tokio::test]
async fn search_joins_rest_ids_to_the_stored_immutable_ids() {
    let (h, mut expected) = searched_mail(true).await;
    let found = h
        .sync
        .search_listing(&SearchQuery::Native("from:ann@example.com".into()), 100)
        .await
        .unwrap();
    let mut ids = h
        .sync
        .metadata_of(&found.refs)
        .await
        .unwrap()
        .into_iter()
        .map(|message| message.id)
        .collect::<Vec<_>>();
    ids.sort();
    expected.sort();
    assert_eq!(ids, expected, "each matching message appears once");
}

#[tokio::test]
async fn search_metadata_outside_the_window_belongs_to_its_account() {
    let (h, _) = searched_mail(false).await;
    let found = h
        .sync
        .search_listing(&SearchQuery::Native("from:ann@example.com".into()), 100)
        .await
        .unwrap();
    let metas = h.sync.metadata_of(&found.refs).await.unwrap();
    assert_eq!(metas.len(), 2);
    for meta in metas {
        assert_eq!(
            meta.account_id, h.account_id,
            "{} lost its account",
            meta.id
        );
    }
}

#[tokio::test]
async fn cached_and_older_search_results_open_without_duplicate_rows() {
    let (h, ids) = searched_mail(true).await;
    let lists = Mailboxes::new(
        Arc::new(Connected(HashMap::from([(
            h.account_id,
            Arc::clone(&h.sync),
        )]))),
        h.db.clone(),
    );
    let scope = Scope::of_mail_accounts(&[Account {
        id: h.account_id,
        email: "me@outlook.com".into(),
        state: AccountState::Ok,
        provider: Provider::Microsoft,
        provider_name: None,
    }]);
    let mailbox = Mailbox::Search {
        query: "from:ann@example.com".into(),
        account_id: Some(h.account_id),
    };
    for threading in [true, false] {
        let listing = lists
            .list(
                &mailbox,
                &scope,
                &View {
                    threading,
                    ..View::default()
                },
                Loaded::nothing(),
            )
            .await
            .unwrap();
        assert_eq!(listing.rows.len(), 2, "one row per matching message");
        for row in listing.rows {
            assert_eq!(row.account_id, h.account_id);
            assert_eq!(row.message_count, 1);
            h.sync.ensure_thread(&row.id).await.unwrap();
            let account = row.account_id;
            let thread = row.id.clone();
            let opened =
                h.db.read(move |c| messages::thread_messages(c, account, &thread))
                    .await
                    .unwrap();
            assert_eq!(opened.len(), 1);
            assert!(ids.contains(&opened[0].id));
            assert!(h.sync.body(&opened[0].id).await.unwrap().text.is_some());
        }
    }
}

#[tokio::test]
async fn search_pages_show_each_match_once_in_both_views() {
    let h = outlook().await;
    let archive = h.fake.folder_id("archive");
    let at = now_millis();
    let expected = (0..31)
        .map(|index| {
            h.fake.deliver(
                &archive,
                FakeMail {
                    at: at
                        - if index == 30 {
                            100 * DAY
                        } else {
                            DAY + index * 1000
                        },
                    ..FakeMail::default()
                },
            )
        })
        .collect::<Vec<_>>();
    h.bootstrap_all().await;
    h.fake.with(|s| s.search_rest_ids = true);
    let scope = Scope::of_mail_accounts(&[Account {
        id: h.account_id,
        email: "me@outlook.com".into(),
        state: AccountState::Ok,
        provider: Provider::Microsoft,
        provider_name: None,
    }]);
    let mailbox = Mailbox::Search {
        query: "from:ann@example.com".into(),
        account_id: Some(h.account_id),
    };
    for threading in [false, true] {
        let lists = Mailboxes::new(
            Arc::new(Connected(HashMap::from([(
                h.account_id,
                Arc::clone(&h.sync),
            )]))),
            h.db.clone(),
        );
        let view = View {
            threading,
            ..View::default()
        };
        let mut rows = Vec::new();
        for page_number in 0..10 {
            let page = lists
                .list(&mailbox, &scope, &view, Loaded::rows(&rows))
                .await
                .unwrap();
            let more = page.more;
            assert!(rows.len() + page.rows.len() <= 31, "search repeats rows");
            rows.extend(page.rows);
            if !more {
                break;
            }
            assert!(page_number < 9, "search pagination never finishes");
        }
        let ids = rows
            .iter()
            .map(|r| r.preview_message_id.clone().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            ids, expected,
            "every matching message appears once, newest first"
        );
        assert!(rows.iter().all(|r| r.message_count == 1));
        let reloaded = lists
            .list(&mailbox, &scope, &view, Loaded::nothing())
            .await
            .unwrap();
        assert_eq!(reloaded.rows.len(), 31);
        assert!(reloaded.rows.iter().all(|r| r.message_count == 1));
    }
}
