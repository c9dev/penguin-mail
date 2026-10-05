//! The CalDAV adapter against FakeDav: calendars, paged reads, tokens the
//! server lost, a server without sync-collection, writes that keep the
//! rest of a resource, and who tells the organizer about an answer.

use std::sync::Arc;

use mailrs_dav::fake::FakeDav;
use mailrs_dav::Kind;
use mailrs_domain::calendar::list::ListEdit;
use mailrs_domain::calendar::{Access, Attachment, Event, Notify, occurrence_id};
use mailrs_domain::invitation::Answer;
use mailrs_gmail::Answered;

use crate::fake::{FakeImap, FakeSmtp};
use crate::services::{CalDav, Imap};
use crate::tests::fake_settings;
use crate::{BackendError, CalendarService, MailBackend};

const WORK: &str = "/cal/work/";
const ME: &str = "me@fastmail.com";
const BOSS: &str = "boss@fastmail.com";

type Adapter = CalDav<FakeDav, Imap<FakeImap, FakeSmtp>>;

async fn adapter() -> (Arc<FakeDav>, Arc<FakeSmtp>, Adapter) {
    let dav = Arc::new(FakeDav::new());
    dav.add_collection(WORK, Kind::Calendar, "Work", Some("#3a87ad"));
    let smtp = Arc::new(FakeSmtp::default());
    let imap = Imap::new(Arc::new(FakeImap::new()), Arc::clone(&smtp), fake_settings());
    imap.mailboxes().await.unwrap();
    let caldav = CalDav::new(Arc::clone(&dav), imap, vec![ME.to_string()]);
    (dav, smtp, caldav)
}

/// A weekly series starting on 5 October 2026 at 09:30 UTC.
fn series_ics(uid: &str, title: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
         DTSTART:20261005T093000Z\r\nDTEND:20261005T100000Z\r\nRRULE:FREQ=WEEKLY\r\nSUMMARY:{title}\r\n\
         ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:{ME}\r\nX-KEEP-ME:yes\r\nSEQUENCE:0\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

/// The same series, organized by someone else who expects an answer.
fn invitation_ics(uid: &str, title: &str) -> String {
    series_ics(uid, title).replace("ATTENDEE;", &format!("ORGANIZER:mailto:{BOSS}\r\nATTENDEE;"))
}

fn one_off_ics(uid: &str, title: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
         DTSTART:20261006T120000Z\r\nDTEND:20261006T130000Z\r\nSUMMARY:{title}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

/// A one-off event of mine with a guest, as the server holds it.
fn meeting_ics(uid: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
         DTSTART:20261006T120000Z\r\nDTEND:20261006T130000Z\r\nSUMMARY:Review\r\nORGANIZER:mailto:{ME}\r\n\
         ATTENDEE;PARTSTAT=ACCEPTED:mailto:{ME}\r\nATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:ann@example.com\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

type Read = (Vec<Event>, Vec<String>, Vec<String>, Option<String>);

/// Every page of a read, from `token`.
async fn read_all(caldav: &Adapter, token: Option<&str>) -> Result<Read, BackendError> {
    let (mut events, mut removed, mut whole) = (Vec::new(), Vec::new(), Vec::new());
    let mut page = None;
    loop {
        let got = caldav.event_changes(WORK, token, page.as_deref(), 0).await?;
        events.extend(got.events);
        removed.extend(got.removed);
        whole.extend(got.whole_series);
        match got.next_page {
            Some(next) => page = Some(next),
            None => return Ok((events, removed, whole, got.next_sync)),
        }
    }
}

#[tokio::test]
async fn the_calendar_list_names_each_calendar_with_its_color_and_access() {
    let (dav, _, caldav) = adapter().await;
    dav.add_collection("/cal/shared/", Kind::Calendar, "Team", None);
    dav.with(|s| s.collections[1].can_write = false);
    let calendars = caldav.calendars().await.unwrap();
    assert_eq!(calendars[0].id, WORK);
    assert_eq!(calendars[0].name, "Work");
    assert_eq!(calendars[0].color, "#3a87ad");
    assert_eq!(calendars[0].access, Access::Owner);
    assert!(calendars[0].primary);
    assert_eq!(calendars[1].access, Access::Reader);
    assert!(!calendars[1].color.is_empty(), "a calendar without a color gets one");
}

#[tokio::test]
async fn a_whole_read_pages_by_fifty_and_ends_with_a_sync_token() {
    let (dav, _, caldav) = adapter().await;
    for n in 0..120 {
        dav.put_resource(&format!("{WORK}e{n}.ics"), &one_off_ics(&format!("u{n}"), &format!("Event {n}")));
    }
    let mut pages = 0;
    let mut page = None;
    loop {
        let got = caldav.event_changes(WORK, None, page.as_deref(), 0).await.unwrap();
        pages += 1;
        assert!(got.events.len() <= mailrs_dav::MULTIGET_BATCH);
        match got.next_page {
            Some(next) => page = Some(next),
            None => {
                assert!(got.next_sync.as_deref().is_some_and(|t| t.starts_with("sync:")));
                break;
            }
        }
    }
    assert_eq!(pages, 3);
}

#[tokio::test]
async fn a_later_read_brings_what_changed_and_what_went() {
    let (dav, _, caldav) = adapter().await;
    dav.put_resource(&format!("{WORK}a.ics"), &one_off_ics("a", "Lunch"));
    dav.put_resource(&format!("{WORK}b.ics"), &one_off_ics("b", "Dinner"));
    let (_, _, _, token) = read_all(&caldav, None).await.unwrap();
    dav.put_resource(&format!("{WORK}a.ics"), &one_off_ics("a", "Lunch moved"));
    dav.remove_resource(&format!("{WORK}b.ics"));
    let (events, removed, _, _) = read_all(&caldav, token.as_deref()).await.unwrap();
    assert_eq!(events.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(), ["Lunch moved"]);
    assert_eq!(removed, ["b"]);
}

#[tokio::test]
async fn a_refused_sync_token_reads_the_calendar_whole() {
    let (dav, _, caldav) = adapter().await;
    dav.put_resource(&format!("{WORK}a.ics"), &one_off_ics("a", "Lunch"));
    let (_, _, _, token) = read_all(&caldav, None).await.unwrap();
    dav.forget_tokens();
    assert!(matches!(read_all(&caldav, token.as_deref()).await, Err(BackendError::StateLost)));
}

#[tokio::test]
async fn a_server_without_sync_collection_reads_whole_when_its_ctag_moves() {
    let (dav, _, caldav) = adapter().await;
    dav.set_sync(false);
    dav.put_resource(&format!("{WORK}a.ics"), &one_off_ics("a", "Lunch"));
    let (_, _, _, token) = read_all(&caldav, None).await.unwrap();
    assert!(token.as_deref().is_some_and(|t| t.starts_with("ctag:")));
    let (events, _, _, again) = read_all(&caldav, token.as_deref()).await.unwrap();
    assert!(events.is_empty(), "an unchanged ctag reads nothing");
    assert_eq!(again, token);
    dav.put_resource(&format!("{WORK}b.ics"), &one_off_ics("b", "Dinner"));
    assert!(matches!(read_all(&caldav, token.as_deref()).await, Err(BackendError::StateLost)));
}

#[tokio::test]
async fn a_series_reads_whole_so_the_copy_can_drop_what_went() {
    let (dav, _, caldav) = adapter().await;
    dav.put_resource(&format!("{WORK}standup.ics"), &series_ics("s", "Standup"));
    let (events, _, whole, _) = read_all(&caldav, None).await.unwrap();
    assert_eq!(whole, ["standup"]);
    assert_eq!(events[0].rules, ["RRULE:FREQ=WEEKLY"]);
}

#[tokio::test]
async fn a_range_read_pages_and_leaves_the_sync_token_alone() {
    let (dav, _, caldav) = adapter().await;
    for n in 0..60 {
        dav.put_resource(&format!("{WORK}e{n}.ics"), &one_off_ics(&format!("u{n}"), "Event"));
    }
    // 6 October 2026, 12:00 UTC, is inside; the range ends before 7 October.
    let (from, to) = (1_791_244_800_000, 1_791_331_200_000);
    let first = caldav.event_range(WORK, from, to, None).await.unwrap();
    assert_eq!(first.events.len(), 50);
    assert_eq!(first.next_sync, None);
    let second = caldav.event_range(WORK, from, to, first.next_page.as_deref()).await.unwrap();
    assert_eq!((second.events.len(), second.next_page), (10, None));
    let outside = caldav.event_range(WORK, to, to + 86_400_000, None).await.unwrap();
    assert!(outside.events.is_empty());
}

#[tokio::test]
async fn an_edit_keeps_the_rest_of_the_resource() {
    let (dav, _, caldav) = adapter().await;
    dav.put_resource(&format!("{WORK}standup.ics"), &series_ics("s", "Standup"));
    let (events, _, _, _) = read_all(&caldav, None).await.unwrap();
    let mut edited = events[0].clone();
    edited.title = "Standup (short)".into();
    let sent = caldav.put_event(&edited, Some(&edited.etag), false, Notify::Nobody).await.unwrap();
    assert_eq!(sent.title, "Standup (short)");
    assert_ne!(sent.etag, edited.etag);
    let body = dav.body(&format!("{WORK}standup.ics")).unwrap();
    assert!(body.contains("X-KEEP-ME:yes"), "{body}");
}

#[tokio::test]
async fn a_second_change_to_one_resource_follows_the_first() {
    let (dav, _, caldav) = adapter().await;
    dav.put_resource(&format!("{WORK}standup.ics"), &series_ics("s", "Standup"));
    let (events, _, _, _) = read_all(&caldav, None).await.unwrap();
    let master = events[0].clone();
    // The queue sends a change to one occurrence, then one to the series,
    // both carrying the etag the copy read before either went out.
    let original = master.start + 7 * 24 * 3_600_000;
    let moved = Event {
        id: occurrence_id(&master, original),
        series: Some(master.id.clone()),
        original_start: Some(original),
        start: original + 3_600_000,
        end: original + 3_600_000 + 1_800_000,
        rules: Vec::new(),
        ..master.clone()
    };
    caldav.put_event(&moved, Some(&master.etag), false, Notify::Nobody).await.unwrap();
    let renamed = Event { title: "Standup (new name)".into(), ..master.clone() };
    caldav
        .put_event(&renamed, Some(&master.etag), false, Notify::Nobody)
        .await
        .expect("the adapter knows it wrote the newer etag");
    let body = dav.body(&format!("{WORK}standup.ics")).unwrap();
    assert!(body.contains("RECURRENCE-ID") && body.contains("Standup (new name)"), "{body}");
}

#[tokio::test]
async fn a_change_made_elsewhere_first_is_changed() {
    let (dav, _, caldav) = adapter().await;
    dav.put_resource(&format!("{WORK}a.ics"), &one_off_ics("a", "Lunch"));
    let (events, _, _, _) = read_all(&caldav, None).await.unwrap();
    dav.put_resource(&format!("{WORK}a.ics"), &one_off_ics("a", "Lunch, theirs"));
    let mine = Event { title: "Lunch, mine".into(), ..events[0].clone() };
    assert!(matches!(caldav.put_event(&mine, Some(&mine.etag), false, Notify::Nobody).await, Err(BackendError::Changed)));
}

#[tokio::test]
async fn a_new_event_is_created_under_its_own_id() {
    let (dav, _, caldav) = adapter().await;
    let event = Event {
        calendar: WORK.into(),
        id: "pm0123456789abcdefghijklmnopqrstuv".into(),
        start: 1_791_300_000_000,
        end: 1_791_303_600_000,
        zone: "UTC".into(),
        title: "Coffee".into(),
        busy: true,
        ..Event::default()
    };
    let sent = caldav.put_event(&event, None, true, Notify::Nobody).await.unwrap();
    assert_eq!(sent.id, event.id);
    assert!(dav.body(&format!("{WORK}{}.ics", event.id)).is_some());
    assert!(
        matches!(caldav.put_event(&event, None, true, Notify::Nobody).await, Err(BackendError::Changed)),
        "a second create finds it there"
    );
}

#[tokio::test]
async fn telling_nobody_keeps_the_server_from_mailing_the_guests() {
    let (dav, _, caldav) = adapter().await;
    dav.put_resource(&format!("{WORK}review.ics"), &meeting_ics("review"));
    let (events, _, _, _) = read_all(&caldav, None).await.unwrap();
    let renamed = Event { title: "Review, moved".into(), ..events[0].clone() };
    caldav.put_event(&renamed, Some(&renamed.etag), false, Notify::Nobody).await.unwrap();
    assert!(dav.body(&format!("{WORK}review.ics")).unwrap().contains("SCHEDULE-AGENT=CLIENT"));
    dav.put_resource(&format!("{WORK}review.ics"), &meeting_ics("review"));
    let (events, _, _, _) = read_all(&caldav, None).await.unwrap();
    let renamed = Event { title: "Review, moved".into(), ..events[0].clone() };
    caldav.put_event(&renamed, Some(&renamed.etag), false, Notify::Guests).await.unwrap();
    assert!(!dav.body(&format!("{WORK}review.ics")).unwrap().contains("SCHEDULE-AGENT"), "the server tells the guests");
}

#[tokio::test]
async fn cancelling_one_occurrence_writes_an_exdate_and_deleting_the_series_removes_it() {
    let (dav, _, caldav) = adapter().await;
    dav.put_resource(&format!("{WORK}standup.ics"), &series_ics("s", "Standup"));
    let (events, _, _, _) = read_all(&caldav, None).await.unwrap();
    let master = &events[0];
    let one = occurrence_id(master, master.start + 7 * 24 * 3_600_000);
    caldav.remove_event(WORK, &one, Some(&master.etag), Notify::Nobody).await.unwrap();
    assert!(dav.body(&format!("{WORK}standup.ics")).unwrap().contains("EXDATE:20261012T093000Z"));
    caldav.remove_event(WORK, &master.id, None, Notify::Nobody).await.unwrap();
    assert!(dav.body(&format!("{WORK}standup.ics")).is_none());
}

#[tokio::test]
async fn deleting_with_nobody_told_silences_the_guests_first() {
    let (dav, _, caldav) = adapter().await;
    dav.put_resource(&format!("{WORK}review.ics"), &meeting_ics("review"));
    caldav.remove_event(WORK, "review", None, Notify::Nobody).await.unwrap();
    assert_eq!(dav.puts(), 1, "the guests are marked before the delete");
    assert!(dav.body(&format!("{WORK}review.ics")).is_none());
    dav.put_resource(&format!("{WORK}review.ics"), &meeting_ics("review"));
    caldav.remove_event(WORK, "review", None, Notify::Guests).await.unwrap();
    assert_eq!(dav.puts(), 1, "the server mails the cancellation");
}

#[tokio::test]
async fn an_import_updates_the_event_a_uid_already_names() {
    let (dav, _, caldav) = adapter().await;
    let event = Event {
        calendar: WORK.into(),
        uid: "from-a-file@example.com".into(),
        start: 1_791_300_000_000,
        end: 1_791_303_600_000,
        zone: "UTC".into(),
        title: "Offsite".into(),
        busy: true,
        ..Event::default()
    };
    let first = caldav.import_event(&event).await.unwrap();
    assert!(dav.body(&format!("{WORK}{}.ics", first.id)).unwrap().contains("UID:from-a-file@example.com"));
    let again = caldav.import_event(&Event { title: "Offsite, renamed".into(), ..event }).await.unwrap();
    assert_eq!(again.id, first.id, "the same UID is the same event");
    assert_eq!(again.title, "Offsite, renamed");
    assert_eq!(dav.with(|s| s.resources.len()), 1);
}

#[tokio::test]
async fn a_move_puts_the_event_on_the_other_calendar_and_takes_it_off_this_one() {
    let (dav, _, caldav) = adapter().await;
    dav.add_collection("/cal/home/", Kind::Calendar, "Home", None);
    dav.put_resource(&format!("{WORK}standup.ics"), &series_ics("s", "Standup"));
    let (events, _, _, _) = read_all(&caldav, None).await.unwrap();
    let moved = caldav.move_event(&events[0], "/cal/home/", Notify::Nobody).await.unwrap();
    assert_eq!((moved.calendar.as_str(), moved.id.as_str()), ("/cal/home/", "standup"));
    assert!(dav.body(&format!("{WORK}standup.ics")).is_none());
    assert!(dav.body("/cal/home/standup.ics").unwrap().contains("X-KEEP-ME:yes"));
}

#[tokio::test]
async fn an_answer_the_server_schedules_is_saved_and_not_mailed() {
    let (dav, smtp, caldav) = adapter().await;
    dav.set_auto_schedule(true);
    dav.put_resource(&format!("{WORK}standup.ics"), &invitation_ics("invite-uid", "Standup"));
    caldav.calendars().await.unwrap();
    let answered = caldav.answer_invitation("invite-uid", ME, Answer::Yes, None, None).await.unwrap();
    assert_eq!(answered, Answered::Done);
    let body = dav.body(&format!("{WORK}standup.ics")).unwrap();
    assert!(body.contains("PARTSTAT=ACCEPTED") && !body.contains("SCHEDULE-AGENT"), "{body}");
    assert!(smtp.sent().is_empty(), "the server mails the organizer");
}

#[tokio::test]
async fn an_answer_to_a_server_that_does_not_schedule_is_mailed_to_the_organizer() {
    let (dav, smtp, caldav) = adapter().await;
    dav.put_resource(&format!("{WORK}standup.ics"), &invitation_ics("invite-uid", "Standup"));
    caldav.calendars().await.unwrap();
    let answered = caldav.answer_invitation("invite-uid", ME, Answer::Yes, None, Some("See you there")).await.unwrap();
    assert_eq!(answered, Answered::Done);
    assert!(dav.body(&format!("{WORK}standup.ics")).unwrap().contains("PARTSTAT=ACCEPTED"));
    let sent = smtp.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, [BOSS]);
    assert!(String::from_utf8_lossy(&sent[0].raw).contains("Subject: Accepted: Standup"));
    let none = caldav.answer_invitation("no-such-uid", ME, Answer::Yes, None, None).await.unwrap();
    assert_eq!(none, Answered::NotOnCalendar);
}

#[tokio::test]
async fn an_answer_from_the_calendar_view_follows_the_same_rule() {
    let (dav, smtp, caldav) = adapter().await;
    dav.put_resource(&format!("{WORK}standup.ics"), &invitation_ics("invite-uid", "Standup"));
    let event = caldav.answer_event(WORK, "standup", ME, Answer::No, None).await.unwrap();
    assert_eq!(event.my_answer, Some(Answer::No));
    assert_eq!(smtp.sent().len(), 1);
    dav.set_auto_schedule(true);
    caldav.answer_event(WORK, "standup", ME, Answer::Yes, None).await.unwrap();
    assert_eq!(smtp.sent().len(), 1, "no second mail from a server that schedules");
}

#[tokio::test]
async fn an_event_without_an_organizer_has_nobody_to_tell() {
    let (dav, smtp, caldav) = adapter().await;
    dav.put_resource(&format!("{WORK}standup.ics"), &series_ics("s", "Standup"));
    caldav.answer_event(WORK, "standup", ME, Answer::Yes, None).await.unwrap();
    assert!(smtp.sent().is_empty());
}

#[tokio::test]
async fn writes_a_caldav_server_cannot_do_are_refused_not_held() {
    let (_, _, caldav) = adapter().await;
    let sent = Arc::default();
    assert!(matches!(caldav.upload_attachment(&Attachment::default(), sent).await, Err(BackendError::Refused(_))));
    assert!(matches!(caldav.share_file("file", "ann@example.com").await, Err(BackendError::Refused(_))));
    assert!(matches!(caldav.edit_list(WORK, &ListEdit::Delete).await, Err(BackendError::Refused(_))));
}

#[tokio::test]
async fn a_refused_login_waits_in_the_queue_and_says_why() {
    let (dav, _, caldav) = adapter().await;
    dav.refuse_login(true);
    assert!(matches!(caldav.calendars().await, Err(BackendError::NeedsReauth)));
    assert!(caldav.login_refused().is_some());
    dav.refuse_login(false);
    caldav.calendars().await.unwrap();
    assert_eq!(caldav.login_refused(), None);
}
