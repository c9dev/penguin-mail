//! One event change through `CalendarCopy::change`, and the question
//! `CalendarCopy::ask_before` says comes first, against the in-memory
//! Gmail and Graph. The window and the assistant both go this way.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::calendar::series::RepeatScope;
use mailrs_domain::calendar::{Access, Calendar, Event, Guest, Notify, Occurrence};
use mailrs_store::calendar as store;

use super::{Connected, Harness, harness};
use crate::calendar_copy::event_change::{
    Action, Ask, Changed, Choice, Edit, EventChange, Facts, Question, Undo, adds_guests, asking, has_other_guests, question,
    unasked,
};
use crate::calendar_copy::{CalendarCopy, new_event_id};
use crate::settings::Permitted;

const NOW: i64 = 1_790_000_000_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;
const EVERY: [RepeatScope; 3] = [RepeatScope::This, RepeatScope::Following, RepeatScope::All];

fn copy(h: &Harness) -> CalendarCopy<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    CalendarCopy::new(Arc::new(Connected(connected)), h.db.clone())
}

fn primary() -> Calendar {
    Calendar {
        id: "primary".into(),
        name: "primary".into(),
        color: "#3584e4".into(),
        access: Access::Owner,
        zone: "UTC".into(),
        primary: true,
        shown: true,
        ..Calendar::default()
    }
}

fn event(id: &str) -> Event {
    Event {
        calendar: "primary".into(),
        id: id.into(),
        title: id.into(),
        zone: "UTC".into(),
        start: NOW,
        end: NOW + HOUR,
        busy: true,
        ..Event::default()
    }
}

fn ann() -> Guest {
    Guest { email: "ann@example.com".into(), ..Guest::default() }
}

fn me() -> Guest {
    Guest { email: "me@example.com".into(), me: true, organizer: true, ..Guest::default() }
}

fn meeting(id: &str) -> Event {
    Event { guests: vec![me(), ann()], ..event(id) }
}

/// Rita organizes it and this account is only a guest.
fn invitation(id: &str) -> Event {
    Event {
        guests: vec![
            Guest { email: "rita@example.com".into(), organizer: true, ..Guest::default() },
            Guest { email: "me@example.com".into(), me: true, ..Guest::default() },
        ],
        organizer: Some("rita@example.com".into()),
        ..event(id)
    }
}

fn standup() -> Event {
    Event { rules: vec!["RRULE:FREQ=DAILY;COUNT=5".into()], ..event("standup") }
}

/// The copy after its first read of a primary calendar holding `events`.
async fn read(h: &Harness, events: Vec<Event>) -> CalendarCopy<Connected> {
    h.fake.with(|s| s.calendars = vec![primary()]);
    for event in events {
        h.fake.put_calendar_event(event);
    }
    let copy = copy(h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    copy
}

async fn on_day(h: &Harness, day: i64) -> Vec<Occurrence> {
    let account = h.account_id;
    let from = NOW + day * DAY;
    h.db.read(move |c| store::occurrences(c, &[account], from, from + DAY, store::CalendarScope::Shown))
        .await
        .unwrap()
}

async fn queue(h: &Harness) -> Vec<store::QueuedChange> {
    let account = h.account_id;
    h.db.read(move |c| store::queued(c, account)).await.unwrap()
}

fn notices(h: &Harness) -> Vec<(String, Notify)> {
    h.fake.with(|s| s.calendar_notices.clone())
}

fn done<T>(answer: Permitted<T>) -> T {
    answer.done().expect("the account may change its calendar")
}

fn moved(o: &Occurrence, by: i64) -> EventChange {
    let edited = Event { start: o.start + by, end: o.end + by, ..Event::clone(&o.event) };
    EventChange::Edit { occurrence: o.clone(), edited, how: Edit { moves: true, ..Edit::default() } }
}

fn the_question(ask: Ask) -> Question {
    match ask {
        Ask::Question(question) => question,
        Ask::Settled(choice) => panic!("expected a question, got {choice:?}"),
    }
}

fn settled(ask: Ask) -> Choice {
    match ask {
        Ask::Settled(choice) => choice,
        Ask::Question(question) => panic!("expected nothing to ask, got {question:?}"),
    }
}

// ---- Through the copy, against the in-memory Gmail ----------------------

#[tokio::test]
async fn a_moved_occurrence_is_held_and_goes_out_once_committed() {
    let h = harness().await;
    let copy = read(&h, vec![standup()]).await;
    let tuesday = on_day(&h, 1).await.remove(0);
    let change = moved(&tuesday, HOUR);
    let asked = the_question(copy.ask_before(h.account_id, &change).unwrap());
    assert_eq!((asked.action, asked.scopes.as_slice()), (Action::Move, EVERY.as_slice()));

    let choice = Choice { scope: Some(RepeatScope::This), notify: Notify::Guests };
    let Changed::Held(held) = done(copy.change(h.account_id, change, choice, Undo::Offer).await.unwrap()) else {
        panic!("Undo was offered, so the change waits on it");
    };
    assert_eq!(on_day(&h, 1).await[0].start, NOW + DAY + HOUR, "the grid shows it at once");
    assert!(queue(&h).await.is_empty(), "nothing queues while Undo is up");
    assert!(copy.still_waiting(&held));

    copy.commit(held).await.unwrap();
    assert_eq!(queue(&h).await.len(), 1);
    copy.send(h.account_id).await.unwrap();
    let moved = h.fake.with(|s| s.calendar_events.iter().find(|e| e.series.as_deref() == Some("standup")).cloned());
    assert_eq!(moved.map(|e| e.start), Some(NOW + DAY + HOUR), "Google holds the one occurrence, moved");
}

#[tokio::test]
async fn undo_of_a_moved_occurrence_puts_it_back_and_queues_nothing() {
    let h = harness().await;
    let copy = read(&h, vec![standup()]).await;
    let tuesday = on_day(&h, 1).await.remove(0);
    let choice = Choice { scope: Some(RepeatScope::This), notify: Notify::Guests };
    let changed = done(copy.change(h.account_id, moved(&tuesday, HOUR), choice, Undo::Offer).await.unwrap());
    let Changed::Held(held) = changed else { panic!("held") };

    copy.revert(held).await.unwrap();
    assert_eq!(on_day(&h, 1).await[0].start, NOW + DAY);
    assert!(queue(&h).await.is_empty());
}

#[tokio::test]
async fn a_removal_without_undo_queues_at_once_with_the_persons_choice() {
    let h = harness().await;
    let copy = read(&h, vec![meeting("review")]).await;
    let opened = on_day(&h, 0).await.remove(0);
    let change = EventChange::Remove(opened);
    let asked = the_question(copy.ask_before(h.account_id, &change).unwrap());
    assert!(asked.ask_guests, "a meeting's delete asks whether to send a cancellation");

    let choice = Choice { scope: None, notify: Notify::Nobody };
    let changed = done(copy.change(h.account_id, change, choice, Undo::Skip).await.unwrap());
    assert!(matches!(changed, Changed::Queued(_)), "{changed:?}");
    assert!(on_day(&h, 0).await.is_empty());
    copy.send(h.account_id).await.unwrap();
    assert!(h.fake.with(|s| s.calendar_events.is_empty()));
    assert_eq!(notices(&h), [("review".to_string(), Notify::Nobody)]);
}

/// A guest changes only their own copy of someone else's event, so the
/// organizer and the other guests hear nothing, whatever the caller
/// passed.
#[tokio::test]
async fn an_edit_to_a_guests_own_event_goes_out_quiet() {
    let h = harness().await;
    let copy = read(&h, vec![invitation("review")]).await;
    let opened = on_day(&h, 0).await.remove(0);
    let edited = Event { color: Some("5".into()), ..Event::clone(&opened.event) };
    let change = EventChange::Edit { occurrence: opened, edited, how: Edit::default() };
    assert_eq!(settled(copy.ask_before(h.account_id, &change).unwrap()).notify, Notify::Nobody);

    let loud = Choice { scope: None, notify: Notify::Guests };
    done(copy.change(h.account_id, change, loud, Undo::Skip).await.unwrap());
    copy.send(h.account_id).await.unwrap();
    assert_eq!(notices(&h), [("review".to_string(), Notify::Nobody)]);
}

#[tokio::test]
async fn a_new_event_tells_its_guests() {
    let h = harness().await;
    let copy = read(&h, Vec::new()).await;
    let id = new_event_id();
    let change = EventChange::New(meeting(&id));
    let choice = settled(copy.ask_before(h.account_id, &change).unwrap());
    assert_eq!(choice, Choice { scope: None, notify: Notify::Guests });

    done(copy.change(h.account_id, change, choice, Undo::Skip).await.unwrap());
    copy.send(h.account_id).await.unwrap();
    assert_eq!(notices(&h), [(id, Notify::Guests)]);
}

#[tokio::test]
async fn a_withheld_calendar_permission_writes_nothing() {
    let h = harness().await;
    let copy = read(&h, vec![meeting("review")]).await;
    let opened = on_day(&h, 0).await.remove(0);
    h.fake.withhold(mailrs_gmail::CALENDAR_SCOPE);
    let choice = Choice { scope: None, notify: Notify::Guests };
    let answer = copy.change(h.account_id, EventChange::Remove(opened), choice, Undo::Offer).await.unwrap();
    assert!(matches!(answer, Permitted::NeedsPermission), "{answer:?}");
    assert_eq!(on_day(&h, 0).await.len(), 1);
}

// ---- A Microsoft account, against the in-memory Graph -------------------

mod outlook {
    use mailrs_graph::{Attendee, DateTimeZone, EmailAddress, GraphEvent};

    use super::*;
    use crate::tests::microsoft::{Outlook, outlook};

    const MONDAY: &str = "2026-10-05";

    fn at(time: &str) -> DateTimeZone {
        DateTimeZone { date_time: format!("{MONDAY}T{time}:00.0000000"), time_zone: "UTC".into() }
    }

    fn millis(text: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(text).unwrap().timestamp_millis()
    }

    async fn meeting_on_outlook() -> (Outlook, CalendarCopy<Connected>, Occurrence) {
        let h = outlook().await;
        let ana = Attendee {
            email_address: EmailAddress { address: Some("ana@example.com".into()), name: None },
            ..Attendee::default()
        };
        h.fake.put_event(
            "cal-1",
            GraphEvent {
                id: "g1".into(),
                subject: Some("Review".into()),
                start: Some(at("09:00")),
                end: Some(at("10:00")),
                attendees: vec![ana],
                ..GraphEvent::default()
            },
        );
        let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
        let copy = CalendarCopy::new(Arc::new(Connected(connected)), h.db.clone());
        copy.refresh(h.account_id, millis("2026-10-01T00:00:00Z")).await.unwrap();
        let (account, from) = (h.account_id, millis(&format!("{MONDAY}T00:00:00Z")));
        let opened = h
            .db
            .read(move |c| store::occurrences(c, &[account], from, from + DAY, store::CalendarScope::Shown))
            .await
            .unwrap()
            .remove(0);
        (h, copy, opened)
    }

    #[tokio::test]
    async fn a_microsoft_account_says_the_guests_get_mail_instead_of_asking() {
        let (h, copy, opened) = meeting_on_outlook().await;
        let asked = the_question(copy.ask_before(h.account_id, &moved(&opened, HOUR)).unwrap());
        assert!(asked.mailed && !asked.ask_guests, "{asked:?}");
    }

    /// Graph mails the guests of every change an organizer makes, so the
    /// queue says so whatever the caller passed.
    #[tokio::test]
    async fn a_microsoft_account_queues_every_change_as_telling_the_guests() {
        let (h, copy, opened) = meeting_on_outlook().await;
        let quiet = Choice { scope: None, notify: Notify::Nobody };
        let changed = done(copy.change(h.account_id, moved(&opened, HOUR), quiet, Undo::Offer).await.unwrap());
        let Changed::Held(held) = changed else { panic!("held") };
        copy.commit(held).await.unwrap();
        let account = h.account_id;
        let queued = h.db.read(move |c| store::queued(c, account)).await.unwrap();
        assert_eq!(queued.iter().map(|q| q.notify).collect::<Vec<_>>(), [Notify::Guests]);
    }
}

// ---- The question rule, as data -----------------------------------------

fn seen() -> Facts {
    Facts { seen: true, ..Facts::default() }
}

/// An account that mails the guests whatever the person picks, as
/// Microsoft does.
fn mails() -> Facts {
    Facts { always_mails: true, ..Facts::default() }
}

#[test]
fn moving_an_event_without_guests_asks_only_to_confirm() {
    let q = question(Action::Move, &[], &[me()], Facts::default()).unwrap();
    assert!(!q.ask_guests && !q.mailed && !q.told && q.scopes.is_empty());
}

#[test]
fn moving_a_meeting_asks_about_the_guests() {
    assert!(question(Action::Move, &[], &[me(), ann()], Facts::default()).unwrap().ask_guests);
}

#[test]
fn answering_a_one_off_invitation_asks_nothing() {
    assert_eq!(question(Action::Answer, &[], &[ann()], Facts::default()), None);
}

#[test]
fn answering_a_repeating_invitation_asks_only_which_occurrences() {
    let q = question(Action::Answer, &[RepeatScope::This, RepeatScope::All], &[ann()], Facts::default()).unwrap();
    assert!(!q.ask_guests, "only the organizer hears an answer");
}

#[test]
fn an_edit_that_keeps_the_time_asks_nothing_of_a_one_off_meeting() {
    assert_eq!(question(Action::Edit, &[], &[ann()], Facts::default()), None);
}

#[test]
fn deleting_a_one_off_event_without_guests_asks_nothing() {
    assert_eq!(question(Action::Delete, &[], &[me()], Facts::default()), None);
}

#[test]
fn a_move_that_adds_guests_tells_them_without_a_choice() {
    let q = question(Action::Move, &[], &[ann()], Facts { adds_guests: true, ..Facts::default() }).unwrap();
    assert!(q.told && !q.ask_guests);
}

#[test]
fn the_account_alone_on_the_list_is_no_guest() {
    assert!(!has_other_guests(&[me()]));
    assert!(has_other_guests(&[me(), ann()]));
}

#[test]
fn a_guest_counts_as_added_whatever_the_case_of_the_address() {
    let bo = Guest { email: "bo@example.com".into(), ..Guest::default() };
    let loud = Guest { email: "ANN@example.com".into(), ..Guest::default() };
    assert!(!adds_guests(&[ann()], &[loud]));
    assert!(adds_guests(&[ann()], &[ann(), bo]));
}

#[test]
fn an_account_that_always_mails_offers_no_choice_about_the_guests() {
    for action in [Action::Move, Action::Delete, Action::Edit] {
        let q = question(action, &[], &[me(), ann()], Facts { seen: true, ..mails() }).unwrap();
        assert!(!q.ask_guests && q.mailed, "{action:?}");
    }
}

#[test]
fn an_unasked_edit_on_such_an_account_never_promises_nobody() {
    assert_eq!(unasked(Action::Edit, Facts::default()).notify, Notify::Nobody);
    assert_eq!(unasked(Action::Edit, mails()).notify, Notify::Guests);
}

#[test]
fn an_account_that_always_mails_with_no_other_guests_asks_nothing_extra() {
    assert_eq!(question(Action::Delete, &[], &[me()], mails()), None);
    assert!(!question(Action::Move, &[], &[me()], mails()).unwrap().mailed);
}

#[test]
fn an_edit_the_guests_see_asks_whether_to_send_an_update() {
    assert!(question(Action::Edit, &[], &[me(), ann()], seen()).unwrap().ask_guests);
    assert_eq!(question(Action::Edit, &[], &[me()], seen()), None, "nobody else is on it");
}

/// Reminders, colour and busy or free are the account's own.
#[test]
fn an_edit_only_the_account_sees_asks_nothing_and_mails_nobody() {
    assert_eq!(question(Action::Edit, &[], &[me(), ann()], Facts::default()), None);
    assert_eq!(unasked(Action::Edit, Facts::default()).notify, Notify::Nobody);
}

#[test]
fn an_edit_the_guests_see_that_nobody_is_asked_about_still_tells_them() {
    assert_eq!(unasked(Action::Edit, seen()).notify, Notify::Guests);
    assert_eq!(unasked(Action::Move, Facts::default()).notify, Notify::Guests);
}

#[test]
fn a_move_with_other_edits_keeps_the_old_time_on_offer() {
    let facts = Facts { seen: true, more_than_time: true, rest_seen: true, ..Facts::default() };
    let q = question(Action::Move, &[], &[me(), ann()], facts).unwrap();
    assert!(q.keeps && q.rest_seen && q.ask_guests);
}

// ---- What an event change asks, read from the change itself ------------

fn occurrence_of(event: Event) -> Occurrence {
    let (start, end) = (event.start, event.end);
    Occurrence { account_id: 1, event: Arc::new(event), start, end }
}

fn edit_of(o: &Occurrence, edited: Event, how: Edit) -> EventChange {
    EventChange::Edit { occurrence: o.clone(), edited, how }
}

#[test]
fn an_owner_is_offered_every_scope() {
    let o = occurrence_of(standup());
    let ask = asking(&edit_of(&o, Event::clone(&o.event), Edit { seen: true, ..Edit::default() }), false);
    assert_eq!(the_question(ask).scopes, EVERY);
}

#[test]
fn a_new_rule_cannot_cover_one_occurrence() {
    let o = occurrence_of(standup());
    let how = Edit { seen: true, rule_changed: true, ..Edit::default() };
    let ask = asking(&edit_of(&o, Event::clone(&o.event), how), false);
    assert_eq!(the_question(ask).scopes, [RepeatScope::Following, RepeatScope::All]);
}

#[test]
fn a_guest_is_never_offered_this_and_following() {
    let o = occurrence_of(Event { rules: standup().rules, ..invitation("standup") });
    let ask = asking(&edit_of(&o, Event::clone(&o.event), Edit::default()), false);
    assert_eq!(the_question(ask).scopes, [RepeatScope::This, RepeatScope::All]);
    let removal = the_question(asking(&EventChange::Remove(o), false));
    assert_eq!(removal.scopes, [RepeatScope::This, RepeatScope::All]);
    assert!(!removal.ask_guests, "a guest's removal never asks about the other guests");
}

#[test]
fn a_series_moving_to_another_calendar_moves_whole() {
    let o = occurrence_of(standup());
    let edited = Event { calendar: "team".into(), ..Event::clone(&o.event) };
    let ask = asking(&edit_of(&o, edited, Edit { seen: true, ..Edit::default() }), false);
    assert_eq!(the_question(ask).scopes, [RepeatScope::All]);
}

#[test]
fn a_new_event_asks_nothing() {
    assert_eq!(settled(asking(&EventChange::New(meeting("pmnew")), false)).notify, Notify::Guests);
}

/// The caller passes the list from before the edit when it had guests,
/// so a guest the edit removed still hears of it.
#[test]
fn a_meeting_that_loses_a_guest_still_asks() {
    let o = occurrence_of(meeting("review"));
    let edited = Event { guests: vec![me()], ..Event::clone(&o.event) };
    let ask = asking(&edit_of(&o, edited, Edit { seen: true, ..Edit::default() }), false);
    assert!(the_question(ask).ask_guests);
}

#[test]
fn an_edit_that_invites_someone_new_tells_everyone_without_asking() {
    let o = occurrence_of(meeting("review"));
    let bo = Guest { email: "bo@example.com".into(), ..Guest::default() };
    let edited = Event { guests: vec![me(), ann(), bo], ..Event::clone(&o.event) };
    let ask = asking(&edit_of(&o, edited.clone(), Edit { seen: true, ..Edit::default() }), false);
    assert_eq!(settled(ask).notify, Notify::Guests);
    let moving = Edit { moves: true, seen: true, ..Edit::default() };
    let q = the_question(asking(&edit_of(&o, edited, moving), false));
    assert!(q.told && !q.ask_guests, "a move still asks, and says who hears: {q:?}");
}

#[test]
fn a_drag_of_a_meeting_on_a_microsoft_account_says_the_guests_get_mail() {
    let o = occurrence_of(meeting("review"));
    let q = the_question(asking(&moved(&o, HOUR), true));
    assert!(q.mailed && !q.ask_guests);
}
