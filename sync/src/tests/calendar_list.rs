//! Changing an account's calendar list through the local copy: each
//! change shows in the store at once, waits in the queue, and reaches the
//! in-memory Gmail on the next send.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::calendar::list::{self, ListEdit};
use mailrs_domain::calendar::{Access, Calendar, Event};
use mailrs_store::calendar as store;
use mailrs_store::calendar_list as store_list;

use super::{Connected, Harness, harness};
use crate::calendar_copy::{CalendarCopy, LIST_EVERY};
use crate::settings::Permitted;
use crate::{BackendError, SyncError};

const NOW: i64 = 1_790_000_000_000;
const HOLIDAYS: &str = "en.portuguese#holiday@group.v.calendar.google.com";

fn copy(h: &Harness) -> CalendarCopy<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    CalendarCopy::new(Arc::new(Connected(connected)), h.db.clone())
}

fn calendar(id: &str, primary: bool) -> Calendar {
    Calendar {
        id: id.into(),
        name: id.into(),
        color: "#3584e4".into(),
        access: Access::Owner,
        zone: "Europe/Lisbon".into(),
        primary,
        shown: true,
        hidden: false,
        reminders: Vec::new(),
    }
}

/// An account with a primary calendar and a team calendar it owns, read
/// into the copy.
async fn read(h: &Harness) -> CalendarCopy<Connected> {
    h.fake.with(|s| s.calendars = vec![calendar("primary", true), calendar("team", false)]);
    let copy = copy(h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    copy
}

async fn calendars(h: &Harness) -> Vec<Calendar> {
    let account = h.account_id;
    h.db.read(move |c| store::calendars(c, account)).await.unwrap()
}

async fn stored(h: &Harness, id: &str) -> Option<Calendar> {
    calendars(h).await.into_iter().find(|c| c.id == id)
}

async fn queued(h: &Harness) -> Vec<ListEdit> {
    let account = h.account_id;
    h.db.read(move |c| store_list::queued_edits(c, account)).await.unwrap().into_iter().map(|q| q.edit).collect()
}

async fn listed(h: &Harness, id: &str) -> Option<bool> {
    let (account, id) = (h.account_id, id.to_string());
    h.db.read(move |c| store_list::listed(c, account, &id)).await.unwrap()
}

fn at_google(h: &Harness, id: &str) -> Option<Calendar> {
    h.fake.with(|s| s.calendars.iter().find(|c| c.id == id).cloned())
}

fn done<T>(answer: Result<Permitted<T>, SyncError>) -> T {
    match answer {
        Ok(Permitted::Done(value)) => value,
        other => panic!("expected Done, got {:?}", other.map(|p| matches!(p, Permitted::Done(_)))),
    }
}

#[tokio::test]
async fn a_new_calendar_shows_at_once_and_takes_googles_id_once_sent() {
    let h = harness().await;
    let copy = read(&h).await;
    let local = done(copy.new_calendar(h.account_id, "Climbing", "#16a766").await);
    assert!(list::is_local(&local));
    let shown = stored(&h, &local).await.expect("the new calendar is in the copy at once");
    assert_eq!((shown.name.as_str(), shown.color.as_str(), shown.access), ("Climbing", "#16a766", Access::Owner));

    let turned_down = copy.send(h.account_id).await.unwrap();
    assert!(turned_down.is_empty(), "{turned_down:?}");
    let made = h.fake.with(|s| s.calendars.iter().find(|c| c.name == "Climbing").cloned()).expect("Google made it");
    assert_eq!(made.color, "#16a766");
    assert_eq!(made.zone, "Europe/Lisbon", "a new calendar takes the primary calendar's zone");
    assert!(stored(&h, &local).await.is_none(), "the local id is gone");
    assert_eq!(stored(&h, &made.id).await.unwrap().name, "Climbing");
    assert!(queued(&h).await.is_empty());
}

#[tokio::test]
async fn a_calendar_made_offline_waits_and_goes_out_later() {
    let h = harness().await;
    let copy = read(&h).await;
    let local = done(copy.new_calendar(h.account_id, "Climbing", "#16a766").await);
    h.fake.fail_next(mailrs_gmail::GmailError::Network("offline".into()));
    assert!(copy.send(h.account_id).await.is_err());
    assert!(stored(&h, &local).await.is_some());
    assert_eq!(queued(&h).await.len(), 1);
    // A read of the list while it waits keeps it.
    copy.refresh(h.account_id, NOW + LIST_EVERY).await.unwrap();
    assert!(stored(&h, &local).await.is_some(), "a read keeps a calendar still waiting to go out");
    copy.send(h.account_id).await.unwrap();
    assert!(h.fake.with(|s| s.calendars.iter().any(|c| c.name == "Climbing")));
}

#[tokio::test]
async fn an_event_saved_on_a_new_calendar_follows_it_to_googles_id() {
    let h = harness().await;
    let copy = read(&h).await;
    let local = done(copy.new_calendar(h.account_id, "Climbing", "#16a766").await);
    let lesson = Event {
        calendar: local.clone(),
        id: crate::calendar_copy::new_event_id(),
        title: "Lesson".into(),
        zone: "UTC".into(),
        start: NOW,
        end: NOW + 3_600_000,
        ..Event::default()
    };
    copy.save(h.account_id, lesson).await.unwrap();
    copy.send(h.account_id).await.unwrap();
    let made = h.fake.with(|s| s.calendars.iter().find(|c| c.name == "Climbing").cloned()).unwrap();
    let at = h.fake.with(|s| s.calendar_events.iter().find(|e| e.title == "Lesson").map(|e| e.calendar.clone()));
    assert_eq!(at, Some(made.id));
}

#[tokio::test]
async fn deleting_a_calendar_not_yet_sent_sends_nothing() {
    let h = harness().await;
    let copy = read(&h).await;
    let local = done(copy.new_calendar(h.account_id, "Climbing", "#16a766").await);
    done(copy.delete_calendar(h.account_id, &local).await);
    assert!(stored(&h, &local).await.is_none());
    assert!(queued(&h).await.is_empty());
    copy.send(h.account_id).await.unwrap();
    assert_eq!(h.fake.with(|s| s.calendars.len()), 2);
}

#[tokio::test]
async fn a_rename_shows_at_once_and_reaches_google() {
    let h = harness().await;
    let copy = read(&h).await;
    done(copy.rename_calendar(h.account_id, "team", "Office").await);
    assert_eq!(stored(&h, "team").await.unwrap().name, "Office");
    copy.send(h.account_id).await.unwrap();
    assert_eq!(at_google(&h, "team").unwrap().name, "Office");
}

#[tokio::test]
async fn a_delete_takes_the_calendar_and_its_events_off_at_once() {
    let h = harness().await;
    h.fake.put_calendar_event(Event { calendar: "team".into(), id: "retro".into(), start: NOW, end: NOW + 1, ..Event::default() });
    let copy = read(&h).await;
    done(copy.delete_calendar(h.account_id, "team").await);
    assert!(stored(&h, "team").await.is_none());
    let account = h.account_id;
    assert!(h.db.read(move |c| store::event(c, account, "team", "retro")).await.unwrap().is_none());
    copy.send(h.account_id).await.unwrap();
    assert!(at_google(&h, "team").is_none());
}

#[tokio::test]
async fn the_primary_calendar_is_not_deleted() {
    let h = harness().await;
    let copy = read(&h).await;
    let answer = copy.delete_calendar(h.account_id, "primary").await;
    assert!(matches!(answer, Err(SyncError::Backend(BackendError::Unsupported))), "{:?}", answer.is_ok());
    assert!(queued(&h).await.is_empty());
}

#[tokio::test]
async fn a_calendar_the_account_does_not_own_is_not_renamed() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true), Calendar { access: Access::Reader, ..calendar("team", false) }]);
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let answer = copy.rename_calendar(h.account_id, "team", "Mine now").await;
    assert!(matches!(answer, Err(SyncError::Backend(BackendError::Unsupported))));
}

#[tokio::test]
async fn without_the_calendars_permission_making_one_asks_for_it() {
    let h = harness().await;
    h.fake.withhold(mailrs_gmail::CALENDARS_SCOPE);
    let copy = read(&h).await;
    let answer = copy.new_calendar(h.account_id, "Climbing", "#16a766").await;
    assert!(matches!(answer, Ok(Permitted::NeedsPermission)));
    assert!(queued(&h).await.is_empty());
    assert_eq!(calendars(&h).await.len(), 2);
    assert!(matches!(copy.rename_calendar(h.account_id, "team", "Office").await, Ok(Permitted::NeedsPermission)));
    assert!(matches!(copy.delete_calendar(h.account_id, "team").await, Ok(Permitted::NeedsPermission)));
}

#[tokio::test]
async fn a_colour_goes_to_google_while_the_list_permission_is_there() {
    let h = harness().await;
    let copy = read(&h).await;
    done(copy.recolor_calendar(h.account_id, "team", Some("#fad165".into())).await);
    assert_eq!(stored(&h, "team").await.unwrap().color, "#fad165");
    copy.send(h.account_id).await.unwrap();
    assert_eq!(at_google(&h, "team").unwrap().color, "#fad165");
}

/// An account that granted only the old read-only list keeps its colour
/// on this computer, as before.
#[tokio::test]
async fn without_the_list_permission_a_colour_stays_on_this_computer() {
    let h = harness().await;
    h.fake.withhold(mailrs_gmail::CALENDAR_LIST_WRITE_SCOPE);
    let copy = read(&h).await;
    done(copy.recolor_calendar(h.account_id, "team", Some("#fad165".into())).await);
    assert_eq!(stored(&h, "team").await.unwrap().color, "#fad165");
    assert!(queued(&h).await.is_empty());
    copy.refresh(h.account_id, NOW + LIST_EVERY).await.unwrap();
    assert_eq!(stored(&h, "team").await.unwrap().color, "#fad165", "the own colour outlasts a read");
    assert_eq!(at_google(&h, "team").unwrap().color, "#3584e4");
}

#[tokio::test]
async fn hiding_sets_googles_flag_so_the_other_devices_follow() {
    let h = harness().await;
    let copy = read(&h).await;
    done(copy.list_calendar(h.account_id, "team", false).await);
    assert_eq!(listed(&h, "team").await, Some(false));
    copy.send(h.account_id).await.unwrap();
    assert!(at_google(&h, "team").unwrap().hidden);
    copy.refresh(h.account_id, NOW + LIST_EVERY).await.unwrap();
    assert_eq!(listed(&h, "team").await, Some(false), "Google's flag agrees now");
    done(copy.list_calendar(h.account_id, "team", true).await);
    copy.send(h.account_id).await.unwrap();
    assert!(!at_google(&h, "team").unwrap().hidden);
}

#[tokio::test]
async fn a_calendar_hidden_on_another_device_leaves_the_list_here() {
    let h = harness().await;
    let copy = read(&h).await;
    h.fake.with(|s| s.calendars[1].hidden = true);
    copy.refresh(h.account_id, NOW + LIST_EVERY).await.unwrap();
    assert_eq!(listed(&h, "team").await, Some(false));
}

#[tokio::test]
async fn without_the_list_permission_hiding_stays_on_this_computer() {
    let h = harness().await;
    h.fake.withhold(mailrs_gmail::CALENDAR_LIST_WRITE_SCOPE);
    let copy = read(&h).await;
    done(copy.list_calendar(h.account_id, "team", false).await);
    assert_eq!(listed(&h, "team").await, Some(false));
    assert!(queued(&h).await.is_empty());
    assert!(!at_google(&h, "team").unwrap().hidden);
}

#[tokio::test]
async fn a_subscription_goes_on_googles_list_read_only() {
    let h = harness().await;
    let copy = read(&h).await;
    let local = done(copy.subscribe(h.account_id, "webcal://example.com/fixtures.ics").await);
    let shown = stored(&h, &local).await.unwrap();
    assert_eq!((shown.name.as_str(), shown.access), ("example.com", Access::Reader));
    copy.send(h.account_id).await.unwrap();
    let added = h.fake.with(|s| s.calendars.iter().find(|c| c.id.ends_with("@import.calendar.google.com")).cloned());
    let added = added.expect("Google put the feed on the list");
    assert_eq!(added.access, Access::Reader);
    assert!(stored(&h, &added.id).await.is_some());
    assert!(h.fake.with(|s| s.subscribed.contains(&"https://example.com/fixtures.ics".to_string())));
}

#[tokio::test]
async fn an_address_that_is_not_a_feed_is_refused_here() {
    let h = harness().await;
    let copy = read(&h).await;
    assert!(copy.subscribe(h.account_id, "example.com/fixtures").await.is_err());
    assert!(queued(&h).await.is_empty());
}

#[tokio::test]
async fn a_holiday_calendar_goes_on_the_list_by_its_id() {
    let h = harness().await;
    let copy = read(&h).await;
    done(copy.add_public(h.account_id, HOLIDAYS, "Holidays in Portugal").await);
    let shown = stored(&h, HOLIDAYS).await.unwrap();
    assert_eq!((shown.name.as_str(), shown.access), ("Holidays in Portugal", Access::Reader));
    copy.send(h.account_id).await.unwrap();
    assert!(at_google(&h, HOLIDAYS).is_some());
}

#[tokio::test]
async fn without_the_list_permission_subscribing_asks_for_it() {
    let h = harness().await;
    h.fake.withhold(mailrs_gmail::CALENDAR_LIST_WRITE_SCOPE);
    let copy = read(&h).await;
    assert!(matches!(copy.add_public(h.account_id, HOLIDAYS, "Holidays").await, Ok(Permitted::NeedsPermission)));
    assert!(matches!(
        copy.subscribe(h.account_id, "https://example.com/a.ics").await,
        Ok(Permitted::NeedsPermission)
    ));
    assert!(queued(&h).await.is_empty());
}

#[tokio::test]
async fn a_change_google_turns_down_leaves_the_queue_and_googles_version_returns() {
    let h = harness().await;
    let copy = read(&h).await;
    done(copy.rename_calendar(h.account_id, "team", "Office").await);
    h.fake.with(|s| s.refuse_list_edits = true);
    let turned_down = copy.send(h.account_id).await.unwrap();
    assert_eq!(turned_down.len(), 1);
    assert_eq!(turned_down[0].title, "Office");
    assert!(turned_down[0].reason.is_some());
    assert!(queued(&h).await.is_empty());
    copy.refresh(h.account_id, NOW + 1).await.unwrap();
    assert_eq!(stored(&h, "team").await.unwrap().name, "team", "the next read puts Google's name back");
}

/// Google has no calendar under a `new:` id, so reading one would only
/// answer 404 and send the copy back to the list every tick until the
/// queue goes out.
#[tokio::test]
async fn a_calendar_still_waiting_to_be_made_is_not_read() {
    let h = harness().await;
    let copy = read(&h).await;
    done(copy.new_calendar(h.account_id, "Climbing", "#16a766").await);
    let before = h.fake.usage().calls_to("calendar.events.list");
    copy.refresh(h.account_id, NOW + LIST_EVERY).await.unwrap();
    assert_eq!(h.fake.usage().calls_to("calendar.events.list") - before, 2, "primary and team only");
}
