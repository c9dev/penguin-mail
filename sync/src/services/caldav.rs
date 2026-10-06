//! A calendar served over CalDAV, behind the calendar service the local
//! copy reads. Events come a page at a time from a `sync-collection`
//! report, or from a whole listing a year back without a token; a server
//! without the report is read whole whenever its ctag moves. A write GETs
//! the resource, changes the one VEVENT, and PUTs it back with If-Match,
//! so every line the app does not edit survives.
//!
//! Answering an invitation saves the new PARTSTAT. A server that
//! schedules (`calendar-auto-schedule`) then mails the organizer itself;
//! one that does not gets the iTIP REPLY from the account's mail adapter,
//! and the attendee line carries `SCHEDULE-AGENT=CLIENT` so a later
//! server upgrade does not send a second one. The adapter never opens the
//! store.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use mailrs_dav::ids::{path_of, resource_href, resource_id};
use mailrs_dav::{DavApi, DavError, Fetched, Kind, MULTIGET_BATCH, Precondition, ical};
use mailrs_domain::calendar::{self as model, Access, Notify, split_occurrence_id};
use mailrs_domain::invitation::{self, Answer, Invitation, Method, Occurrence, Scope, When};
use mailrs_domain::translate::gettext;
use mailrs_domain::{Address, EpochMillis};

use super::dav_read::{Reads, TOKEN_CTAG, TOKEN_SYNC, backend};
use super::{AnyMail, CalendarService, MailBackend};
use crate::BackendError;
use crate::invitations::mail;

/// Etags this adapter wrote, newest last, so a second change to one
/// resource follows the first rather than reading as changed elsewhere.
const WRITTEN_KEPT: usize = 256;

/// The color a calendar without one gets: GNOME's blue.
const DEFAULT_COLOR: &str = "#3584e4";

/// Rounds of `sync-collection` one read follows when the server cuts an
/// answer short. Twenty cover a calendar far past any person's.
const SYNC_ROUNDS: usize = 20;

pub struct CalDav<D> {
    api: Arc<D>,
    /// The account's mail, which carries the REPLY of a server that does
    /// not schedule.
    mail: AnyMail,
    me: Arc<Vec<String>>,
    reads: Arc<Mutex<Reads>>,
    written: Arc<Mutex<VecDeque<(String, String)>>>,
    refused: Arc<Mutex<Option<String>>>,
    /// Collections that answered no `sync-collection`, read by ctag.
    no_sync: Arc<Mutex<HashSet<String>>>,
    /// The calendar list last read, for the answer to an invitation.
    listed: Arc<Mutex<Vec<String>>>,
}

impl<D> Clone for CalDav<D> {
    fn clone(&self) -> Self {
        CalDav {
            api: Arc::clone(&self.api),
            mail: self.mail.clone(),
            me: Arc::clone(&self.me),
            reads: Arc::clone(&self.reads),
            written: Arc::clone(&self.written),
            refused: Arc::clone(&self.refused),
            no_sync: Arc::clone(&self.no_sync),
            listed: Arc::clone(&self.listed),
        }
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The key a range read waits under, apart from the calendar's own read,
/// so neither replaces the other.
fn range_key(calendar: &str) -> String {
    format!("{calendar}\0range")
}

/// The name a resource gets for an imported UID: letters, digits and
/// hyphens, so no server or path rule can object.
fn id_for_uid(uid: &str) -> String {
    uid.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn refused(err: DavError) -> BackendError {
    BackendError::Refused(err.to_string())
}

impl<D: DavApi> CalDav<D> {
    pub fn new(api: Arc<D>, mail: AnyMail, me: Vec<String>) -> CalDav<D> {
        CalDav {
            api,
            mail,
            me: Arc::new(me),
            reads: Arc::default(),
            written: Arc::default(),
            refused: Arc::default(),
            no_sync: Arc::default(),
            listed: Arc::default(),
        }
    }

    /// Why the server last refused the account's login, until it takes it.
    pub fn login_refused(&self) -> Option<String> {
        locked(&self.refused).clone()
    }

    fn err(&self, err: DavError) -> BackendError {
        backend(err, &self.refused)
    }

    fn wrote(&self, href: &str, etag: &str) -> bool {
        locked(&self.written).iter().any(|(h, e)| path_of(h) == path_of(href) && e == etag)
    }

    fn remember(&self, href: &str, etag: &str) {
        let mut written = locked(&self.written);
        written.push_back((href.to_string(), etag.to_string()));
        while written.len() > WRITTEN_KEPT {
            written.pop_front();
        }
    }

    /// The etag the server holds is the one the copy held, or one this
    /// adapter wrote since.
    fn current(&self, href: &str, held: Option<&str>, now: &str) -> Result<(), BackendError> {
        match held {
            Some(held) if !held.is_empty() && held != now && !self.wrote(href, now) => Err(BackendError::Changed),
            _ => Ok(()),
        }
    }

    /// What the first page of a read lists: hrefs to fetch, hrefs gone,
    /// and the token after.
    async fn listing(&self, calendar: &str, token: Option<&str>, from: EpochMillis) -> Result<(Vec<String>, Vec<String>, String), BackendError> {
        match token {
            Some(t) if t.starts_with(TOKEN_SYNC) => {
                let (mut changed, mut removed) = (Vec::new(), Vec::new());
                let mut at = t[TOKEN_SYNC.len()..].to_string();
                for _ in 0..SYNC_ROUNDS {
                    let synced = match self.api.sync(calendar, &at).await {
                        Err(DavError::NoSyncCollection) => {
                            locked(&self.no_sync).insert(calendar.to_string());
                            return Err(BackendError::StateLost);
                        }
                        other => other.map_err(|e| self.err(e))?,
                    };
                    changed.extend(synced.changed.into_iter().map(|m| m.href));
                    removed.extend(synced.removed);
                    at = synced.token;
                    if !synced.more {
                        break;
                    }
                }
                Ok((changed, removed, format!("{TOKEN_SYNC}{at}")))
            }
            Some(t) if t.starts_with(TOKEN_CTAG) => {
                let state = self.api.state(calendar).await.map_err(|e| self.err(e))?;
                match state.ctag.as_deref() == Some(&t[TOKEN_CTAG.len()..]) {
                    true => Ok((Vec::new(), Vec::new(), t.to_string())),
                    // Without the report nothing names what changed; the
                    // copy reads the calendar whole and sweeps.
                    false => Err(BackendError::StateLost),
                }
            }
            Some(_) => Err(BackendError::StateLost),
            None => {
                let state = self.api.state(calendar).await.map_err(|e| self.err(e))?;
                let members = self.api.members(calendar, Kind::Calendar, Some((from, i64::MAX))).await.map_err(|e| self.err(e))?;
                let reads_sync = !locked(&self.no_sync).contains(calendar);
                let token = match (state.sync_token, reads_sync) {
                    (Some(sync), true) => format!("{TOKEN_SYNC}{sync}"),
                    _ => format!("{TOKEN_CTAG}{}", state.ctag.unwrap_or_default()),
                };
                Ok((members.into_iter().map(|m| m.href).collect(), Vec::new(), token))
            }
        }
    }

    /// The next batch of the read waiting under `key`. `with_token` ends
    /// the last page with the read's sync token; a range read has none.
    async fn page(&self, key: &str, calendar: &str, serial: u64, with_token: bool) -> Result<model::EventPage, BackendError> {
        let taken = locked(&self.reads).take(key, serial, MULTIGET_BATCH).ok_or(BackendError::StateLost)?;
        let fetched = match taken.hrefs.is_empty() {
            true => Vec::new(),
            false => self.api.fetch(calendar, Kind::Calendar, &taken.hrefs).await.map_err(|e| self.err(e))?,
        };
        let mut page = model::EventPage::default();
        let found: HashSet<String> = fetched.iter().map(|f| path_of(&f.href)).collect();
        for resource in fetched {
            match ical::read_resource(&resource.body, calendar, &resource.href, &resource.etag, &self.me) {
                Ok(read) => {
                    page.whole_series.extend(read.series);
                    page.events.extend(read.events);
                }
                Err(err) => tracing::warn!(href = %resource.href, %err, "skipped a calendar resource Penguin Mail cannot read"),
            }
        }
        // A member listed and gone before the fetch has gone.
        page.removed.extend(taken.hrefs.iter().filter(|h| !found.contains(&path_of(h))).map(|h| resource_id(h)));
        page.removed.extend(taken.removed.iter().map(|h| resource_id(h)));
        match taken.last {
            true => page.next_sync = with_token.then_some(taken.token),
            false => page.next_page = Some(serial.to_string()),
        }
        Ok(page)
    }

    /// `id` as the event the copy keeps, read out of a resource just
    /// written.
    fn read_back(&self, calendar: &str, href: &str, etag: &str, body: &str, id: &str) -> Result<model::Event, BackendError> {
        let read = ical::read_resource(body, calendar, href, etag, &self.me).map_err(refused)?;
        read.events.into_iter().find(|e| e.id == id).ok_or(BackendError::NotFound)
    }

    /// The etag after a PUT, from the answer or, when the server did not
    /// say, from a GET.
    async fn etag_after(&self, href: &str, answered: Option<String>) -> Result<String, BackendError> {
        match answered {
            Some(etag) => Ok(etag),
            None => Ok(self.api.get(href).await.map_err(|e| self.err(e))?.etag),
        }
    }

    /// PUTs `body` at `href` and answers the etag it now has, remembered.
    async fn write(&self, href: &str, body: &str, when: Precondition) -> Result<String, BackendError> {
        let answered = self.api.put(href, body, Kind::Calendar, when).await.map_err(|e| self.err(e))?;
        let etag = self.etag_after(href, answered).await?;
        self.remember(href, &etag);
        Ok(etag)
    }

    /// For a change that tells nobody: the resource with its guests
    /// marked `SCHEDULE-AGENT=CLIENT`, saved first when it was not. A
    /// DELETE or a move would otherwise make the server mail a
    /// cancellation, and DavApi sends no `Schedule-Reply` header to stop
    /// it. Any other `notify` leaves the resource as it is.
    async fn silenced(&self, calendar: &str, current: Fetched, notify: Notify) -> Result<Fetched, BackendError> {
        if notify != Notify::Nobody {
            return Ok(current);
        }
        let Ok(read) = ical::read_resource(&current.body, calendar, &current.href, &current.etag, &self.me) else {
            return Ok(current);
        };
        let Some(master) = read.events.into_iter().find(|e| e.series.is_none()) else {
            return Ok(current);
        };
        let text = ical::write_event_notifying(Some(&current.body), &master, &self.me, crate::now_millis(), Notify::Nobody).map_err(refused)?;
        // The writer rewrites a file in its own layout, so only a new
        // SCHEDULE-AGENT line is worth a PUT.
        if text.matches("SCHEDULE-AGENT").count() <= current.body.matches("SCHEDULE-AGENT").count() {
            return Ok(current);
        }
        let etag = self.write(&current.href, &text, Precondition::Match(current.etag)).await?;
        Ok(Fetched { href: current.href, etag, body: text })
    }

    /// Marks the answer on the resource `found` and, on a server that
    /// does not schedule, mails the organizer. Answers the event the
    /// answer covers, or `None` when the account is not a guest of it.
    #[expect(clippy::too_many_arguments, reason = "each is part of the answer")]
    async fn answer_in(
        &self,
        calendar: &str,
        found: Fetched,
        id: &str,
        me: &str,
        answer: Answer,
        occurrence: Option<EpochMillis>,
        note: Option<&str>,
    ) -> Result<Option<model::Event>, BackendError> {
        let mut who = vec![me.to_string()];
        who.extend(self.me.iter().cloned());
        let schedules = self.api.auto_schedule().await.map_err(|e| self.err(e))?;
        let now = crate::now_millis();
        let written = match occurrence {
            Some(original) => ical::answer_occurrence(&found.body, &who, answer, original, now, schedules),
            None => ical::answer_scheduled(&found.body, &who, answer, now, schedules),
        };
        let Some(text) = written.map_err(refused)? else {
            return Ok(None);
        };
        let etag = self.write(&found.href, &text, Precondition::Match(found.etag)).await?;
        let read = ical::read_resource(&text, calendar, &found.href, &etag, &who).map_err(refused)?;
        let master = read.events.iter().find(|e| e.series.is_none()).cloned();
        if !schedules && let Some(master) = &master {
            self.tell_organizer(master, me, answer, occurrence, note, now).await?;
        }
        let event = read.events.into_iter().find(|e| e.id == id).or(master);
        Ok(event)
    }

    /// Mails the organizer the REPLY. An event with no organizer, or one
    /// the account organizes, has nobody to tell.
    async fn tell_organizer(&self, event: &model::Event, me: &str, answer: Answer, occurrence: Option<EpochMillis>, note: Option<&str>, now: EpochMillis) -> Result<(), BackendError> {
        let Some(organizer) = event.organizer.as_deref().map(str::trim).filter(|o| !o.is_empty() && !o.eq_ignore_ascii_case(me)) else {
            return Ok(());
        };
        let when = match event.all_day {
            true => {
                let day = |at: EpochMillis| DateTime::<Utc>::from_timestamp_millis(at).map(|d| d.date_naive());
                match (day(event.start), day(event.end - 1)) {
                    (Some(first), Some(last)) => Some(When::Days { first, last: last.max(first) }),
                    _ => None,
                }
            }
            false => Some(When::At { starts_at: event.start, ends_at: Some(event.end) }),
        };
        let occurrence = occurrence.map(|at| Occurrence {
            written: format!(":{}", DateTime::<Utc>::from_timestamp_millis(at).unwrap_or_default().format("%Y%m%dT%H%M%SZ")),
            at: Some(at),
        });
        let scope = match occurrence {
            Some(_) => Scope::Occurrence,
            None => Scope::Series,
        };
        let organizer = Address { name: None, email: organizer.to_string() };
        let invitation = Invitation {
            uid: event.uid.clone(),
            sequence: event.sequence,
            method: Method::Reply,
            summary: event.title.clone(),
            when,
            location: None,
            description: None,
            organizer: Some(organizer.clone()),
            guests: Vec::new(),
            repeats: None,
            occurrence,
            zone: None,
            rules: Vec::new(),
        };
        let me = Address { name: None, email: me.to_string() };
        let raw = mail::itip(
            &me,
            &organizer,
            &mail::reply_subject(answer, &invitation.summary),
            &mail::reply_prose(&me, answer, &invitation.summary),
            "REPLY",
            &invitation::reply_with_note(&invitation, &me, answer, scope, note, now),
            now,
        )
        .map_err(BackendError::Refused)?;
        self.mail.send(&raw, None).await?;
        Ok(())
    }
}

impl<D: DavApi> CalendarService for CalDav<D> {
    async fn calendars(&self) -> Result<Vec<model::Calendar>, BackendError> {
        let homes = self.api.homes().await.map_err(|e| self.err(e))?;
        let home = homes.calendar.ok_or(BackendError::Unsupported)?;
        let found = self.api.collections(&home, Kind::Calendar).await.map_err(|e| self.err(e))?;
        *locked(&self.refused) = None;
        let home_path = path_of(&home);
        let calendars: Vec<model::Calendar> = found
            .into_iter()
            .enumerate()
            .map(|(index, c)| model::Calendar {
                access: match (c.can_write, path_of(&c.href).starts_with(&home_path)) {
                    (true, true) => Access::Owner,
                    (true, false) => Access::Writer,
                    (false, _) => Access::Reader,
                },
                zone: c.timezone.as_deref().and_then(ical::calendar_zone).unwrap_or_default(),
                color: c.color.unwrap_or_else(|| DEFAULT_COLOR.to_string()),
                name: c.name,
                id: c.href,
                primary: index == 0,
                shown: true,
                hidden: false,
                reminders: Vec::new(),
            })
            .collect();
        *locked(&self.listed) = calendars.iter().map(|c| c.id.clone()).collect();
        Ok(calendars)
    }

    async fn event_changes(&self, calendar: &str, token: Option<&str>, page: Option<&str>, from: EpochMillis) -> Result<model::EventPage, BackendError> {
        if let Some(page) = page {
            let serial = page.parse().map_err(|_| BackendError::StateLost)?;
            return self.page(calendar, calendar, serial, true).await;
        }
        let (hrefs, removed, next) = self.listing(calendar, token, from).await?;
        let serial = locked(&self.reads).start(calendar, hrefs, removed, next);
        self.page(calendar, calendar, serial, true).await
    }

    async fn event_range(&self, calendar: &str, from: EpochMillis, to: EpochMillis, page: Option<&str>) -> Result<model::EventPage, BackendError> {
        let key = range_key(calendar);
        if let Some(page) = page {
            let serial = page.parse().map_err(|_| BackendError::StateLost)?;
            return self.page(&key, calendar, serial, false).await;
        }
        let members = self.api.members(calendar, Kind::Calendar, Some((from, to))).await.map_err(|e| self.err(e))?;
        let serial = locked(&self.reads).start(&key, members.into_iter().map(|m| m.href).collect(), Vec::new(), String::new());
        self.page(&key, calendar, serial, false).await
    }

    async fn put_event(&self, event: &model::Event, etag: Option<&str>, create: bool, notify: Notify) -> Result<model::Event, BackendError> {
        let owner = event.series.as_deref().unwrap_or(&event.id);
        let href = resource_href(&event.calendar, owner);
        let now = crate::now_millis();
        if create && event.series.is_none() {
            let body = ical::write_event_notifying(None, event, &self.me, now, notify).map_err(refused)?;
            let etag = self.write(&href, &body, Precondition::NoneMatch).await?;
            return self.read_back(&event.calendar, &href, &etag, &body, &event.id);
        }
        let current = self.api.get(&href).await.map_err(|e| self.err(e))?;
        self.current(&href, etag, &current.etag)?;
        let body = ical::write_event_notifying(Some(&current.body), event, &self.me, now, notify).map_err(refused)?;
        let etag = self.write(&href, &body, Precondition::Match(current.etag)).await?;
        self.read_back(&event.calendar, &href, &etag, &body, &event.id)
    }

    async fn remove_event(&self, calendar: &str, id: &str, etag: Option<&str>, notify: Notify) -> Result<(), BackendError> {
        let now = crate::now_millis();
        if let Some((series, original)) = split_occurrence_id(id) {
            let href = resource_href(calendar, series);
            let current = self.api.get(&href).await.map_err(|e| self.err(e))?;
            self.current(&href, etag, &current.etag)?;
            return match ical::cancel_occurrence(&current.body, original, now).map_err(refused)? {
                Some(text) => {
                    self.write(&href, &text, Precondition::Match(current.etag)).await?;
                    Ok(())
                }
                None => self.api.delete(&href, Some(&current.etag)).await.map_err(|e| self.err(e)),
            };
        }
        let href = resource_href(calendar, id);
        let current = self.api.get(&href).await.map_err(|e| self.err(e))?;
        self.current(&href, etag, &current.etag)?;
        let current = self.silenced(calendar, current, notify).await?;
        self.api.delete(&href, Some(&current.etag)).await.map_err(|e| self.err(e))
    }

    async fn import_event(&self, event: &model::Event) -> Result<model::Event, BackendError> {
        if event.uid.trim().is_empty() {
            return Err(BackendError::Refused(gettext("An event needs an ID to be imported.")));
        }
        let now = crate::now_millis();
        match self.api.find_uid(&event.calendar, &event.uid).await.map_err(|e| self.err(e))? {
            Some(found) => {
                let id = resource_id(&found.href);
                let body = ical::write_event_notifying(Some(&found.body), &model::Event { id: id.clone(), ..event.clone() }, &self.me, now, Notify::Nobody).map_err(refused)?;
                let etag = self.write(&found.href, &body, Precondition::Match(found.etag)).await?;
                self.read_back(&event.calendar, &found.href, &etag, &body, &id)
            }
            None => {
                let id = if event.id.is_empty() { id_for_uid(&event.uid) } else { event.id.clone() };
                let href = resource_href(&event.calendar, &id);
                let body = ical::write_event_notifying(None, &model::Event { id: id.clone(), ..event.clone() }, &self.me, now, Notify::Nobody).map_err(refused)?;
                let etag = self.write(&href, &body, Precondition::NoneMatch).await?;
                self.read_back(&event.calendar, &href, &etag, &body, &id)
            }
        }
    }

    async fn upload_attachment(&self, _file: &model::Attachment, _sent: Arc<std::sync::atomic::AtomicU64>) -> Result<model::Attachment, BackendError> {
        Err(BackendError::Refused(gettext("A CalDAV calendar cannot store attached files.")))
    }

    async fn share_file(&self, _file_id: &str, _email: &str) -> Result<(), BackendError> {
        Err(BackendError::Refused(gettext("A CalDAV calendar has no files to share.")))
    }

    async fn move_event(&self, event: &model::Event, destination: &str, notify: Notify) -> Result<model::Event, BackendError> {
        let owner = event.series.as_deref().unwrap_or(&event.id);
        let from = resource_href(&event.calendar, owner);
        let current = self.api.get(&from).await.map_err(|e| self.err(e))?;
        self.current(&from, Some(&event.etag), &current.etag)?;
        let current = self.silenced(&event.calendar, current, notify).await?;
        let to = resource_href(destination, owner);
        let etag = self.write(&to, &current.body, Precondition::NoneMatch).await?;
        if let Err(err) = self.api.delete(&from, Some(&current.etag)).await {
            // Half a move would leave the event on both calendars and
            // every retry refused, so the copy at the destination goes.
            if let Err(undo) = self.api.delete(&to, Some(&etag)).await {
                tracing::warn!(%undo, "could not undo half a move between calendars");
            }
            return Err(self.err(err));
        }
        self.read_back(destination, &to, &etag, &current.body, owner)
    }

    async fn answer_event(&self, calendar: &str, id: &str, me: &str, answer: Answer, note: Option<&str>) -> Result<model::Event, BackendError> {
        let (owner, occurrence) = match split_occurrence_id(id) {
            Some((series, original)) => (series, Some(original)),
            None => (id, None),
        };
        let found = self.api.get(&resource_href(calendar, owner)).await.map_err(|e| self.err(e))?;
        self.answer_in(calendar, found, id, me, answer, occurrence, note)
            .await?
            .ok_or_else(|| BackendError::Refused(gettext("You are not a guest of this event.")))
    }

    async fn edit_list(&self, _calendar: &str, _edit: &model::list::ListEdit) -> Result<Option<model::Calendar>, BackendError> {
        Err(BackendError::Refused(gettext("Penguin Mail cannot change the calendar list of a CalDAV server.")))
    }
}
