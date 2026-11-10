//! Smart badges use fixtures, including server mail outside the local store.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::{Connected, Harness, harness};
use crate::fake::meta;
use crate::mailbox::{Counts, Mailbox, Mailboxes, SmartCountState, View};
use mailrs_domain::SmartMailbox;
use mailrs_domain::smart::{Condition, Field, UnreadCount};

fn smart(mode: UnreadCount) -> Mailbox {
    Mailbox::Smart(SmartMailbox {
        trash_sent_without_asking: false,
        id: "smart-unread".into(),
        name: "Inbox and sent".into(),
        account: None,
        match_all: true,
        unread_count: mode,
        conditions: vec![Condition {
            field: Field::InMailbox,
            value: r#"["role:inbox","role:sent"]"#.into(),
        }],
    })
}

fn lists(h: &Harness) -> Mailboxes<Connected> {
    Mailboxes::new(
        Arc::new(Connected(HashMap::from([(
            h.account_id,
            Arc::clone(&h.sync),
        )]))),
        h.db.clone(),
    )
}

async fn count(lists: &Mailboxes<Connected>, mailbox: &Mailbox, refresh: bool) -> Counts {
    let view = View {
        now: crate::now_millis(),
        ..View::default()
    };
    if refresh {
        lists
            .counts(std::slice::from_ref(mailbox), mailbox, &view)
            .await
            .unwrap()
    } else {
        lists
            .cached_counts(std::slice::from_ref(mailbox), mailbox, &view)
            .await
            .unwrap()
    }
}

async fn ready(lists: &Mailboxes<Connected>, mailbox: &Mailbox) -> Counts {
    let mut changed = lists.counts_changed();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let counts = count(lists, mailbox, true).await;
            if counts.smart.get(mailbox) != Some(&SmartCountState::Pending) {
                return counts;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .expect("the fixture answers")
}

#[tokio::test]
async fn hidden_and_local_smart_counts_do_not_search_the_server() {
    let h = harness().await;
    h.fake.seed(meta(
        "a",
        "same",
        crate::now_millis(),
        &["INBOX", "SENT", "UNREAD"],
    ));
    h.fake
        .seed(meta("b", "same", crate::now_millis(), &["INBOX", "UNREAD"]));
    h.bootstrap_all().await;
    h.fake.seed(meta(
        "remote",
        "remote",
        crate::now_millis(),
        &["INBOX", "UNREAD"],
    ));
    h.fake.with(|s| s.searched.clear());
    let lists = lists(&h);
    let hidden = smart(UnreadCount::Hidden);
    let counts = count(&lists, &hidden, true).await;
    assert!(!counts.mailboxes.contains_key(&hidden));
    assert!(!counts.smart.contains_key(&hidden));
    let local = smart(UnreadCount::Local);
    let counts = count(&lists, &local, true).await;
    assert_eq!(counts.mailboxes[&local], 2);
    assert_eq!(counts.smart[&local], SmartCountState::Local);
    assert!(h.fake.with(|s| s.searched.is_empty()));
}

#[tokio::test]
async fn exact_smart_counts_page_deduplicate_cache_and_refresh() {
    let h = harness().await;
    h.bootstrap_all().await;
    for n in 0..503 {
        h.fake.seed(meta(
            &format!("m{n}"),
            "same-thread",
            crate::now_millis(),
            &["INBOX", "SENT", "UNREAD"],
        ));
    }
    h.fake
        .seed(meta("read", "read", crate::now_millis(), &["INBOX"]));
    h.fake.with(|s| {
        s.page_size = 100;
        s.searched.clear();
    });
    let lists = lists(&h);
    let mailbox = smart(UnreadCount::Server);
    let counts = ready(&lists, &mailbox).await;
    assert_eq!(counts.mailboxes[&mailbox], 503);
    assert_eq!(counts.smart[&mailbox], SmartCountState::Server);
    let calls = h.fake.with(|s| s.searched.len());
    assert!(calls > 1, "every page is read");
    assert_eq!(count(&lists, &mailbox, true).await.mailboxes[&mailbox], 503);
    assert_eq!(h.fake.with(|s| s.searched.len()), calls);
    h.fake
        .seed(meta("new", "new", crate::now_millis(), &["SENT", "UNREAD"]));
    lists.forget_remote();
    assert_eq!(ready(&lists, &mailbox).await.mailboxes[&mailbox], 504);
}

#[tokio::test]
async fn pending_exact_counts_do_not_block_and_hiding_cancels_them() {
    let h = harness().await;
    h.bootstrap_all().await;
    let mut held = h.fake.hold("users.messages.list");
    let lists = lists(&h);
    let mailbox = smart(UnreadCount::Server);
    let counts = count(&lists, &mailbox, true).await;
    assert_eq!(counts.smart[&mailbox], SmartCountState::Pending);
    assert!(!counts.mailboxes.contains_key(&mailbox));
    held.entered().await;
    let counts = count(&lists, &smart(UnreadCount::Hidden), true).await;
    assert!(counts.smart.is_empty());
    held.release();
    let cached = count(&lists, &mailbox, false).await;
    assert_eq!(cached.smart[&mailbox], SmartCountState::Pending);
    assert!(
        !cached.mailboxes.contains_key(&mailbox),
        "the canceled answer cannot return"
    );
}

#[tokio::test]
async fn a_missing_account_does_not_make_a_partial_count_exact() {
    let h = harness().await;
    h.bootstrap_all().await;
    h.db.write(|c| mailrs_store::accounts::insert_account(c, "offline@example.com", 0))
        .await
        .unwrap();
    let lists = lists(&h);
    let mailbox = smart(UnreadCount::Server);
    let mut changed = lists.counts_changed();
    count(&lists, &mailbox, true).await;
    tokio::time::timeout(Duration::from_secs(2), changed.changed())
        .await
        .unwrap()
        .unwrap();
    let counts = count(&lists, &mailbox, false).await;
    assert_eq!(counts.smart[&mailbox], SmartCountState::Unavailable);
    assert!(!counts.mailboxes.contains_key(&mailbox));
}

#[tokio::test]
async fn exact_counts_respect_each_folders_account() {
    let h = harness().await;
    h.bootstrap_all().await;
    h.fake.seed(meta(
        "inbox",
        "inbox",
        crate::now_millis(),
        &["INBOX", "UNREAD"],
    ));
    h.fake.seed(meta(
        "sent",
        "sent",
        crate::now_millis(),
        &["SENT", "UNREAD"],
    ));
    let lists = lists(&h);
    let Mailbox::Smart(mut selected) = smart(UnreadCount::Server) else {
        unreachable!()
    };
    selected.conditions[0].value = r#"[{"account":"me@example.com","mailbox":"role:inbox"},{"account":"other@example.com","mailbox":"role:sent"}]"#.into();
    let mailbox = Mailbox::Smart(selected);
    assert_eq!(ready(&lists, &mailbox).await.mailboxes[&mailbox], 1);
}

#[tokio::test]
async fn an_imap_smart_count_combines_folders_and_text_conditions() {
    let h = super::imap_harness().await;
    h.imap.deliver(
        "INBOX",
        b"Subject: Wanted\r\n\r\nHi".to_vec(),
        crate::now_millis(),
    );
    h.imap.deliver(
        "Sent",
        b"Subject: Other\r\n\r\nHi".to_vec(),
        crate::now_millis(),
    );
    let lists = Mailboxes::new(
        Arc::new(Connected(HashMap::from([(
            h.account_id,
            Arc::clone(&h.sync),
        )]))),
        h.db.clone(),
    );
    let Mailbox::Smart(mut selected) = smart(UnreadCount::Server) else {
        unreachable!()
    };
    selected.conditions.push(Condition {
        field: Field::Subject,
        value: "Wanted".into(),
    });
    let mailbox = Mailbox::Smart(selected);
    let counts = ready(&lists, &mailbox).await;
    assert_eq!(counts.smart[&mailbox], SmartCountState::Server);
    assert_eq!(counts.mailboxes[&mailbox], 1);
}

#[tokio::test]
async fn a_read_while_counting_discards_the_old_generation() {
    let h = harness().await;
    h.fake.seed(meta(
        "one",
        "thread",
        crate::now_millis(),
        &["INBOX", "UNREAD"],
    ));
    h.bootstrap_all().await;
    let lists = lists(&h);
    let mailbox = smart(UnreadCount::Server);
    let mut held = h.fake.hold("users.messages.list");
    let mut changed = lists.counts_changed();
    count(&lists, &mailbox, true).await;
    held.entered().await;
    h.sync
        .triage_thread("thread", &crate::TriageAction::MarkRead)
        .await
        .unwrap();
    held.release();
    tokio::time::timeout(Duration::from_secs(2), changed.changed())
        .await
        .unwrap()
        .unwrap();
    let old = count(&lists, &mailbox, false).await;
    assert_eq!(old.smart[&mailbox], SmartCountState::Pending);
    assert!(!old.mailboxes.contains_key(&mailbox));
    assert_eq!(ready(&lists, &mailbox).await.mailboxes[&mailbox], 0);
}

#[tokio::test(start_paused = true)]
async fn smart_server_counts_expire_even_without_a_local_mail_change() {
    let h = harness().await;
    h.bootstrap_all().await;
    let lists = lists(&h);
    let mailbox = smart(UnreadCount::Server);
    assert_eq!(ready(&lists, &mailbox).await.mailboxes[&mailbox], 0);
    h.fake.seed(meta(
        "new",
        "new",
        crate::now_millis(),
        &["INBOX", "UNREAD"],
    ));
    let mut changed = lists.counts_changed();
    tokio::time::advance(Duration::from_secs(61)).await;
    changed.changed().await.unwrap();
    assert_eq!(ready(&lists, &mailbox).await.mailboxes[&mailbox], 1);
}
