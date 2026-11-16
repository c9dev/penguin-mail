//! What the calendar view and the assistant read from the copy: a range,
//! a search, one event, and each account's calendar list. Every read
//! answers from the store as it stands, with no wait for a first sync.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::calendar::{Access, Calendar, Event};
use mailrs_store::calendar::{self as store, CalendarScope};

use super::{Connected, Harness, harness};
use crate::calendar_copy::CalendarCopy;

const NOW: i64 = 1_790_000_000_000;
const HOUR: i64 = 3_600_000;

fn copy(h: &Harness) -> CalendarCopy<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    CalendarCopy::new(Arc::new(Connected(connected)), h.db.clone())
}

fn calendar(id: &str, shown: bool) -> Calendar {
    Calendar {
        id: id.into(),
        name: id.into(),
        color: "#3584e4".into(),
        access: Access::Owner,
        zone: "UTC".into(),
        primary: id == "primary",
        shown,
        hidden: false,
        reminders: Vec::new(),
        all_day_reminders: None,
    }
}

fn event(calendar: &str, id: &str, title: &str) -> Event {
    Event {
        calendar: calendar.into(),
        id: id.into(),
        title: title.into(),
        zone: "UTC".into(),
        start: NOW,
        end: NOW + HOUR,
        busy: true,
        ..Event::default()
    }
}

/// A copy that has read two calendars, "primary" shown and "team"
/// hidden, with one event on each.
async fn filled() -> (Harness, CalendarCopy<Connected>) {
    let h = harness().await;
    h.fake
        .with(|s| s.calendars = vec![calendar("primary", true), calendar("team", false)]);
    h.fake
        .put_calendar_event(event("primary", "a", "Design review"));
    h.fake.put_calendar_event(event("team", "b", "Team lunch"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    (h, copy)
}

#[tokio::test]
async fn a_range_read_leaves_a_hidden_calendar_out() {
    let (h, copy) = filled().await;
    let found = copy
        .occurrences(
            &[h.account_id],
            NOW - HOUR,
            NOW + 2 * HOUR,
            CalendarScope::Shown,
        )
        .await
        .unwrap();
    let ids: Vec<&str> = found.iter().map(|o| o.event.id.as_str()).collect();
    assert_eq!(ids, ["a"]);
}

#[tokio::test]
async fn a_range_read_of_every_calendar_takes_the_hidden_one_too() {
    let (h, copy) = filled().await;
    let found = copy
        .occurrences(
            &[h.account_id],
            NOW - HOUR,
            NOW + 2 * HOUR,
            CalendarScope::All,
        )
        .await
        .unwrap();
    assert_eq!(found.len(), 2);
}

#[tokio::test]
async fn a_search_finds_an_event_by_a_word_of_its_title() {
    let (h, copy) = filled().await;
    let found = copy
        .search(&[h.account_id], "design", NOW, CalendarScope::Shown, 50)
        .await
        .unwrap();
    let ids: Vec<&str> = found.iter().map(|o| o.event.id.as_str()).collect();
    assert_eq!(ids, ["a"]);
}

#[tokio::test]
async fn one_event_comes_back_by_its_calendar_and_id() {
    let (h, copy) = filled().await;
    let found = copy.event(h.account_id, "primary", "a").await.unwrap();
    assert_eq!(found.map(|e| e.title), Some("Design review".to_string()));
}

#[tokio::test]
async fn an_event_the_copy_lacks_comes_back_as_none() {
    let (h, copy) = filled().await;
    assert!(
        copy.event(h.account_id, "primary", "gone")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn the_calendar_list_names_what_the_person_took_off_it() {
    let (h, copy) = filled().await;
    let account = h.account_id;
    h.db.write(move |c| store::set_listed(c, account, "team", false))
        .await
        .unwrap();
    let listed = copy.listed(&[account]).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].account_id, account);
    let names: Vec<&str> = listed[0].calendars.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(names, ["primary", "team"]);
    assert_eq!(listed[0].unlisted, ["team"]);
}

#[tokio::test]
async fn showing_a_calendar_again_brings_its_events_back_into_a_range_read() {
    let (h, copy) = filled().await;
    copy.show_calendar(h.account_id, "team", true)
        .await
        .unwrap();
    let found = copy
        .occurrences(
            &[h.account_id],
            NOW - HOUR,
            NOW + 2 * HOUR,
            CalendarScope::Shown,
        )
        .await
        .unwrap();
    assert_eq!(found.len(), 2);
}

#[tokio::test]
async fn every_account_the_store_holds_is_named_for_a_read_across_them() {
    let (h, copy) = filled().await;
    assert_eq!(copy.every_account().await.unwrap(), vec![h.account_id]);
}

#[tokio::test]
async fn a_calendar_counts_the_events_its_delete_would_take() {
    let (h, copy) = filled().await;
    assert_eq!(copy.event_count(h.account_id, "team").await.unwrap(), 1);
}

#[tokio::test]
async fn birthday_reads_follow_contact_edits_and_deletions_without_storing_events() {
    use mailrs_store::address_book::{self, Contact};
    let h = harness().await;
    let copy = copy(&h);
    let account = h.account_id;
    let from = chrono::NaiveDate::from_ymd_opt(2027, 7, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis();
    let to = from + 31 * 24 * HOUR;
    h.db.write(move |c| {
        address_book::save(
            c,
            &[Contact {
                account_id: account,
                resource: "people/sample".into(),
                name: Some("Sample Person".into()),
                details: mailrs_domain::contact::Details {
                    birthday: "1982-07-27".into(),
                    ..Default::default()
                },
                ..Default::default()
            }],
        )
    })
    .await
    .unwrap();
    assert!(copy.birthdays(&[], from, to).await.unwrap().is_empty());
    let found = copy.birthdays(&[account], from, to).await.unwrap();
    assert_eq!(found.len(), 1);
    assert!(found[0].event.title.ends_with("(45)"));
    let id = &found[0].event.id;
    assert_eq!(
        copy.birthday(account, id).await.unwrap().as_ref(),
        Some(found[0].event.as_ref())
    );
    assert!(copy.birthday(account, "bad-id").await.unwrap().is_none());
    assert!(
        copy.occurrences(&[account], from, to, CalendarScope::All)
            .await
            .unwrap()
            .is_empty()
    );
    h.db.write(move |c| {
        let mut contact = address_book::list(c)?.remove(0);
        contact.details.birthday = "--08-10".into();
        address_book::save(c, &[contact])
    })
    .await
    .unwrap();
    assert!(
        copy.birthdays(&[account], from, to)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !copy
            .birthday(account, id)
            .await
            .unwrap()
            .unwrap()
            .title
            .contains('(')
    );
    h.db.write(move |c| address_book::forget(c, account, &["people/sample".into()]))
        .await
        .unwrap();
    assert!(copy.birthday(account, id).await.unwrap().is_none());
    assert!(
        copy.birthdays(&[account], from, to)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        copy.listed(&[account]).await.unwrap()[0]
            .calendars
            .is_empty()
    );
}
