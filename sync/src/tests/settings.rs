use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Local, NaiveDate, TimeZone};
use mailrs_domain::{EpochMillis, Filter, FilterAction, FilterCriteria, MailSet, Role};
use mailrs_gmail::GmailError;

use super::{Connected, Harness, harness};
use crate::hidden;
use crate::settings::{AccountSettings, AutomaticReply, HIDE_MY_EMAIL_LABEL, Permitted, Replaced};
use crate::{AccountServices, AccountSync, BackendError, SyncError};

fn settings(h: &Harness) -> AccountSettings<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    AccountSettings::new(Arc::new(Connected(connected)), h.db.clone())
}

/// Settings over the harness's account served by `services` in place of
/// the full fake.
fn settings_over(h: &Harness, services: AccountServices) -> AccountSettings<Connected> {
    let (sender, _events) = async_channel::unbounded();
    let sync = Arc::new(AccountSync::new(h.account_id, services, h.db.clone(), sender));
    let connected = HashMap::from([(h.account_id, sync)]);
    AccountSettings::new(Arc::new(Connected(connected)), h.db.clone())
}

/// Local midnight at the start of the given day.
fn day(year: i32, month: u32, date: u32) -> EpochMillis {
    let start = NaiveDate::from_ymd_opt(year, month, date)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .unwrap();
    Local
        .from_local_datetime(&start)
        .earliest()
        .unwrap()
        .timestamp_millis()
}

fn away(first: EpochMillis, last: EpochMillis) -> AutomaticReply {
    AutomaticReply {
        enabled: true,
        subject: "Away".into(),
        body: "Back soon.".into(),
        first_day: Some(first),
        last_day: Some(last),
        ..AutomaticReply::default()
    }
}

#[tokio::test]
async fn an_automatic_reply_ends_the_midnight_after_its_last_day() {
    let h = harness().await;
    let settings = settings(&h);
    let reply = away(day(2026, 3, 1), day(2026, 3, 3));

    let stored = settings
        .set_automatic_reply(h.account_id, &reply)
        .await
        .unwrap();
    assert_eq!(stored, Permitted::Done(()));

    let gmail = h.fake.with(|s| s.vacation.clone());
    assert_eq!(gmail.start, Some(day(2026, 3, 1)));
    assert_eq!(gmail.end, Some(day(2026, 3, 4)), "Gmail's end is exclusive");

    let read = settings
        .automatic_reply(h.account_id)
        .await
        .unwrap()
        .done()
        .unwrap();
    assert_eq!(read.last_day, Some(day(2026, 3, 3)), "the same last day");
    assert_eq!(read, reply);
}

#[tokio::test]
async fn a_one_day_reply_keeps_that_day() {
    let h = harness().await;
    let settings = settings(&h);
    let reply = away(day(2026, 7, 14), day(2026, 7, 14));

    settings
        .set_automatic_reply(h.account_id, &reply)
        .await
        .unwrap();
    assert_eq!(
        h.fake.with(|s| s.vacation.end),
        Some(day(2026, 7, 15)),
        "one whole day"
    );
    let read = settings
        .automatic_reply(h.account_id)
        .await
        .unwrap()
        .done()
        .unwrap();
    assert_eq!(read.first_day, read.last_day);
}

#[tokio::test]
async fn a_missing_permission_is_its_own_answer() {
    let h = harness().await;
    let settings = settings(&h);

    h.fake.fail_next(GmailError::MissingScope);
    assert_eq!(
        settings.automatic_reply(h.account_id).await.unwrap(),
        Permitted::NeedsPermission
    );

    h.fake.fail_next(GmailError::MissingScope);
    assert_eq!(
        settings.rules(h.account_id).await.unwrap(),
        Permitted::NeedsPermission
    );

    h.fake.fail_next(GmailError::MissingScope);
    assert_eq!(
        settings
            .block_sender(h.account_id, "spam@example.com")
            .await
            .unwrap(),
        Permitted::NeedsPermission
    );

    h.fake.fail_next(GmailError::Network("offline".into()));
    let err = settings.rules(h.account_id).await.unwrap_err();
    assert!(err.to_string().contains("offline"), "{err}");
}

#[tokio::test]
async fn rules_are_created_and_deleted() {
    let h = harness().await;
    let settings = settings(&h);
    assert!(settings.rules(h.account_id).await.unwrap() == Permitted::Done(vec![]));

    let created = settings
        .block_sender(h.account_id, "ads@example.com")
        .await
        .unwrap()
        .done()
        .unwrap();
    let id = created.id.clone().expect("Gmail gives the rule an id");

    let rules = settings.rules(h.account_id).await.unwrap().done().unwrap();
    assert_eq!(rules, [created]);
    assert_eq!(rules[0].criteria.from.as_deref(), Some("ads@example.com"));
    assert_eq!(rules[0].action.add, [MailSet::Role(Role::Trash)]);
    assert_eq!(rules[0].action.remove, [MailSet::Role(Role::Inbox)]);

    settings.delete_rule(h.account_id, &id).await.unwrap();
    assert_eq!(
        settings.rules(h.account_id).await.unwrap().done().unwrap(),
        []
    );
}

/// A rule from news@example.com that skips the Inbox, made at the fake.
async fn a_news_rule(h: &Harness, settings: &AccountSettings<Connected>) -> Filter {
    let rule = Filter {
        criteria: FilterCriteria {
            from: Some("news@example.com".into()),
            ..FilterCriteria::default()
        },
        action: FilterAction {
            remove: vec![MailSet::Role(Role::Inbox)],
            ..FilterAction::default()
        },
        ..Filter::default()
    };
    settings
        .add_rule(h.account_id, rule)
        .await
        .unwrap()
        .done()
        .unwrap()
}

/// The same rule marking its mail read as well.
fn marks_read(rule: &Filter) -> Filter {
    let mut edited = rule.clone();
    edited.action.remove.push(MailSet::Unseen);
    edited
}

#[tokio::test]
async fn replacing_a_rule_leaves_only_the_new_one() {
    let h = harness().await;
    let settings = settings(&h);
    let old = a_news_rule(&h, &settings).await;

    let replaced = settings
        .replace_rule(h.account_id, &old, marks_read(&old))
        .await
        .unwrap()
        .done()
        .unwrap();
    let Replaced::Swapped(new) = replaced else {
        panic!("the old rule should be gone: {replaced:?}");
    };
    assert_ne!(new.id, old.id, "Gmail gives the new rule an id of its own");
    assert_eq!(
        settings.rules(h.account_id).await.unwrap().done().unwrap(),
        [new]
    );
}

#[tokio::test]
async fn a_read_only_rule_is_never_replaced() {
    let h = harness().await;
    let settings = settings(&h);
    let old = Filter {
        read_only: true,
        ..a_news_rule(&h, &settings).await
    };

    let result = settings
        .replace_rule(h.account_id, &old, marks_read(&old))
        .await;
    assert!(matches!(result, Err(SyncError::Backend(BackendError::Refused(_)))), "{result:?}");
    assert_eq!(h.fake.with(|s| s.filters.len()), 1);
}

#[tokio::test]
async fn a_refused_create_keeps_the_old_rule() {
    let h = harness().await;
    let settings = settings(&h);
    let old = a_news_rule(&h, &settings).await;

    h.fake.fail_call(
        "users.settings.filters.create",
        0,
        GmailError::Http {
            status: 400,
            body: "bad".into(),
        },
    );
    let result = settings
        .replace_rule(h.account_id, &old, marks_read(&old))
        .await;
    assert!(result.is_err(), "{result:?}");
    assert_eq!(
        h.fake.with(|s| s.usage.calls_to("users.settings.filters.delete")),
        0,
        "nothing deletes the old rule when the new one never came"
    );
    assert_eq!(
        settings.rules(h.account_id).await.unwrap().done().unwrap(),
        [old]
    );
}

#[tokio::test]
async fn a_refused_delete_says_both_rules_run() {
    let h = harness().await;
    let settings = settings(&h);
    let old = a_news_rule(&h, &settings).await;

    h.fake.fail_call(
        "users.settings.filters.delete",
        0,
        GmailError::Network("offline".into()),
    );
    let replaced = settings
        .replace_rule(h.account_id, &old, marks_read(&old))
        .await
        .unwrap()
        .done()
        .unwrap();
    let Replaced::BothRun { new, error } = replaced else {
        panic!("the delete failed, so both rules should run: {replaced:?}");
    };
    assert!(error.to_string().contains("offline"), "{error}");
    assert_eq!(
        settings.rules(h.account_id).await.unwrap().done().unwrap(),
        [old, new]
    );
}

/// Gmail refuses a filter identical to one it has, so saving an edit that
/// changed nothing must not ask it to make one.
#[tokio::test]
async fn an_unchanged_rule_is_left_alone() {
    let h = harness().await;
    let settings = settings(&h);
    let old = a_news_rule(&h, &settings).await;
    let made = h.fake.with(|s| s.usage.calls_to("users.settings.filters.create"));

    let replaced = settings
        .replace_rule(h.account_id, &old, Filter { id: None, ..old.clone() })
        .await
        .unwrap()
        .done()
        .unwrap();
    assert!(matches!(replaced, Replaced::Swapped(ref kept) if *kept == old));
    assert_eq!(
        h.fake.with(|s| s.usage.calls_to("users.settings.filters.create")),
        made
    );
}

#[tokio::test]
async fn a_hidden_address_gets_the_label_and_its_filter() {
    let h = harness().await;
    let settings = settings(&h);

    let made = settings
        .create_hidden_address(h.account_id, "dana@example.com", " shop.example ", &[])
        .await
        .unwrap()
        .done()
        .unwrap();
    assert!(made.address.starts_with("dana+"), "{}", made.address);
    assert!(made.address.ends_with("@example.com"), "{}", made.address);
    assert!(hidden::is_alias(&made.address));
    assert_eq!(made.account, "dana@example.com");
    assert_eq!(made.note, "shop.example");
    assert!(made.active);
    let label = h
        .fake
        .with(|s| {
            s.labels
                .iter()
                .find(|l| l.name == HIDE_MY_EMAIL_LABEL)
                .cloned()
        })
        .expect("the Hide My Email label");

    let rules = settings.rules(h.account_id).await.unwrap().done().unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].id, made.label_filter);
    assert_eq!(rules[0].criteria.to.as_deref(), Some(made.address.as_str()));
    assert_eq!(rules[0].action.add, [MailSet::Mailbox(label.id)]);
    assert_eq!(made.trash_filter, None);

    let second = settings
        .create_hidden_address(
            h.account_id,
            "dana@example.com",
            "",
            std::slice::from_ref(&made),
        )
        .await
        .unwrap()
        .done()
        .unwrap();
    assert_ne!(second.address, made.address);
    let named = h.fake.with(|s| {
        s.labels
            .iter()
            .filter(|l| l.name == HIDE_MY_EMAIL_LABEL)
            .count()
    });
    assert_eq!(named, 1, "the second address reuses the label");
    assert_eq!(
        hidden::find(&[made.clone(), second], &made.address.to_uppercase()),
        Some(&made),
        "an address is found whatever its case"
    );
}

#[tokio::test]
async fn turning_a_hidden_address_off_and_on_moves_its_mail() {
    let h = harness().await;
    let settings = settings(&h);
    let made = settings
        .create_hidden_address(h.account_id, "dana@example.com", "", &[])
        .await
        .unwrap()
        .done()
        .unwrap();

    let off = settings
        .set_hidden_address_active(h.account_id, &made, false)
        .await
        .unwrap()
        .done()
        .unwrap();
    assert!(!off.active);
    assert_eq!(off.label_filter, made.label_filter, "mail keeps its label");
    let trash = off
        .trash_filter
        .clone()
        .expect("a rule that trashes its mail");
    let rules = settings.rules(h.account_id).await.unwrap().done().unwrap();
    let trashing = rules.iter().find(|r| r.id.as_deref() == Some(&trash));
    let trashing = trashing.expect("the trash rule");
    assert_eq!(trashing.criteria.to.as_deref(), Some(made.address.as_str()));
    assert_eq!(trashing.action.add, [MailSet::Role(Role::Trash)]);
    assert_eq!(trashing.action.remove, [MailSet::Role(Role::Inbox)]);

    let on = settings
        .set_hidden_address_active(h.account_id, &off, true)
        .await
        .unwrap()
        .done()
        .unwrap();
    assert_eq!(on, made, "the trash rule is gone");
    assert_eq!(
        settings.rules(h.account_id).await.unwrap().done().unwrap(),
        rules
            .iter()
            .filter(|r| r.id != off.trash_filter)
            .cloned()
            .collect::<Vec<_>>()
    );

    settings
        .delete_hidden_address(h.account_id, &off)
        .await
        .unwrap();
    assert_eq!(
        settings.rules(h.account_id).await.unwrap().done().unwrap(),
        [],
        "deleting the address leaves no rules"
    );
}

#[tokio::test]
async fn a_hidden_address_waits_on_the_settings_permission() {
    let h = harness().await;
    let settings = settings(&h);
    h.fake.withhold(mailrs_gmail::SETTINGS_SCOPE);
    assert_eq!(
        settings
            .create_hidden_address(h.account_id, "dana@example.com", "", &[])
            .await
            .unwrap(),
        Permitted::NeedsPermission
    );
    assert!(h.fake.with(|s| s.filters.is_empty()));
}

#[tokio::test]
async fn an_account_without_rules_says_the_server_cannot() {
    let h = harness().await;
    let mut services = AccountServices::fake(Arc::clone(&h.fake));
    services.rules = None;
    let settings = settings_over(&h, services);
    assert!(matches!(
        settings.rules(h.account_id).await,
        Err(SyncError::Backend(BackendError::Unsupported))
    ));
    assert_eq!(h.fake.with(|s| s.usage.calls_to("users.settings.filters.list")), 0);
}
