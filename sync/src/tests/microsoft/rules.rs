use mailrs_domain::{Filter, FilterAction, FilterCriteria, MailSet, Role, Vacation, category};
use mailrs_graph::{EmailAddress, MessageRule, Recipient, RuleActions, RulePredicates};

use super::outlook;
use crate::settings::{Replaced, replace_via};
use crate::services::MailBackend;
use crate::{AutoReplyService, BackendError, RulesService};

fn from(address: &str) -> RulePredicates {
    RulePredicates {
        from_addresses: vec![Recipient { email_address: EmailAddress { name: None, address: Some(address.into()) } }],
        ..RulePredicates::default()
    }
}

fn focus(address: &str, other: bool) -> Filter {
    let set = vec![MailSet::Category(category::OTHER.into())];
    let (add, remove) = if other { (set, vec![]) } else { (vec![], set) };
    Filter {
        criteria: FilterCriteria { from: Some(address.into()), ..FilterCriteria::default() },
        action: FilterAction { add, remove, ..FilterAction::default() },
        ..Filter::default()
    }
}

#[tokio::test]
async fn a_plain_rule_reads_as_a_filter() {
    let h = outlook().await;
    let archive = h.fake.folder_id("archive");
    h.fake.with(|s| {
        s.rules.push(MessageRule {
            id: "r1".into(),
            display_name: "News".into(),
            sequence: 1,
            is_enabled: true,
            conditions: Some(from("news@example.com")),
            actions: Some(RuleActions {
                move_to_folder: Some(archive.clone()),
                mark_as_read: Some(true),
                stop_processing_rules: Some(true),
                ..RuleActions::default()
            }),
            ..MessageRule::default()
        })
    });
    h.sync.services().mail.mailboxes().await.unwrap();
    let filters = h.sync.services().rules.clone().unwrap().filters().await.unwrap();
    let rule = &filters[0];
    assert!(!rule.read_only);
    assert_eq!(rule.criteria.from.as_deref(), Some("news@example.com"));
    assert!(rule.action.add.contains(&MailSet::Role(Role::Archive)));
    assert!(rule.action.remove.contains(&MailSet::Unseen));
}

#[tokio::test]
async fn a_rule_the_filter_cannot_hold_is_read_only() {
    let h = outlook().await;
    h.fake.with(|s| {
        s.rules.push(MessageRule {
            id: "r1".into(),
            is_enabled: true,
            conditions: Some(from("a@example.com")),
            exceptions: Some(from("b@example.com")),
            actions: Some(RuleActions { delete: Some(true), ..RuleActions::default() }),
            ..MessageRule::default()
        });
        let mut importance = RuleActions::default();
        importance.other.insert("markImportance".into(), serde_json::json!("high"));
        s.rules.push(MessageRule {
            id: "r2".into(),
            is_enabled: true,
            conditions: Some(from("c@example.com")),
            actions: Some(importance),
            ..MessageRule::default()
        });
    });
    let filters = h.sync.services().rules.clone().unwrap().filters().await.unwrap();
    assert_eq!(filters.len(), 2);
    assert!(filters.iter().all(|f| f.read_only), "{filters:?}");
}

#[tokio::test]
async fn blocking_a_sender_writes_a_rule_that_deletes() {
    let h = outlook().await;
    h.sync.services().mail.mailboxes().await.unwrap();
    let made = h.sync.services().rules.clone().unwrap().create_filter(&Filter::block("pest@example.com")).await.unwrap();
    assert!(made.id.is_some());
    let rule = h.fake.with(|s| s.rules[0].clone());
    assert_eq!(rule.actions.unwrap().delete, Some(true));
    assert_eq!(rule.conditions.unwrap().from_addresses[0].email_address.address.as_deref(), Some("pest@example.com"));
}

#[tokio::test]
async fn a_tag_rule_assigns_the_category() {
    let h = outlook().await;
    h.fake.add_category("Red", "preset0");
    h.sync.services().mail.mailboxes().await.unwrap();
    let filter = Filter {
        criteria: FilterCriteria { subject: Some("invoice".into()), ..FilterCriteria::default() },
        action: FilterAction { add: vec![MailSet::Mailbox("category:Red".into())], ..FilterAction::default() },
        ..Filter::default()
    };
    h.sync.services().rules.clone().unwrap().create_filter(&filter).await.unwrap();
    let rule = h.fake.with(|s| s.rules[0].clone());
    assert_eq!(rule.actions.unwrap().assign_categories, ["Red"]);
    assert_eq!(rule.conditions.unwrap().subject_contains, ["invoice"]);
}

#[tokio::test]
async fn a_focus_rule_is_an_override_both_ways() {
    let h = outlook().await;
    let rules = h.sync.services().rules.clone().unwrap();
    let made = rules.create_filter(&focus("shop@example.com", true)).await.unwrap();
    assert!(made.id.as_deref().is_some_and(|id| id.starts_with("override:")));
    assert_eq!(h.fake.with(|s| s.overrides[0].classify_as.clone()), "other");
    assert!(h.fake.with(|s| s.rules.is_empty()), "no message rule for it");
    let listed = rules.filters().await.unwrap();
    assert_eq!(listed, std::slice::from_ref(&made));
    rules.delete_filter(made.id.as_deref().unwrap()).await.unwrap();
    assert!(h.fake.with(|s| s.overrides.is_empty()));
}

#[tokio::test]
async fn an_edited_override_still_writes_an_override() {
    let h = outlook().await;
    let rules = h.sync.services().rules.clone().unwrap();
    let made = rules.create_filter(&focus("shop@example.com", true)).await.unwrap();
    let old = made.id.clone().unwrap();
    // The form hands back the filter it showed, with a new sender.
    let edited = Filter { criteria: FilterCriteria { from: Some("deals@example.com".into()), ..made.criteria.clone() }, ..made };
    let replaced = replace_via(&rules, &h.db, h.account_id, &old, edited).await.unwrap();
    assert!(matches!(replaced, Replaced::Swapped(_)));
    assert!(h.fake.with(|s| s.rules.is_empty()), "no message rule for it");
    let senders = h.fake.with(|s| {
        s.overrides.iter().map(|o| o.sender_email_address.address.clone().unwrap()).collect::<Vec<_>>()
    });
    assert_eq!(senders, ["deals@example.com"]);
}

#[tokio::test]
async fn a_focused_override_lists_as_a_removal() {
    let h = outlook().await;
    let rules = h.sync.services().rules.clone().unwrap();
    rules.create_filter(&focus("boss@example.com", false)).await.unwrap();
    let listed = rules.filters().await.unwrap();
    assert_eq!(listed[0].action.remove, [MailSet::Category(category::OTHER.into())]);
    assert!(listed[0].action.add.is_empty());
}

#[tokio::test]
async fn a_filter_outlook_cannot_hold_is_refused() {
    let h = outlook().await;
    let filter = Filter {
        criteria: FilterCriteria { negated_query: Some("unsubscribe".into()), ..FilterCriteria::default() },
        action: FilterAction { add: vec![MailSet::Role(Role::Trash)], ..FilterAction::default() },
        ..Filter::default()
    };
    let refused = h.sync.services().rules.clone().unwrap().create_filter(&filter).await;
    assert!(matches!(refused, Err(BackendError::Refused(_))));
}

#[tokio::test]
async fn the_automatic_reply_goes_both_ways() {
    let h = outlook().await;
    let replies = h.sync.services().auto_reply.clone().unwrap();
    let away = Vacation {
        enabled: true,
        subject: "ignored".into(),
        body: "Away until Monday.\nAsk Bo.".into(),
        contacts_only: true,
        domain_only: false,
        start: Some(1_790_000_000_000),
        end: Some(1_790_600_000_000),
    };
    replies.set_vacation(&away).await.unwrap();
    let held = h.fake.with(|s| s.replies.clone());
    assert_eq!(held.status, "scheduled");
    assert_eq!(held.external_audience, "contactsOnly");
    assert!(held.internal_reply_message.contains("Ask Bo."));
    let back = replies.vacation().await.unwrap();
    assert_eq!(back, Vacation { subject: String::new(), ..away.clone() });
    replies
        .set_vacation(&Vacation { domain_only: true, contacts_only: false, start: None, end: None, ..away })
        .await
        .unwrap();
    let held = h.fake.with(|s| s.replies.clone());
    assert_eq!((held.status.as_str(), held.external_audience.as_str()), ("alwaysEnabled", "none"));
}

#[tokio::test]
async fn an_organization_that_blocks_rules_turns_them_off() {
    let h = outlook().await;
    h.fake.refuse(crate::fake::Area::Rules, mailrs_graph::GraphError::AccessDenied { code: "ErrorAccessDenied".into() });
    assert!(matches!(h.sync.services().rules.clone().unwrap().filters().await, Err(BackendError::Unsupported)));
    assert!(!h.sync.services().offers().rules);
}
