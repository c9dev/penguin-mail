//! In-memory servers for sync's tests and `penguin-mail --demo`: a Gmail
//! here, and an IMAP server and SMTP sink in [`FakeImap`] and [`FakeSmtp`].
//! Seed a mailbox, hand the fake to an `AccountSync`, and every read and
//! write the app makes goes through the same code the real client does.
//!
//! Callers change the Gmail mailbox directly through [`FakeGmail::with`]. The
//! changes Gmail would record in history are recorded here too, so a sync
//! replays them. [`fill_store`] gives a new account the store its first
//! sync against this mailbox would leave, for callers that want mail on
//! screen before the engine starts.

mod imap;
mod one_click;
mod query;
mod sent;

pub use imap::{
    FakeImap, FakeMailbox, FakeMessage, FakeSmtp, ImapState, SmtpState, Submitted, raw_message,
};
pub use one_click::FakeOneClick;
pub use sent::read_sent;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mail_builder::MessageBuilder;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::raw::Raw;
use mail_builder::mime::MimePart;
use mailrs_domain::calendar;
use mailrs_domain::invitation::Answer;
use mailrs_domain::{
    Address, EpochMillis, Filter, MessageBody, MessageMeta, Protection, Vacation,
};
use mailrs_gmail::labels;
use mailrs_gmail::model::{Header, Message, MessagePart, PartBody};
use mailrs_gmail::{
    AccountQuota, Answered, BATCH_LIMIT, Busy, CALENDAR_LIST_SCOPE, CALENDAR_SCOPE, CONTACTS_SCOPE,
    CONTACTS_WRITE_SCOPE, ConnectionsPage, ContactFields, DELETE_SCOPE, Event, EventFields,
    GmailError, Guest, HistoryChange, HistoryPage, LabelColor, MessagePage, MessageRef, Person,
    Priority, Profile, QuotaLimiter, RemoteLabel, SETTINGS_SCOPE, SendAs, Series, cost, limiter,
};

use crate::api::{DraftRef, GmailApi, SavedDraft};
use crate::{AccountSync, SyncError};
use query::Query;

pub struct FakeGmail {
    state: Mutex<FakeState>,
    /// The budget the app paces itself against, as the real client does.
    /// Without one the fake answers as fast as it is asked.
    quota: Option<std::sync::Arc<AccountQuota>>,
    /// Gmail's own per-user rate. With one, a call that arrives on an empty
    /// bucket comes back rate limited, the way Gmail answers 429.
    limit: Option<QuotaLimiter>,
}

/// A call held open by [`FakeGmail::hold`], seen from the fake's side.
struct Hold {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

/// A call held open by [`FakeGmail::hold`], seen from the test's side.
pub struct Held {
    entered: tokio::sync::oneshot::Receiver<()>,
    release: tokio::sync::oneshot::Sender<()>,
}

impl Held {
    /// Resolves once the held call is waiting.
    pub async fn entered(&mut self) {
        let _ = (&mut self.entered).await;
    }

    /// Lets the held call answer.
    pub fn release(self) {
        let _ = self.release.send(());
    }
}

pub struct FakeState {
    pub email: String,
    pub history_id: u64,
    /// `history` answers 404 for starts below this, as Gmail does once history ages out.
    pub history_floor: u64,
    pub labels: Vec<RemoteLabel>,
    pub messages: HashMap<String, MessageMeta>,
    pub history: Vec<(u64, HistoryChange)>,
    pub bodies: HashMap<String, MessageBody>,
    /// The most a page of a listing or of history holds. A listing gives
    /// back as many as it asked for up to this, as Gmail gives up to 500.
    pub page_size: usize,
    /// Errors returned by the next calls, one per call.
    pub failures: VecDeque<GmailError>,
    /// Errors returned by a later call to one method, after the calls to
    /// it that come first have answered.
    planned: Vec<Planned>,
    /// How many of the next calls panic, as a bug in the code reading
    /// Gmail's answer would.
    pub panics: usize,
    /// Errors returned by the next sends after Gmail has sent the message,
    /// one per send, as when the connection drops before the answer comes
    /// back.
    pub lost: VecDeque<GmailError>,
    /// Calls a test holds open, by method, until it lets them answer.
    held: HashMap<&'static str, Hold>,
    /// What the calls so far would have cost against the real API.
    pub usage: Usage,
    /// Fetches of a message's body, of every kind: `message_body`,
    /// `raw_message` and `message_structure` alike.
    pub body_fetches: usize,
    /// Fetches of a message's raw RFC 822 bytes.
    pub raw_fetches: usize,
    /// Fetches of a message's `format=full` part tree.
    pub structure_fetches: usize,
    /// Fetches of a message's metadata alone.
    pub metadata_fetches: usize,
    pub remote_writes: Vec<String>,
    /// Raw messages sent, with their thread ids.
    pub sent: Vec<(Vec<u8>, Option<String>)>,
    /// Reads a sent message's bytes into the copy Gmail files under Sent.
    /// The demo sets one, so what a person sends shows up in Sent and in
    /// its conversation. Sync's tests leave it unset: the fake then keeps
    /// no copy, and the mailbox holds only what a test put there.
    pub sent_copy: Option<ReadSent>,
    /// Draft id to raw content.
    pub drafts: HashMap<String, Vec<u8>>,
    /// Message id backing each draft.
    pub draft_messages: HashMap<String, String>,
    pub attachments: HashMap<(String, String), Vec<u8>>,
    /// RFC 822 bytes to hand back for a message instead of the ones the
    /// fake builds from its metadata. An export test seeds one to put a
    /// line the mbox writer must quote inside a real message.
    pub raws: HashMap<String, Vec<u8>>,
    pub display_name: Option<String>,
    pub signature: Option<String>,
    /// Extra verified send-as addresses, beyond the account's own.
    pub send_as: Vec<SendAs>,
    pub vacation: Vacation,
    pub filters: Vec<Filter>,
    /// Filters made so far. Gmail never hands a deleted filter's id to a
    /// new one, so ids count up from this rather than from the list.
    pub filters_made: usize,
    /// The account's contacts, in the order the People API would list
    /// them. A fake with none answers an empty address book.
    pub contacts: Vec<Person>,
    /// Photo bytes by URL. A URL nobody seeded answers `NotFound`.
    pub photos: HashMap<String, Vec<u8>>,
    /// The events on this account's calendar, by their iCalendar UID, with
    /// the answer this account gave each one. A UID that is not here is on
    /// nobody's calendar and cannot be answered.
    pub calendar: HashMap<String, Option<Answer>>,
    /// What the account already has on, as a start, an end and a title.
    /// An invitation for a time one of these covers clashes with it.
    pub busy: Vec<(EpochMillis, EpochMillis, String)>,
    /// The repeating events on the calendar, by their iCalendar UID: the
    /// rule without its `RRULE:` prefix, and when each occurrence starts.
    pub series: HashMap<String, (String, Vec<EpochMillis>)>,
    /// The occurrence each answer named, oldest first, and `None` for an
    /// answer that covered the whole series.
    pub answered_occurrences: Vec<Option<EpochMillis>>,
    /// The events on the primary calendar that the event calls list and
    /// change. Kept apart from `calendar` and `busy`, which stand in for
    /// the calls an invitation makes.
    pub events: Vec<Event>,
    next_event: u32,
    /// The calendars `calendars()` lists, for the local copy.
    pub calendars: Vec<calendar::Calendar>,
    /// Every event on those calendars, series and changed occurrences
    /// alike, as the change feed hands them out.
    pub calendar_events: Vec<calendar::Event>,
    /// Each change in order: calendar, event id, and whether it went. A
    /// sync token is a position in this log.
    calendar_log: Vec<(String, String, bool)>,
    /// Play Google forgetting every sync token: a read with one answers
    /// `ExpiredSyncToken`.
    pub expire_calendar_tokens: bool,
    /// Play Google's answer to a write on an event already deleted: 410
    /// Gone, which the client reads as `ExpiredSyncToken`, rather than
    /// the 404 the fake gives otherwise.
    pub deleted_answers_gone: bool,
    /// Calendars Google no longer has. Every read of one and every write
    /// to one answers `NotFound`.
    pub deleted_calendars: Vec<String>,
    /// The OAuth scopes the account has not granted. A call that needs one
    /// answers `MissingScope`, as Google does until the user says yes.
    /// Change it through [`FakeGmail::withhold`] and [`FakeGmail::grant`].
    pub withheld: BTreeSet<&'static str>,
    /// The page that turns the Calendar API on, set to play a Google Cloud
    /// project that has it switched off. Every calendar call then answers
    /// `ApiDisabled` whatever the account granted.
    pub calendar_off: Option<String>,
    /// The time `newer_than` and `older_than` count back from. `None`
    /// reads the clock; a test whose mail sits at fixed dates pins it.
    pub clock: Option<EpochMillis>,
    /// The search text of each `users.messages.list` call, oldest first,
    /// so a test can hold the app to the searches it sent before.
    pub searched: Vec<String>,
}

/// Reads a sent message's bytes, or answers `None` to keep no copy.
pub type ReadSent = Box<dyn Fn(&[u8]) -> Option<SentCopy> + Send>;

/// A sent message as [`FakeState::sent_copy`] reads it. The fake fills in
/// the id, the thread and the labels, as Gmail does.
pub struct SentCopy {
    pub meta: MessageMeta,
    pub body: MessageBody,
    /// The `In-Reply-To` and `References` ids, which put a reply sent
    /// without a thread id into the conversation it answers.
    pub references: Vec<String>,
    /// Each attachment's bytes, by the attachment id `body` names.
    pub files: Vec<(String, Vec<u8>)>,
}

/// An error a test plans for one method's call, after `skip` calls to it.
struct Planned {
    method: &'static str,
    skip: usize,
    err: GmailError,
}

/// Calls made and quota units spent, priced from Gmail's usage-limits
/// table. Gmail charges for a call whether or not it succeeds, so failures
/// count here too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub calls: u32,
    pub units: u32,
    /// Calls Gmail turned down for quota, with a 429.
    pub refused: u32,
    /// How long the user's own calls waited for budget. Backfill waiting
    /// costs nobody anything; this is the wait a person sits through.
    pub foreground_wait: Duration,
    /// Calls per Gmail method, such as `messages.batchModify`.
    pub by_method: BTreeMap<&'static str, u32>,
}

impl Usage {
    pub fn calls_to(&self, method: &str) -> u32 {
        self.by_method.get(method).copied().unwrap_or(0)
    }
}

/// A message for account 1.
pub fn meta(id: &str, thread: &str, date: EpochMillis, labels: &[&str]) -> MessageMeta {
    let mut m = MessageMeta {
        account_id: 1,
        id: id.into(),
        thread_id: thread.into(),
        rfc822_msgid: Some(format!("<{id}@example.com>")),
        from: Some(Address {
            name: Some("Ann".into()),
            email: "ann@example.com".into(),
        }),
        to: vec![],
        cc: vec![],
        subject: format!("Subject {id}"),
        date,
        snippet: format!("snippet {id}"),
        size: 100,
        has_attachments: false,
        held: Default::default(),
        roles: vec![],
        list_unsubscribe: None,
        one_click: false,
    };
    let owned: Vec<String> = labels.iter().map(|l| l.to_string()).collect();
    labels::set_label_ids(&mut m, &owned);
    m
}

/// Changes the Gmail labels `meta` carries through `edit`, as Gmail does
/// when a label goes on or comes off.
pub fn edit_labels(meta: &mut MessageMeta, edit: impl FnOnce(&mut Vec<String>)) {
    let mut ids = labels::label_ids(meta);
    edit(&mut ids);
    labels::set_label_ids(meta, &ids);
}

/// Whether `meta` carries the Gmail label `label`.
pub fn has_label(meta: &MessageMeta, label: &str) -> bool {
    labels::label_ids(meta).iter().any(|l| l == label)
}

/// The part path of a fixture body's `index`th file in the message
/// `raw_message` builds: the readable text is part 1 of a
/// `multipart/mixed`, and the files follow it.
pub fn attachment_path(index: usize) -> String {
    (index + 2).to_string()
}

impl Default for FakeGmail {
    fn default() -> Self {
        FakeGmail::new()
    }
}

impl FakeGmail {
    /// An empty mailbox for one account, with Gmail's own labels.
    pub fn new() -> Self {
        let label = |id: &str, kind: &str| RemoteLabel {
            id: id.into(),
            name: id.into(),
            kind: Some(kind.into()),
            color: None,
        };
        FakeGmail {
            quota: None,
            limit: None,
            state: Mutex::new(FakeState {
                email: "me@example.com".into(),
                history_id: 100,
                history_floor: 0,
                // Gmail lists its role labels, and a message's roles come
                // from that listing.
                labels: labels::ROLES
                    .iter()
                    .map(|(id, _)| label(id, "system"))
                    .chain([
                        label("UNREAD", "system"),
                        label("STARRED", "system"),
                        label("Label_1", "user"),
                    ])
                    .collect(),
                messages: HashMap::new(),
                history: Vec::new(),
                bodies: HashMap::new(),
                page_size: 2,
                failures: VecDeque::new(),
                planned: Vec::new(),
                panics: 0,
                lost: VecDeque::new(),
                held: HashMap::new(),
                usage: Usage::default(),
                body_fetches: 0,
                raw_fetches: 0,
                structure_fetches: 0,
                metadata_fetches: 0,
                remote_writes: Vec::new(),
                sent: Vec::new(),
                sent_copy: None,
                drafts: HashMap::new(),
                draft_messages: HashMap::new(),
                attachments: HashMap::new(),
                raws: HashMap::new(),
                display_name: Some("Me".into()),
                signature: None,
                send_as: Vec::new(),
                vacation: Vacation::default(),
                filters: Vec::new(),
                filters_made: 0,
                contacts: Vec::new(),
                photos: HashMap::new(),
                calendar: HashMap::new(),
                busy: Vec::new(),
                series: HashMap::new(),
                answered_occurrences: Vec::new(),
                events: Vec::new(),
                next_event: 0,
                calendars: Vec::new(),
                calendar_events: Vec::new(),
                calendar_log: Vec::new(),
                expire_calendar_tokens: false,
                deleted_answers_gone: false,
                deleted_calendars: Vec::new(),
                withheld: BTreeSet::new(),
                calendar_off: None,
                clock: None,
                searched: Vec::new(),
            }),
        }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut FakeState) -> R) -> R {
        f(&mut self.state.lock().expect("fake state poisoned"))
    }

    /// Files a copy of each message sent under Sent, as Gmail does, for the
    /// account the store knows this mailbox as.
    pub fn keep_sent_copies(&self, account_id: mailrs_domain::AccountId) {
        self.with(|s| s.sent_copy = Some(Box::new(move |raw| read_sent(raw, account_id))));
    }

    /// Leaves only the labels `ids` in Gmail's list.
    pub fn keep_labels(&self, ids: &[&str]) {
        self.with(|s| s.labels.retain(|l| ids.contains(&l.id.as_str())));
    }

    /// A message that already exists. No history. Whether a listing returns
    /// it follows from its date and labels, as it does in Gmail.
    pub fn seed(&self, meta: MessageMeta) {
        self.with(|s| {
            s.messages.insert(meta.id.clone(), meta);
        });
    }

    /// A message arriving now, recorded in history. Gmail's own filters
    /// archive whatever lands on a muted thread and carry the mute label
    /// over to it, so a message delivered into one arrives that way here.
    pub fn deliver(&self, mut meta: MessageMeta) {
        self.with(|s| {
            if s.thread_is_muted(&meta.thread_id) {
                edit_labels(&mut meta, |ids| {
                    ids.retain(|l| l != labels::INBOX);
                    ids.push(labels::MUTE.into());
                });
            }
            let change = HistoryChange::MessageAdded {
                id: meta.id.clone(),
                thread_id: meta.thread_id.clone(),
            };
            s.messages.insert(meta.id.clone(), meta);
            s.record(change);
        });
    }

    pub fn remote_delete(&self, id: &str) {
        self.with(|s| {
            if let Some(meta) = s.messages.remove(id) {
                s.record(HistoryChange::MessageDeleted {
                    id: id.into(),
                    thread_id: meta.thread_id,
                });
            }
        });
    }

    /// Deletes a message without recording history, as if it happened during a gap.
    pub fn remote_delete_silently(&self, id: &str) {
        self.with(|s| {
            s.messages.remove(id);
        });
    }

    pub fn remote_relabel(&self, id: &str, add: &[&str], remove: &[&str]) {
        self.with(|s| {
            let Some(meta) = s.messages.get_mut(id) else {
                return;
            };
            let thread_id = meta.thread_id.clone();
            edit_labels(meta, |ids| {
                for label in add {
                    if !ids.iter().any(|l| l == label) {
                        ids.push(label.to_string());
                    }
                }
                ids.retain(|l| !remove.contains(&l.as_str()));
            });
            if !add.is_empty() {
                s.record(HistoryChange::LabelsAdded {
                    id: id.into(),
                    thread_id: thread_id.clone(),
                    label_ids: add.iter().map(|l| l.to_string()).collect(),
                });
            }
            if !remove.is_empty() {
                s.record(HistoryChange::LabelsRemoved {
                    id: id.into(),
                    thread_id,
                    label_ids: remove.iter().map(|l| l.to_string()).collect(),
                });
            }
        });
    }

    /// Makes every history id recorded so far too old to replay.
    pub fn expire_history(&self) {
        self.with(|s| {
            s.history_id += 1;
            s.history_floor = s.history_id;
        });
    }

    /// Paces calls through `quota`, the way the real client does, and
    /// answers 429 once Gmail's own budget for the account is spent. A test
    /// that wants to watch the app share one account's budget asks for
    /// this; the rest leave it off and the fake answers at once.
    pub fn under_quota(mut self, quota: std::sync::Arc<AccountQuota>) -> Self {
        self.quota = Some(quota);
        self.limit = Some(QuotaLimiter::gmail_server());
        self
    }

    pub fn fail_next(&self, err: GmailError) {
        self.with(|s| s.failures.push_back(err));
    }

    /// Fails a later call to `method`, such as `"users.history.list"`,
    /// with `err`: the first `skip` calls to it answer as usual.
    pub fn fail_call(&self, method: &'static str, skip: usize, err: GmailError) {
        self.with(|s| s.planned.push(Planned { method, skip, err }));
    }

    /// Makes the next `count` calls panic.
    pub fn panic_next(&self, count: usize) {
        self.with(|s| s.panics += count);
    }

    /// Holds the next call to `method`, such as `"users.threads.get"`,
    /// before it answers. A test uses this to change the mailbox while the
    /// app waits on Gmail: [`Held::entered`] resolves once the call is
    /// waiting, and [`Held::release`] lets it go on. A call that reads the
    /// mailbox before it waits answers with what it read, as a slow reply
    /// from Gmail would.
    pub fn hold(&self, method: &'static str) -> Held {
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        self.with(|s| {
            s.held.insert(
                method,
                Hold {
                    entered: entered_tx,
                    release: release_rx,
                },
            )
        });
        Held { entered, release }
    }

    /// Waits out a hold a test put on `method`, if there is one.
    async fn wait_if_held(&self, method: &'static str) {
        if let Some(hold) = self.with(|s| s.held.remove(method)) {
            let _ = hold.entered.send(());
            let _ = hold.release.await;
        }
    }

    /// Takes back one of the account's OAuth scopes, such as
    /// `mailrs_gmail::SETTINGS_SCOPE`. Calls that need it fail until
    /// [`FakeGmail::grant`] hands it over.
    pub fn withhold(&self, scope: &'static str) {
        self.with(|s| s.withheld.insert(scope));
    }

    pub fn grant(&self, scope: &'static str) {
        self.with(|s| s.withheld.remove(scope));
    }

    /// Google's answer to a call that needs `scope`.
    fn needs(&self, scope: &'static str) -> Result<(), GmailError> {
        match self.with(|s| s.withheld.contains(scope)) {
            true => Err(GmailError::MissingScope),
            false => Ok(()),
        }
    }

    /// Google's answer to a calendar call: the Calendar API switched off in
    /// the project, the calendar scope not granted yet, or yes.
    fn calendar_open(&self) -> Result<(), GmailError> {
        if let Some(url) = self.with(|s| s.calendar_off.clone()) {
            return Err(GmailError::ApiDisabled {
                service: "Google Calendar API".into(),
                enable_url: url,
            });
        }
        self.needs(CALENDAR_SCOPE)
    }

    /// Waits for budget, charges one call to the meter, and hands back the
    /// failure a test queued for it, if any. Every API method starts here,
    /// so the usage counts what the real client would have spent.
    async fn call(&self, method: &'static str, units: u32) -> Result<(), GmailError> {
        self.wait_if_held(method).await;
        // The panic comes after the lock is let go, so the fake keeps
        // answering the calls that follow.
        let panics = self.with(|s| {
            let panics = s.panics > 0;
            s.panics = s.panics.saturating_sub(1);
            panics
        });
        assert!(!panics, "the fake Gmail panicked on {method}, as a test asked");
        let mut waited = Duration::ZERO;
        if let Some(quota) = &self.quota {
            let priority = limiter::priority();
            let started = tokio::time::Instant::now();
            quota.acquire(units, priority).await;
            if priority == Priority::Foreground {
                waited = started.elapsed();
            }
        }
        let refused = self
            .limit
            .as_ref()
            .is_some_and(|limit| !limit.try_acquire(units));
        self.with(|s| {
            s.usage.calls += 1;
            s.usage.units += units;
            s.usage.foreground_wait += waited;
            *s.usage.by_method.entry(method).or_default() += 1;
            if refused {
                s.usage.refused += 1;
                // Gmail hands out a Retry-After of a second on a per-user
                // rate limit, and charges for the call all the same.
                return Err(GmailError::RateLimited {
                    retry_after: Some(std::time::Duration::from_secs(1)),
                });
            }
            if let Some(at) = s.planned.iter().position(|p| p.method == method) {
                match s.planned[at].skip {
                    0 => return Err(s.planned.remove(at).err),
                    _ => s.planned[at].skip -= 1,
                }
            }
            s.failures.pop_front().map_or(Ok(()), Err)
        })
    }

    /// One page of a search, narrowed to a label by id when one is named,
    /// as `messages.list` with `labelIds` is.
    async fn listing(
        &self,
        label_id: Option<&str>,
        query: &str,
        page_token: Option<&str>,
        page_size: u32,
    ) -> Result<MessagePage, GmailError> {
        self.call("users.messages.list", cost::LIST).await?;
        let start = match page_token {
            None => 0,
            Some(token) => token.parse::<usize>().map_err(|_| GmailError::Http {
                status: 400,
                body: "Invalid pageToken".into(),
            })?,
        };
        Ok(self.with(|s| {
            s.searched.push(query.to_string());
            let mut found = s.search(query);
            if let Some(label_id) = label_id {
                found.retain(|id| has_label(&s.messages[id], label_id));
            }
            let size = s.page_size.min(page_size.max(1) as usize);
            let end = (start + size).min(found.len());
            let messages = found[start.min(end)..end]
                .iter()
                .map(|id| MessageRef {
                    id: id.clone(),
                    thread_id: s.messages[id].thread_id.clone(),
                })
                .collect();
            MessagePage {
                messages,
                next_page_token: (end < found.len()).then(|| end.to_string()),
            }
        }))
    }

    /// Calls and units since the last reset.
    pub fn usage(&self) -> Usage {
        self.with(|s| s.usage.clone())
    }

    /// Starts counting again from zero.
    pub fn reset_usage(&self) {
        self.with(|s| s.usage = Usage::default());
    }

    /// Puts an event on a calendar, or replaces it, as someone changing it
    /// on their phone would. Its etag counts the versions.
    pub fn put_calendar_event(&self, mut event: calendar::Event) {
        self.with(|s| {
            let version = s
                .calendar_events
                .iter()
                .find(|e| e.calendar == event.calendar && e.id == event.id)
                .and_then(|e| e.etag.trim_matches('"').parse::<u32>().ok())
                .unwrap_or(0);
            event.etag = format!("\"{}\"", version + 1);
            s.calendar_events.retain(|e| !(e.calendar == event.calendar && e.id == event.id));
            s.calendar_log.push((event.calendar.clone(), event.id.clone(), false));
            s.calendar_events.push(event);
        });
    }

    /// Google's answer to a write on an event it does not hold.
    fn deleted(&self) -> GmailError {
        match self.with(|s| s.deleted_answers_gone) {
            true => GmailError::ExpiredSyncToken,
            false => GmailError::NotFound,
        }
    }

    /// Google's answer to a write on a calendar it no longer has.
    fn calendar_held(&self, calendar: &str) -> Result<(), GmailError> {
        match self.with(|s| s.deleted_calendars.iter().any(|c| c == calendar)) {
            true => Err(GmailError::NotFound),
            false => Ok(()),
        }
    }

    /// The series an occurrence id (`<series>_<start>`) names on `calendar`,
    /// and the occurrence's original start.
    fn series_of(&self, calendar: &str, id: &str) -> Option<(calendar::Event, EpochMillis)> {
        let (series, start) = calendar::split_occurrence_id(id)?;
        let held = self.with(|s| {
            s.calendar_events.iter().find(|e| e.calendar == calendar && e.id == series && !e.rules.is_empty()).cloned()
        })?;
        Some((held, start))
    }

    /// Removes an event from a calendar, as someone deleting it elsewhere
    /// would, and records it in the change log a sync token reads from.
    pub fn drop_calendar_event(&self, calendar: &str, id: &str) {
        self.with(|s| {
            s.calendar_events.retain(|e| !(e.calendar == calendar && e.id == id));
            s.calendar_log.push((calendar.to_string(), id.to_string(), true));
        });
    }
}

impl FakeState {
    fn record(&mut self, change: HistoryChange) {
        self.history_id += 1;
        self.history.push((self.history_id, change));
    }

    /// Files the copy of a sent message under Sent, recorded in history,
    /// when the fake has a [`FakeState::sent_copy`] reader. Gmail puts the
    /// copy in the thread it was sent into, or else in the thread of the
    /// message it replies to, or else in a thread of its own.
    fn file_sent(&mut self, id: &str, raw: &[u8], thread_id: Option<&str>) {
        let Some(copy) = self.sent_copy.as_ref().and_then(|read| read(raw)) else {
            return;
        };
        let SentCopy {
            mut meta,
            body,
            references,
            files,
        } = copy;
        for (attachment_id, bytes) in files {
            self.attachments
                .insert((id.to_string(), attachment_id), bytes);
        }
        let bare = |id: &str| id.trim_matches(['<', '>']).to_string();
        let references: Vec<String> = references.iter().map(|r| bare(r)).collect();
        let answered = self.messages.values().find_map(|m| {
            let msgid = bare(m.rfc822_msgid.as_deref()?);
            references.contains(&msgid).then(|| m.thread_id.clone())
        });
        meta.thread_id = thread_id
            .map(str::to_string)
            .or(answered)
            .unwrap_or_else(|| id.to_string());
        meta.id = id.to_string();
        labels::set_label_ids(&mut meta, &[labels::SENT.into()]);
        let change = HistoryChange::MessageAdded {
            id: meta.id.clone(),
            thread_id: meta.thread_id.clone(),
        };
        self.bodies.insert(meta.id.clone(), body);
        self.messages.insert(meta.id.clone(), meta);
        self.record(change);
    }

    /// Takes a draft's current message out of the mailbox, recorded in
    /// history, as saving over the draft, sending it or deleting it does.
    fn drop_draft_message(&mut self, draft_id: &str) {
        let Some(message_id) = self.draft_messages.remove(draft_id) else {
            return;
        };
        if let Some(gone) = self.messages.remove(&message_id) {
            self.record(HistoryChange::MessageDeleted {
                id: message_id,
                thread_id: gone.thread_id,
            });
        }
    }

    /// The time a new message is dated: the pinned clock, else now.
    fn now(&self) -> EpochMillis {
        self.clock.unwrap_or_else(crate::now_millis)
    }

    /// Whether any message of the thread carries Gmail's mute label.
    fn thread_is_muted(&self, thread_id: &str) -> bool {
        self.messages.values().any(|m| {
            m.thread_id == thread_id && has_label(m, labels::MUTE)
        })
    }

    /// The ids a search returns, newest first.
    fn search(&self, query: &str) -> Vec<String> {
        let query = Query::parse(query);
        let now = self.clock.unwrap_or_else(crate::now_millis);
        let mut hits: Vec<&MessageMeta> = self
            .messages
            .values()
            .filter(|m| query.matches(m, &self.labels, now))
            .collect();
        hits.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.id.cmp(&b.id)));
        hits.into_iter().map(|m| m.id.clone()).collect()
    }
}

impl GmailApi for FakeGmail {
    fn quota(&self) -> Option<&AccountQuota> {
        self.quota.as_deref()
    }

    async fn profile(&self) -> Result<Profile, GmailError> {
        self.call("users.getProfile", cost::PROFILE).await?;
        Ok(self.with(|s| Profile {
            email_address: s.email.clone(),
            history_id: s.history_id,
        }))
    }

    async fn labels(&self) -> Result<Vec<RemoteLabel>, GmailError> {
        self.call("users.labels.list", cost::LABELS).await?;
        Ok(self.with(|s| s.labels.clone()))
    }

    async fn list_messages(
        &self,
        query: &str,
        page_token: Option<&str>,
        page_size: u32,
    ) -> Result<MessagePage, GmailError> {
        self.listing(None, query, page_token, page_size).await
    }

    async fn list_labelled(
        &self,
        label_id: &str,
        query: &str,
        page_token: Option<&str>,
        page_size: u32,
    ) -> Result<MessagePage, GmailError> {
        self.listing(Some(label_id), query, page_token, page_size)
            .await
    }

    async fn message_metadata(&self, id: &str) -> Result<MessageMeta, GmailError> {
        self.call("users.messages.get", cost::GET).await?;
        self.with(|s| {
            s.metadata_fetches += 1;
            s.messages.get(id).cloned().ok_or(GmailError::NotFound)
        })
    }

    async fn thread_metadata(&self, thread_id: &str) -> Result<Vec<MessageMeta>, GmailError> {
        // Read before the call so a held call answers with the thread as it
        // was when the app asked, as a slow reply from Gmail does.
        let answer = self.with(|s| {
            let mut metas: Vec<MessageMeta> = s
                .messages
                .values()
                .filter(|m| m.thread_id == thread_id)
                .cloned()
                .collect();
            if metas.is_empty() {
                return Err(GmailError::NotFound);
            }
            metas.sort_by_key(|m| m.date);
            Ok(metas)
        });
        self.call("users.threads.get", cost::THREAD).await?;
        answer
    }

    async fn history(
        &self,
        start: u64,
        page_token: Option<&str>,
    ) -> Result<HistoryPage, GmailError> {
        self.call("users.history.list", cost::HISTORY).await?;
        self.with(|s| {
            if start < s.history_floor {
                return Err(GmailError::NotFound);
            }
            let pending: Vec<HistoryChange> = s
                .history
                .iter()
                .filter(|(h, _)| *h > start)
                .map(|(_, c)| c.clone())
                .collect();
            let offset = page_token
                .and_then(|t| t.parse::<usize>().ok())
                .unwrap_or(0);
            let end = (offset + s.page_size).min(pending.len());
            Ok(HistoryPage {
                changes: pending[offset.min(end)..end].to_vec(),
                next_page_token: (end < pending.len()).then(|| end.to_string()),
                history_id: s.history_id,
            })
        })
    }

    async fn modify_labels(
        &self,
        id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), GmailError> {
        self.call("users.messages.modify", cost::MODIFY).await?;
        self.with(|s| {
            s.remote_writes.push(format!(
                "modify {id} +{} -{}",
                add.join(","),
                remove.join(",")
            ))
        });
        let add: Vec<&str> = add.iter().map(String::as_str).collect();
        let remove: Vec<&str> = remove.iter().map(String::as_str).collect();
        self.remote_relabel(id, &add, &remove);
        Ok(())
    }

    async fn batch_modify(
        &self,
        ids: &[String],
        add: &[String],
        remove: &[String],
    ) -> Result<(), GmailError> {
        assert!(
            ids.len() <= BATCH_LIMIT,
            "batch of {} exceeds Gmail's limit of {BATCH_LIMIT}",
            ids.len()
        );
        self.call("users.messages.batchModify", cost::BATCH_MODIFY)
            .await?;
        self.with(|s| {
            s.remote_writes.push(format!(
                "batchModify {} +{} -{}",
                ids.join(","),
                add.join(","),
                remove.join(",")
            ))
        });
        let add: Vec<&str> = add.iter().map(String::as_str).collect();
        let remove: Vec<&str> = remove.iter().map(String::as_str).collect();
        for id in ids {
            self.remote_relabel(id, &add, &remove);
        }
        Ok(())
    }

    async fn delete_messages(&self, ids: &[String]) -> Result<(), GmailError> {
        self.call("users.messages.batchDelete", cost::BATCH_DELETE)
            .await?;
        self.needs(DELETE_SCOPE)?;
        self.with(|s| s.remote_writes.push(format!("delete {}", ids.join(","))));
        for id in ids {
            self.remote_delete(id);
        }
        Ok(())
    }

    async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<String, GmailError> {
        self.call("users.messages.send", cost::SEND).await?;
        self.with(|s| {
            s.sent.push((raw.to_vec(), thread_id.map(str::to_string)));
            let id = format!("sent{}", s.sent.len());
            s.file_sent(&id, raw, thread_id);
            s.lost.pop_front().map_or(Ok(id), Err)
        })
    }

    async fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SavedDraft, GmailError> {
        match draft_id {
            Some(_) => self.call("users.drafts.update", cost::DRAFT_UPDATE).await?,
            None => self.call("users.drafts.create", cost::DRAFT_CREATE).await?,
        }
        self.with(|s| {
            let id = match draft_id {
                Some(id) if !s.drafts.contains_key(id) => return Err(GmailError::NotFound),
                Some(id) => id.to_string(),
                None => format!("draft{}", s.drafts.len() + 1),
            };
            // Each save gives the draft a new message, which history
            // records as the old one leaving and the new one arriving.
            s.drop_draft_message(&id);
            let message_id = format!("{id}-m{}", s.history_id + 1);
            let thread_id = thread_id.map_or_else(|| format!("{id}-t"), str::to_string);
            let mut draft = meta(&message_id, &thread_id, s.now(), &[labels::DRAFT]);
            draft.from = Some(Address {
                name: s.display_name.clone(),
                email: s.email.clone(),
            });
            draft.subject = header(raw, "Subject").unwrap_or_default();
            // Gmail lists a draft by its opening words. With a reader for
            // sent mail, the fake can do the same.
            if let Some(copy) = s.sent_copy.as_ref().and_then(|read| read(raw)) {
                draft.snippet = copy.meta.snippet;
            }
            s.messages.insert(message_id.clone(), draft);
            s.record(HistoryChange::MessageAdded {
                id: message_id.clone(),
                thread_id: thread_id.clone(),
            });
            s.drafts.insert(id.clone(), raw.to_vec());
            s.draft_messages.insert(id.clone(), message_id.clone());
            Ok(SavedDraft {
                thread_id,
                draft_id: id,
                message_id,
            })
        })
    }

    async fn send_draft(&self, draft_id: &str) -> Result<String, GmailError> {
        self.call("users.drafts.send", cost::SEND).await?;
        self.with(|s| {
            let raw = s.drafts.remove(draft_id).ok_or(GmailError::NotFound)?;
            s.drop_draft_message(draft_id);
            let id = format!("sent{}", s.sent.len() + 1);
            s.file_sent(&id, &raw, None);
            s.sent.push((raw, None));
            s.lost.pop_front().map_or(Ok(id), Err)
        })
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), GmailError> {
        self.call("users.drafts.delete", cost::DRAFT_DELETE).await?;
        self.with(|s| {
            s.drop_draft_message(draft_id);
            s.drafts
                .remove(draft_id)
                .map(|_| ())
                .ok_or(GmailError::NotFound)
        })
    }

    async fn list_drafts(&self) -> Result<Vec<DraftRef>, GmailError> {
        // Gmail charges per page and the real client follows every page
        // token, so the count of drafts is what this costs.
        let pages = self.with(|s| s.draft_messages.len().div_ceil(s.page_size).max(1));
        for _ in 0..pages {
            self.call("users.drafts.list", cost::DRAFT_LIST).await?;
        }
        Ok(self.with(|s| {
            s.draft_messages
                .iter()
                .map(|(draft_id, message_id)| DraftRef {
                    draft_id: draft_id.clone(),
                    message_id: message_id.clone(),
                })
                .collect()
        }))
    }

    async fn send_as(&self) -> Result<Vec<SendAs>, GmailError> {
        self.call("users.settings.sendAs.list", cost::SEND_AS)
            .await?;
        Ok(self.with(|s| {
            let mut all = vec![SendAs {
                send_as_email: s.email.clone(),
                display_name: s.display_name.clone().unwrap_or_default(),
                is_default: true,
                is_primary: true,
                signature: s.signature.as_deref().map(signature_html).unwrap_or_default(),
                verification_status: None,
            }];
            all.extend(s.send_as.iter().cloned());
            all
        }))
    }

    async fn attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, GmailError> {
        self.call("users.messages.attachments.get", cost::ATTACHMENT)
            .await?;
        self.with(|s| {
            s.attachments
                .get(&(message_id.to_string(), attachment_id.to_string()))
                .cloned()
                .ok_or(GmailError::NotFound)
        })
    }

    async fn vacation(&self) -> Result<Vacation, GmailError> {
        self.call("users.settings.getVacation", cost::SETTINGS)
            .await?;
        self.needs(SETTINGS_SCOPE)?;
        Ok(self.with(|s| s.vacation.clone()))
    }

    async fn set_vacation(&self, vacation: &Vacation) -> Result<(), GmailError> {
        self.call("users.settings.updateVacation", cost::SETTINGS)
            .await?;
        self.needs(SETTINGS_SCOPE)?;
        self.with(|s| s.vacation = vacation.clone());
        Ok(())
    }

    async fn answer_invitation(
        &self,
        ical_uid: &str,
        _me: &str,
        answer: Answer,
        occurrence: Option<EpochMillis>,
    ) -> Result<Answered, GmailError> {
        // The Calendar API spends none of the Gmail budget, so this call
        // is priced at nothing and only the failure queue applies.
        self.call("calendar.events.patch", 0).await?;
        self.calendar_open()?;
        Ok(self.with(|s| {
            s.answered_occurrences.push(occurrence);
            match s.calendar.get_mut(ical_uid) {
                Some(held) => {
                    *held = Some(answer);
                    Answered::Done
                }
                None => Answered::NotOnCalendar,
            }
        }))
    }

    async fn busy_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Result<Vec<Busy>, GmailError> {
        self.call("calendar.events.list", 0).await?;
        self.calendar_open()?;
        Ok(self.with(|s| {
            s.busy
                .iter()
                .filter(|(starts, ends, _)| *starts < to && *ends > from)
                .map(|(_, _, summary)| Busy {
                    uid: format!("busy-{summary}"),
                    summary: summary.clone(),
                })
                .collect()
        }))
    }

    /// Counts the occurrences left only for a rule with a `COUNT`, as the
    /// real client does, so a test sees the same answer Google would give.
    async fn series(
        &self,
        ical_uid: &str,
        from: EpochMillis,
    ) -> Result<Option<Series>, GmailError> {
        self.call("calendar.events.list", 0).await?;
        self.calendar_open()?;
        Ok(self.with(|s| {
            let (rule, starts) = s.series.get(ical_uid)?;
            let counted = rule.to_ascii_uppercase().contains("COUNT=");
            Some(Series {
                rule: rule.clone(),
                left: counted.then(|| starts.iter().filter(|&&at| at >= from).count() as u32),
            })
        }))
    }

    async fn events_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Result<Vec<Event>, GmailError> {
        self.call("calendar.events.list", 0).await?;
        self.calendar_open()?;
        let mut events: Vec<Event> = self.with(|s| {
            s.events
                .iter()
                .filter(|event| {
                    crate::calendar::span(event)
                        .is_some_and(|(starts, ends)| starts < to && ends.max(starts + 1) > from)
                })
                .cloned()
                .collect()
        });
        events.sort_by_key(|event| crate::calendar::span(event).map(|(starts, _)| starts));
        Ok(events)
    }

    async fn create_event(&self, fields: &EventFields) -> Result<Event, GmailError> {
        self.call("calendar.events.insert", 0).await?;
        self.calendar_open()?;
        Ok(self.with(|s| {
            s.next_event += 1;
            let mut event = Event {
                id: format!("event-{}", s.next_event),
                uid: format!("event-{}@google.com", s.next_event),
                busy: true,
                ..Event::default()
            };
            apply(&mut event, fields);
            s.events.push(event.clone());
            event
        }))
    }

    async fn update_event(&self, id: &str, fields: &EventFields) -> Result<Event, GmailError> {
        self.call("calendar.events.patch", 0).await?;
        self.calendar_open()?;
        self.with(|s| {
            let event = s.events.iter_mut().find(|e| e.id == id)?;
            apply(event, fields);
            Some(event.clone())
        })
        .ok_or(GmailError::NotFound)
    }

    async fn delete_event(&self, id: &str) -> Result<(), GmailError> {
        self.call("calendar.events.delete", 0).await?;
        self.calendar_open()?;
        self.with(|s| {
            let before = s.events.len();
            s.events.retain(|e| e.id != id);
            if s.events.len() == before {
                Err(GmailError::NotFound)
            } else {
                Ok(())
            }
        })
    }

    async fn calendars(&self) -> Result<Vec<calendar::Calendar>, GmailError> {
        self.call("calendar.calendarList.list", 0).await?;
        self.calendar_open()?;
        self.needs(CALENDAR_LIST_SCOPE)?;
        Ok(self.with(|s| s.calendars.clone()))
    }

    /// The whole calendar without a token, one page of `page_size` events
    /// at a time; with one, every event the log touched since.
    async fn event_changes(
        &self,
        calendar: &str,
        token: Option<&str>,
        page: Option<&str>,
        _from: EpochMillis,
    ) -> Result<calendar::EventPage, GmailError> {
        self.call("calendar.events.list", 0).await?;
        self.calendar_open()?;
        self.calendar_held(calendar)?;
        self.with(|s| {
            let end = s.calendar_log.len().to_string();
            match token {
                Some(_) if s.expire_calendar_tokens => Err(GmailError::ExpiredSyncToken),
                Some(token) => {
                    let since: usize = token.parse().unwrap_or(0);
                    let mut page = calendar::EventPage { next_sync: Some(end), ..calendar::EventPage::default() };
                    let mut seen = std::collections::HashSet::new();
                    for (cal, id, _) in s.calendar_log.iter().skip(since).rev() {
                        if cal != calendar || !seen.insert(id.clone()) {
                            continue;
                        }
                        match s.calendar_events.iter().find(|e| &e.calendar == cal && &e.id == id) {
                            Some(event) => page.events.push(event.clone()),
                            None => page.removed.push(id.clone()),
                        }
                    }
                    Ok(page)
                }
                None => {
                    let from: usize = page.and_then(|p| p.parse().ok()).unwrap_or(0);
                    let all: Vec<calendar::Event> =
                        s.calendar_events.iter().filter(|e| e.calendar == calendar).cloned().collect();
                    let slice: Vec<calendar::Event> = all.iter().skip(from).take(s.page_size).cloned().collect();
                    let next = from + slice.len();
                    let more = next < all.len();
                    Ok(calendar::EventPage {
                        events: slice,
                        removed: Vec::new(),
                        next_page: more.then(|| next.to_string()),
                        next_sync: (!more).then_some(end),
                    })
                }
            }
        })
    }

    /// A create answers 409 when the id is already on the calendar and a
    /// change answers `NotFound` when it is not, as Google does.
    async fn put_event(
        &self,
        event: &calendar::Event,
        etag: Option<&str>,
        create: bool,
    ) -> Result<calendar::Event, GmailError> {
        self.call(if create { "calendar.events.insert" } else { "calendar.events.patch" }, 0).await?;
        self.calendar_open()?;
        self.calendar_held(&event.calendar)?;
        let held = self.with(|s| {
            s.calendar_events.iter().find(|e| e.calendar == event.calendar && e.id == event.id).cloned()
        });
        match (&held, create) {
            (Some(_), true) => return Err(GmailError::Http { status: 409, body: "duplicate".into() }),
            // An occurrence of a series has an id before anyone changes
            // it; the first change makes it an event of its own.
            (None, false) if self.series_of(&event.calendar, &event.id).is_some() => {}
            (None, false) => return Err(self.deleted()),
            _ => {}
        }
        if let (Some(held), Some(etag)) = (&held, etag)
            && held.etag != etag
        {
            return Err(GmailError::Changed);
        }
        let mut stored = event.clone();
        stored.pending = false;
        if stored.uid.is_empty() {
            stored.uid = format!("{}@google.com", stored.id);
        }
        self.put_calendar_event(stored);
        Ok(self.with(|s| {
            s.calendar_events
                .iter()
                .find(|e| e.calendar == event.calendar && e.id == event.id)
                .cloned()
                .expect("just stored")
        }))
    }

    async fn remove_event(&self, calendar: &str, id: &str, etag: Option<&str>) -> Result<(), GmailError> {
        self.call("calendar.events.delete", 0).await?;
        self.calendar_open()?;
        self.calendar_held(calendar)?;
        let held = self.with(|s| s.calendar_events.iter().find(|e| e.calendar == calendar && e.id == id).cloned());
        match (held, etag) {
            // Deleting one occurrence of a series leaves a cancelled
            // occurrence behind, which the change feed hands out.
            (None, _) => match self.series_of(calendar, id) {
                Some((series, start)) => {
                    self.put_calendar_event(calendar::Event {
                        id: id.to_string(),
                        rules: Vec::new(),
                        status: calendar::Status::Cancelled,
                        series: Some(series.id.clone()),
                        original_start: Some(start),
                        start,
                        end: start + (series.end - series.start),
                        ..series
                    });
                    Ok(())
                }
                None => Err(self.deleted()),
            },
            (Some(held), Some(etag)) if held.etag != etag => Err(GmailError::Changed),
            _ => {
                self.drop_calendar_event(calendar, id);
                Ok(())
            }
        }
    }

    async fn create_label(&self, name: &str) -> Result<RemoteLabel, GmailError> {
        self.call("users.labels.create", cost::LABELS).await?;
        Ok(self.with(|s| {
            let label = RemoteLabel {
                id: format!("Label_{}", s.labels.len() + 1),
                name: name.to_string(),
                kind: Some("user".into()),
                color: None,
            };
            s.labels.push(label.clone());
            label
        }))
    }

    async fn rename_label(&self, id: &str, name: &str) -> Result<RemoteLabel, GmailError> {
        self.call("users.labels.patch", cost::LABELS).await?;
        self.with(|s| {
            let label = s
                .labels
                .iter_mut()
                .find(|l| l.id == id)
                .ok_or(GmailError::NotFound)?;
            label.name = name.to_string();
            Ok(label.clone())
        })
    }

    async fn delete_label(&self, id: &str) -> Result<(), GmailError> {
        self.call("users.labels.delete", cost::LABELS).await?;
        self.with(|s| {
            s.labels.retain(|l| l.id != id);
            for message in s.messages.values_mut() {
                edit_labels(message, |ids| ids.retain(|l| l != id));
            }
        });
        Ok(())
    }

    async fn filters(&self) -> Result<Vec<Filter>, GmailError> {
        self.call("users.settings.filters.list", cost::SETTINGS)
            .await?;
        self.needs(SETTINGS_SCOPE)?;
        Ok(self.with(|s| s.filters.clone()))
    }

    async fn create_filter(&self, filter: &Filter) -> Result<Filter, GmailError> {
        self.call("users.settings.filters.create", cost::SETTINGS)
            .await?;
        self.needs(SETTINGS_SCOPE)?;
        Ok(self.with(|s| {
            s.filters_made += 1;
            let created = Filter {
                id: Some(format!("filter{}", s.filters_made)),
                ..filter.clone()
            };
            s.filters.push(created.clone());
            created
        }))
    }

    async fn delete_filter(&self, id: &str) -> Result<(), GmailError> {
        self.call("users.settings.filters.delete", cost::SETTINGS)
            .await?;
        self.needs(SETTINGS_SCOPE)?;
        self.with(|s| {
            let before = s.filters.len();
            s.filters.retain(|f| f.id.as_deref() != Some(id));
            if s.filters.len() == before {
                Err(GmailError::NotFound)
            } else {
                Ok(())
            }
        })
    }

    /// The message as it arrived: the bytes a caller seeded in `raws`, or
    /// ones built from the stored metadata and body, which is enough for
    /// View Source, for a reply to quote, and for an export.
    async fn raw_message(&self, id: &str) -> Result<Vec<u8>, GmailError> {
        self.call("users.messages.get", cost::GET).await?;
        self.with(|s| {
            s.raw_fetches += 1;
            s.body_fetches += 1;
            match s.raws.get(id) {
                Some(raw) => Ok(raw.clone()),
                None => built_raw(s, id),
            }
        })
    }

    /// The message's `format=full` part tree: the same message
    /// `raw_message` would send, read back as Gmail's own part shape.
    async fn message_structure(&self, id: &str) -> Result<Message, GmailError> {
        self.call("users.messages.get", cost::GET).await?;
        self.with(|s| {
            s.structure_fetches += 1;
            s.body_fetches += 1;
            let meta = s.messages.get(id).cloned().ok_or(GmailError::NotFound)?;
            let raw = match s.raws.get(id) {
                Some(raw) => raw.clone(),
                None => built_raw(s, id)?,
            };
            let payload = gmail_payload(s, id, &raw);
            let label_ids = labels::label_ids(&meta);
            Ok(Message {
                id: id.to_string(),
                thread_id: meta.thread_id,
                label_ids,
                snippet: meta.snippet,
                internal_date: Some(meta.date),
                size_estimate: raw.len() as i64,
                payload: Some(payload),
            })
        })
    }

    async fn set_label_color(
        &self,
        id: &str,
        color: &LabelColor,
    ) -> Result<RemoteLabel, GmailError> {
        self.call("users.labels.patch", cost::LABELS).await?;
        self.with(|s| {
            let label = s
                .labels
                .iter_mut()
                .find(|l| l.id == id)
                .ok_or(GmailError::NotFound)?;
            label.color = Some(color.clone());
            Ok(label.clone())
        })
    }

    /// Contacts a page at a time, with the page token counting from zero.
    /// A sync token means nothing changed, so the reply is empty and
    /// carries the same token back.
    async fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<ConnectionsPage, GmailError> {
        self.call("people.connections.list", cost::CONNECTIONS)
            .await?;
        self.needs(CONTACTS_SCOPE)?;
        if let Some(token) = sync_token {
            return Ok(ConnectionsPage {
                next_sync_token: Some(token.to_string()),
                ..ConnectionsPage::default()
            });
        }
        let from: usize = page_token.and_then(|t| t.parse().ok()).unwrap_or(0);
        self.with(|s| {
            let page: Vec<Person> = s
                .contacts
                .iter()
                .skip(from)
                .take(s.page_size)
                .cloned()
                .collect();
            let next = from + page.len();
            let more = next < s.contacts.len();
            Ok(ConnectionsPage {
                people: page,
                deleted: Vec::new(),
                next_page_token: more.then(|| next.to_string()),
                next_sync_token: (!more).then(|| format!("sync-{}", s.contacts.len())),
            })
        })
    }

    async fn contact_photo(&self, url: &str) -> Result<Vec<u8>, GmailError> {
        self.with(|s| s.photos.get(url).cloned())
            .ok_or(GmailError::NotFound)
    }

    /// Counts the threads whose messages carry the label, as Gmail's
    /// `threadsTotal` does.
    async fn label_threads(&self, id: &str) -> Result<u64, GmailError> {
        self.call("users.labels.get", cost::LABELS).await?;
        self.with(|s| {
            if !s.labels.iter().any(|l| l.id == id) {
                return Err(GmailError::NotFound);
            }
            let threads: BTreeSet<&str> = s
                .messages
                .values()
                .filter(|m| has_label(m, id))
                .map(|m| m.thread_id.as_str())
                .collect();
            Ok(threads.len() as u64)
        })
    }

    async fn create_contact(&self, fields: &ContactFields) -> Result<Person, GmailError> {
        self.call("people.createContact", cost::CONTACT_WRITE)
            .await?;
        self.needs(CONTACTS_WRITE_SCOPE)?;
        self.with(|s| {
            let mut person = Person {
                resource: format!("people/c{}", s.contacts.len() + 1),
                ..Person::default()
            };
            fill_person(&mut person, fields);
            s.contacts.push(person.clone());
            Ok(person)
        })
    }

    async fn update_contact(
        &self,
        resource: &str,
        fields: &ContactFields,
    ) -> Result<Person, GmailError> {
        self.call("people.updateContact", cost::CONTACT_WRITE)
            .await?;
        self.needs(CONTACTS_WRITE_SCOPE)?;
        self.with(|s| {
            let person = s
                .contacts
                .iter_mut()
                .find(|p| p.resource == resource)
                .ok_or(GmailError::NotFound)?;
            fill_person(person, fields);
            Ok(person.clone())
        })
    }
}

/// Writes the fields a contact change names over `person`, as Google does.
fn fill_person(person: &mut Person, fields: &ContactFields) {
    if let Some(name) = &fields.name {
        person.name = Some(name.clone()).filter(|n| !n.is_empty());
    }
    if let Some(emails) = &fields.emails {
        person.emails = emails.clone();
    }
    if let Some(phones) = &fields.phones {
        person.phone = phones.first().cloned();
    }
    if let Some(organization) = &fields.organization {
        person.organization = Some(organization.clone()).filter(|o| !o.is_empty());
    }
}

/// The HTML Gmail would hold for a signature the fake keeps as text: one
/// line of it per line of text.
fn signature_html(text: &str) -> String {
    text.lines()
        .map(|line| {
            line.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        })
        .collect::<Vec<_>>()
        .join("<br>")
}

/// The message Gmail's `format=raw` would send for a fixture: the
/// metadata's headers, the body's text and HTML as an alternative, its
/// calendar inline, and each file with the bytes seeded under its handle
/// in `attachments`.
fn built_raw(state: &FakeState, id: &str) -> Result<Vec<u8>, GmailError> {
    let meta = state.messages.get(id).ok_or(GmailError::NotFound)?;
    let body = state.bodies.get(id).cloned().unwrap_or_else(|| MessageBody {
        text: Some(meta.snippet.clone()),
        ..MessageBody::default()
    });
    let address = |a: &Address| (a.name.clone().unwrap_or_default(), a.email.clone());
    let mut message = MessageBuilder::new()
        .subject(meta.subject.as_str())
        .date(meta.date / 1000);
    // A message with nobody in To or Cc carries no such header.
    if !meta.to.is_empty() {
        message = message.to(meta.to.iter().map(address).collect::<Vec<_>>());
    }
    if !meta.cc.is_empty() {
        message = message.cc(meta.cc.iter().map(address).collect::<Vec<_>>());
    }
    if let Some(from) = &meta.from {
        message = message.from(address(from));
    }
    if let Some(msgid) = &meta.rfc822_msgid {
        message = message.message_id(msgid.trim_matches(['<', '>']));
    }
    if let Some(header) = &body.list_unsubscribe {
        message = message.header("List-Unsubscribe", Raw::new(header.as_str()));
    }
    if body.one_click_unsubscribe {
        message = message.header("List-Unsubscribe-Post", Raw::new("List-Unsubscribe=One-Click"));
    }
    // The headers the details panel reads its three lines from, so a
    // fixture's provenance survives the trip through the raw message.
    let provenance = &body.provenance;
    if let Some(domain) = &provenance.mailed_by {
        message = message.header("Return-Path", Raw::new(format!("<bounces@{domain}>")));
    }
    if let Some(domain) = &provenance.signed_by {
        let signature = format!("v=1; a=rsa-sha256; d={domain}; s=fake");
        message = message.header("DKIM-Signature", Raw::new(signature));
    }
    if let Some(encrypted) = provenance.encrypted {
        let with = if encrypted { "ESMTPS" } else { "SMTP" };
        let received = format!("from mail.example by mx.example with {with} id fake");
        message = message.header("Received", Raw::new(received));
    }
    let mut readable = Vec::new();
    if let Some(text) = &body.text {
        readable.push(MimePart::new("text/plain", text.as_str()));
    }
    if let Some(html) = &body.html {
        readable.push(MimePart::new("text/html", html.as_str()));
    }
    if let Some(ics) = &body.calendar {
        readable.push(MimePart::new("text/calendar; method=REQUEST", ics.as_str()));
    }
    let text = match readable.len() {
        0 => MimePart::new("text/plain", ""),
        1 => readable.remove(0),
        _ => MimePart::new("multipart/alternative", readable),
    };
    let mut parts = vec![text];
    for file in &body.attachments {
        let bytes = file
            .attachment_id
            .as_ref()
            .and_then(|handle| state.attachments.get(&(id.to_string(), handle.clone())))
            .cloned()
            .unwrap_or_default();
        parts.push(match &file.content_id {
            // An inline picture keeps its name, as a mail program sends it.
            Some(cid) => MimePart::new(
                ContentType::new(file.mime_type.clone()).attribute("name", file.filename.clone()),
                bytes,
            )
            .header(
                "Content-Disposition",
                ContentType::new("inline").attribute("filename", file.filename.clone()),
            )
            .cid(cid.as_str()),
            None => MimePart::new(file.mime_type.clone(), bytes).attachment(file.filename.clone()),
        });
    }
    let content = MimePart::new("multipart/mixed", parts);
    let content = match body.protection {
        Some(protection) => protect(protection, content),
        None => content,
    };
    message
        .body(content)
        .write_to_vec()
        .map_err(|err| GmailError::Http {
            status: 500,
            body: err.to_string(),
        })
}

/// Wraps `content` the way `protection` would arrive on the wire, so a
/// fixture body that sets [`MessageBody::protection`] still reads back
/// as one. S/MIME's opaque shapes replace `content` outright: the real
/// message sits inside the signature or the envelope, not beside it,
/// and nothing here has a real one to put there.
fn protect<'x>(protection: Protection, content: MimePart<'x>) -> MimePart<'x> {
    const PGP_SIGNATURE: &str = "-----BEGIN PGP SIGNATURE-----\r\n-----END PGP SIGNATURE-----\r\n";
    match protection {
        Protection::Signed => MimePart::new(
            "multipart/signed; protocol=\"application/pgp-signature\"",
            vec![
                content,
                MimePart::new("application/pgp-signature", PGP_SIGNATURE).attachment("signature.asc"),
            ],
        ),
        Protection::SmimeSigned => MimePart::new(
            "multipart/signed; protocol=\"application/pkcs7-signature\"",
            vec![
                content,
                MimePart::new("application/pkcs7-signature", "sig").attachment("smime.p7s"),
            ],
        ),
        Protection::Encrypted => MimePart::new(
            "multipart/encrypted; protocol=\"application/pgp-encrypted\"",
            vec![
                MimePart::new("application/pgp-encrypted", "Version: 1"),
                MimePart::new("application/octet-stream", "data").attachment("encrypted.asc"),
            ],
        ),
        Protection::SmimeOpaque => {
            MimePart::new("application/pkcs7-mime; smime-type=signed-data; name=\"smime.p7m\"", "data")
                .attachment("smime.p7m")
        }
        Protection::SmimeEnveloped => MimePart::new(
            "application/pkcs7-mime; smime-type=enveloped-data; name=\"smime.p7m\"",
            "data",
        )
        .attachment("smime.p7m"),
    }
}

/// Gmail's `format=full` part tree for `raw`: Gmail's `partId`s, the
/// headers each part carries, and the bytes inline for a part without a
/// file name. A named part, the calendar text of an invitation among
/// them, comes by a handle `attachment` answers, as Gmail sends it.
fn gmail_payload(state: &mut FakeState, message_id: &str, raw: &[u8]) -> MessagePart {
    fn convert(state: &mut FakeState, message_id: &str, part: &mailrs_mime::Part, part_id: String) -> MessagePart {
        let mut content_type = part.mime_type.clone();
        if let Some(charset) = &part.charset {
            content_type.push_str(&format!("; charset=\"{charset}\""));
        }
        if let Some(protocol) = &part.protocol {
            content_type.push_str(&format!("; protocol=\"{protocol}\""));
        }
        if let Some(smime_type) = &part.smime_type {
            content_type.push_str(&format!("; smime-type={smime_type}"));
        }
        let mut headers = vec![Header {
            name: "Content-Type".into(),
            value: content_type,
        }];
        if part.attachment {
            headers.push(Header {
                name: "Content-Disposition".into(),
                value: "attachment".into(),
            });
        }
        if let Some(cid) = &part.content_id {
            headers.push(Header { name: "Content-ID".into(), value: format!("<{cid}>") });
        }
        if let Some(subject) = &part.subject {
            headers.push(Header { name: "Subject".into(), value: subject.clone() });
        }
        let named = part.filename.is_some();
        let mut body = PartBody { size: part.size, ..PartBody::default() };
        // Gmail sends a forwarded message's parts, not the message itself
        // as one blob, and gives it no handle.
        let data = match part.mime_type.as_str() {
            "message/rfc822" if !named => &None,
            _ => &part.data,
        };
        match (data, named) {
            (Some(bytes), false) => body.data = Some(URL_SAFE_NO_PAD.encode(bytes)),
            (Some(bytes), true) => {
                let handle = format!("ref-{message_id}-{part_id}");
                state.attachments.insert((message_id.into(), handle.clone()), bytes.clone());
                body.attachment_id = Some(handle);
            }
            (None, _) => {}
        }
        MessagePart {
            parts: part
                .children
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let id = match part_id.as_str() {
                        "" => i.to_string(),
                        parent => format!("{parent}.{i}"),
                    };
                    convert(state, message_id, c, id)
                })
                .collect(),
            part_id,
            mime_type: part.mime_type.clone(),
            filename: part.filename.clone().unwrap_or_default(),
            headers,
            body,
        }
    }
    let parts = mailrs_mime::parts(raw).unwrap_or_default();
    let mut payload = convert(state, message_id, &parts.root, String::new());
    // The root carries the message's own headers, as Gmail sends them,
    // in place of `convert`'s made-up Content-Type: the real one is
    // among them, with its own params `convert` cannot know (Content-
    // Type's `boundary`, and every other header besides).
    payload.headers = parts.headers.iter().map(|(n, v)| Header { name: n.clone(), value: v.clone() }).collect();
    payload
}

/// Leaves `sync`'s store as a new account's first sync against this
/// mailbox would: the labels, the history cursor, and every message in the
/// window, stored by sync's own bootstrap and backfill. The engine runs the
/// same steps a tick at a time; this runs them back to back, so the demo
/// and the assistant's tests start on a full store that cannot disagree
/// with what sync would have written.
pub async fn fill_store(sync: &AccountSync) -> Result<(), SyncError> {
    sync.bootstrap().await?;
    while sync.backfill_step().await? {}
    Ok(())
}

/// Writes what `fields` sets onto `event`, as Google's patch does. A guest
/// who stays on a new list keeps the answer they gave.
fn apply(event: &mut Event, fields: &EventFields) {
    if let Some(summary) = &fields.summary {
        event.summary = summary.clone();
    }
    if let Some(start) = &fields.start {
        event.start = Some(start.clone());
    }
    if let Some(end) = &fields.end {
        event.end = Some(end.clone());
    }
    if let Some(location) = &fields.location {
        event.location = location.clone();
    }
    if let Some(description) = &fields.description {
        event.description = description.clone();
    }
    if let Some(guests) = &fields.guests {
        event.guests = guests
            .iter()
            .map(|email| {
                event
                    .guests
                    .iter()
                    .find(|g| g.email.eq_ignore_ascii_case(email))
                    .cloned()
                    .unwrap_or_else(|| Guest {
                        email: email.clone(),
                        name: None,
                        answer: "needsAction".into(),
                        me: false,
                    })
            })
            .collect();
    }
}

/// The first header called `name` in RFC 822 bytes, unfolded no further
/// than the fake needs.
fn header(raw: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(raw);
    let head = text.split("\r\n\r\n").next().unwrap_or_default();
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
}
