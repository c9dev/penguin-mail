//! Reading every calendar of every account into the store and keeping it
//! fresh, against the in-memory Gmail.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::calendar::series::{RepeatScope, Step};
use mailrs_domain::calendar::{Access, Calendar, Event, Notify, Occurrence, Status, occurrence_id};
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

/// An account that granted `calendar.events` but not the list
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

/// The scopes the store already knows about say the calendar is
/// withheld before any call goes out, so the refresh costs nothing.
#[tokio::test]
async fn an_account_that_withheld_the_calendar_costs_no_call() {
    let h = harness().await;
    h.fake.withhold(mailrs_gmail::CALENDAR_SCOPE);
    let done = copy(&h).refresh(h.account_id, NOW).await.unwrap();
    assert!(matches!(done, Permitted::NeedsPermission));
    assert_eq!(h.fake.usage().calls_to("calendar.calendarList.list"), 0);
}

/// The list scope alone is withheld, so the refresh skips straight to
/// the primary calendar rather than listing first and catching the
/// refusal.
#[tokio::test]
async fn an_account_that_withheld_the_list_reads_its_primary_without_listing() {
    let h = harness().await;
    h.fake.withhold(mailrs_gmail::CALENDAR_LIST_SCOPE);
    h.fake.put_calendar_event(event("me@example.com", "a"));
    let done = copy(&h).refresh(h.account_id, NOW).await.unwrap();
    assert!(matches!(done, Permitted::Done(ref r) if r.events == 1), "{done:?}");
    assert_eq!(h.fake.usage().calls_to("calendar.calendarList.list"), 0);
    assert!(stored(&h, "me@example.com", "a").await.is_some());
}

/// Google cannot say which calendar scope a refusal is for, so the first
/// refresh finds out with one list call and one read. After that the
/// account costs nothing until half an hour passes or the person grants
/// the permission, and it keeps no calendar that would read as synced.
#[tokio::test]
async fn without_any_calendar_permission_the_account_is_left_alone() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.withhold(mailrs_gmail::CALENDAR_SCOPE);
    let copy = copy(&h);
    let accounts = [h.account_id];
    let first = copy.refresh_due(&accounts, NOW, true).await.unwrap();
    assert_eq!(first.needs_permission, vec![h.account_id]);
    let calls = || h.fake.usage().calls_to("calendar.events.list") + h.fake.usage().calls_to("calendar.calendarList.list");
    let before = calls();
    let again = copy.refresh_due(&accounts, NOW + READ_EVERY_OPEN, true).await.unwrap();
    assert_eq!(calls(), before, "no call a minute later");
    assert!(again.needs_permission.is_empty(), "nothing new to say");
    let account = h.account_id;
    assert!(h.db.read(move |c| store::calendars(c, account)).await.unwrap().is_empty(), "no fallback calendar");

    h.fake.grant(mailrs_gmail::CALENDAR_SCOPE);
    copy.permission_changed(h.account_id);
    copy.refresh_due(&accounts, NOW + 2 * READ_EVERY_OPEN, true).await.unwrap();
    assert!(h.db.read(move |c| store::synced(c, account)).await.unwrap(), "a grant reads at once");
}

/// A withheld list scope is known from the store already, so every
/// refresh fills the primary calendar without spending a call, not just
/// the first: there is no refusal left to wait out.
#[tokio::test]
async fn a_withheld_list_permission_never_costs_a_call() {
    let h = harness().await;
    h.fake.withhold(mailrs_gmail::CALENDAR_LIST_SCOPE);
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    copy.refresh(h.account_id, NOW + READ_EVERY_OPEN).await.unwrap();
    copy.refresh(h.account_id, NOW + LIST_EVERY).await.unwrap();
    assert_eq!(h.fake.usage().calls_to("calendar.calendarList.list"), 0);
}

/// A calendar the person hid is read at the slow, tray
/// cadence even while the window is open, rather than every minute like
/// the calendars they look at.
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
/// calendar, IMAP today, is skipped without an error or a wasted call.
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

/// One account's trouble, such as one not yet
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

/// The app ticks every 15 s and a first read
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

/// Google saw the create; only the answer
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

/// `send` once inferred a
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

/// A `Save` that meets a 404 once hit the catch-all `Err` arm, which stopped the whole send and
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

/// An edit that lands while the first is
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

/// The person deletes a new event while its create is on the way to
/// Google. The delete wins: Google takes the create, so the delete has to
/// follow it there, and the answer must not put the event back.
#[tokio::test]
async fn a_delete_made_while_the_create_is_in_flight_wins() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let id = new_event_id();
    copy.save(h.account_id, event("primary", &id)).await.unwrap();

    let mut held = h.fake.hold("calendar.events.insert");
    let account_id = h.account_id;
    let send = copy.send(account_id);
    let race = async {
        held.entered().await;
        copy.remove(account_id, "primary", &id).await.unwrap();
        held.release();
    };
    let (result, ()) = tokio::join!(send, race);
    assert!(result.unwrap().is_empty());

    assert!(stored(&h, "primary", &id).await.is_none(), "the answer does not bring it back");
    assert!(h.fake.with(|s| s.calendar_events.iter().all(|e| e.id != id)), "the delete reached Google");
    assert!(queue(&h).await.is_empty());
}

/// A calendar removed elsewhere answers 404 until the list is read again.
/// The calendars after it are still read, and the list is read again on
/// the next refresh rather than half an hour later.
#[tokio::test]
async fn a_calendar_that_fails_to_read_leaves_the_rest_to_be_read() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true), calendar("archive", false), calendar("team", false)]);
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    h.fake.with(|s| {
        s.calendars.retain(|c| c.id != "archive");
        s.deleted_calendars.push("archive".into());
    });
    h.fake.put_calendar_event(event("team", "c"));

    copy.refresh(h.account_id, NOW + READ_EVERY_OPEN).await.unwrap();
    assert!(stored(&h, "team", "c").await.is_some(), "the calendar after the failing one is read");

    copy.refresh(h.account_id, NOW + 2 * READ_EVERY_OPEN).await.unwrap();
    let account = h.account_id;
    let left = h.db.read(move |c| store::calendars(c, account)).await.unwrap();
    assert!(left.iter().all(|c| c.id != "archive"), "the list is read again at once");
}

/// What the code logs at warning level while `run` runs, on this thread.
async fn warnings<F: std::future::Future>(run: F) -> String {
    #[derive(Clone, Default)]
    struct Log(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Log {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let log = Log::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    run.await;
    String::from_utf8(log.0.lock().unwrap().clone()).unwrap()
}

/// An account still starting, signed out or just removed has no engine
/// to read with. The app ticks every 15 seconds, so it is skipped quietly
/// rather than warned about twice a tick.
#[tokio::test]
async fn an_account_that_is_not_running_is_skipped_quietly() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    let copy = copy(&h);
    let missing = h.account_id + 1;
    let logged = warnings(async {
        copy.refresh_due(&[missing, h.account_id], NOW, true).await.unwrap();
    })
    .await;
    assert_eq!(logged, "");
}

/// A whole read writes each page to the store as it arrives and holds no
/// more than that page, so a calendar of 10,000 events never sits in
/// memory whole. A read cut off after three pages has already stored
/// those three, and has kept no token, so the next read walks it again.
/// The heap counter cannot pin this: the store frees each page on its own
/// thread, so the counter for the test thread only ever sees pages added.
#[tokio::test]
async fn a_whole_read_holds_one_page_at_a_time() {
    let h = harness().await;
    h.fake.with(|s| {
        s.calendars = vec![calendar("primary", true)];
        s.page_size = 250;
    });
    for n in 0..10_000 {
        h.fake.put_calendar_event(event("primary", &format!("e{n}")));
    }
    h.fake.fail_call("calendar.events.list", 3, mailrs_gmail::GmailError::Network("gone".into()));

    assert!(copy(&h).refresh(h.account_id, NOW).await.is_err());

    let account = h.account_id;
    let held: i64 = h
        .db
        .read(move |c| {
            Ok(c.query_row("SELECT COUNT(*) FROM events WHERE account_id = ?1", [account], |row| row.get(0))?)
        })
        .await
        .unwrap();
    assert_eq!(held, 750, "each page went to the store before the next was asked for");
    assert_eq!(h.db.read(move |c| store::token(c, account, "primary")).await.unwrap(), None);
}

// Series edits, and changes held while their Undo toast is up.

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;

fn standup() -> Event {
    Event { rules: vec!["RRULE:FREQ=DAILY;COUNT=5".into()], ..event("primary", "standup") }
}

async fn read_series(h: &Harness) -> CalendarCopy<Connected> {
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(standup());
    let copy = copy(h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    copy
}

/// The occurrences whose start falls on day `day` after `NOW`.
async fn on_day(h: &Harness, day: i64) -> Vec<Occurrence> {
    let account = h.account_id;
    let from = NOW + day * DAY;
    h.db.read(move |c| store::occurrences(c, &[account], from, from + DAY, store::CalendarScope::Shown))
        .await
        .unwrap()
}

fn on_google(h: &Harness, id: &str) -> Option<Event> {
    h.fake.with(|s| s.calendar_events.iter().find(|e| e.id == id).cloned())
}

fn held<T>(answer: Permitted<T>) -> T {
    answer.done().expect("the account may change its calendar")
}

#[tokio::test]
async fn moving_one_occurrence_goes_out_as_a_patch_of_that_occurrence() {
    let h = harness().await;
    let copy = read_series(&h).await;
    let tuesday = on_day(&h, 1).await.remove(0);
    let edited = Event { start: tuesday.start + HOUR, end: tuesday.end + HOUR, ..Event::clone(&tuesday.event) };
    let steps = copy.change_steps(h.account_id, &tuesday, edited, Some(RepeatScope::This)).await.unwrap();
    held(copy.apply(h.account_id, steps).await.unwrap());
    assert_eq!(on_day(&h, 1).await[0].start, NOW + DAY + HOUR);
    assert_eq!(queue(&h).await[0].kind, store::ChangeKind::Save, "never a create");
    assert!(copy.send(h.account_id).await.unwrap().is_empty());
    let id = occurrence_id(&standup(), NOW + DAY);
    let sent = on_google(&h, &id).unwrap();
    assert_eq!(sent.series.as_deref(), Some("standup"));
    assert_eq!(sent.start, NOW + DAY + HOUR);
    assert_eq!(h.fake.with(|s| s.calendar_events.len()), 2, "no second series was made");
    assert_eq!(on_google(&h, "standup").unwrap().rules, standup().rules);
}

#[tokio::test]
async fn this_and_following_splits_the_series_on_google() {
    let h = harness().await;
    let copy = read_series(&h).await;
    let third = on_day(&h, 2).await.remove(0);
    let edited = Event { title: "Longer stand-up".into(), ..Event::clone(&third.event) };
    let edited = Event { start: third.start, end: third.end, ..edited };
    let steps = copy.change_steps(h.account_id, &third, edited, Some(RepeatScope::Following)).await.unwrap();
    held(copy.apply(h.account_id, steps).await.unwrap());
    assert!(copy.send(h.account_id).await.unwrap().is_empty());
    let events = h.fake.with(|s| s.calendar_events.clone());
    let old = events.iter().find(|e| e.id == "standup").unwrap();
    assert!(old.rules[0].contains("UNTIL="), "{:?}", old.rules);
    let new = events.iter().find(|e| e.title == "Longer stand-up").unwrap();
    assert_eq!(new.rules, vec!["RRULE:FREQ=DAILY;COUNT=3".to_string()]);
    assert_eq!(on_day(&h, 1).await[0].event.title, "standup");
    assert_eq!(on_day(&h, 3).await[0].event.title, "Longer stand-up");
    assert!(queue(&h).await.is_empty());
}

/// A guest answered after the copy last read the series, so Google turns
/// the cut down with 412. The new half and the removals queued behind it
/// must not go out, or the tail shows twice and its guests are invited
/// to a copy, and the copy goes back to Google's series.
#[tokio::test]
async fn a_split_whose_cut_is_turned_down_sends_nothing_after_it() {
    let h = harness().await;
    let moved_id = occurrence_id(&standup(), NOW + 3 * DAY);
    h.fake.put_calendar_event(Event {
        id: moved_id.clone(),
        rules: Vec::new(),
        series: Some("standup".into()),
        original_start: Some(NOW + 3 * DAY),
        start: NOW + 3 * DAY + HOUR,
        end: NOW + 3 * DAY + 2 * HOUR,
        title: "Moved".into(),
        ..standup()
    });
    let copy = read_series(&h).await;
    let third = on_day(&h, 2).await.remove(0);
    let edited = Event { title: "Longer stand-up".into(), ..Event::clone(&third.event) };
    let edited = Event { start: third.start, end: third.end, ..edited };
    let steps = copy.change_steps(h.account_id, &third, edited, Some(RepeatScope::Following)).await.unwrap();
    assert_eq!(steps.len(), 3, "the cut, the new half and the moved occurrence's removal: {steps:?}");
    let new_id = steps.iter().map(Step::key).map(|(_, id)| id).find(|id| id != "standup" && id != &moved_id).unwrap();
    held(copy.apply(h.account_id, steps).await.unwrap());
    // The guest's answer moves the series' etag on Google.
    h.fake.put_calendar_event(on_google(&h, "standup").unwrap());

    let turned_down = copy.send(h.account_id).await.unwrap();

    assert_eq!(turned_down.len(), 1, "{turned_down:?}");
    assert!(on_google(&h, &new_id).is_none(), "the new half was never created");
    assert_eq!(on_google(&h, &moved_id).unwrap().title, "Moved", "the moved occurrence stays");
    assert_eq!(on_google(&h, "standup").unwrap().rules, standup().rules);
    assert!(queue(&h).await.is_empty());
    assert!(stored(&h, "primary", &new_id).await.is_none(), "the copy drops the new half");
    let day_three: Vec<String> = on_day(&h, 3).await.iter().map(|o| o.event.title.clone()).collect();
    assert_eq!(day_three, vec!["Moved".to_string()]);
    let day_two: Vec<String> = on_day(&h, 2).await.iter().map(|o| o.event.title.clone()).collect();
    assert_eq!(day_two, vec!["standup".to_string()]);
}

/// The moved occurrence already had an unsent edit, so its removal folds
/// into that row, which sits ahead of the cut in the queue. It still
/// waits for the cut: nothing goes out when the cut is turned down, and
/// it goes out in the same send when the cut is taken.
#[tokio::test]
async fn a_removal_queued_ahead_of_its_cut_waits_for_it() {
    for refused in [true, false] {
        let h = harness().await;
        let moved_id = occurrence_id(&standup(), NOW + 3 * DAY);
        let moved = Event {
            id: moved_id.clone(),
            rules: Vec::new(),
            series: Some("standup".into()),
            original_start: Some(NOW + 3 * DAY),
            start: NOW + 3 * DAY + HOUR,
            end: NOW + 3 * DAY + 2 * HOUR,
            title: "Moved".into(),
            ..standup()
        };
        h.fake.put_calendar_event(moved);
        let copy = read_series(&h).await;
        let mut edited = stored(&h, "primary", &moved_id).await.unwrap();
        edited.title = "Moved again".into();
        copy.save(h.account_id, edited).await.unwrap();
        let third = on_day(&h, 2).await.remove(0);
        let edited = Event { title: "Longer stand-up".into(), ..Event::clone(&third.event) };
        let edited = Event { start: third.start, end: third.end, ..edited };
        let steps = copy.change_steps(h.account_id, &third, edited, Some(RepeatScope::Following)).await.unwrap();
        held(copy.apply(h.account_id, steps).await.unwrap());
        assert_eq!(queue(&h).await[0].event, moved_id, "the removal kept the earlier row's place");
        if refused {
            h.fake.put_calendar_event(on_google(&h, "standup").unwrap());
        }

        copy.send(h.account_id).await.unwrap();

        if refused {
            // The removal folded into the edit's row, and the edit goes out.
            assert_eq!(on_google(&h, &moved_id).unwrap().title, "Moved again");
            assert_eq!(stored(&h, "primary", &moved_id).await.unwrap().title, "Moved again");
        } else {
            assert!(on_google(&h, &moved_id).is_none());
        }
        assert!(queue(&h).await.is_empty(), "refused: {refused}");
    }
}

/// A "this and following" split whose cut Google takes, then whose new
/// half it turns down: the tail of the series would be gone. The cut
/// series gets its rules back, the moved occurrence after the cut is
/// kept, and the person hears why.
async fn split_with_new_half_refused(h: &Harness, copy: &CalendarCopy<Connected>, restart: bool) {
    let moved_id = occurrence_id(&standup(), NOW + 3 * DAY);
    let third = on_day(h, 2).await.remove(0);
    let edited = Event { title: "Longer stand-up".into(), ..Event::clone(&third.event) };
    let edited = Event { start: third.start, end: third.end, ..edited };
    let steps = copy.change_steps(h.account_id, &third, edited, Some(RepeatScope::Following)).await.unwrap();
    let new_id = steps.iter().map(Step::key).map(|(_, id)| id).find(|id| id != "standup" && id != &moved_id).unwrap();
    h.fake.with(|s| s.refuse_new_events = true);
    let turned_down = if restart {
        held(copy.hold(h.account_id, steps).await.unwrap());
        let next_run = self::copy(h);
        next_run.recover_holds().await.unwrap();
        next_run.send(h.account_id).await.unwrap()
    } else {
        held(copy.apply(h.account_id, steps).await.unwrap());
        copy.send(h.account_id).await.unwrap()
    };

    assert_eq!(turned_down.len(), 1, "{turned_down:?}");
    assert!(turned_down[0].reason.as_deref().unwrap_or_default().contains("Invalid recurrence rule."));
    assert!(on_google(h, &new_id).is_none());
    assert_eq!(on_google(h, "standup").unwrap().rules, standup().rules, "the series repeats as before");
    assert_eq!(on_google(h, &moved_id).unwrap().title, "Moved", "the moved occurrence stays");
    assert!(queue(h).await.is_empty());
    assert!(stored(h, "primary", &new_id).await.is_none());
    for (day, title) in [(2, "standup"), (3, "Moved"), (4, "standup")] {
        let titles: Vec<String> = on_day(h, day).await.iter().map(|o| o.event.title.clone()).collect();
        assert_eq!(titles, vec![title.to_string()], "day {day}");
    }
}

async fn series_with_a_moved_occurrence(h: &Harness) -> CalendarCopy<Connected> {
    h.fake.put_calendar_event(Event {
        id: occurrence_id(&standup(), NOW + 3 * DAY),
        rules: Vec::new(),
        series: Some("standup".into()),
        original_start: Some(NOW + 3 * DAY),
        start: NOW + 3 * DAY + HOUR,
        end: NOW + 3 * DAY + 2 * HOUR,
        title: "Moved".into(),
        ..standup()
    });
    read_series(h).await
}

#[tokio::test]
async fn a_split_whose_new_half_is_turned_down_puts_the_series_back() {
    let h = harness().await;
    let copy = series_with_a_moved_occurrence(&h).await;
    split_with_new_half_refused(&h, &copy, false).await;
}

#[tokio::test]
async fn a_split_recovered_after_a_restart_still_puts_the_series_back() {
    let h = harness().await;
    let copy = series_with_a_moved_occurrence(&h).await;
    split_with_new_half_refused(&h, &copy, true).await;
}

#[tokio::test]
async fn all_events_changes_the_series_itself() {
    let h = harness().await;
    let copy = read_series(&h).await;
    let third = on_day(&h, 2).await.remove(0);
    let edited = Event { title: "Team stand-up".into(), ..Event::clone(&third.event) };
    let edited = Event { start: third.start + HOUR, end: third.end + HOUR, ..edited };
    let steps = copy.change_steps(h.account_id, &third, edited, Some(RepeatScope::All)).await.unwrap();
    held(copy.apply(h.account_id, steps).await.unwrap());
    assert!(copy.send(h.account_id).await.unwrap().is_empty());
    let series = on_google(&h, "standup").unwrap();
    assert_eq!((series.title.as_str(), series.start), ("Team stand-up", NOW + HOUR));
    assert_eq!(series.rules, standup().rules);
    assert_eq!(h.fake.with(|s| s.calendar_events.len()), 1);
    assert_eq!(on_day(&h, 0).await[0].start, NOW + HOUR);
}

/// The stand-up as Rita organizes it and this account attends, skipping
/// the fourth day and adding a sixth.
fn attended_standup() -> Event {
    use mailrs_domain::calendar::Guest;
    Event {
        guests: vec![
            Guest { email: "rita@example.com".into(), organizer: true, ..Guest::default() },
            Guest { email: "me@example.com".into(), me: true, ..Guest::default() },
        ],
        rules: vec![
            "RRULE:FREQ=DAILY;COUNT=5".into(),
            "EXDATE:20260924T141320Z".into(),
            "RDATE:20260928T141320Z".into(),
        ],
        ..standup()
    }
}

/// A guest opens the third day, changes their reminders, colour and busy,
/// and saves under `scope`. The editor hands over the event it opened,
/// which for an occurrence nobody changed is the series, starting on the
/// first day.
async fn guest_saves_the_third_day(h: &Harness, scope: RepeatScope) {
    use mailrs_domain::calendar::{Reminder, ReminderMethod};
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(attended_standup());
    let copy = copy(h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let third = on_day(h, 2).await.remove(0);
    let edited = Event {
        reminders: Some(vec![Reminder { minutes: 30, method: ReminderMethod::Notification }]),
        color: Some("#f4511e".into()),
        busy: false,
        ..Event::clone(&third.event)
    };
    let steps = copy.change_steps(h.account_id, &third, edited, Some(scope)).await.unwrap();
    held(copy.apply(h.account_id, steps).await.unwrap());
    assert!(copy.send(h.account_id).await.unwrap().is_empty());
    assert!(queue(h).await.is_empty());
}

/// Every day of the copy starts where it did: the first three, the
/// fourth skipped, the fifth, and the added one.
async fn every_day_keeps_its_time(h: &Harness) {
    for day in [0, 1, 2, 4, 7] {
        let shown = on_day(h, day).await;
        assert_eq!(shown.len(), 1, "day {day}");
        assert_eq!((shown[0].start, shown[0].end), (NOW + day * DAY, NOW + day * DAY + HOUR), "day {day}");
    }
    assert!(on_day(h, 3).await.is_empty(), "the skipped day stays skipped");
}

#[tokio::test]
async fn a_guests_change_to_one_occurrence_moves_nothing() {
    let h = harness().await;
    guest_saves_the_third_day(&h, RepeatScope::This).await;
    let series = on_google(&h, "standup").unwrap();
    assert_eq!((series.start, series.end, &series.rules), (NOW, NOW + HOUR, &attended_standup().rules));
    let one = on_google(&h, &occurrence_id(&attended_standup(), NOW + 2 * DAY)).unwrap();
    assert_eq!((one.start, one.end), (NOW + 2 * DAY, NOW + 2 * DAY + HOUR));
    assert_eq!((one.color.as_deref(), one.busy), (Some("#f4511e"), false));
    every_day_keeps_its_time(&h).await;
    assert!(!on_day(&h, 2).await[0].event.busy);
}

#[tokio::test]
async fn a_guests_change_to_all_events_moves_nothing() {
    let h = harness().await;
    guest_saves_the_third_day(&h, RepeatScope::All).await;
    let series = on_google(&h, "standup").unwrap();
    assert_eq!((series.start, series.end, &series.rules), (NOW, NOW + HOUR, &attended_standup().rules));
    assert_eq!((series.color.as_deref(), series.busy), (Some("#f4511e"), false));
    assert_eq!(h.fake.with(|s| s.calendar_events.len()), 1, "no occurrence or series was made");
    every_day_keeps_its_time(&h).await;
    assert!(!on_day(&h, 0).await[0].event.busy);
}

#[tokio::test]
async fn cancelling_one_occurrence_leaves_a_gap_and_tells_google() {
    let h = harness().await;
    let copy = read_series(&h).await;
    let tuesday = on_day(&h, 1).await.remove(0);
    let steps = copy.delete_steps(h.account_id, &tuesday, Some(RepeatScope::This)).await.unwrap();
    assert!(matches!(steps.as_slice(), [Step::Cancel(_)]));
    held(copy.apply(h.account_id, steps).await.unwrap());
    assert!(on_day(&h, 1).await.is_empty());
    copy.send(h.account_id).await.unwrap();
    assert!(queue(&h).await.is_empty());
    let id = occurrence_id(&standup(), NOW + DAY);
    assert!(!stored(&h, "primary", &id).await.unwrap().pending, "the row settles once the removal went out");
    assert_eq!(on_google(&h, &id).unwrap().status, Status::Cancelled);
    // The next read brings Google's cancelled occurrence, and the gap stays.
    copy.refresh(h.account_id, NOW + READ_EVERY_OPEN).await.unwrap();
    assert!(on_day(&h, 1).await.is_empty());
}

#[tokio::test]
async fn a_new_event_held_then_committed_is_created_on_google() {
    let h = harness().await;
    let copy = read_series(&h).await;
    let id = new_event_id();
    let lunch = Event { id: id.clone(), title: "Lunch".into(), ..event("primary", &id) };
    let held_change = held(copy.hold(h.account_id, vec![Step::Save(lunch)]).await.unwrap());
    copy.commit(held_change).await.unwrap();
    assert_eq!(queue(&h).await[0].kind, store::ChangeKind::Create);
    assert!(copy.send(h.account_id).await.unwrap().is_empty());
    assert_eq!(on_google(&h, &id).unwrap().title, "Lunch");
    assert!(!stored(&h, "primary", &id).await.unwrap().pending);
}

/// An occurrence id holds `_`, which Google refuses in a new event's id,
/// so an occurrence saved without a change of its own still goes out as
/// a change of that occurrence.
#[tokio::test]
async fn saving_an_unchanged_occurrence_queues_a_change_not_a_create() {
    let h = harness().await;
    let copy = read_series(&h).await;
    let id = occurrence_id(&standup(), NOW + DAY);
    let one = Event {
        id: id.clone(),
        rules: Vec::new(),
        series: Some("standup".into()),
        original_start: Some(NOW + DAY),
        start: NOW + DAY,
        end: NOW + DAY + HOUR,
        title: "Tuesday".into(),
        ..standup()
    };
    copy.save(h.account_id, one).await.unwrap();
    assert_eq!(queue(&h).await[0].kind, store::ChangeKind::Save);
    assert!(copy.send(h.account_id).await.unwrap().is_empty());
    assert_eq!(on_google(&h, &id).unwrap().title, "Tuesday");
}

#[tokio::test]
async fn a_held_change_waits_for_commit() {
    let h = harness().await;
    let copy = read_series(&h).await;
    let held_change = held(
        copy.hold(h.account_id, vec![Step::Remove { calendar: "primary".into(), id: "standup".into() }])
            .await
            .unwrap(),
    );
    assert!(on_day(&h, 0).await.is_empty(), "it leaves the grid at once");
    assert!(queue(&h).await.is_empty(), "and reaches the queue only on commit");
    copy.commit(held_change).await.unwrap();
    assert_eq!(queue(&h).await.len(), 1);
    copy.send(h.account_id).await.unwrap();
    assert!(on_google(&h, "standup").is_none());
}

/// Google cancels a series' changed occurrences along with the series,
/// so a moved Tuesday does not outlive the delete on Google or in the
/// copy read back from it.
#[tokio::test]
async fn deleting_a_series_cancels_its_changed_occurrences_on_google() {
    let h = harness().await;
    let tuesday_id = occurrence_id(&standup(), NOW + DAY);
    h.fake.put_calendar_event(Event {
        id: tuesday_id.clone(),
        rules: Vec::new(),
        series: Some("standup".into()),
        original_start: Some(NOW + DAY),
        start: NOW + DAY + HOUR,
        end: NOW + DAY + 2 * HOUR,
        ..standup()
    });
    let copy = read_series(&h).await;
    let tuesday = on_day(&h, 1).await.remove(0);
    let steps = copy.delete_steps(h.account_id, &tuesday, Some(RepeatScope::All)).await.unwrap();
    held(copy.apply(h.account_id, steps).await.unwrap());
    assert!(copy.send(h.account_id).await.unwrap().is_empty());

    assert!(on_google(&h, "standup").is_none());
    assert_eq!(on_google(&h, &tuesday_id).map(|e| e.status), Some(Status::Cancelled));
    copy.refresh(h.account_id, NOW + READ_EVERY_OPEN).await.unwrap();
    assert!(on_day(&h, 1).await.is_empty());
}

/// A series with a moved Tuesday, deleted whole and then taken back: every
/// row returns as it was, the changed occurrence and its guests included.
#[tokio::test]
async fn a_held_delete_can_be_taken_back() {
    let h = harness().await;
    h.fake.put_calendar_event(Event {
        id: occurrence_id(&standup(), NOW + DAY),
        rules: Vec::new(),
        series: Some("standup".into()),
        original_start: Some(NOW + DAY),
        start: NOW + DAY + HOUR,
        end: NOW + DAY + 2 * HOUR,
        guests: vec![mailrs_domain::calendar::Guest { email: "ann@example.com".into(), ..Default::default() }],
        ..standup()
    });
    let copy = read_series(&h).await;
    let tuesday_id = occurrence_id(&standup(), NOW + DAY);
    let (series_before, tuesday_before) =
        (stored(&h, "primary", "standup").await.unwrap(), stored(&h, "primary", &tuesday_id).await.unwrap());
    let tuesday = on_day(&h, 1).await.remove(0);
    let steps = copy.delete_steps(h.account_id, &tuesday, Some(RepeatScope::All)).await.unwrap();
    let held_change = held(copy.hold(h.account_id, steps).await.unwrap());
    assert!(stored(&h, "primary", "standup").await.is_none());
    assert!(stored(&h, "primary", &tuesday_id).await.is_none());
    copy.revert(held_change).await.unwrap();
    assert_eq!(stored(&h, "primary", "standup").await.unwrap(), series_before);
    assert_eq!(stored(&h, "primary", &tuesday_id).await.unwrap(), tuesday_before);
    assert!(queue(&h).await.is_empty());
}

#[tokio::test]
async fn a_held_edit_can_be_taken_back() {
    let h = harness().await;
    let copy = read_series(&h).await;
    let before = stored(&h, "primary", "standup").await.unwrap();
    let third = on_day(&h, 2).await.remove(0);
    let edited = Event { title: "Longer stand-up".into(), ..Event::clone(&third.event) };
    let steps = copy.change_steps(h.account_id, &third, edited, Some(RepeatScope::Following)).await.unwrap();
    let new_id = steps.iter().map(Step::key).map(|(_, id)| id).find(|id| id != "standup").unwrap();
    let held_change = held(copy.hold(h.account_id, steps).await.unwrap());
    copy.revert(held_change).await.unwrap();
    assert_eq!(stored(&h, "primary", "standup").await.unwrap(), before);
    assert!(stored(&h, "primary", &new_id).await.is_none(), "the new half goes");
    assert_eq!(on_day(&h, 3).await[0].event.title, "standup");
}

#[tokio::test]
async fn a_held_move_survives_a_refresh_before_the_toast_closes() {
    let h = harness().await;
    let copy = read_series(&h).await;
    let mut mine = stored(&h, "primary", "standup").await.unwrap();
    mine.title = "Mine".into();
    let held_change = held(copy.hold(h.account_id, vec![Step::Save(mine)]).await.unwrap());
    // Google forgot the token, so the refresh reads the calendar whole.
    h.fake.with(|s| s.expire_calendar_tokens = true);
    copy.refresh(h.account_id, NOW + READ_EVERY_OPEN).await.unwrap();
    assert_eq!(stored(&h, "primary", "standup").await.unwrap().title, "Mine");
    copy.commit(held_change).await.unwrap();
    h.fake.with(|s| s.expire_calendar_tokens = false);
    copy.send(h.account_id).await.unwrap();
    assert_eq!(on_google(&h, "standup").unwrap().title, "Mine");
}

/// Deleting a series takes its changed occurrences off the copy too. A
/// whole read during the toast skips the held series, and must skip its
/// changed occurrences with it, or they would stand alone on the grid.
#[tokio::test]
async fn a_held_series_delete_keeps_its_changed_occurrences_off_the_grid() {
    let h = harness().await;
    h.fake.put_calendar_event(Event {
        id: occurrence_id(&standup(), NOW + DAY),
        rules: Vec::new(),
        series: Some("standup".into()),
        original_start: Some(NOW + DAY),
        start: NOW + DAY + HOUR,
        end: NOW + DAY + 2 * HOUR,
        ..standup()
    });
    let copy = read_series(&h).await;
    let held_change = held(
        copy.hold(h.account_id, vec![Step::Remove { calendar: "primary".into(), id: "standup".into() }])
            .await
            .unwrap(),
    );
    h.fake.with(|s| s.expire_calendar_tokens = true);
    copy.refresh(h.account_id, NOW + READ_EVERY_OPEN).await.unwrap();
    assert!(on_day(&h, 1).await.is_empty(), "{:?}", on_day(&h, 1).await);
    // Committed and not yet sent, the removal still keeps them off.
    copy.commit(held_change).await.unwrap();
    copy.refresh(h.account_id, NOW + 2 * READ_EVERY_OPEN).await.unwrap();
    assert!(on_day(&h, 1).await.is_empty());
}

/// Only one Undo toast shows at a time, so holding a second change
/// commits the first. Undo or commit on the first afterwards does nothing.
#[tokio::test]
async fn a_new_held_change_commits_the_one_before() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(event("primary", "a"));
    h.fake.put_calendar_event(event("primary", "b"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let remove = |id: &str| vec![Step::Remove { calendar: "primary".into(), id: id.into() }];
    let first = held(copy.hold(h.account_id, remove("a")).await.unwrap());
    let second = held(copy.hold(h.account_id, remove("b")).await.unwrap());
    assert_eq!(queue(&h).await.iter().map(|q| q.event.as_str()).collect::<Vec<_>>(), vec!["a"]);
    copy.revert(first.clone()).await.unwrap();
    assert!(stored(&h, "primary", "a").await.is_none(), "too late to take it back");
    copy.commit(first).await.unwrap();
    assert_eq!(queue(&h).await.len(), 1, "and it is not queued twice");
    copy.commit(second).await.unwrap();
    assert_eq!(queue(&h).await.len(), 2);
}

/// A hold whose write fails leaves nothing held, so later reads still
/// bring the provider's changes to the events it named.
#[tokio::test]
async fn a_hold_that_fails_leaves_reads_alone() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(event("primary", "a"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let mine = Event { title: "Mine".into(), ..stored(&h, "primary", "a").await.unwrap() };
    // No calendar "gone" is stored, so its row breaks the foreign key.
    let steps = vec![Step::Save(mine), Step::Save(event("gone", "b"))];
    assert!(copy.hold(h.account_id, steps).await.is_err());
    assert_eq!(stored(&h, "primary", "a").await.unwrap().title, "a", "nothing was written");
    h.fake.put_calendar_event(Event { title: "Theirs".into(), ..event("primary", "a") });
    copy.refresh(h.account_id, NOW + READ_EVERY_OPEN).await.unwrap();
    assert_eq!(stored(&h, "primary", "a").await.unwrap().title, "Theirs");
}

/// Two edits to one date made offline queue one change, which goes out
/// once the network is back.
#[tokio::test]
async fn editing_one_occurrence_twice_offline_queues_one_change() {
    let h = harness().await;
    let copy = read_series(&h).await;
    for title in ["First", "Second"] {
        let tuesday = on_day(&h, 1).await.remove(0);
        let edited = Event { title: title.into(), ..Event::clone(&tuesday.event) };
        let edited = Event { start: tuesday.start, end: tuesday.end, ..edited };
        let steps = copy.change_steps(h.account_id, &tuesday, edited, Some(RepeatScope::This)).await.unwrap();
        held(copy.apply(h.account_id, steps).await.unwrap());
    }
    assert_eq!(queue(&h).await.len(), 1);
    h.fake.fail_next(mailrs_gmail::GmailError::Network("gone".into()));
    assert!(copy.send(h.account_id).await.is_err());
    assert_eq!(queue(&h).await.len(), 1);
    assert!(copy.send(h.account_id).await.unwrap().is_empty());
    let id = occurrence_id(&standup(), NOW + DAY);
    assert_eq!(on_google(&h, &id).unwrap().title, "Second");
    assert!(queue(&h).await.is_empty());
}

#[tokio::test]
async fn an_account_without_a_calendar_takes_no_edit() {
    let h = imap_harness().await;
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    let copy = CalendarCopy::new(Arc::new(Connected(connected)), h.db.clone());
    let steps = vec![Step::Save(event("primary", "a"))];
    let answer = copy.hold(h.account_id, steps.clone()).await;
    assert!(
        matches!(answer, Err(crate::SyncError::Backend(crate::BackendError::Unsupported))),
        "{answer:?}"
    );
    let answer = copy.apply(h.account_id, steps).await;
    assert!(matches!(answer, Err(crate::SyncError::Backend(crate::BackendError::Unsupported))));
}

#[tokio::test]
async fn an_account_that_withheld_the_calendar_is_asked_for_it() {
    let h = harness().await;
    let copy = read_series(&h).await;
    h.fake.withhold(mailrs_gmail::CALENDAR_SCOPE);
    let remove = vec![Step::Remove { calendar: "primary".into(), id: "standup".into() }];
    assert!(matches!(copy.hold(h.account_id, remove.clone()).await.unwrap(), Permitted::NeedsPermission));
    assert!(matches!(copy.apply(h.account_id, remove).await.unwrap(), Permitted::NeedsPermission));
    assert!(stored(&h, "primary", "standup").await.is_some());
    assert!(queue(&h).await.is_empty());
}

/// A change held under its Undo toast must survive the app quitting or
/// crashing: nothing commits it, nothing reverts it, and yet the next
/// start still queues it, since no toast survived to offer Undo over it.
#[tokio::test]
async fn a_held_change_survives_a_crash_and_is_queued_at_the_next_start() {
    let h = harness().await;
    let first_run = read_series(&h).await;
    held(
        first_run
            .hold(h.account_id, vec![Step::Remove { calendar: "primary".into(), id: "standup".into() }])
            .await
            .unwrap(),
    );
    assert!(queue(&h).await.is_empty(), "not queued until committed or recovered");
    // The run ends here with no clean shutdown, as a crash would: no
    // commit, no revert, only what `hold` already wrote to the store.
    drop(first_run);

    let next_run = copy(&h);
    next_run.recover_holds().await.unwrap();
    assert_eq!(queue(&h).await.len(), 1, "the held change is queued at the next start");
    next_run.send(h.account_id).await.unwrap();
    assert!(on_google(&h, "standup").is_none());
}

/// Recovering twice, such as a second call before the window opens,
/// queues the change once.
#[tokio::test]
async fn recovering_holds_a_second_time_queues_nothing_more() {
    let h = harness().await;
    let first_run = read_series(&h).await;
    held(
        first_run
            .hold(h.account_id, vec![Step::Remove { calendar: "primary".into(), id: "standup".into() }])
            .await
            .unwrap(),
    );
    drop(first_run);

    let next_run = copy(&h);
    next_run.recover_holds().await.unwrap();
    next_run.recover_holds().await.unwrap();
    assert_eq!(queue(&h).await.len(), 1);
}

/// The view polls `still_waiting` while its toast is up, to close it
/// without an Undo that would do nothing once an assistant edit, made
/// through `apply`, commits the change waiting before it.
#[tokio::test]
async fn still_waiting_says_no_once_an_apply_commits_the_change_before_it() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(event("primary", "a"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let removed = held(
        copy.hold(h.account_id, vec![Step::Remove { calendar: "primary".into(), id: "a".into() }])
            .await
            .unwrap(),
    );
    assert!(copy.still_waiting(&removed));
    // The assistant's own edit, elsewhere, made through `apply`.
    held(copy.apply(h.account_id, vec![Step::Save(event("primary", "b"))]).await.unwrap());
    assert!(!copy.still_waiting(&removed), "apply committed it, same as holding a new change would");
}

fn with_guest(calendar: &str, id: &str) -> Event {
    Event {
        guests: vec![mailrs_domain::calendar::Guest { email: "ann@example.com".into(), ..Default::default() }],
        ..event(calendar, id)
    }
}

fn notices(h: &Harness) -> Vec<(String, Notify)> {
    h.fake.with(|s| s.calendar_notices.clone())
}

#[tokio::test]
async fn a_new_event_with_a_guest_reaches_google_with_invitations_on() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let id = new_event_id();
    held(copy.apply(h.account_id, vec![Step::Save(with_guest("primary", &id))]).await.unwrap());
    copy.send(h.account_id).await.unwrap();

    assert_eq!(on_google(&h, &id).unwrap().guests[0].email, "ann@example.com");
    assert_eq!(notices(&h), [(id, Notify::Guests)]);
}

#[tokio::test]
async fn a_move_kept_from_the_guests_goes_out_with_updates_off() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(with_guest("primary", "review"));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let moved = Event { start: NOW + 3_600_000, end: NOW + 7_200_000, ..stored(&h, "primary", "review").await.unwrap() };
    held(copy.apply_with(h.account_id, vec![Step::Save(moved)], Notify::Nobody).await.unwrap());
    copy.send(h.account_id).await.unwrap();

    assert_eq!(notices(&h), [("review".to_string(), Notify::Nobody)]);
}

/// The choice rides the held change through a crash before its Undo
/// toast closes, and goes out with it at the next start.
#[tokio::test]
async fn a_quiet_delete_held_through_a_restart_stays_quiet() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![calendar("primary", true)]);
    h.fake.put_calendar_event(with_guest("primary", "review"));
    let first_run = copy(&h);
    first_run.refresh(h.account_id, NOW).await.unwrap();
    let remove = vec![Step::Remove { calendar: "primary".into(), id: "review".into() }];
    held(first_run.hold_with(h.account_id, remove, Notify::Nobody).await.unwrap());
    drop(first_run);

    let next_run = copy(&h);
    next_run.recover_holds().await.unwrap();
    next_run.send(h.account_id).await.unwrap();

    assert!(on_google(&h, "review").is_none());
    assert_eq!(notices(&h), [("review".to_string(), Notify::Nobody)]);
}
