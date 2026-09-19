use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Local, NaiveDate, TimeZone};
use mailrs_domain::EpochMillis;
use mailrs_gmail::GmailError;

use super::{Connected, Harness, harness};
use crate::settings::{
    AccountSettings, AutomaticReply, HIDE_MY_EMAIL_LABEL, HiddenFilters, Permitted,
};

fn settings(h: &Harness) -> AccountSettings<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
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
    assert_eq!(rules[0].action.add_label_ids, ["TRASH"]);
    assert_eq!(rules[0].action.remove_label_ids, ["INBOX"]);

    settings.delete_rule(h.account_id, &id).await.unwrap();
    assert_eq!(
        settings.rules(h.account_id).await.unwrap().done().unwrap(),
        []
    );
}

#[tokio::test]
async fn a_hidden_address_gets_the_label_and_its_filter() {
    let h = harness().await;
    let settings = settings(&h);

    let filters = settings
        .hide_address(h.account_id, "dana+kite.fern482@example.com")
        .await
        .unwrap()
        .done()
        .unwrap();
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
    assert_eq!(rules[0].id, filters.label);
    assert_eq!(
        rules[0].criteria.to.as_deref(),
        Some("dana+kite.fern482@example.com")
    );
    assert_eq!(rules[0].action.add_label_ids, [label.id]);
    assert_eq!(filters.trash, None);

    settings
        .hide_address(h.account_id, "dana+moss.olive017@example.com")
        .await
        .unwrap();
    let named = h.fake.with(|s| {
        s.labels
            .iter()
            .filter(|l| l.name == HIDE_MY_EMAIL_LABEL)
            .count()
    });
    assert_eq!(named, 1, "the second address reuses the label");
}

#[tokio::test]
async fn turning_a_hidden_address_off_and_on_moves_its_mail() {
    let h = harness().await;
    let settings = settings(&h);
    let address = "dana+kite.fern482@example.com";
    let made = settings
        .hide_address(h.account_id, address)
        .await
        .unwrap()
        .done()
        .unwrap();

    let off = settings
        .set_address_active(h.account_id, address, false, &made)
        .await
        .unwrap()
        .done()
        .unwrap();
    assert_eq!(off.label, made.label, "mail keeps its label");
    let trash = off.trash.clone().expect("a rule that trashes its mail");
    let rules = settings.rules(h.account_id).await.unwrap().done().unwrap();
    let trashing = rules.iter().find(|r| r.id.as_deref() == Some(&trash));
    let trashing = trashing.expect("the trash rule");
    assert_eq!(trashing.criteria.to.as_deref(), Some(address));
    assert_eq!(trashing.action.add_label_ids, ["TRASH"]);
    assert_eq!(trashing.action.remove_label_ids, ["INBOX"]);

    let on = settings
        .set_address_active(h.account_id, address, true, &off)
        .await
        .unwrap()
        .done()
        .unwrap();
    assert_eq!(on, made, "the trash rule is gone");
    assert_eq!(
        settings.rules(h.account_id).await.unwrap().done().unwrap(),
        rules
            .iter()
            .filter(|r| r.id != off.trash)
            .cloned()
            .collect::<Vec<_>>()
    );

    settings.unhide_address(h.account_id, &on).await.unwrap();
    assert_eq!(
        settings.rules(h.account_id).await.unwrap().done().unwrap(),
        [],
        "deleting the address leaves no rules"
    );
    assert_eq!(
        settings
            .set_address_active(h.account_id, address, true, &HiddenFilters::default())
            .await
            .unwrap()
            .done()
            .unwrap(),
        HiddenFilters::default()
    );
}
