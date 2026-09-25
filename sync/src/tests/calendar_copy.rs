//! Reading every calendar of every account into the store and keeping it
//! fresh, against the in-memory Gmail.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::calendar::{Access, Calendar, Event};
use mailrs_store::calendar as store;

use super::{Connected, Harness, harness, imap_harness};
use crate::calendar_copy::{CalendarCopy, LIST_EVERY, READ_EVERY_OPEN, READ_EVERY_TRAY, new_event_id};
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

#[tokio::test]
async fn a_saved_event_shows_at_once_and_goes_out_on_send() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let id = new_event_id();
    copy.save(h.account_id, event("primary", &id)).await.unwrap();
    assert!(stored(&h, "primary", &id).await.unwrap().pending);
    assert!(copy.send(h.account_id).await.unwrap().is_empty());
    let held = stored(&h, "primary", &id).await.unwrap();
    assert!(!held.pending);
    assert_eq!(held.etag, "\"1\"");
    assert!(h.fake.with(|s| s.calendar_events.iter().any(|e| e.id == id)));
}

#[tokio::test]
async fn an_edit_that_meets_a_newer_version_keeps_googles() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(event("primary", "a"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let mut mine = stored(&h, "primary", "a").await.unwrap();
    mine.title = "Mine".into();
    copy.save(h.account_id, mine).await.unwrap();
    // Someone changes it on their phone before the queue sends.
    h.fake.put_calendar_event(Event { title: "Theirs".into(), ..event("primary", "a") });
    let turned_down = copy.send(h.account_id).await.unwrap();
    assert_eq!(turned_down.len(), 1);
    assert_eq!(turned_down[0].reason, None);
    assert_eq!(stored(&h, "primary", "a").await.unwrap().title, "Theirs");
    let account = h.account_id;
    assert!(h.db.read(move |c| store::queued(c, account)).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_network_failure_keeps_the_rest_of_the_queue_in_order() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    copy.save(h.account_id, event("primary", "one")).await.unwrap();
    copy.save(h.account_id, event("primary", "two")).await.unwrap();
    h.fake.fail_next(mailrs_gmail::GmailError::Network("gone".into()));
    assert!(copy.send(h.account_id).await.is_err());
    let account = h.account_id;
    let held = h.db.read(move |c| store::queued(c, account)).await.unwrap();
    assert_eq!(held.iter().map(|q| q.event.as_str()).collect::<Vec<_>>(), vec!["one", "two"]);
    copy.send(h.account_id).await.unwrap();
    assert!(h.db.read(move |c| store::queued(c, account)).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_removed_event_leaves_at_once_and_on_google_after_send() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(event("primary", "a"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    copy.remove(h.account_id, "primary", "a").await.unwrap();
    assert!(stored(&h, "primary", "a").await.is_none());
    copy.send(h.account_id).await.unwrap();
    assert!(h.fake.with(|s| s.calendar_events.is_empty()));
}

#[tokio::test]
async fn a_queued_change_survives_a_refresh_that_runs_before_it_is_sent() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(event("primary", "a"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let mut mine = stored(&h, "primary", "a").await.unwrap();
    mine.title = "Mine".into();
    copy.save(h.account_id, mine).await.unwrap();
    h.fake.with(|s| s.expire_calendar_tokens = true);
    copy.refresh(h.account_id, NOW + READ_EVERY_OPEN).await.unwrap();
    assert_eq!(stored(&h, "primary", "a").await.unwrap().title, "Mine");
}

#[test]
fn a_new_id_is_one_google_accepts() {
    let id = new_event_id();
    assert_eq!(id.len(), 32);
    assert!(id.chars().all(|c| c.is_ascii_digit() || ('a'..='v').contains(&c)));
}

/// reconcile.md Task 6 item 6. Google saw the create; only the answer
/// telling us so was lost. `send` must ask what changed rather than
/// asking to create the id a second time.
#[tokio::test]
async fn a_create_whose_answer_was_lost_is_not_sent_twice() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let id = new_event_id();
    copy.save(h.account_id, event("primary", &id)).await.unwrap();
    // The create reached Google, but this computer never heard back.
    h.fake.put_calendar_event(event("primary", &id));
    let turned_down = copy.send(h.account_id).await.unwrap();
    assert!(turned_down.is_empty());
    let account = h.account_id;
    assert!(h.db.read(move |c| store::queued(c, account)).await.unwrap().is_empty());
    assert!(stored(&h, "primary", &id).await.is_some());
}

/// The bug reconcile.md Task 6 item 2 names: `send` used to infer a
/// create from an empty etag. A row `enqueue` marked `Save` (because an
/// edit is already queued for the event, ruling out a create) must still
/// go out as a change even when its etag column is empty, or a second
/// edit to a brand-new event would try to create it again and meet a
/// refusal instead of just updating it.
#[tokio::test]
async fn a_queued_save_with_no_etag_is_not_sent_as_a_create() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(event("primary", "a"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let mut edited = event("primary", "a");
    edited.title = "Edited".into();
    let account_id = h.account_id;
    h.db.write(move |c| store::enqueue(c, account_id, store::ChangeKind::Save, &edited)).await.unwrap();
    assert!(copy.send(h.account_id).await.unwrap().is_empty());
    assert_eq!(h.fake.usage().calls_to("calendar.events.insert"), 0);
    assert_eq!(h.fake.usage().calls_to("calendar.events.patch"), 1);
    assert_eq!(stored(&h, "primary", "a").await.unwrap().title, "Edited");
}

/// The bug reconcile.md Task 6 item 3 names: a `Save` that meets a 404
/// used to hit the catch-all `Err` arm, which stopped the whole send and
/// left every change behind it stuck for good.
#[tokio::test]
async fn an_edit_to_an_event_deleted_elsewhere_leaves_the_queue() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(event("primary", "a"));
    h.fake.put_calendar_event(event("primary", "b"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let mut mine = stored(&h, "primary", "a").await.unwrap();
    mine.title = "Mine".into();
    copy.save(h.account_id, mine).await.unwrap();
    let mut second = stored(&h, "primary", "b").await.unwrap();
    second.title = "Second".into();
    copy.save(h.account_id, second).await.unwrap();
    // Someone deletes "a" elsewhere before the queue sends.
    h.fake.drop_calendar_event("primary", "a");
    let turned_down = copy.send(h.account_id).await.unwrap();
    assert_eq!(turned_down.len(), 1);
    assert_eq!(turned_down[0].reason.as_deref(), Some("deleted elsewhere"));
    assert!(stored(&h, "primary", "a").await.is_none());
    // The change behind it in the queue still went out.
    assert_eq!(stored(&h, "primary", "b").await.unwrap().title, "Second");
    let account = h.account_id;
    assert!(h.db.read(move |c| store::queued(c, account)).await.unwrap().is_empty());
}

/// reconcile.md Task 6 item 4: an edit that lands while the first is
/// still waiting on Google must not be lost, and must not be sent
/// against the etag it started with once the first one changed it.
#[tokio::test]
async fn an_edit_during_a_send_goes_out_against_the_new_version() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(event("primary", "a"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let mut mine = stored(&h, "primary", "a").await.unwrap();
    mine.title = "Mine".into();
    copy.save(h.account_id, mine.clone()).await.unwrap();

    let mut held = h.fake.hold("calendar.events.patch");
    let account_id = h.account_id;
    let send = copy.send(account_id);
    let race = async {
        held.entered().await;
        let mut again = mine.clone();
        again.title = "Mine again".into();
        copy.save(account_id, again).await.unwrap();
        held.release();
    };
    let (result, ()) = tokio::join!(send, race);
    assert!(result.unwrap().is_empty());

    let account = h.account_id;
    let queued = h.db.read(move |c| store::queued(c, account)).await.unwrap();
    assert_eq!(queued.len(), 1, "the edit that landed mid-send is still queued, not lost");
    assert_eq!(queued[0].body.as_ref().map(|e| e.title.as_str()), Some("Mine again"));
    let on_google = h.fake.with(|s| s.calendar_events.iter().find(|e| e.id == "a").cloned()).unwrap();
    assert_eq!(queued[0].etag.as_deref(), Some(on_google.etag.as_str()), "targets the version just written");

    assert!(copy.send(h.account_id).await.unwrap().is_empty(), "the merged edit goes out with no conflict");
    assert_eq!(stored(&h, "primary", "a").await.unwrap().title, "Mine again");
}

async fn queue(h: &Harness) -> Vec<store::QueuedChange> {
    let account = h.account_id;
    h.db.read(move |c| store::queued(c, account)).await.unwrap()
}

/// Google answers a delete of an event it already deleted with 410 Gone,
/// which the client reads the way it reads an expired sync token. The
/// event is gone, which is what the delete asked for, so the change
/// after it still goes out.
#[tokio::test]
async fn a_delete_google_answers_with_gone_completes_and_the_queue_moves_on() {
    let h = harness().await;
    h.fake.with(|s| {
        s.calendars = vec![calendar("primary", true)];
        s.deleted_answers_gone = true;
    });
    h.fake.put_calendar_event(event("primary", "a"));
    h.fake.put_calendar_event(event("primary", "b"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    copy.remove(h.account_id, "primary", "a").await.unwrap();
    let mut second = stored(&h, "primary", "b").await.unwrap();
    second.title = "Second".into();
    copy.save(h.account_id, second).await.unwrap();
    // The first delete reached Google, and the app quit before it heard.
    h.fake.drop_calendar_event("primary", "a");

    let turned_down = copy.send(h.account_id).await.unwrap();

    assert!(turned_down.is_empty(), "{turned_down:?}");
    assert!(queue(&h).await.is_empty());
    let on_google = h.fake.with(|s| s.calendar_events.iter().find(|e| e.id == "b").cloned()).unwrap();
    assert_eq!(on_google.title, "Second");
}

#[tokio::test]
async fn an_edit_google_answers_with_gone_leaves_the_queue_and_the_copy() {
    let h = harness().await;
    h.fake.with(|s| {
        s.calendars = vec![calendar("primary", true)];
        s.deleted_answers_gone = true;
    });
    h.fake.put_calendar_event(event("primary", "a"));
    h.fake.put_calendar_event(event("primary", "b"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let mut mine = stored(&h, "primary", "a").await.unwrap();
    mine.title = "Mine".into();
    copy.save(h.account_id, mine).await.unwrap();
    let mut second = stored(&h, "primary", "b").await.unwrap();
    second.title = "Second".into();
    copy.save(h.account_id, second).await.unwrap();
    h.fake.drop_calendar_event("primary", "a");

    copy.send(h.account_id).await.unwrap();

    assert!(queue(&h).await.is_empty());
    assert!(stored(&h, "primary", "a").await.is_none());
    assert_eq!(stored(&h, "primary", "b").await.unwrap().title, "Second");
}

/// `calendar_changes` rows outlive a calendar that left the list, so a
/// new event can wait for a calendar Google no longer has.
#[tokio::test]
async fn a_new_event_on_a_calendar_that_is_gone_leaves_the_queue() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true), calendar("team", false)]);
    h.fake.put_calendar_event(event("primary", "b"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let id = new_event_id();
    copy.save(h.account_id, event("team", &id)).await.unwrap();
    let mut second = stored(&h, "primary", "b").await.unwrap();
    second.title = "Second".into();
    copy.save(h.account_id, second).await.unwrap();
    h.fake.with(|s| {
        s.calendars.retain(|c| c.id != "team");
        s.deleted_calendars.push("team".into());
    });

    let turned_down = copy.send(h.account_id).await.unwrap();

    assert_eq!(turned_down.len(), 1);
    assert!(queue(&h).await.is_empty());
    assert!(stored(&h, "team", &id).await.is_none());
    assert_eq!(stored(&h, "primary", "b").await.unwrap().title, "Second");
}

/// Only a failure that may pass, such as the network going, stops the
/// send. Anything else turns the one change down, so no single change
/// can hold the account's queue.
#[tokio::test]
async fn a_change_google_cannot_take_for_any_other_reason_leaves_the_queue() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(event("primary", "a"));
    h.fake.put_calendar_event(event("primary", "b"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let mut mine = stored(&h, "primary", "a").await.unwrap();
    mine.title = "Mine".into();
    copy.save(h.account_id, mine).await.unwrap();
    let mut second = stored(&h, "primary", "b").await.unwrap();
    second.title = "Second".into();
    copy.save(h.account_id, second).await.unwrap();
    h.fake.fail_next(mailrs_gmail::GmailError::Decode("not JSON".into()));

    let turned_down = copy.send(h.account_id).await.unwrap();

    assert_eq!(turned_down.len(), 1);
    assert!(turned_down[0].reason.is_some());
    assert!(queue(&h).await.is_empty());
    assert_eq!(stored(&h, "primary", "a").await.unwrap().title, "a", "Google's version is back");
    assert_eq!(stored(&h, "primary", "b").await.unwrap().title, "Second");
}

/// Google took the create and its answer was lost, so the row is still a
/// create when the person edits the event. The next send meets 409 for
/// the id, and the edit must still reach Google.
#[tokio::test]
async fn an_edit_after_a_create_whose_answer_was_lost_still_goes_out() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let id = new_event_id();
    copy.save(h.account_id, event("primary", &id)).await.unwrap();
    // The create reached Google, but this computer never heard back.
    h.fake.put_calendar_event(event("primary", &id));
    let mut edited = stored(&h, "primary", &id).await.unwrap();
    edited.title = "Edited".into();
    copy.save(h.account_id, edited).await.unwrap();

    let turned_down = copy.send(h.account_id).await.unwrap();

    assert!(turned_down.is_empty(), "{turned_down:?}");
    assert!(queue(&h).await.is_empty());
    let on_google = h.fake.with(|s| s.calendar_events.iter().find(|e| e.id == id).cloned()).unwrap();
    assert_eq!(on_google.title, "Edited");
    let held = stored(&h, "primary", &id).await.unwrap();
    assert_eq!((held.title.as_str(), held.pending), ("Edited", false));
}
