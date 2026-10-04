//! The found servers, local rules with their watermark, and the queue of
//! rule changes, as the store keeps them.

mod common;

use common::{db, meta};
use mailrs_domain::{Filter, FilterAction, FilterCriteria, MailSet, Role};
use mailrs_store::local_rules::{self, OVERLAP};
use mailrs_store::messages::{self, Change};
use mailrs_store::rule_changes::{self, RuleChange};
use mailrs_store::services::{self, FoundService, ServiceKind};

// Part 4 adds a field to `Filter`; the update keeps this literal compiling
// when it merges, and clippy sees it as needless until then.
#[allow(clippy::needless_update)]
fn rule(id: &str, from: &str) -> Filter {
    Filter {
        id: Some(id.into()),
        criteria: FilterCriteria { from: Some(from.into()), ..FilterCriteria::default() },
        action: FilterAction { remove: vec![MailSet::Role(Role::Inbox)], ..FilterAction::default() },
        ..Filter::default()
    }
}

#[test]
fn a_found_service_is_kept_with_its_user_and_whether_it_is_confirmed() {
    let (conn, a) = db();
    let found = FoundService {
        kind: ServiceKind::CalDav,
        url: "https://caldav.fastmail.com/".into(),
        user_name: "me@fastmail.com".into(),
        confirmed: true,
        source: "table".into(),
    };
    services::save(&conn, a, &found).unwrap();
    let moved = FoundService { url: "https://dav.example.net/".into(), confirmed: false, ..found.clone() };
    services::save(&conn, a, &moved).unwrap();
    assert_eq!(services::load(&conn, a).unwrap(), [moved], "a second save replaces the first");
    services::remove(&conn, a, ServiceKind::CalDav).unwrap();
    assert!(services::load(&conn, a).unwrap().is_empty());
}

#[test]
fn local_rules_list_in_the_order_they_were_made() {
    let (conn, a) = db();
    local_rules::add(&conn, a, &rule("local-b", "b@example.com")).unwrap();
    local_rules::add(&conn, a, &rule("local-a", "a@example.com")).unwrap();
    let ids: Vec<Option<String>> = local_rules::list(&conn, a).unwrap().into_iter().map(|f| f.id).collect();
    assert_eq!(ids, [Some("local-b".into()), Some("local-a".into())]);
    assert!(local_rules::remove(&conn, a, "local-b").unwrap());
    assert!(!local_rules::remove(&conn, a, "local-b").unwrap(), "a rule already gone reports so");
    assert_eq!(local_rules::list(&conn, a).unwrap().len(), 1);
}

/// Inbox mail at `dates`, one message each, as the sync engine stores it.
fn inbox(conn: &rusqlite::Connection, a: i64, dates: &[(&str, i64)]) {
    let changes: Vec<Change> = dates
        .iter()
        .map(|(id, at)| Change::Upsert { meta: Box::new(meta(a, id, id, *at, &["INBOX"])), generation: 1 })
        .collect();
    messages::apply(conn, a, &changes).unwrap();
}

#[test]
fn candidates_start_at_the_watermark_oldest_first() {
    let (conn, a) = db();
    let hour = OVERLAP;
    inbox(&conn, a, &[("old", 1_000), ("new-2", 10 * hour + 2), ("new-1", 10 * hour + 1)]);
    assert!(local_rules::candidates(&conn, a, 100).unwrap().is_empty(), "no watermark, no rules have run");
    local_rules::start_running(&conn, a, 10 * hour).unwrap();
    let ids: Vec<String> = local_rules::candidates(&conn, a, 100).unwrap().into_iter().map(|m| m.id).collect();
    assert_eq!(ids, ["new-1", "new-2"]);
}

#[test]
fn a_burst_in_one_second_is_offered_once_each() {
    let (conn, a) = db();
    local_rules::start_running(&conn, a, 0).unwrap();
    inbox(&conn, a, &[("a", 5_000), ("b", 5_000)]);
    local_rules::mark_ran(&conn, a, &[("a".into(), 5_000)]).unwrap();
    // "c" arrives later in the same second the watermark reached.
    inbox(&conn, a, &[("c", 5_000)]);
    let ids: Vec<String> = local_rules::candidates(&conn, a, 100).unwrap().into_iter().map(|m| m.id).collect();
    assert_eq!(ids, ["b", "c"]);
}

#[test]
fn the_ran_set_keeps_only_the_last_hour() {
    let (conn, a) = db();
    local_rules::start_running(&conn, a, 0).unwrap();
    local_rules::mark_ran(&conn, a, &[("early".into(), 1_000)]).unwrap();
    local_rules::mark_ran(&conn, a, &[("late".into(), 1_000 + 2 * OVERLAP)]).unwrap();
    let kept: i64 = conn
        .query_row("SELECT COUNT(*) FROM local_rules_ran WHERE account_id = ?1", [a], |r| r.get(0))
        .unwrap();
    assert_eq!(kept, 1);
    assert_eq!(local_rules::ran_until(&conn, a).unwrap(), Some(1_000 + 2 * OVERLAP));
}

#[test]
fn start_running_never_moves_a_watermark_back() {
    let (conn, a) = db();
    local_rules::start_running(&conn, a, 500).unwrap();
    local_rules::start_running(&conn, a, 900).unwrap();
    assert_eq!(local_rules::ran_until(&conn, a).unwrap(), Some(500));
}

#[test]
fn rule_changes_wait_in_order() {
    let (conn, a) = db();
    rule_changes::enqueue(&conn, a, &RuleChange::Create(rule("r2", "x@example.com"))).unwrap();
    let second = rule_changes::enqueue(&conn, a, &RuleChange::Delete("r1".into())).unwrap();
    let queued = rule_changes::queued(&conn, a).unwrap();
    assert_eq!(queued.len(), 2);
    assert!(matches!(&queued[0].change, RuleChange::Create(f) if f.id.as_deref() == Some("r2")));
    assert_eq!(queued[1].change, RuleChange::Delete("r1".into()));
    rule_changes::dequeue(&conn, second).unwrap();
    assert_eq!(rule_changes::queued(&conn, a).unwrap().len(), 1);
}
