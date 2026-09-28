//! Files attached to events: the upload the editor starts, and one the
//! queue makes for a file attached while offline, against the in-memory
//! Gmail.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use mailrs_domain::calendar::{Access, Attachment, Calendar, Event, Guest, UploadProblem};
use mailrs_gmail::{DRIVE_FILE_SCOPE, GmailError};
use mailrs_store::calendar as store;

use super::{Connected, Harness, harness};
use crate::calendar_copy::CalendarCopy;
use crate::settings::Permitted;

const NOW: i64 = 1_790_000_000_000;

fn copy(h: &Harness) -> CalendarCopy<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    CalendarCopy::new(Arc::new(Connected(connected)), h.db.clone())
}

fn primary() -> Calendar {
    Calendar {
        id: "primary".into(),
        name: "Me".into(),
        color: "#3584e4".into(),
        access: Access::Owner,
        zone: "UTC".into(),
        primary: true,
        shown: true,
        hidden: false,
        reminders: Vec::new(),
    }
}

/// A file on this computer, waiting to upload.
fn waiting(path: &Path) -> Attachment {
    Attachment {
        title: path.file_name().unwrap().to_string_lossy().into_owned(),
        mime_type: "text/plain".into(),
        waiting: Some(path.display().to_string()),
        share: Some(true),
        ..Attachment::default()
    }
}

fn guest(email: &str, me: bool) -> Guest {
    Guest { email: email.into(), me, ..Guest::default() }
}

fn review(files: Vec<Attachment>) -> Event {
    Event {
        calendar: "primary".into(),
        id: "pmreview0123".into(),
        title: "Review".into(),
        zone: "UTC".into(),
        start: NOW,
        end: NOW + 3_600_000,
        busy: true,
        attachments: Some(files),
        ..Event::default()
    }
}

fn file(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

async fn stored(h: &Harness, id: &str) -> Option<Event> {
    let (account, id) = (h.account_id, id.to_string());
    h.db.read(move |c| store::event(c, account, "primary", &id)).await.unwrap()
}

async fn queued(h: &Harness) -> usize {
    let account = h.account_id;
    h.db.read(move |c| store::queued(c, account)).await.unwrap().len()
}

fn on_google(h: &Harness, id: &str) -> Option<Event> {
    h.fake.with(|s| s.calendar_events.iter().find(|e| e.id == id).cloned())
}

#[tokio::test]
async fn the_editor_s_upload_answers_the_drive_file() {
    let h = harness().await;
    let dir = tempfile::tempdir().unwrap();
    let path = file(dir.path(), "Agenda.txt", "first item");
    let sent = Arc::new(AtomicU64::new(0));
    let done = copy(&h).upload(h.account_id, waiting(&path), Arc::clone(&sent)).await.unwrap();
    let Permitted::Done(uploaded) = done else { panic!("the upload needs no permission here") };
    assert_eq!(uploaded.title, "Agenda.txt");
    assert!(uploaded.link().is_some(), "{uploaded:?}");
    assert_eq!(uploaded.waiting, None);
    assert_eq!(uploaded.share, Some(true), "a file the app uploads is shared with the guests unless unticked");
    assert_eq!(h.fake.with(|s| s.drive_files.clone()), vec![("Agenda.txt".to_string(), b"first item".to_vec())]);
}

#[tokio::test]
async fn an_account_without_drive_access_is_asked_for_it_and_uploads_nothing() {
    let h = harness().await;
    h.fake.withhold(DRIVE_FILE_SCOPE);
    let dir = tempfile::tempdir().unwrap();
    let path = file(dir.path(), "Agenda.txt", "first item");
    let done = copy(&h).upload(h.account_id, waiting(&path), Arc::default()).await.unwrap();
    assert!(matches!(done, Permitted::NeedsPermission));
    assert!(h.fake.with(|s| s.drive_files.is_empty()));
}

#[tokio::test]
async fn a_file_attached_offline_uploads_when_the_app_is_back_online() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![primary()]);
    let dir = tempfile::tempdir().unwrap();
    let path = file(dir.path(), "Notes.txt", "bring the slides");
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    h.fake.with(|s| s.offline = true);
    copy.save(h.account_id, review(vec![waiting(&path)])).await.unwrap();
    assert!(copy.send(h.account_id).await.is_err(), "offline, the change waits");
    assert_eq!(queued(&h).await, 1);
    assert!(h.fake.with(|s| s.drive_files.is_empty()));

    h.fake.with(|s| s.offline = false);
    let turned_down = copy.send(h.account_id).await.unwrap();
    assert!(turned_down.is_empty(), "{turned_down:?}");
    assert_eq!(queued(&h).await, 0);
    assert_eq!(h.fake.with(|s| s.drive_files.clone()), vec![("Notes.txt".to_string(), b"bring the slides".to_vec())]);
    let sent = on_google(&h, "pmreview0123").unwrap().attachments.unwrap();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].link().is_some() && sent[0].waiting.is_none(), "{sent:?}");
    let kept = stored(&h, "pmreview0123").await.unwrap().attachments.unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!((kept[0].file_id.as_str(), kept[0].link()), (sent[0].file_id.as_str(), sent[0].link()));
    assert_eq!(kept[0].waiting, None, "the copy holds the uploaded file, not the path");
    assert_eq!(kept[0].share, Some(true), "and remembers it is shared with the guests");
}

#[tokio::test]
async fn a_file_moved_before_its_upload_goes_out_without_it_and_says_so() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![primary()]);
    let dir = tempfile::tempdir().unwrap();
    let kept = file(dir.path(), "Agenda.txt", "first item");
    let moved = file(dir.path(), "Notes.txt", "bring the slides");
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    copy.save(h.account_id, review(vec![waiting(&kept), waiting(&moved)])).await.unwrap();
    std::fs::remove_file(&moved).unwrap();

    let turned_down = copy.send(h.account_id).await.unwrap();
    assert_eq!(turned_down.len(), 1, "{turned_down:?}");
    assert_eq!(turned_down[0].left_out.as_deref(), Some("Notes.txt"));
    assert_eq!(turned_down[0].title, "Review");
    assert_eq!(queued(&h).await, 0);
    let sent = on_google(&h, "pmreview0123").unwrap().attachments.unwrap();
    assert_eq!(sent.iter().map(|a| a.title.as_str()).collect::<Vec<_>>(), ["Agenda.txt"]);
    let stored = stored(&h, "pmreview0123").await.unwrap().attachments.unwrap();
    assert_eq!(stored.len(), 2, "the moved file stays on the event here, marked");
    assert_eq!(stored[0].file_id, sent[0].file_id);
    assert_eq!(stored[1].title, "Notes.txt");
    assert_eq!(stored[1].problem, Some(UploadProblem::NotFound));
    // Another send leaves it be: it waits for the person to take it off.
    copy.send(h.account_id).await.unwrap();
    assert_eq!(h.fake.with(|s| s.drive_files.len()), 1);
}

#[tokio::test]
async fn a_file_uploaded_before_the_write_failed_does_not_upload_again() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![primary()]);
    let dir = tempfile::tempdir().unwrap();
    let path = file(dir.path(), "Notes.txt", "bring the slides");
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    copy.save(h.account_id, review(vec![waiting(&path)])).await.unwrap();
    h.fake.fail_call("calendar.events.insert", 0, GmailError::Network("dropped".into()));
    assert!(copy.send(h.account_id).await.is_err());
    // The file is on Drive now, and the queued change links to it.
    std::fs::remove_file(&path).unwrap();
    let turned_down = copy.send(h.account_id).await.unwrap();
    assert!(turned_down.is_empty(), "{turned_down:?}");
    assert_eq!(h.fake.with(|s| s.drive_files.len()), 1);
    assert_eq!(on_google(&h, "pmreview0123").unwrap().attachments.unwrap().len(), 1);
}

#[tokio::test]
async fn removing_an_attachment_takes_it_off_the_event_and_leaves_drive_alone() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![primary()]);
    let dir = tempfile::tempdir().unwrap();
    let path = file(dir.path(), "Notes.txt", "bring the slides");
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    copy.save(h.account_id, review(vec![waiting(&path)])).await.unwrap();
    copy.send(h.account_id).await.unwrap();
    let mut event = stored(&h, "pmreview0123").await.unwrap();
    event.attachments = Some(Vec::new());
    copy.save(h.account_id, event).await.unwrap();
    copy.send(h.account_id).await.unwrap();
    assert_eq!(on_google(&h, "pmreview0123").unwrap().attachments, Some(Vec::new()));
    assert_eq!(h.fake.with(|s| s.drive_files.len()), 1, "the Drive file stays");
}

#[tokio::test]
async fn a_change_to_an_event_whose_attachments_are_unknown_keeps_googles() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![primary()]);
    let drive = Attachment {
        title: "Budget".into(),
        file_url: "https://drive.google.com/file/d/2def/view".into(),
        mime_type: "application/vnd.google-apps.spreadsheet".into(),
        file_id: "2def".into(),
        ..Attachment::default()
    };
    h.fake.put_calendar_event(review(vec![drive.clone()]));
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    // A row from before the copy read attachments.
    let mut event = stored(&h, "pmreview0123").await.unwrap();
    event.attachments = None;
    event.title = "Review, moved".into();
    copy.save(h.account_id, event).await.unwrap();
    copy.send(h.account_id).await.unwrap();
    let on_google = on_google(&h, "pmreview0123").unwrap();
    assert_eq!(on_google.title, "Review, moved");
    assert_eq!(on_google.attachments, Some(vec![drive]));
}

/// Without Drive access the file waits on its own: the event's other
/// changes, and every other calendar write queued behind it, still reach
/// Google.
#[tokio::test]
async fn a_queued_edit_behind_a_refused_upload_still_reaches_google() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![primary()]);
    h.fake.put_calendar_event(Event { id: "standup".into(), title: "Standup".into(), ..review(Vec::new()) });
    h.fake.withhold(DRIVE_FILE_SCOPE);
    let dir = tempfile::tempdir().unwrap();
    let path = file(dir.path(), "Notes.txt", "bring the slides");
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    copy.save(h.account_id, review(vec![waiting(&path)])).await.unwrap();
    let mut standup = stored(&h, "standup").await.unwrap();
    standup.title = "Standup, moved".into();
    copy.save(h.account_id, standup).await.unwrap();

    let turned_down = copy.send(h.account_id).await.unwrap();
    assert!(turned_down.is_empty(), "{turned_down:?}");
    assert_eq!(queued(&h).await, 0, "nothing waits in the queue");
    assert_eq!(on_google(&h, "standup").unwrap().title, "Standup, moved");
    assert_eq!(on_google(&h, "pmreview0123").unwrap().attachments, Some(Vec::new()));
    let kept = stored(&h, "pmreview0123").await.unwrap().attachments.unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].problem, Some(UploadProblem::NeedsAccess));

    // Access granted: the next send uploads the file and links it.
    h.fake.grant(DRIVE_FILE_SCOPE);
    copy.send(h.account_id).await.unwrap();
    assert_eq!(h.fake.with(|s| s.drive_files.len()), 1);
    let linked = on_google(&h, "pmreview0123").unwrap().attachments.unwrap();
    assert_eq!(linked.len(), 1);
    assert!(linked[0].link().is_some());
    let kept = stored(&h, "pmreview0123").await.unwrap().attachments.unwrap();
    assert_eq!(kept[0].problem, None);
    assert_eq!(kept[0].waiting, None);
}

/// The guests become readers of an uploaded file before the event goes
/// out, and a guest added later gets it on the next save.
#[tokio::test]
async fn the_guests_can_open_an_uploaded_file() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![primary()]);
    let dir = tempfile::tempdir().unwrap();
    let path = file(dir.path(), "Notes.txt", "bring the slides");
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let meeting = Event {
        guests: vec![guest("me@example.com", true), guest("ana@example.com", false), guest("bo@example.com", false)],
        ..review(vec![waiting(&path)])
    };
    copy.save(h.account_id, meeting).await.unwrap();
    copy.send(h.account_id).await.unwrap();
    assert_eq!(
        h.fake.with(|s| s.drive_shares.clone()),
        vec![
            ("drive1".to_string(), "ana@example.com".to_string(), 0),
            ("drive1".to_string(), "bo@example.com".to_string(), 0),
        ],
        "each guest but the account, before any event write"
    );

    let mut later = stored(&h, "pmreview0123").await.unwrap();
    later.guests.push(guest("cy@example.com", false));
    copy.save(h.account_id, later).await.unwrap();
    copy.send(h.account_id).await.unwrap();
    let shares = h.fake.with(|s| s.drive_shares.clone());
    assert_eq!(shares.len(), 3, "{shares:?}");
    assert_eq!(shares[2], ("drive1".to_string(), "cy@example.com".to_string(), 1));
}

#[tokio::test]
async fn a_file_the_person_keeps_to_themselves_is_not_shared() {
    let h = harness().await;
    h.fake.with(|s| s.calendars = vec![primary()]);
    let dir = tempfile::tempdir().unwrap();
    let path = file(dir.path(), "Notes.txt", "bring the slides");
    let copy = copy(&h);
    copy.refresh(h.account_id, NOW).await.unwrap();
    let private = Attachment { share: Some(false), ..waiting(&path) };
    let meeting = Event { guests: vec![guest("ana@example.com", false)], ..review(vec![private]) };
    copy.save(h.account_id, meeting).await.unwrap();
    copy.send(h.account_id).await.unwrap();
    assert!(h.fake.with(|s| s.drive_shares.is_empty()));
    assert_eq!(on_google(&h, "pmreview0123").unwrap().attachments.unwrap().len(), 1);
}
