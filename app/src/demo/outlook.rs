//! The demo's Outlook account: Dana's personal Outlook.com mailbox on the
//! in-memory Graph, with Outlook's categories as tags, a Focused and an
//! Other inbox, a folder of her own, a rule, a weekly standup on her
//! calendar, and two contacts.

use std::sync::Arc;

use chrono::Datelike;
use mailrs_domain::EpochMillis;
use mailrs_graph::{
    DateTimeZone, EmailAddress, GraphContact, GraphEvent, MessageRule, PatternedRecurrence,
    Recipient, RecurrencePattern, RecurrenceRange, RuleActions, RulePredicates,
};
use mailrs_store::{Db, StoreError, accounts};
use mailrs_sync::fake::{FakeGraph, FakeMail, fill_store};
use mailrs_sync::services::microsoft::MicrosoftSettings;
use mailrs_sync::{AccountServices, AccountSync, SyncError};

pub const OUTLOOK: &str = "dana.reyes@outlook.com";

const HOUR: i64 = 60 * 60 * 1000;
const DAY: i64 = 24 * HOUR;

pub fn settings() -> MicrosoftSettings {
    MicrosoftSettings {
        address: OUTLOOK.into(),
        provider_name: "Outlook".into(),
        window_days: mailrs_sync::DEFAULT_WINDOW_DAYS,
    }
}

/// Adds the Outlook account, fills its Graph, and lets sync fill the store
/// from it. The returned sync stays alive so the calendar copy can read
/// the account's calendar before the window opens.
pub async fn seed_outlook(
    db: &Db,
    now: EpochMillis,
) -> Result<(i64, Arc<FakeGraph>, Arc<AccountSync>), SyncError> {
    let account_id = db
        .write(move |c| {
            accounts::insert_microsoft_account(c, OUTLOOK, "Outlook", now)?.ok_or(
                StoreError::Corrupt { column: "accounts.provider", value: OUTLOOK.to_string() },
            )
        })
        .await?;
    let graph = Arc::new(FakeGraph::new());
    graph.with(|s| s.me = OUTLOOK.into());
    graph.add_category("Travel", "preset7");
    graph.add_category("Family", "preset4");
    let inbox = graph.folder_id("inbox");
    let trips = graph.add_folder("Trips", None);
    let lisbon = graph.deliver(
        &inbox,
        FakeMail {
            from: ("TAP Air Portugal", "booking@flytap.example"),
            to: OUTLOOK,
            subject: "Your booking to Lisbon is confirmed",
            text: "Flight TP 1351 on Friday, 18:40. Seat 12A.",
            at: now - 2 * HOUR,
            ..FakeMail::default()
        },
    );
    graph.tag(&lisbon, &["Travel"]);
    let mum = graph.deliver(
        &inbox,
        FakeMail {
            from: ("Mum", "mum@family.example"),
            to: OUTLOOK,
            subject: "Sunday lunch",
            text: "Bring the salad bowl back, please.",
            at: now - 5 * HOUR,
            ..FakeMail::default()
        },
    );
    graph.tag(&mum, &["Family"]);
    for (from, subject, hours) in [
        (("Streamly", "news@streamly.example"), "Three new series this week", 3),
        (("CityBikes", "hello@citybikes.example"), "Your monthly ride summary", 9),
    ] {
        let id = graph.deliver(
            &inbox,
            FakeMail {
                from,
                to: OUTLOOK,
                subject,
                text: "Read it in your browser.",
                at: now - hours * HOUR,
                ..FakeMail::default()
            },
        );
        graph.classify(&id, true);
    }
    graph.deliver(
        &trips,
        FakeMail {
            from: ("Hotel Alfama", "stay@alfama.example"),
            to: OUTLOOK,
            subject: "Check-in from 15:00",
            text: "Your room is ready from three.",
            at: now - 3 * DAY,
            ..FakeMail::default()
        },
    );
    graph.with(|s| {
        s.rules.push(MessageRule {
            id: "rule-1".into(),
            display_name: "Bikes to Other".into(),
            sequence: 1,
            is_enabled: true,
            conditions: Some(RulePredicates {
                from_addresses: vec![Recipient {
                    email_address: EmailAddress {
                        name: None,
                        address: Some("hello@citybikes.example".into()),
                    },
                }],
                ..RulePredicates::default()
            }),
            actions: Some(RuleActions { mark_as_read: Some(true), ..RuleActions::default() }),
            ..MessageRule::default()
        });
    });
    let (master, occurrence) = standup(now);
    graph.put_event("cal-1", master);
    graph.put_event("cal-1", occurrence);
    graph.put_contact(
        "contacts-1",
        GraphContact {
            id: "contact-1".into(),
            display_name: Some("Mum".into()),
            email_addresses: vec![EmailAddress {
                name: None,
                address: Some("mum@family.example".into()),
            }],
            ..GraphContact::default()
        },
    );
    graph.put_contact(
        "contacts-1",
        GraphContact {
            id: "contact-2".into(),
            display_name: Some("Rui Costa".into()),
            email_addresses: vec![EmailAddress {
                name: None,
                address: Some("rui@work.example".into()),
            }],
            company_name: Some("Contoso".into()),
            ..GraphContact::default()
        },
    );
    let (events, _) = async_channel::unbounded();
    let sync = Arc::new(AccountSync::new(
        account_id,
        AccountServices::microsoft(Arc::clone(&graph), settings()),
        db.clone(),
        events,
    ));
    fill_store(&sync).await?;
    Ok((account_id, graph, sync))
}

/// A weekly standup on Monday mornings that started a few weeks ago, with
/// one occurrence. Graph names a series only through its occurrences, so
/// the calendar delta finds the master through that one.
fn standup(now: EpochMillis) -> (GraphEvent, GraphEvent) {
    let today = chrono::DateTime::from_timestamp_millis(now).unwrap_or_default().date_naive();
    let back = i64::from(today.weekday().num_days_from_monday());
    let this_monday = today - chrono::Duration::days(back);
    let start = this_monday - chrono::Duration::days(21);
    let at = |day: chrono::NaiveDate, time: &str| DateTimeZone {
        date_time: format!("{day}T{time}"),
        time_zone: "UTC".into(),
    };
    let master = GraphEvent {
        id: "standup".into(),
        ical_uid: Some("standup@outlook.example".into()),
        etag: Some("W/\"1\"".into()),
        subject: Some("Standup".into()),
        start: Some(at(start, "09:00:00.0000000")),
        end: Some(at(start, "09:15:00.0000000")),
        kind: Some("seriesMaster".into()),
        show_as: Some("busy".into()),
        recurrence: Some(PatternedRecurrence {
            pattern: RecurrencePattern {
                kind: "weekly".into(),
                interval: 1,
                days_of_week: vec!["monday".into()],
                ..RecurrencePattern::default()
            },
            range: RecurrenceRange {
                kind: "noEnd".into(),
                start_date: start.to_string(),
                ..RecurrenceRange::default()
            },
        }),
        ..GraphEvent::default()
    };
    let next = this_monday + chrono::Duration::days(7);
    let occurrence = GraphEvent {
        id: "standup-next".into(),
        subject: Some("Standup".into()),
        start: Some(at(next, "09:00:00.0000000")),
        end: Some(at(next, "09:15:00.0000000")),
        kind: Some("occurrence".into()),
        series_master_id: Some("standup".into()),
        original_start: Some(format!("{next}T09:00:00Z")),
        etag: Some("W/\"1\"".into()),
        ..GraphEvent::default()
    };
    (master, occurrence)
}
