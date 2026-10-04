//! Local rules through the engine: new Inbox mail runs through them once,
//! oldest first, through the same mail actions the window uses, and Undo
//! takes a rule's change back.

use std::sync::Arc;

use mailrs_domain::{Filter, FilterAction, FilterCriteria, MailSet, Role};

use crate::fake::FakeImap;
use crate::rules::RulesEngine;
use crate::services::local::LocalRules;
use crate::tests::{Connected, imap_harness, imap_harness_on};
use crate::{BackendError, MailActions, OneClick, RulesService};

fn archive_from(address: &str) -> Filter {
    Filter {
        id: None,
        criteria: FilterCriteria { from: Some(address.into()), ..FilterCriteria::default() },
        action: FilterAction { remove: vec![MailSet::Role(Role::Inbox)], ..FilterAction::default() },
        ..Filter::default()
    }
}

fn letter(n: u32, from: &str) -> Vec<u8> {
    format!("From: {from}\r\nTo: me@example.com\r\nSubject: Note {n}\r\nMessage-ID: <n{n}@example.com>\r\n\r\nHi.\r\n")
        .into_bytes()
}

/// The engine over one IMAP harness, with the account's rules service.
async fn engine_for(h: &crate::tests::ImapHarness) -> (RulesEngine<Connected>, LocalRules) {
    let accounts = Arc::new(Connected([(h.account_id, Arc::clone(&h.sync))].into()));
    let actions =
        Arc::new(MailActions::new(Arc::clone(&accounts), h.db.clone(), OneClick::Fake(Arc::default())));
    (RulesEngine::new(accounts, h.db.clone(), actions), LocalRules::new(h.db.clone(), h.account_id))
}

#[tokio::test]
async fn a_rule_runs_on_mail_that_arrives_after_it_was_made() {
    let h = imap_harness().await;
    // Two hours back: past the hour the watermark looks behind it.
    h.imap.deliver("INBOX", letter(1, "news@example.com"), crate::now_millis() - 7_200_000);
    h.bootstrap().await;
    let (engine, rules) = engine_for(&h).await;
    rules.create_filter(&archive_from("news@example.com")).await.unwrap();
    assert!(
        engine.run_due(h.account_id).await.unwrap().acted_on.is_empty(),
        "mail from before the rule stays put"
    );
    h.imap.deliver("INBOX", letter(2, "news@example.com"), crate::now_millis() + 1_000);
    h.sync.incremental().await.unwrap();
    let ran = engine.run_due(h.account_id).await.unwrap();
    assert_eq!(ran.acted_on.len(), 1);
    assert!(
        h.imap.messages_in("INBOX").iter().all(|m| !m.contains("Note 2")),
        "the server moved it out of the Inbox"
    );
}

#[tokio::test]
async fn a_burst_in_one_second_runs_each_message_once() {
    let h = imap_harness().await;
    h.bootstrap().await;
    let (engine, rules) = engine_for(&h).await;
    rules.create_filter(&archive_from("news@example.com")).await.unwrap();
    let at = crate::now_millis() + 2_000;
    for n in 0..3 {
        h.imap.deliver("INBOX", letter(n, "news@example.com"), at);
    }
    h.sync.incremental().await.unwrap();
    let first = engine.run_due(h.account_id).await.unwrap();
    assert_eq!(first.acted_on.len(), 3);
    let second = engine.run_due(h.account_id).await.unwrap();
    assert_eq!(second.looked_at, 0, "{second:?}");
}

#[tokio::test]
async fn a_pass_cut_short_runs_the_rest_at_the_next_start() {
    let h = imap_harness().await;
    h.bootstrap().await;
    let (engine, rules) = engine_for(&h).await;
    rules.create_filter(&archive_from("news@example.com")).await.unwrap();
    let at = crate::now_millis() + 2_000;
    for n in 0..(crate::rules::PASS as u32 + 5) {
        h.imap.deliver("INBOX", letter(n, "news@example.com"), at + i64::from(n));
    }
    h.sync.incremental().await.unwrap();
    let first = engine.run_due(h.account_id).await.unwrap();
    assert_eq!(first.looked_at, crate::rules::PASS);
    // A new start: a new engine over the same store.
    let (restarted, _) = engine_for(&h).await;
    let rest = restarted.run_due(h.account_id).await.unwrap();
    assert_eq!(rest.looked_at, 5);
}

#[tokio::test]
async fn undo_takes_a_rule_s_change_back() {
    let h = imap_harness().await;
    h.bootstrap().await;
    let (engine, rules) = engine_for(&h).await;
    rules.create_filter(&archive_from("news@example.com")).await.unwrap();
    h.imap.deliver("INBOX", letter(1, "news@example.com"), crate::now_millis() + 1_000);
    h.sync.incremental().await.unwrap();
    engine.run_due(h.account_id).await.unwrap();
    let undone = engine.actions().undo().await.expect("the rule's change is on the undo stack");
    assert!(undone.outcome.failed.is_empty());
    assert!(h.imap.messages_in("INBOX").iter().any(|m| m.contains("Note 1")));
}

#[tokio::test]
async fn rules_run_in_the_order_they_were_made() {
    let h = imap_harness_on(FakeImap::new(), crate::tests::fake_settings()).await;
    h.imap.add_mailbox("Newsletters", None);
    h.bootstrap().await;
    let (engine, rules) = engine_for(&h).await;
    let file = Filter {
        action: FilterAction { add: vec![MailSet::Mailbox("Newsletters".into())], ..FilterAction::default() },
        ..archive_from("news@example.com")
    };
    rules.create_filter(&file).await.unwrap();
    rules.create_filter(&archive_from("news@example.com")).await.unwrap();
    h.imap.deliver("INBOX", letter(1, "news@example.com"), crate::now_millis() + 1_000);
    h.sync.incremental().await.unwrap();
    engine.run_due(h.account_id).await.unwrap();
    // The first rule moved it to Newsletters; the second no longer finds
    // it in the Inbox, so it does not go on to the Archive.
    assert!(h.imap.messages_in("Newsletters").iter().any(|m| m.contains("Note 1")));
}

#[tokio::test]
async fn a_rule_that_reads_the_size_runs_on_big_mail_only() {
    let h = imap_harness().await;
    h.bootstrap().await;
    let (engine, rules) = engine_for(&h).await;
    let big = Filter {
        criteria: FilterCriteria {
            size: Some(2_000),
            size_comparison: Some("larger".into()),
            ..FilterCriteria::default()
        },
        ..archive_from("unused")
    };
    rules.create_filter(&big).await.unwrap();
    let at = crate::now_millis() + 1_000;
    h.imap.deliver("INBOX", letter(1, "a@example.com"), at);
    let mut long = letter(2, "b@example.com");
    long.extend(std::iter::repeat_n(b'x', 4_000));
    h.imap.deliver("INBOX", long, at + 1);
    h.sync.incremental().await.unwrap();
    assert_eq!(engine.run_due(h.account_id).await.unwrap().acted_on.len(), 1);
    assert!(h.imap.messages_in("INBOX").iter().any(|m| m.contains("Note 1")));
    assert!(h.imap.messages_in("INBOX").iter().all(|m| !m.contains("Note 2")));
}

#[tokio::test]
async fn an_edited_local_rule_keeps_its_place() {
    let h = imap_harness().await;
    let (_, rules) = engine_for(&h).await;
    let first = rules.create_filter(&archive_from("a@example.com")).await.unwrap();
    rules.create_filter(&archive_from("b@example.com")).await.unwrap();
    let edited = rules.replace_filter(first.id.as_deref().unwrap(), &archive_from("z@example.com")).await.unwrap();
    assert_ne!(edited.id, first.id);
    let now = rules.filters().await.unwrap();
    assert_eq!(now.len(), 2);
    assert_eq!(now[0], edited, "the edited rule stays first");
    assert_eq!(now[1].criteria.from.as_deref(), Some("b@example.com"));
    assert!(matches!(
        rules.replace_filter("local-missing", &archive_from("y@example.com")).await,
        Err(BackendError::NotFound)
    ));
}

#[tokio::test]
async fn a_read_only_rule_is_never_changed_or_deleted() {
    let h = imap_harness().await;
    let (_, rules) = engine_for(&h).await;
    let kept = Filter { read_only: true, ..archive_from("a@example.com") };
    let made = rules.create_filter(&kept).await.unwrap();
    let id = made.id.as_deref().unwrap();
    assert!(matches!(rules.replace_filter(id, &archive_from("z@example.com")).await, Err(BackendError::Refused(_))));
    assert!(matches!(rules.delete_filter(id).await, Err(BackendError::Refused(_))));
    assert_eq!(rules.filters().await.unwrap(), [made]);
}

#[tokio::test]
async fn a_new_rule_gets_a_local_id_and_a_deleted_one_is_gone() {
    let h = imap_harness().await;
    let (_, rules) = engine_for(&h).await;
    let made = rules.create_filter(&archive_from("a@example.com")).await.unwrap();
    let id = made.id.clone().unwrap();
    assert!(id.starts_with("local-") && id.len() == "local-".len() + 16, "{id}");
    rules.delete_filter(&id).await.unwrap();
    assert!(rules.filters().await.unwrap().is_empty());
    assert!(matches!(rules.delete_filter(&id).await, Err(BackendError::NotFound)));
}
