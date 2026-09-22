//! An in-memory Gmail. Sync's tests and `penguin-mail --demo` both run on
//! it: seed its mailbox, hand it to an `AccountSync`, and every read and
//! write the app makes goes through the same code the real client does.
//!
//! Callers change the mailbox directly through [`FakeGmail::with`]. The
//! changes Gmail would record in history are recorded here too, so a sync
//! replays them. [`fill_store`] gives a new account the store its first
//! sync against this mailbox would leave, for callers that want mail on
//! screen before the engine starts.

mod query;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use mailrs_domain::invitation::Answer;
use mailrs_domain::{
    Address, EpochMillis, Filter, MessageBody, MessageMeta, Vacation, system_label,
};
use mailrs_gmail::{
    AccountQuota, Answered, BATCH_LIMIT, Busy, CALENDAR_SCOPE, CONTACTS_SCOPE,
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
    /// Page size for both listings and history.
    pub page_size: usize,
    /// Errors returned by the next calls, one per call.
    pub failures: VecDeque<GmailError>,
    /// Calls a test holds open, by method, until it lets them answer.
    held: HashMap<&'static str, Hold>,
    /// What the calls so far would have cost against the real API.
    pub usage: Usage,
    pub body_fetches: usize,
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
    /// The one-click unsubscribe URLs posted to, oldest first.
    pub unsubscribed: Vec<String>,
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
    MessageMeta {
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
        label_ids: labels.iter().map(|l| l.to_string()).collect(),
    }
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
                labels: vec![
                    label("INBOX", "system"),
                    label("UNREAD", "system"),
                    label("STARRED", "system"),
                    label("Label_1", "user"),
                ],
                messages: HashMap::new(),
                history: Vec::new(),
                bodies: HashMap::new(),
                page_size: 2,
                failures: VecDeque::new(),
                held: HashMap::new(),
                usage: Usage::default(),
                body_fetches: 0,
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
                unsubscribed: Vec::new(),
                contacts: Vec::new(),
                photos: HashMap::new(),
                calendar: HashMap::new(),
                busy: Vec::new(),
                series: HashMap::new(),
                answered_occurrences: Vec::new(),
                events: Vec::new(),
                next_event: 0,
                withheld: BTreeSet::new(),
                calendar_off: None,
                clock: None,
            }),
        }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut FakeState) -> R) -> R {
        f(&mut self.state.lock().expect("fake state poisoned"))
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
                meta.label_ids.retain(|l| l != system_label::INBOX);
                meta.label_ids.push(system_label::MUTE.into());
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
            for label in add {
                if !meta.has_label(label) {
                    meta.label_ids.push(label.to_string());
                }
            }
            meta.label_ids.retain(|l| !remove.contains(&l.as_str()));
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
            s.failures.pop_front().map_or(Ok(()), Err)
        })
    }

    /// Calls and units since the last reset.
    pub fn usage(&self) -> Usage {
        self.with(|s| s.usage.clone())
    }

    /// Starts counting again from zero.
    pub fn reset_usage(&self) {
        self.with(|s| s.usage = Usage::default());
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
        meta.label_ids = vec![system_label::SENT.into()];
        let change = HistoryChange::MessageAdded {
            id: meta.id.clone(),
            thread_id: meta.thread_id.clone(),
        };
        self.bodies.insert(meta.id.clone(), body);
        self.messages.insert(meta.id.clone(), meta);
        self.record(change);
    }

    /// Whether any message of the thread carries Gmail's mute label.
    fn thread_is_muted(&self, thread_id: &str) -> bool {
        self.messages
            .values()
            .any(|m| m.thread_id == thread_id && m.has_label(system_label::MUTE))
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
            let found = s.search(query);
            let end = (start + s.page_size).min(found.len());
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

    async fn message_metadata(&self, id: &str) -> Result<MessageMeta, GmailError> {
        self.call("users.messages.get", cost::GET).await?;
        self.with(|s| s.messages.get(id).cloned().ok_or(GmailError::NotFound))
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

    async fn message_body(&self, id: &str) -> Result<MessageBody, GmailError> {
        self.call("users.messages.get", cost::GET).await?;
        self.with(|s| {
            s.body_fetches += 1;
            s.bodies.get(id).cloned().ok_or(GmailError::NotFound)
        })
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

    async fn trash(&self, id: &str) -> Result<(), GmailError> {
        self.call("users.messages.trash", cost::TRASH).await?;
        self.with(|s| s.remote_writes.push(format!("trash {id}")));
        self.remote_relabel(id, &["TRASH"], &["INBOX"]);
        Ok(())
    }

    async fn untrash(&self, id: &str) -> Result<(), GmailError> {
        self.call("users.messages.untrash", cost::TRASH).await?;
        self.with(|s| s.remote_writes.push(format!("untrash {id}")));
        self.remote_relabel(id, &["INBOX"], &["TRASH"]);
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
        Ok(self.with(|s| {
            s.sent.push((raw.to_vec(), thread_id.map(str::to_string)));
            let id = format!("sent{}", s.sent.len());
            s.file_sent(&id, raw, thread_id);
            id
        }))
    }

    async fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        _thread_id: Option<&str>,
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
            let message_id = format!("{id}-m{}", raw.len());
            s.drafts.insert(id.clone(), raw.to_vec());
            s.draft_messages.insert(id.clone(), message_id.clone());
            Ok(SavedDraft {
                thread_id: format!("{id}-t"),
                draft_id: id,
                message_id,
            })
        })
    }

    async fn send_draft(&self, draft_id: &str) -> Result<String, GmailError> {
        self.call("users.drafts.send", cost::SEND).await?;
        self.with(|s| {
            let raw = s.drafts.remove(draft_id).ok_or(GmailError::NotFound)?;
            s.draft_messages.remove(draft_id);
            let id = format!("sent{}", s.sent.len() + 1);
            s.file_sent(&id, &raw, None);
            s.sent.push((raw, None));
            Ok(id)
        })
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), GmailError> {
        self.call("users.drafts.delete", cost::DRAFT_DELETE).await?;
        self.with(|s| {
            s.draft_messages.remove(draft_id);
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
                signature: s.signature.clone().unwrap_or_default(),
                verification_status: None,
            }];
            all.extend(s.send_as.iter().cloned());
            all
        }))
    }

    async fn display_name(&self) -> Result<Option<String>, GmailError> {
        self.call("users.settings.sendAs.list", cost::SEND_AS)
            .await?;
        Ok(self.with(|s| s.display_name.clone()))
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

    async fn signature(&self) -> Result<Option<String>, GmailError> {
        self.call("users.settings.sendAs.list", cost::SEND_AS)
            .await?;
        Ok(self.with(|s| s.signature.clone()))
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
                message.label_ids.retain(|l| l != id);
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

    async fn one_click_unsubscribe(&self, url: &str) -> Result<(), GmailError> {
        // The list's server is not Gmail, so nothing counts against the
        // quota, but a test can still make the post fail.
        self.with(|s| match s.failures.pop_front() {
            Some(err) => Err(err),
            None => {
                s.unsubscribed.push(url.to_string());
                Ok(())
            }
        })
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
            let meta = s.messages.get(id).ok_or(GmailError::NotFound)?;
            if let Some(raw) = s.raws.get(id) {
                return Ok(raw.clone());
            }
            let text = s
                .bodies
                .get(id)
                .and_then(|b| b.text.clone())
                .unwrap_or_else(|| meta.snippet.clone());
            let from = meta.from.as_ref().map(|a| a.email.as_str()).unwrap_or("");
            let date = chrono::DateTime::from_timestamp_millis(meta.date)
                .unwrap_or_default()
                .to_rfc2822();
            // A draft reopens from these bytes, so they carry the people.
            let mut people = String::new();
            for (header, list) in [("To", &meta.to), ("Cc", &meta.cc)] {
                if !list.is_empty() {
                    let named: Vec<String> = list
                        .iter()
                        .map(|a| match &a.name {
                            Some(name) => format!("\"{name}\" <{}>", a.email),
                            None => a.email.clone(),
                        })
                        .collect();
                    people.push_str(&format!("{header}: {}\r\n", named.join(", ")));
                }
            }
            Ok(format!(
                "From: {from}\r\n{people}Date: {date}\r\nSubject: {}\r\nMessage-ID: {}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n",
                meta.subject,
                meta.rfc822_msgid.clone().unwrap_or_default(),
                text.replace('\n', "\r\n")
            )
            .into_bytes())
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
                .filter(|m| m.label_ids.iter().any(|l| l == id))
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

/// Leaves `sync`'s store as a new account's first sync against this
/// mailbox would: the labels, the history cursor, and every message in the
/// window, stored by sync's own bootstrap and backfill. The engine runs the
/// same steps a tick at a time; this runs them back to back, so the demo
/// and the assistant's tests start on a full store that cannot disagree
/// with what sync would have written.
pub async fn fill_store(sync: &AccountSync<FakeGmail>) -> Result<(), SyncError> {
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
