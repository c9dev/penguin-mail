//! Reading every calendar of every account into the store and keeping it
//! fresh, against the in-memory Gmail.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::calendar::{Access, Calendar, Event};
use mailrs_store::calendar as store;

use super::{Connected, Harness, harness, imap_harness};
use crate::calendar_copy::{CalendarCopy, LIST_EVERY, READ_EVERY_OPEN, READ_EVERY_TRAY};
use crate::settings::Permitted;

const NOW: i64 = 1_790_000_000_000;

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
        zone: "UTC".into(),
        primary,
        shown: true,
        reminders: Vec::new(),
    }
}

fn hidden(id: &str) -> Calendar {
    Calendar { shown: false, ..calendar(id, false) }
}

fn event(calendar: &str, id: &str) -> Event {
    Event {
        calendar: calendar.into(),
        id: id.into(),
        title: id.into(),
        zone: "UTC".into(),
        start: NOW,
        end: NOW + 3_600_000,
        busy: true,
        ..Event::default()
    }
}

async fn stored(h: &Harness, calendar: &str, id: &str) -> Option<Event> {
    let (account, calendar, id) = (h.account_id, calendar.to_string(), id.to_string());
    h.db.read(move |c| store::event(c, account, &calendar, &id)).await.unwrap()
}

#[tokio::test]
async fn the_first_refresh_reads_every_calendar_whole() {
    let h = harness().await;
    h.fake.with(|s| {
        s.calendars = vec![calendar("primary", true), calendar("team", false)];
        s.page_size = 1;
    });
    h.fake.put_calendar_event(event("primary", "a"));
    h.fake.put_calendar_event(event("primary", "b"));
    h.fake.put_calendar_event(event("team", "c"));
    let done = copy(&h).refresh(h.account_id, NOW).await.unwrap();
    assert!(matches!(done, Permitted::Done(ref r) if r.events == 3));
    assert!(stored(&h, "team", "c").await.is_some());
    let account = h.account_id;
    assert!(h.db.read(move |c| store::synced(c, account)).await.unwrap());
}

#[tokio::test]
async fn a_later_refresh_takes_only_the_changes() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(event("primary", "a"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    h.fake.put_calendar_event(Event { title: "Moved".into(), ..event("primary", "a") });
    h.fake.drop_calendar_event("primary", "a");
    h.fake.put_calendar_event(event("primary", "b"));
    copy.refresh(h.account_id, NOW + READ_EVERY_OPEN).await.unwrap();
    assert!(stored(&h, "primary", "a").await.is_none());
    assert!(stored(&h, "primary", "b").await.is_some());
}

#[tokio::test]
async fn an_expired_token_reads_the_calendar_again_and_drops_what_went() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(event("primary", "a"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    // Google forgot the token, and "a" went while nobody was reading.
    h.fake.with(|s| {
        s.expire_calendar_tokens = true;
        s.calendar_events.clear();
    });
    h.fake.put_calendar_event(event("primary", "b"));
    copy.refresh(h.account_id, NOW + READ_EVERY_OPEN).await.unwrap();
    assert!(stored(&h, "primary", "a").await.is_none());
    assert!(stored(&h, "primary", "b").await.is_some());
}

#[tokio::test]
async fn a_calendar_that_left_the_list_leaves_the_store() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true), calendar("team", false)]);
    h.fake.put_calendar_event(event("team", "c"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    copy.refresh(h.account_id, NOW + LIST_EVERY).await.unwrap();
    assert!(stored(&h, "team", "c").await.is_none());
}

/// Ruling R2: an account that granted `calendar.events` but not the list
/// scope still has a primary calendar, addressed by the account's own
/// address, which `calendar.events` allows on its own. The owner's own
/// account is one of these, so the copy must not come out empty for it.
#[tokio::test]
async fn without_the_list_permission_the_primary_calendar_still_fills() {
    let h = harness().await;
    h.fake.withhold(mailrs_gmail::CALENDAR_LIST_SCOPE);
    h.fake.put_calendar_event(event("me@example.com", "a"));
    let done = copy(&h).refresh(h.account_id, NOW).await.unwrap();
    assert!(matches!(done, Permitted::Done(ref r) if r.events == 1), "{done:?}");
    assert!(stored(&h, "me@example.com", "a").await.is_some());
}

/// Without even `calendar.events` there is no calendar to fall back to,
/// so the refresh says the permission is needed, as it always has.
#[tokio::test]
async fn without_any_calendar_permission_the_refresh_says_so() {
    let h = harness().await;
    h.fake.withhold(mailrs_gmail::CALENDAR_SCOPE);
    let done = copy(&h).refresh(h.account_id, NOW).await.unwrap();
    assert!(matches!(done, Permitted::NeedsPermission));
}

/// reconcile.md Task 5 item 4: recording `last_list` only after a
/// successful call meant a refused list was asked for again on every
/// tick. It now waits `LIST_EVERY` whether the call succeeded or not.
#[tokio::test]
async fn a_missing_list_permission_asks_at_most_every_half_hour() {
    let h = harness().await;
    h.fake.withhold(mailrs_gmail::CALENDAR_LIST_SCOPE);
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let calls = || h.fake.usage().calls_to("calendar.calendarList.list");
    let before = calls();
    copy.refresh(h.account_id, NOW + READ_EVERY_OPEN).await.unwrap();
    assert_eq!(calls(), before, "a refusal still waits half an hour");
    copy.refresh(h.account_id, NOW + LIST_EVERY).await.unwrap();
    assert_eq!(calls(), before + 1);
}

/// Spec section 3: a calendar the person hid is read at the slow, tray
/// cadence even while the window is open, rather than every minute like
/// the calendars they look at (reconcile.md Task 5 item 5).
#[tokio::test]
async fn a_hidden_calendar_is_read_only_every_five_minutes() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true), hidden("team")]);
    h.fake.put_calendar_event(event("team", "c"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    assert!(stored(&h, "team", "c").await.is_some(), "the first read covers every calendar");

    h.fake.put_calendar_event(event("team", "d"));
    copy.refresh(h.account_id, NOW + READ_EVERY_OPEN).await.unwrap();
    assert!(stored(&h, "team", "d").await.is_none(), "a minute is too soon for a hidden calendar");

    copy.refresh(h.account_id, NOW + READ_EVERY_TRAY).await.unwrap();
    assert!(stored(&h, "team", "d").await.is_some());
}

#[tokio::test]
async fn the_cadence_is_a_minute_open_and_five_in_the_tray() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    let copy = copy(&h);
    let accounts = [h.account_id];
    copy.refresh_due(&accounts, NOW, true).await.unwrap();
    let calls = || h.fake.usage().calls_to("calendar.events.list");
    let before = calls();
    copy.refresh_due(&accounts, NOW + READ_EVERY_OPEN - 1, true).await.unwrap();
    assert_eq!(calls(), before);
    copy.refresh_due(&accounts, NOW + READ_EVERY_OPEN, true).await.unwrap();
    assert_eq!(calls(), before + 1);
    copy.refresh_due(&accounts, NOW + READ_EVERY_OPEN + READ_EVERY_OPEN, false).await.unwrap();
    assert_eq!(calls(), before + 1, "the tray waits five minutes");
    copy.refresh_due(&accounts, NOW + READ_EVERY_OPEN + READ_EVERY_TRAY, false).await.unwrap();
    assert_eq!(calls(), before + 2);
}

/// The provider-neutrality rule: an account whose provider offers no
/// calendar, IMAP today, is skipped without an error or a wasted call
/// (reconcile.md Task 5 item 2).
#[tokio::test]
async fn an_account_without_a_calendar_is_skipped_quietly() {
    let h = imap_harness().await;
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    let copy = CalendarCopy::new(Arc::new(Connected(connected)), h.db.clone());
    let done = copy.refresh(h.account_id, NOW).await.unwrap();
    assert!(matches!(done, Permitted::Done(ref r) if r.events == 0));
    let account = h.account_id;
    assert!(h.db.read(move |c| store::calendars(c, account)).await.unwrap().is_empty());
    assert!(!h.db.read(move |c| store::synced(c, account)).await.unwrap());
}

/// reconcile.md Task 5 item 3: one account's trouble, such as one not yet
/// running, used to abort `refresh_due` with `?` before it reached any
/// account after it.
#[tokio::test]
async fn an_account_that_is_not_running_leaves_the_rest_to_be_read() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(event("primary", "a"));
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    let copy = CalendarCopy::new(Arc::new(Connected(connected)), h.db.clone());
    let missing = h.account_id + 1;
    let refreshed = copy.refresh_due(&[missing, h.account_id], NOW, true).await.unwrap();
    assert_eq!(refreshed.events, 1, "the account after the missing one is still read");
    assert!(stored(&h, "primary", "a").await.is_some());
}

/// reconcile.md Task 5 item 8: the app ticks every 15 s and a first read
/// of a large calendar can take longer, so a second `refresh_due` while
/// one is already under way leaves every account alone rather than
/// reading it twice. The second account's own cadence has never been
/// touched, so only the running pass, not the per-account cadence gate,
/// can be what holds it back.
#[tokio::test]
async fn a_refresh_already_running_leaves_a_second_one_alone() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(event("primary", "a"));

    let fake2 = Arc::new(crate::fake::FakeGmail::new());
    fake2.with(|s| s.calendars = vec![calendar("primary", true)]);
    fake2.put_calendar_event(event("primary", "z"));
    let account2 = h
        .db
        .write(|c| mailrs_store::accounts::insert_account(c, "second@example.com", 0))
        .await
        .unwrap();
    let (sender2, _events2) = async_channel::unbounded();
    let sync2 = Arc::new(crate::AccountSync::new(
        account2,
        crate::AccountServices::fake(Arc::clone(&fake2)),
        h.db.clone(),
        sender2,
    ));

    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync)), (account2, sync2)]);
    let copy = CalendarCopy::new(Arc::new(Connected(connected)), h.db.clone());
    let accounts = [h.account_id, account2];

    let mut held = h.fake.hold("calendar.events.list");
    let first = copy.refresh_due(&accounts, NOW, true);
    let second = async {
        held.entered().await;
        let again = copy.refresh_due(&accounts, NOW, true).await.unwrap();
        assert_eq!(again.events, 0, "a run already under way is left alone");
        held.release();
    };
    let (first, ()) = tokio::join!(first, second);
    assert_eq!(first.unwrap().events, 2, "both accounts are read once the first pass finishes");
}
