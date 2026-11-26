//! The calendar of a JMAP account whose session offers JMAP Calendars in
//! draft 32's shape, over JSCalendar. JMAP's change feed covers the whole
//! account while the calendar copy reads one calendar at a time, so every
//! calendar's token is the account's CalendarEvent state, and one
//! `/changes` answer serves each calendar that asks from that state. An
//! event that left a calendar is a removal there and an event in the
//! calendar it went to; the copy removes by calendar and id.
//!
//! The store keeps no JSCalendar object, so an event's etag carries what
//! a write needs to tell the person's change from another app's: the
//! object's `updated` stamp, then a short hash of each part of the event
//! as read (see [`Part`]). A save fetches the event as the server holds it
//! now and applies only the parts whose hash the person's copy no longer
//! matches. Where the server's part changed too, the server's version
//! stays and the save says so.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use mailrs_domain::EpochMillis;
use mailrs_domain::calendar::{self as model, Access, Notify, occurrence_id, split_occurrence_id};
use mailrs_domain::invitation::Answer;
use mailrs_domain::translate::gettext;
use mailrs_jmap::{EventWrite, JmapCalendar, JmapError, pointer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{Jmap, JmapApi, backend, jscal};
use crate::services::CalendarService;
use crate::{BackendError, now_millis};

/// The colour a calendar without one gets: GNOME's blue, as for CalDAV.
const DEFAULT_COLOR: &str = "#3584e4";
/// Events per query page, and changes per `/changes` call, so one page of
/// either holds at most this many event objects.
pub(crate) const PAGE: usize = 200;
/// Pages of `/changes` one read follows while the server has more. The
/// read then ends at the state it got to, and the next one goes on from
/// there, so a burst of any size moves the copy forward every time.
pub(crate) const ROUNDS: usize = 20;
/// Hex digits of each part's hash in an etag: 24 bits, so two different
/// values of one part match by chance once in 16 million.
const DIGITS: usize = 6;
const DAY: EpochMillis = 86_400_000;
/// Tries at one write. `ifInState` covers every event of the account, so
/// another app's write anywhere answers `stateMismatch`; each try reads
/// the event again first.
const TRIES: usize = 3;

/// One page of the account's event changes since a state: at most
/// [`PAGE`] changed events, the ones destroyed, and the state they lead to.
#[derive(Debug, Default)]
pub(crate) struct EventChanges {
    since: String,
    new_state: String,
    more: bool,
    objects: Vec<Value>,
    destroyed: Vec<String>,
}

/// Where a read stands between pages. A whole read keeps the next
/// position and the state its first page was read at, which becomes the
/// calendar's token. A read of changes keeps the state the last page
/// reached and how many pages it has read.
#[derive(Serialize, Deserialize)]
struct Cursor {
    #[serde(default)]
    position: u64,
    #[serde(default)]
    state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    changes_from: Option<String>,
    #[serde(default)]
    round: usize,
}

/// Year 1 and the end of year 9999, the span `chrono` and a server's date
/// parser both take, so an open-ended range still reads as one.
const EARLIEST: EpochMillis = -62_135_596_800_000;
const LATEST: EpochMillis = 253_402_300_799_000;

/// An instant as the event filter's `after` and `before` take it: a
/// LocalDateTime, read in the query's `timeZone`, which defaults to
/// Etc/UTC. Cyrus refuses the UTC form with `Z` as `invalidArguments`.
fn filter_time(at: EpochMillis) -> String {
    DateTime::<Utc>::from_timestamp_millis(at.clamp(EARLIEST, LATEST))
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string()
}

fn access_of(calendar: &JmapCalendar) -> Access {
    let rights = &calendar.my_rights;
    if rights.may_admin {
        Access::Owner
    } else if rights.may_write_all || rights.may_write_own {
        Access::Writer
    } else if rights.may_read_items {
        Access::Reader
    } else {
        Access::FreeBusy
    }
}

/// One thing the person can change on an event, as the fingerprint in its
/// etag covers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Title,
    Time,
    Repeat,
    Place,
    Link,
    Notes,
    Reminders,
    Guests,
    Answer,
    Color,
    Privacy,
    Busy,
    Status,
}

/// The parts in the order their hashes sit in an etag.
const PARTS: [Part; 13] = [
    Part::Title,
    Part::Time,
    Part::Repeat,
    Part::Place,
    Part::Link,
    Part::Notes,
    Part::Reminders,
    Part::Guests,
    Part::Answer,
    Part::Color,
    Part::Privacy,
    Part::Busy,
    Part::Status,
];

/// FNV-1a, folded to 24 bits. Its output never changes between builds,
/// unlike std's hasher, so an etag stored by one version reads in the next.
fn fnv(bytes: &[u8]) -> u32 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    ((hash ^ (hash >> 32)) & 0xff_ffff) as u32
}

impl Part {
    fn hash(self, event: &model::Event) -> u32 {
        let bytes = match self {
            // The editor trims the title and the place, and gives an
            // all-day event the zone UTC, so neither counts as a change.
            Part::Title => serde_json::to_vec(event.title.trim()),
            Part::Time => {
                serde_json::to_vec(&(event.start, event.end, event.all_day, timed_zone(event)))
            }
            Part::Repeat => serde_json::to_vec(&event.rules),
            Part::Place => serde_json::to_vec(event.place.trim()),
            Part::Link => serde_json::to_vec(&event.conference),
            Part::Notes => serde_json::to_vec(&event.description),
            Part::Reminders => serde_json::to_vec(&event.reminders),
            // Addresses only: the editor drops names, and other guests'
            // answers are theirs, so neither counts as the person's change.
            Part::Guests => {
                let mut addresses: Vec<String> = event
                    .guests
                    .iter()
                    .map(|g| g.email.to_ascii_lowercase())
                    .collect();
                addresses.sort_unstable();
                serde_json::to_vec(&addresses)
            }
            Part::Answer => serde_json::to_vec(&event.my_answer),
            Part::Color => serde_json::to_vec(&event.color),
            Part::Privacy => serde_json::to_vec(&event.private),
            Part::Busy => serde_json::to_vec(&event.busy),
            Part::Status => serde_json::to_vec(&event.status),
        };
        fnv(&bytes.unwrap_or_default())
    }

    /// Copies this part of `from` onto `into`.
    fn take(self, from: &model::Event, into: &mut model::Event) {
        match self {
            Part::Title => into.title.clone_from(&from.title),
            Part::Time => {
                (into.start, into.end, into.all_day) = (from.start, from.end, from.all_day);
                into.zone.clone_from(&from.zone);
            }
            Part::Repeat => into.rules.clone_from(&from.rules),
            Part::Place => into.place.clone_from(&from.place),
            Part::Link => into.conference.clone_from(&from.conference),
            Part::Notes => into.description.clone_from(&from.description),
            Part::Reminders => into.reminders.clone_from(&from.reminders),
            // The addresses only: a guest on both sides keeps the record
            // `into` holds, so a name another app changed stays.
            Part::Guests => {
                into.guests = from
                    .guests
                    .iter()
                    .map(|guest| {
                        into.guests
                            .iter()
                            .find(|g| g.email.eq_ignore_ascii_case(&guest.email))
                            .unwrap_or(guest)
                            .clone()
                    })
                    .collect();
            }
            Part::Answer => into.my_answer = from.my_answer,
            Part::Color => into.color.clone_from(&from.color),
            Part::Privacy => into.private = from.private,
            Part::Busy => into.busy = from.busy,
            Part::Status => into.status = from.status,
        }
    }

    /// The part in plain words, for "Someone else changed {field}".
    fn words(self) -> String {
        match self {
            Part::Title => gettext("the title"),
            Part::Time => gettext("the time"),
            Part::Repeat => gettext("the repeat"),
            Part::Place => gettext("the place"),
            Part::Link => gettext("the meeting link"),
            Part::Notes => gettext("the notes"),
            Part::Reminders => gettext("the reminders"),
            Part::Guests => gettext("the guests"),
            Part::Answer => gettext("your answer"),
            Part::Color => gettext("the color"),
            Part::Privacy => gettext("the privacy"),
            Part::Busy => gettext("whether it shows as busy"),
            Part::Status => gettext("the status"),
        }
    }
}

/// The zone a timed event is read in; an all-day event has none.
fn timed_zone(event: &model::Event) -> &str {
    if event.all_day { "" } else { &event.zone }
}

/// Where the hash of an event's shape sits in a print, after the parts:
/// its length and its clock, which a changed occurrence compares with the
/// series it came from.
const SHAPE: usize = PARTS.len();

fn shape(event: &model::Event) -> u32 {
    fnv(
        &serde_json::to_vec(&(event.end - event.start, event.all_day, timed_zone(event)))
            .unwrap_or_default(),
    )
}

/// The hashes of `event`'s parts and of its shape, in etag order.
fn print_of(event: &model::Event) -> Vec<u32> {
    PARTS
        .iter()
        .map(|part| part.hash(event))
        .chain([shape(event)])
        .collect()
}

/// `event` with its etag, the object's `updated` stamp as read, followed
/// by the fingerprint of the event as read.
fn stamped(mut event: model::Event) -> model::Event {
    let print: String = print_of(&event)
        .iter()
        .map(|hash| format!("{hash:0DIGITS$x}"))
        .collect();
    event.etag = format!("{}|{print}", event.etag);
    event
}

/// The `updated` stamp and the part hashes an etag carries, or `None` for
/// an etag without a fingerprint this version can read.
fn fingerprint(etag: &str) -> Option<(&str, Vec<u32>)> {
    let (updated, print) = etag.rsplit_once('|')?;
    if print.len() != (SHAPE + 1) * DIGITS || !print.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let hashes = print.as_bytes().chunks(DIGITS).map(|digits| {
        std::str::from_utf8(digits)
            .ok()
            .and_then(|d| u32::from_str_radix(d, 16).ok())
    });
    Some((updated, hashes.collect::<Option<Vec<u32>>>()?))
}

/// The event as the server holds it now, `now`, with each part the person
/// changed since `print` taken from `edited`. Where the server's part
/// changed too, to something else, the server's stays; those parts come
/// back second.
fn merge(edited: &model::Event, print: &[u32], now: &model::Event) -> (model::Event, Vec<Part>) {
    let mut merged = now.clone();
    let mut kept = Vec::new();
    for (part, was) in PARTS.iter().zip(print) {
        let mine = part.hash(edited);
        if mine == *was {
            continue;
        }
        let theirs = part.hash(now);
        if theirs != *was && theirs != mine {
            kept.push(*part);
        } else {
            part.take(edited, &mut merged);
        }
    }
    (merged, kept)
}

/// [`merge`] for an occurrence changed for the first time, whose print is
/// the series' as the copy read it. The time counts as the person's change
/// when the occurrence no longer starts at `original` or its shape no
/// longer matches the series'; the server's, when the series' time changed
/// or the occurrence has an override of its own. An occurrence has no
/// repeat, so the repeat is never compared.
fn merge_from_series(
    edited: &model::Event,
    print: &[u32],
    now: &model::Event,
    series: &model::Event,
    original: EpochMillis,
) -> (model::Event, Vec<Part>) {
    let mut merged = now.clone();
    let mut kept = Vec::new();
    for (part, was) in PARTS.iter().zip(print) {
        let (mine, theirs, same) = match part {
            Part::Repeat => continue,
            Part::Time => (
                edited.start != original || shape(edited) != print[SHAPE],
                part.hash(series) != *was
                    || shape(now) != shape(series)
                    || Some(now.start) != now.original_start,
                (edited.start, edited.end) == (now.start, now.end),
            ),
            _ => (
                part.hash(edited) != *was,
                part.hash(now) != *was,
                part.hash(edited) == part.hash(now),
            ),
        };
        if !mine {
            continue;
        }
        if theirs && !same {
            kept.push(*part);
        } else {
            part.take(edited, &mut merged);
        }
    }
    (merged, kept)
}

/// Whether `series`, as the server holds it now, has an occurrence that
/// starts at `at`.
fn occurs(series: &model::Event, at: EpochMillis) -> bool {
    model::expand(series, at - 1, at + 1)
        .iter()
        .any(|(start, _)| *start == at)
}

/// The one occurrence of `series` on the day `original` fell on, in the
/// series' zone, for an occurrence whose series another app moved to
/// another time of day. `None` when the day has none, or several.
fn same_day(series: &model::Event, original: EpochMillis) -> Option<EpochMillis> {
    let zone: Tz = series.zone.parse().unwrap_or(Tz::UTC);
    let day = |at: EpochMillis| {
        DateTime::<Utc>::from_timestamp_millis(at).map(|d| d.with_timezone(&zone).date_naive())
    };
    let wanted = day(original)?;
    let found: Vec<EpochMillis> = model::expand(series, original - 2 * DAY, original + 2 * DAY)
        .into_iter()
        .map(|(start, _)| start)
        .filter(|start| day(*start) == Some(wanted))
        .collect();
    match found.as_slice() {
        [one] => Some(*one),
        _ => None,
    }
}

/// The events one object holds, each with its fingerprinted etag.
fn read_events(object: &Value, calendar: &str, me: &[String]) -> Vec<model::Event> {
    jscal::events_of(object, calendar, me)
        .into_iter()
        .map(stamped)
        .collect()
}

/// The `updated` stamp an object carries.
fn updated(object: &Value) -> &str {
    object
        .get("updated")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// One patch entry that puts `value` at `inner` inside the override `key`
/// of `current`, or makes it the whole override with no `inner`. A path
/// into a map the object lacks is an invalidPatch, so a missing override
/// or a missing map goes whole.
fn override_entry(
    current: &Value,
    key: &str,
    inner: Option<&str>,
    value: Value,
) -> (String, Value) {
    let overrides = current
        .get("recurrenceOverrides")
        .and_then(Value::as_object);
    let held = overrides
        .and_then(|o| o.get(key))
        .is_some_and(|o| o.get("excluded").and_then(Value::as_bool) != Some(true));
    let whole = |value: Value| match inner {
        Some(inner) => json!({ inner: value }),
        None => value,
    };
    match (overrides, inner) {
        (Some(_), Some(inner)) if held => (
            format!("recurrenceOverrides/{}/{}", pointer(key), pointer(inner)),
            value,
        ),
        (Some(_), _) => (
            format!("recurrenceOverrides/{}", pointer(key)),
            whole(value),
        ),
        (None, _) => ("recurrenceOverrides".into(), json!({ key: whole(value) })),
    }
}

/// Puts one patch entry in `patch`. Two entries for one new override, or
/// for a series with no overrides yet, name the same path with an object
/// each; their keys join, since a patch names a path once.
fn merge_entry(patch: &mut Map<String, Value>, path: String, value: Value) {
    match (patch.get_mut(&path), value) {
        (Some(Value::Object(held)), Value::Object(more)) => {
            for (key, value) in more {
                merge_entry(held, key, value);
            }
        }
        (_, value) => {
            patch.insert(path, value);
        }
    }
}

/// What one save sends: the patch, and the parts where the server's
/// version stayed.
struct Plan {
    patch: Map<String, Value>,
    kept: Vec<Part>,
    /// The id of the event the save lands on: an occurrence of a series
    /// another app moved has a new one.
    want: String,
}

impl<J: JmapApi> Jmap<J> {
    /// The account's own addresses, in lower case, for telling its
    /// participant apart.
    pub(super) fn me(&self) -> Vec<String> {
        let mut me = vec![self.settings().address.to_ascii_lowercase()];
        me.extend(
            self.known()
                .identities
                .iter()
                .map(|i| i.email.to_ascii_lowercase()),
        );
        me
    }

    /// One event's JSCalendar object as the server holds it now, and the
    /// account's CalendarEvent state it was read at.
    async fn event_object(&self, id: &str) -> Result<(Value, String), BackendError> {
        let got = self
            .api()
            .calendar_events(&[id.to_string()])
            .await
            .map_err(backend)?;
        let object = got.list.into_iter().next().ok_or(BackendError::NotFound)?;
        Ok((object, got.state))
    }

    /// Writes one event, from `state` when given. A server that mails
    /// nobody refuses `send`; the change then goes in without it.
    async fn write_event(
        &self,
        write: &EventWrite,
        send: bool,
        state: Option<&str>,
    ) -> Result<Option<Value>, JmapError> {
        match self.api().set_calendar_event(write, send, state).await {
            Err(JmapError::Method { ref kind, .. })
                if send && kind == "noSupportedScheduleMethods" =>
            {
                tracing::info!(
                    "the calendar server sends no invitations; the change is saved without them"
                );
                self.api().set_calendar_event(write, false, state).await
            }
            other => other,
        }
    }

    /// The event `want` from the object `id` as the server holds it now.
    async fn read_back(
        &self,
        calendar: &str,
        id: &str,
        want: &str,
    ) -> Result<model::Event, BackendError> {
        let (object, _) = self.event_object(id).await?;
        let me = self.me();
        if let Some(found) = read_events(&object, calendar, &me)
            .into_iter()
            .find(|e| e.id == want)
        {
            return Ok(found);
        }
        // An occurrence with no override of its own, as the series shows
        // it; it has no version of its own either, as the copy expects.
        let (_, original) = split_occurrence_id(want).ok_or(BackendError::NotFound)?;
        let occurrence =
            jscal::occurrence_of(&object, calendar, &me, original).ok_or(BackendError::NotFound)?;
        Ok(model::Event {
            etag: String::new(),
            ..occurrence
        })
    }

    /// The object with `uid` on `calendar`, if the server holds one.
    async fn by_uid(&self, calendar: &str, uid: &str) -> Result<Option<Value>, BackendError> {
        let filter = json!({ "inCalendar": calendar, "uid": uid });
        let found = self
            .api()
            .calendar_events_page(&filter, 0, 1)
            .await
            .map_err(backend)?;
        Ok(found.events.into_iter().next())
    }

    /// Changes the server's `object` to match `event`, the version the
    /// person chose to keep, and answers it as the server then holds it.
    async fn overwrite(
        &self,
        object: &Value,
        event: &model::Event,
        send: bool,
    ) -> Result<model::Event, BackendError> {
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .ok_or(BackendError::NotFound)?
            .to_string();
        let same = model::Event {
            id: id.clone(),
            series: None,
            original_start: None,
            ..event.clone()
        };
        let patch = jscal::patch_of(&same, object, &self.me())?;
        if !patch.is_empty() {
            self.write_event(
                &EventWrite::Update {
                    id: id.clone(),
                    patch,
                },
                send,
                None,
            )
            .await
            .map_err(backend)?;
        }
        self.read_back(&event.calendar, &id, &id).await
    }

    /// Creates `event` under the UID the copy gave it. JMAP takes no id
    /// from the client, so a create whose answer was lost, or whose read
    /// back failed, would make a second event and invite the guests twice
    /// when tried again. The UID finds the event the first try made, and
    /// the try goes onto it.
    async fn create(&self, event: &model::Event, send: bool) -> Result<model::Event, BackendError> {
        let mut event = event.clone();
        if event.uid.trim().is_empty() {
            event.uid = model::made_uid(&event.id);
        }
        if let Some(object) = self.by_uid(&event.calendar, &event.uid).await? {
            return self.overwrite(&object, &event, send).await;
        }
        let mut object = jscal::object_of(&event)?;
        object.insert(
            "calendarIds".into(),
            json!({ event.calendar.clone(): true }),
        );
        let created = self
            .write_event(&EventWrite::Create(object), send, None)
            .await
            .map_err(backend)?;
        let id = created
            .as_ref()
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BackendError::Refused(gettext("The server kept the event but did not say its id."))
            })?
            .to_string();
        self.read_back(&event.calendar, &id, &id).await
    }

    /// The patch that saves `event`, a series or one changed occurrence,
    /// onto `current`. `etag` is the version the person edited: its
    /// fingerprint says which parts they changed. An etag without one
    /// stands for the whole event, so it goes only while the server holds
    /// that same version.
    fn plan(
        &self,
        event: &model::Event,
        etag: Option<&str>,
        current: &Value,
    ) -> Result<Plan, BackendError> {
        let me = self.me();
        if let (Some(_), Some(original)) = (&event.series, event.original_start) {
            return self.plan_occurrence(event, etag, current, original, &me);
        }
        let now = read_events(current, &event.calendar, &me)
            .into_iter()
            .find(|e| e.id == event.id);
        let (wanted, kept) = match (etag.filter(|e| !e.is_empty()), now) {
            (Some(etag), Some(now)) => match fingerprint(etag) {
                Some((_, print)) => merge(event, &print, &now),
                None if etag == updated(current) => (event.clone(), Vec::new()),
                None => return Err(BackendError::Changed),
            },
            (Some(_), None) => return Err(BackendError::NotFound),
            (None, _) => (event.clone(), Vec::new()),
        };
        Ok(Plan {
            patch: jscal::patch_of(&wanted, current, &me)?,
            kept,
            want: event.id.clone(),
        })
    }

    /// [`Self::plan`] for one occurrence of a series. An occurrence with an
    /// override compares with its own version; one changed for the first
    /// time compares with the series' version it came from, and lands on
    /// the occurrence the series has now on that day when another app
    /// moved the series. The patch never names a recurrence id the rule no
    /// longer makes, which would add an occurrence.
    fn plan_occurrence(
        &self,
        event: &model::Event,
        etag: Option<&str>,
        current: &Value,
        original: EpochMillis,
        me: &[String],
    ) -> Result<Plan, BackendError> {
        let series = read_events(current, &event.calendar, me)
            .into_iter()
            .next()
            .ok_or(BackendError::NotFound)?;
        let entry = current
            .get("recurrenceOverrides")
            .and_then(|o| o.get(jscal::override_key(current, original)));
        if entry
            .and_then(|o| o.get("excluded"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            // Another app deleted it.
            return Err(BackendError::NotFound);
        }
        let own = etag.filter(|e| !e.is_empty());
        let series_print = fingerprint(&event.series_etag).map(|(_, print)| print);
        let target = match (
            entry.is_some() || occurs(&series, original),
            own,
            &series_print,
        ) {
            (true, _, _) => original,
            (false, None, Some(print))
                if Part::Time.hash(&series) != print[Part::Time as usize] =>
            {
                same_day(&series, original).ok_or(BackendError::Changed)?
            }
            // Its override went, or its series changed in a way that
            // leaves no occurrence to put the change on.
            _ => return Err(BackendError::Changed),
        };
        let now = jscal::occurrence_of(current, &event.calendar, me, target)
            .ok_or(BackendError::NotFound)?;
        let (mut wanted, kept) = match (own, &series_print) {
            (Some(etag), _) => match fingerprint(etag) {
                Some((_, print)) => merge(event, &print, &now),
                None if etag == updated(current) => (event.clone(), Vec::new()),
                None => return Err(BackendError::Changed),
            },
            (None, Some(print)) => merge_from_series(event, print, &now, &series, original),
            // A change queued before the copy kept the series' version.
            (None, None) => (event.clone(), Vec::new()),
        };
        wanted.id.clone_from(&now.id);
        wanted.series.clone_from(&now.series);
        wanted.original_start = Some(target);
        let key = jscal::override_key(current, target);
        let value = jscal::override_of(&wanted, current, me)?;
        let held = current
            .get("recurrenceOverrides")
            .and_then(|o| o.get(&key))
            .is_some();
        // An empty override where none was changes nothing.
        let patch = match !held && value.as_object().is_some_and(Map::is_empty) {
            true => Map::new(),
            false => Map::from_iter([override_entry(current, &key, None, value)]),
        };
        Ok(Plan {
            patch,
            kept,
            want: wanted.id,
        })
    }

    /// One try at saving a change to an event the server has. `None` when
    /// another write reached the server between the read and the write.
    async fn try_change(
        &self,
        event: &model::Event,
        etag: Option<&str>,
        send: bool,
    ) -> Result<Option<model::Event>, BackendError> {
        let owner = event
            .series
            .as_deref()
            .filter(|_| event.original_start.is_some())
            .unwrap_or(&event.id);
        let (current, state) = self.event_object(owner).await?;
        let Plan { patch, kept, want } = self.plan(event, etag, &current)?;
        if !patch.is_empty() {
            let update = EventWrite::Update {
                id: owner.to_string(),
                patch,
            };
            match self.write_event(&update, send, Some(&state)).await {
                Err(JmapError::StateMismatch) => return Ok(None),
                other => other.map_err(backend)?,
            };
        }
        let mut saved = self.read_back(&event.calendar, owner, &want).await?;
        saved.kept_theirs = kept.into_iter().map(Part::words).collect();
        Ok(Some(saved))
    }

    /// One try at deleting an event, or excluding one occurrence. `None`
    /// when another write reached the server between the read and the
    /// write.
    async fn try_remove(
        &self,
        calendar: &str,
        id: &str,
        etag: Option<&str>,
        send: bool,
    ) -> Result<Option<()>, BackendError> {
        if let Some((series, original)) = split_occurrence_id(id) {
            let (current, state) = self.event_object(series).await?;
            let key = jscal::exclusion_key(&current, original)?;
            // A recurrence id the rule no longer makes would exclude
            // nothing: another app moved the series since.
            let held = current
                .get("recurrenceOverrides")
                .and_then(|o| o.get(&key))
                .is_some();
            let now = read_events(&current, calendar, &self.me())
                .into_iter()
                .next()
                .ok_or(BackendError::NotFound)?;
            if !held && !occurs(&now, original) {
                return Err(BackendError::Changed);
            }
            let (path, value) = override_entry(&current, &key, None, json!({ "excluded": true }));
            let update = EventWrite::Update {
                id: series.to_string(),
                patch: Map::from_iter([(path, value)]),
            };
            return match self.write_event(&update, send, Some(&state)).await {
                Err(JmapError::StateMismatch) => Ok(None),
                other => other.map(|_| Some(())).map_err(backend),
            };
        }
        let (current, state) = self.event_object(id).await?;
        if let Some(etag) = etag.filter(|e| !e.is_empty()) {
            // The parts, since a server may keep no `updated` stamp.
            let unchanged = match fingerprint(etag) {
                Some((_, print)) => read_events(&current, calendar, &self.me())
                    .first()
                    .is_some_and(|now| print_of(now) == print),
                None => etag == updated(&current),
            };
            if !unchanged {
                return Err(BackendError::Changed);
            }
        }
        match self
            .write_event(&EventWrite::Destroy(id.to_string()), send, Some(&state))
            .await
        {
            Err(JmapError::StateMismatch) => Ok(None),
            other => other.map(|_| Some(())).map_err(backend),
        }
    }

    async fn page_of(
        &self,
        calendar: &str,
        filter: &Value,
        position: u64,
        state: Option<String>,
        with_token: bool,
    ) -> Result<model::EventPage, BackendError> {
        let page = self
            .api()
            .calendar_events_page(filter, position, PAGE)
            .await
            .map_err(backend)?;
        let state = state.unwrap_or(page.state);
        let me = self.me();
        let mut out = model::EventPage::default();
        for object in &page.events {
            if let Some(id) = object.get("id").and_then(Value::as_str) {
                out.whole_series.push(id.to_string());
            }
            out.events.extend(read_events(object, calendar, &me));
        }
        let next = position + page.events.len() as u64;
        if !page.events.is_empty() && page.total.is_some_and(|total| next < total) {
            out.next_page = serde_json::to_string(&Cursor {
                position: next,
                state,
                changes_from: None,
                round: 0,
            })
            .ok();
        } else if with_token {
            out.next_sync = Some(state);
        }
        Ok(out)
    }

    /// How many event objects the adapter keeps from its last read of
    /// changes.
    #[cfg(test)]
    pub(crate) fn kept_calendar_objects(&self) -> usize {
        self.known()
            .calendar_changes
            .as_ref()
            .map_or(0, |kept| kept.objects.len())
    }

    async fn changes_since(&self, since: &str) -> Result<Arc<EventChanges>, BackendError> {
        let kept = self.known().calendar_changes.clone();
        if let Some(kept) = kept.filter(|k| k.since == since) {
            return Ok(kept);
        }
        // The page goes before the next is read, so the copy holds one.
        self.known().calendar_changes = None;
        let changes = self
            .api()
            .calendar_event_changes(since, PAGE)
            .await
            .map_err(backend)?;
        let mut ids = changes.created;
        ids.extend(changes.updated);
        ids.sort_unstable();
        ids.dedup();
        // A server that answers more than it was asked for is read whole
        // again, a page at a time, rather than held at once.
        if ids.len() + changes.destroyed.len() > PAGE {
            return Err(BackendError::StateLost);
        }
        let objects = match ids.is_empty() {
            true => Vec::new(),
            false => {
                self.api()
                    .calendar_events(&ids)
                    .await
                    .map_err(backend)?
                    .list
            }
        };
        let read = Arc::new(EventChanges {
            since: since.to_string(),
            new_state: changes.new_state,
            more: changes.has_more_changes,
            objects,
            destroyed: changes.destroyed,
        });
        self.known().calendar_changes = Some(Arc::clone(&read));
        Ok(read)
    }

    /// One page of the changes to `calendar` since `since`, the `round`th
    /// of this read.
    async fn changed_in(
        &self,
        calendar: &str,
        since: &str,
        round: usize,
    ) -> Result<model::EventPage, BackendError> {
        let changes = self.changes_since(since).await?;
        let me = self.me();
        let mut page = model::EventPage::default();
        for object in &changes.objects {
            let Some(id) = object.get("id").and_then(Value::as_str) else {
                continue;
            };
            match object
                .get("calendarIds")
                .and_then(|c| c.get(calendar))
                .and_then(Value::as_bool)
                == Some(true)
            {
                true => {
                    page.whole_series.push(id.to_string());
                    page.events.extend(read_events(object, calendar, &me));
                }
                // In another calendar now; removing an id this calendar
                // never held changes nothing.
                false => page.removed.push(id.to_string()),
            }
        }
        page.removed.extend(changes.destroyed.iter().cloned());
        page.next_sync = Some(changes.new_state.clone());
        if changes.more && round + 1 < ROUNDS {
            let next = Cursor {
                position: 0,
                state: String::new(),
                changes_from: Some(changes.new_state.clone()),
                round: round + 1,
            };
            page.next_page = serde_json::to_string(&next).ok();
        }
        Ok(page)
    }
}

impl<J: JmapApi> CalendarService for Jmap<J> {
    async fn calendars(&self) -> Result<Vec<model::Calendar>, BackendError> {
        let listed = self.api().calendars().await.map_err(backend)?;
        let names_a_default = listed.iter().any(|c| c.is_default);
        Ok(listed
            .into_iter()
            .enumerate()
            .map(|(index, c)| model::Calendar {
                access: access_of(&c),
                zone: c.time_zone.clone().unwrap_or_default(),
                color: c
                    .color
                    .clone()
                    .filter(|color| color.starts_with('#'))
                    .unwrap_or_else(|| DEFAULT_COLOR.to_string()),
                primary: if names_a_default {
                    c.is_default
                } else {
                    index == 0
                },
                shown: true,
                hidden: false,
                reminders: c
                    .default_alerts_with_time
                    .as_ref()
                    .map(jscal::default_reminders)
                    .unwrap_or_default(),
                // JMAP keeps the two apart: a calendar with timed defaults
                // and none for all-day events reminds of no all-day event.
                all_day_reminders: Some(
                    c.default_alerts_without_time
                        .as_ref()
                        .map(jscal::default_reminders)
                        .unwrap_or_default(),
                ),
                supports_teams: false,
                name: c.name,
                id: c.id,
            })
            .collect())
    }

    async fn event_changes(
        &self,
        calendar: &str,
        token: Option<&str>,
        page: Option<&str>,
        from: EpochMillis,
    ) -> Result<model::EventPage, BackendError> {
        let filter = json!({ "inCalendar": calendar, "after": filter_time(from) });
        if let Some(page) = page {
            let cursor: Cursor = serde_json::from_str(page).map_err(|_| BackendError::StateLost)?;
            if let Some(from) = &cursor.changes_from {
                return self.changed_in(calendar, from, cursor.round).await;
            }
            return self
                .page_of(calendar, &filter, cursor.position, Some(cursor.state), true)
                .await;
        }
        match token {
            Some(since) => self.changed_in(calendar, since, 0).await,
            None => self.page_of(calendar, &filter, 0, None, true).await,
        }
    }

    async fn event_range(
        &self,
        calendar: &str,
        from: EpochMillis,
        to: EpochMillis,
        page: Option<&str>,
    ) -> Result<model::EventPage, BackendError> {
        let filter = json!({ "inCalendar": calendar, "after": filter_time(from), "before": filter_time(to) });
        let position = match page {
            Some(page) => {
                serde_json::from_str::<Cursor>(page)
                    .map_err(|_| BackendError::StateLost)?
                    .position
            }
            None => 0,
        };
        self.page_of(calendar, &filter, position, None, false).await
    }

    async fn put_event(
        &self,
        event: &model::Event,
        etag: Option<&str>,
        create: bool,
        notify: Notify,
    ) -> Result<model::Event, BackendError> {
        let send = notify == Notify::Guests;
        if create {
            return self.create(event, send).await;
        }
        // Another write between the read and the write: read again and
        // apply the person's parts once more, at most twice.
        for _ in 0..TRIES {
            if let Some(saved) = self.try_change(event, etag, send).await? {
                return Ok(saved);
            }
        }
        Err(BackendError::Changed)
    }

    async fn remove_event(
        &self,
        calendar: &str,
        id: &str,
        etag: Option<&str>,
        notify: Notify,
    ) -> Result<(), BackendError> {
        let send = notify == Notify::Guests;
        for _ in 0..TRIES {
            if let Some(()) = self.try_remove(calendar, id, etag, send).await? {
                return Ok(());
            }
        }
        Err(BackendError::Changed)
    }

    async fn import_event(&self, event: &model::Event) -> Result<model::Event, BackendError> {
        if event.uid.trim().is_empty() {
            return Err(BackendError::Refused(gettext(
                "An event needs an ID to be imported.",
            )));
        }
        let Some(object) = self.by_uid(&event.calendar, &event.uid).await? else {
            return self.create(event, false).await;
        };
        // A file holding one occurrence (a RECURRENCE-ID and no rule) of a
        // series the calendar has changes that occurrence, as CalDAV does.
        if let Some(original) = event.original_start {
            let id = object
                .get("id")
                .and_then(Value::as_str)
                .ok_or(BackendError::NotFound)?
                .to_string();
            let me = self.me();
            let series = read_events(&object, &event.calendar, &me)
                .into_iter()
                .next()
                .ok_or(BackendError::NotFound)?;
            let one = model::Event {
                id: occurrence_id(&series, original),
                series: Some(id.clone()),
                ..event.clone()
            };
            let key = jscal::override_key(&object, original);
            let entry =
                override_entry(&object, &key, None, jscal::override_of(&one, &object, &me)?);
            let update = EventWrite::Update {
                id: id.clone(),
                patch: Map::from_iter([entry]),
            };
            self.write_event(&update, false, None)
                .await
                .map_err(backend)?;
            return self.read_back(&event.calendar, &id, &one.id).await;
        }
        // The file is the version the person chose to keep, whole.
        self.overwrite(&object, event, false).await
    }

    async fn upload_attachment(
        &self,
        _file: &model::Attachment,
        _sent: Arc<std::sync::atomic::AtomicU64>,
    ) -> Result<model::Attachment, BackendError> {
        Err(BackendError::Refused(gettext(
            "Penguin Mail cannot attach files to events on this JMAP calendar.",
        )))
    }

    async fn share_file(&self, _file_id: &str, _email: &str) -> Result<(), BackendError> {
        Err(BackendError::Refused(gettext(
            "This JMAP calendar has no files to share.",
        )))
    }

    async fn move_event(
        &self,
        event: &model::Event,
        destination: &str,
        notify: Notify,
    ) -> Result<model::Event, BackendError> {
        let owner = event.series.as_deref().unwrap_or(&event.id).to_string();
        let mut patch = Map::new();
        patch.insert("calendarIds".into(), json!({ destination: true }));
        self.write_event(
            &EventWrite::Update {
                id: owner.clone(),
                patch,
            },
            notify == Notify::Guests,
            None,
        )
        .await
        .map_err(backend)?;
        self.read_back(destination, &owner, &owner).await
    }

    /// Saves the answer on the account's participant and lets the server
    /// mail the organizer. A server that mails nobody gets the answer
    /// saved, and the organizer gets the iTIP REPLY by mail, as from a
    /// CalDAV server that does not schedule.
    async fn answer_event(
        &self,
        calendar: &str,
        id: &str,
        me: &str,
        answer: Answer,
        note: Option<&str>,
    ) -> Result<model::Event, BackendError> {
        let (owner, occurrence) = match split_occurrence_id(id) {
            Some((series, original)) => (series, Some(original)),
            None => (id, None),
        };
        let (object, _) = self.event_object(owner).await?;
        let mut who = vec![me.to_ascii_lowercase()];
        who.extend(self.me());
        let Some(participant) = jscal::participant_for(&object, &who) else {
            return Err(BackendError::Refused(gettext(
                "You are not a guest of this event.",
            )));
        };
        // The note goes on the participant too, where a server that
        // schedules puts it in the REPLY it sends.
        let mut fields = vec![("participationStatus", json!(jscal::status_of(Some(answer))))];
        if let Some(note) = note.filter(|n| !n.trim().is_empty()) {
            fields.push(("participationComment", json!(note)));
        }
        let mut patch = Map::new();
        for (field, value) in fields {
            let (path, value) = match occurrence {
                None => (
                    format!("participants/{}/{field}", pointer(&participant)),
                    value,
                ),
                Some(original) => {
                    let key = jscal::override_key(&object, original);
                    let over = object.get("recurrenceOverrides").and_then(|o| o.get(&key));
                    let escaped = pointer(&participant);
                    let whole = format!("participants/{escaped}");
                    // An override may replace the participant map, or one
                    // participant, whole; the answer then goes inside that
                    // copy, since a key inside another key is an invalidPatch.
                    if over
                        .and_then(|o| o.get("participants"))
                        .is_some_and(|p| p.get(&participant).is_some())
                    {
                        (
                            format!(
                                "recurrenceOverrides/{}/participants/{escaped}/{field}",
                                pointer(&key)
                            ),
                            value,
                        )
                    } else if over
                        .and_then(|o| o.get(&whole))
                        .is_some_and(Value::is_object)
                    {
                        (
                            format!(
                                "recurrenceOverrides/{}/{}/{field}",
                                pointer(&key),
                                pointer(&whole)
                            ),
                            value,
                        )
                    } else {
                        override_entry(&object, &key, Some(&format!("{whole}/{field}")), value)
                    }
                }
            };
            merge_entry(&mut patch, path, value);
        }
        let update = EventWrite::Update {
            id: owner.to_string(),
            patch,
        };
        match self.api().set_calendar_event(&update, true, None).await {
            Ok(_) => {}
            Err(JmapError::Method { ref kind, .. }) if kind == "noSupportedScheduleMethods" => {
                self.api()
                    .set_calendar_event(&update, false, None)
                    .await
                    .map_err(backend)?;
                let series = jscal::events_of(&object, calendar, &who)
                    .into_iter()
                    .next()
                    .ok_or(BackendError::NotFound)?;
                crate::invitations::mail::tell_organizer(
                    self,
                    &series,
                    me,
                    answer,
                    occurrence,
                    note,
                    now_millis(),
                )
                .await?;
            }
            Err(err) => return Err(backend(err)),
        }
        self.read_back(calendar, owner, id).await
    }

    async fn edit_list(
        &self,
        _calendar: &str,
        _edit: &model::list::ListEdit,
    ) -> Result<Option<model::Calendar>, BackendError> {
        Err(BackendError::Refused(gettext(
            "Penguin Mail cannot change the calendar list of a JMAP server.",
        )))
    }
}
