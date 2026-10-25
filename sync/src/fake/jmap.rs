//! An in-memory JMAP server for sync's tests and the demo: mailboxes with
//! roles, messages held as raw bytes with their places and marks, a
//! change log per type the way a server keeps states, blobs, identities
//! and submissions. A state older than `forget_changes` cannot be
//! calculated, as a server that kept too little history answers.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use mailrs_jmap::{
    AddressBook, BodyPart, CALENDARS, CORE, CalendarEvents, CalendarRights, Canceled, CardChanges,
    CardWrite, CardsPage, ChangesResponse, Email, EmailAddress, EmailQuery, EventWrite, EventsPage,
    Got, HELD, Header, HeldSubmission, Hit, Hold, Identity, Imported, JmapCalendar, JmapError,
    Listed, MAIL, MDN, MailChanges, Mailbox, MailboxWrite, MaskedEmail, MaskedState, MdnOut,
    Pushed, QUOTA, Queried, Quota, SUBMISSION, Saved, Session, SessionAccount, SetError,
    SetResponse, SieveScript, Submission, SubmissionState, Thread, Threads, VacationResponse,
    max_delayed_send,
};
use serde_json::{Map, Value, json};
use tokio::sync::Notify;

use crate::services::jmap::JmapApi;
use crate::services::jmap::jscal::set_path;

type Answer<T> = Result<T, JmapError>;

const ACCOUNT: &str = "A1";

/// Whether any part below `part` is a file.
fn has_file(part: &mailrs_mime::Part) -> bool {
    part.attachment || part.children.iter().any(has_file)
}

/// `part` as a server describes it in `bodyStructure`.
fn body_part(part: &mailrs_mime::Part, blob: &str) -> BodyPart {
    let leaf = part.children.is_empty();
    BodyPart {
        part_id: leaf.then(|| part.path.clone()),
        blob_id: leaf.then(|| format!("{blob}/{}", part.path)),
        size: u64::try_from(part.size).unwrap_or_default(),
        mime_type: part.mime_type.clone(),
        charset: part.charset.clone(),
        disposition: part.attachment.then(|| "attachment".to_string()),
        cid: part.content_id.clone(),
        name: part.filename.clone(),
        sub_parts: (!leaf).then(|| part.children.iter().map(|c| body_part(c, blob)).collect()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeMailbox {
    pub name: String,
    pub parent_id: Option<String>,
    pub role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeEmail {
    pub raw: Vec<u8>,
    pub blob_id: String,
    pub thread_id: String,
    pub mailbox_ids: BTreeSet<String>,
    pub keywords: BTreeSet<String>,
    /// Milliseconds since 1970.
    pub received_at: i64,
}

/// A message the fake holds to send later, by its submission id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeHold {
    pub email_id: String,
    /// When it goes, in milliseconds.
    pub send_at: i64,
    /// `pending`, `final` or `canceled`.
    pub status: String,
    pub rcpt_to: Vec<String>,
}

/// One submission the server took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub identity_id: String,
    pub mail_from: String,
    pub rcpt_to: Vec<String>,
    pub raw: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Created,
    Updated,
    Destroyed,
}

pub struct JmapState {
    pub session: Session,
    pub mailboxes: BTreeMap<String, FakeMailbox>,
    pub emails: BTreeMap<String, FakeEmail>,
    pub blobs: HashMap<String, Vec<u8>>,
    pub identities: Vec<Identity>,
    pub sent: Vec<Sent>,
    /// Every method the adapter called, in order, for tests that count.
    pub calls: Vec<String>,
    /// Blobs that answer as larger than any cap, without holding the bytes.
    pub oversized: BTreeSet<String>,
    /// The most hits one `Email/query` answers, whatever limit it was given:
    /// RFC 8620 5.5 lets a server return fewer than asked.
    pub query_clamp: Option<usize>,
    /// Refuses `inMailbox` under an `OR` or a `NOT`, and a second
    /// `inMailbox` in one filter, as Apache James 3.9 does ("Nested
    /// inMailbox filters are not supported").
    pub flat_mailbox_filters: bool,
    /// Messages the server refuses to change or destroy, as `forbidden`.
    pub refused: BTreeSet<String>,
    /// Another client moves this message just before the next Email/set.
    pub move_before_set: Option<(String, String)>,
    /// The error type the server gives every submission, after the import
    /// it rides with was made.
    pub submission_refusal: Option<String>,
    /// The next submission goes out, and then its answer is lost on the
    /// way back, as a dropped connection does.
    pub lose_submission_answer: bool,
    /// The next submission goes out, and the client cannot tell from the
    /// answer or a look whether it did.
    pub unclear_submission_answer: bool,
    /// Keeps one copy of the same bytes, as Cyrus and Fastmail do: an
    /// import of a message already there answers that message.
    pub one_copy: bool,
    /// A mailbox id and the message count it claims, for a mailbox too big
    /// to fill in a test.
    pub total_override: Option<(String, u64)>,
    /// A message destroyed right after the next `Email/query` answers, as
    /// another client deletes it while a listing pages on.
    pub vanish_after_query: Option<String>,
    /// Refuses `anchor` in a query, as Apache James 3.9 does
    /// (`invalidArguments`).
    pub refuse_anchors: bool,
    /// From the next query with an anchor on, refuses this many queries,
    /// with or without one, as `invalidArguments` with this description.
    pub refuse_from_anchor: Option<(u32, String)>,
    refusing: Option<(u32, String)>,
    /// How many queries asked with an anchor.
    pub anchored_queries: u32,
    /// Refuses a new mailbox that names a role, as `invalidProperties`.
    pub refuse_mailbox_roles: bool,
    /// Refuses every new mailbox, as `forbidden`.
    pub refuse_mailbox_creates: bool,
    /// The automatic reply the server holds.
    pub vacation: VacationResponse,
    /// Contact cards by id, as JSContact JSON.
    pub cards: BTreeMap<String, Value>,
    pub books: Vec<AddressBook>,
    /// Sieve scripts by id: name, text, whether active.
    pub scripts: BTreeMap<String, (String, String, bool)>,
    pub masked: BTreeMap<String, MaskedEmail>,
    /// Holds by submission id.
    pub holds: BTreeMap<String, FakeHold>,
    pub quotas: Vec<Quota>,
    /// Receipts sent with `MDN/send`.
    pub mdns: Vec<MdnOut>,
    /// What the next `MDN/send` answers in place of its answer, once. A
    /// `Network` error is the lost answer: the receipt went. Any other
    /// is a refusal, and nothing went.
    pub mdn_failure: Option<JmapError>,
    /// Whether `EmailSubmission/get` answers; Apache James's does not.
    pub submission_get: bool,
    /// The answer the next hold gets in place of a hold, once. A refusal
    /// leaves nothing behind. [`JmapError::HoldUnknown`] is the lost
    /// answer: the server keeps the hold and the client never hears.
    pub hold_failure: Option<JmapError>,
    /// The next cancel arrives just after the server sent the hold and
    /// forgot it, so it answers `notFound`.
    pub cancel_races_send: bool,
    pub calendars: Vec<JmapCalendar>,
    /// Events by id, as JSCalendar.
    pub events: BTreeMap<String, Value>,
    /// Whether the server mails guests itself (`sendSchedulingMessages`).
    pub schedules: bool,
    /// Events written with `sendSchedulingMessages`, in order.
    pub scheduled: Vec<String>,
    /// How many event writes still answer `stateMismatch`, as when another
    /// client changed the calendar since the `ifInState` the write named.
    pub state_mismatches: u32,
    /// The next `CalendarEvent/set` lands, and then its answer is lost on
    /// the way back, as a dropped connection or a gateway timeout does.
    pub lose_event_answer: bool,
    /// How many `CalendarEvent/get` calls still fail, as the network does.
    pub failing_event_gets: u32,
    /// The answer the next `EmailSubmission/query` gets, once.
    pub submission_query_failure: Option<JmapError>,
    /// How long an upload takes.
    pub upload_delay: std::time::Duration,
    log: Vec<(u64, &'static str, String, Op)>,
    state: u64,
    forgotten: u64,
    next: u64,
    push: bool,
    push_ends: Option<std::time::Duration>,
    /// Until when push is paused after a stall, as the client keeps it.
    push_paused_until: Option<tokio::time::Instant>,
}

impl JmapState {
    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }

    fn record(&mut self, kind: &'static str, id: &str, op: Op) {
        self.state += 1;
        self.log.push((self.state, kind, id.to_string(), op));
    }

    /// The id of the mailbox with `role`.
    pub fn role_id(&self, role: &str) -> Option<String> {
        self.mailboxes
            .iter()
            .find(|(_, m)| m.role.as_deref() == Some(role))
            .map(|(id, _)| id.clone())
    }

    /// The most mailboxes one message may sit in, when the session says.
    fn mailbox_limit(&self) -> Option<u64> {
        let account = self.session.accounts.values().next()?;
        account
            .account_capabilities
            .get(MAIL)?
            .get("maxMailboxesPerEmail")?
            .as_u64()
    }

    /// Why a message may not sit in `places`, as a server refuses it.
    fn check_places(&self, places: &BTreeSet<String>) -> Result<(), SetError> {
        let invalid = |why: &str| SetError {
            kind: "invalidProperties".into(),
            description: Some(why.into()),
        };
        if places.is_empty() {
            return Err(invalid("an Email must be in a mailbox"));
        }
        if places.iter().any(|m| !self.mailboxes.contains_key(m)) {
            return Err(invalid("a mailbox does not exist"));
        }
        if self
            .mailbox_limit()
            .is_some_and(|limit| places.len() as u64 > limit)
        {
            return Err(invalid("an Email is in too many mailboxes"));
        }
        Ok(())
    }

    /// Records a mailbox change for every mailbox whose counts a message's
    /// change moves: where it came to or left, and all of its mailboxes when
    /// it appears, goes, or is read or unread.
    fn settle(&mut self, before: Option<&FakeEmail>, after: Option<&FakeEmail>) {
        let seen = |e: Option<&FakeEmail>| e.is_some_and(|e| e.keywords.contains("$seen"));
        let places = |e: Option<&FakeEmail>| e.map(|e| e.mailbox_ids.clone()).unwrap_or_default();
        let (was, now) = (places(before), places(after));
        let touched: BTreeSet<String> =
            if before.is_none() || after.is_none() || seen(before) != seen(after) {
                was.union(&now).cloned().collect()
            } else {
                was.symmetric_difference(&now).cloned().collect()
            };
        let touched: Vec<String> = touched
            .into_iter()
            .filter(|m| self.mailboxes.contains_key(m))
            .collect();
        for mailbox in touched {
            self.record("Mailbox", &mailbox, Op::Updated);
        }
    }

    /// A blob by id: a whole message, or one part of one as `blob/path`.
    fn blob(&self, id: &str) -> Option<Vec<u8>> {
        if let Some(whole) = self.blobs.get(id) {
            return Some(whole.clone());
        }
        let (blob, path) = id.split_once('/')?;
        let parts = mailrs_mime::parts(self.blobs.get(blob)?)?;
        parts.find(path)?.data.clone()
    }

    /// The Email as `Email/get` answers it. The structure and headers
    /// come only when `bodies` asks for them, as a server sends only the
    /// properties requested. Each leaf's blob id is the message's blob id,
    /// a slash and the part's section number.
    fn email(&self, id: &str, bodies: bool) -> Option<Email> {
        let held = self.emails.get(id)?;
        let parts = mailrs_mime::parts(&held.raw).unwrap_or_default();
        let addresses = |name: &str| {
            parts.header(name).map(|v| {
                mailrs_mime::address::parse_address_list(v)
                    .into_iter()
                    .map(|a| EmailAddress {
                        name: a.name,
                        email: a.email,
                    })
                    .collect::<Vec<_>>()
            })
        };
        let received =
            chrono::DateTime::from_timestamp_millis(held.received_at).unwrap_or_default();
        Some(Email {
            id: id.to_string(),
            blob_id: held.blob_id.clone(),
            thread_id: held.thread_id.clone(),
            mailbox_ids: held.mailbox_ids.iter().map(|m| (m.clone(), true)).collect(),
            keywords: held.keywords.iter().map(|k| (k.clone(), true)).collect(),
            size: held.raw.len() as u64,
            received_at: Some(received.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            message_id: parts
                .header("Message-ID")
                .map(|m| vec![m.trim().trim_matches(['<', '>']).to_string()]),
            from: addresses("From"),
            to: addresses("To"),
            cc: addresses("Cc"),
            subject: parts.header("Subject").map(str::to_string),
            preview: Some(
                String::from_utf8_lossy(&held.raw)
                    .lines()
                    .last()
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            ),
            has_attachment: has_file(&parts.root),
            list_unsubscribe: parts.header("List-Unsubscribe").map(str::to_string),
            list_unsubscribe_post: parts.header("List-Unsubscribe-Post").map(str::to_string),
            headers: bodies.then(|| {
                parts
                    .headers
                    .iter()
                    .map(|(name, value)| Header {
                        name: name.clone(),
                        value: value.clone(),
                    })
                    .collect()
            }),
            body_structure: bodies.then(|| body_part(&parts.root, &held.blob_id)),
        })
    }

    fn changes(&self, kind: &'static str, since: &str, max: usize) -> Answer<ChangesResponse> {
        let since: u64 = since
            .parse()
            .map_err(|_| JmapError::CannotCalculateChanges)?;
        if since < self.forgotten || since > self.state {
            return Err(JmapError::CannotCalculateChanges);
        }
        let mut seen: BTreeMap<String, Op> = BTreeMap::new();
        let mut new_state = since;
        let mut more = false;
        for (at, _, id, op) in self
            .log
            .iter()
            .filter(|(at, k, _, _)| *at > since && *k == kind)
        {
            if seen.len() >= max && !seen.contains_key(id) {
                more = true;
                break;
            }
            new_state = *at;
            let merged = match (seen.get(id), op) {
                (Some(Op::Created), Op::Updated) => Op::Created,
                (_, op) => *op,
            };
            seen.insert(id.clone(), merged);
        }
        if !more {
            new_state = self.state.max(since);
        }
        let pick = |want: Op| {
            seen.iter()
                .filter(|(_, op)| **op == want)
                .map(|(id, _)| id.clone())
                .collect()
        };
        Ok(ChangesResponse {
            old_state: since.to_string(),
            new_state: new_state.to_string(),
            has_more_changes: more,
            created: pick(Op::Created),
            updated: pick(Op::Updated),
            destroyed: pick(Op::Destroyed),
            updated_properties: None,
        })
    }

    /// Whether `email` meets `filter`. Like a server, it refuses an
    /// operator or a condition it does not know (`unsupportedFilter`,
    /// RFC 8620 5.5) rather than matching everything.
    fn matches(&self, email: &FakeEmail, filter: &Value) -> Answer<bool> {
        let unsupported = |what: &str| JmapError::Method {
            kind: "unsupportedFilter".into(),
            description: format!("no {what}"),
        };
        if let Some(op) = filter.get("operator").and_then(Value::as_str) {
            let conditions: Vec<&Value> = filter["conditions"]
                .as_array()
                .map(|c| c.iter().collect())
                .unwrap_or_default();
            let mut each = Vec::with_capacity(conditions.len());
            for c in conditions {
                each.push(self.matches(email, c)?);
            }
            return match op {
                "AND" => Ok(each.iter().all(|m| *m)),
                "OR" => Ok(each.iter().any(|m| *m)),
                "NOT" => Ok(!each.iter().any(|m| *m)),
                _ => Err(unsupported("such operator")),
            };
        }
        let text = String::from_utf8_lossy(&email.raw).to_lowercase();
        let parts = mailrs_mime::parts(&email.raw).unwrap_or_default();
        let header_has = |name: &str, needle: &Value| {
            needle.as_str().is_none_or(|n| {
                parts
                    .header(name)
                    .is_some_and(|v| v.to_lowercase().contains(&n.to_lowercase()))
            })
        };
        let millis = |v: &Value| {
            v.as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.timestamp_millis())
        };
        let Some(conditions) = filter.as_object() else {
            return Err(unsupported("filter object"));
        };
        for (key, value) in conditions {
            let holds = match key.as_str() {
                "inMailbox" => value
                    .as_str()
                    .is_some_and(|m| email.mailbox_ids.contains(m)),
                "inMailboxOtherThan" => value
                    .as_array()
                    .is_some_and(|ms| email.mailbox_ids.iter().any(|m| !ms.iter().any(|x| x == m))),
                "after" => millis(value).is_some_and(|at| email.received_at >= at),
                "before" => millis(value).is_some_and(|at| email.received_at < at),
                "hasKeyword" => value.as_str().is_some_and(|k| email.keywords.contains(k)),
                "notKeyword" => value.as_str().is_some_and(|k| !email.keywords.contains(k)),
                "from" => header_has("From", value),
                "to" => header_has("To", value),
                "cc" => header_has("Cc", value),
                "bcc" => header_has("Bcc", value),
                "subject" => header_has("Subject", value),
                "text" | "body" => value
                    .as_str()
                    .is_some_and(|t| text.contains(&t.to_lowercase())),
                "hasAttachment" => value.as_bool() == Some(has_file(&parts.root)),
                "minSize" => value.as_u64().is_some_and(|n| email.raw.len() as u64 >= n),
                "maxSize" => value.as_u64().is_some_and(|n| (email.raw.len() as u64) < n),
                "header" => {
                    let pair: Vec<&str> = value
                        .as_array()
                        .map(|a| a.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default();
                    match pair.as_slice() {
                        [name] => parts.header(name).is_some(),
                        [name, wanted] => parts
                            .header(name)
                            .is_some_and(|v| v.trim() == wanted.trim()),
                        _ => false,
                    }
                }
                _ => return Err(unsupported("such condition")),
            };
            if !holds {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

pub struct FakeJmap {
    state: Mutex<JmapState>,
    pushed: Notify,
}

fn session(with: &[&str], mailboxes_per_email: Option<u64>) -> Session {
    let capabilities: BTreeMap<String, Value> = std::iter::once((CORE.to_string(), json!({
        "maxCallsInRequest": 16, "maxObjectsInGet": 500, "maxObjectsInSet": 500, "maxSizeRequest": 10_000_000,
        "maxConcurrentRequests": 4, "maxSizeUpload": 50_000_000, "maxConcurrentUpload": 2
    })))
    .chain(with.iter().map(|c| (c.to_string(), json!({}))))
    .collect();
    let mut account_capabilities: BTreeMap<String, Value> =
        with.iter().map(|c| (c.to_string(), json!({}))).collect();
    account_capabilities.insert(
        MAIL.into(),
        json!({ "maxMailboxesPerEmail": mailboxes_per_email }),
    );
    Session {
        capabilities,
        accounts: BTreeMap::from([(
            ACCOUNT.to_string(),
            SessionAccount {
                name: "me@example.com".into(),
                is_personal: true,
                is_read_only: false,
                account_capabilities,
            },
        )]),
        primary_accounts: with
            .iter()
            .map(|c| (c.to_string(), ACCOUNT.to_string()))
            .collect(),
        username: "me@example.com".into(),
        api_url: "https://jmap.example.com/api/".into(),
        download_url: "https://jmap.example.com/download/{accountId}/{blobId}/{name}?type={type}"
            .into(),
        upload_url: "https://jmap.example.com/upload/{accountId}/".into(),
        event_source_url: Some("https://jmap.example.com/events/".into()),
        state: "s1".into(),
    }
}

impl Default for FakeJmap {
    fn default() -> Self {
        FakeJmap::new()
    }
}

impl FakeJmap {
    pub fn new() -> FakeJmap {
        let mut state = JmapState {
            session: session(&[MAIL, SUBMISSION], Some(1000)),
            mailboxes: BTreeMap::new(),
            emails: BTreeMap::new(),
            blobs: HashMap::new(),
            identities: vec![Identity {
                id: "I1".into(),
                name: "Me".into(),
                email: "me@example.com".into(),
                reply_to: None,
                bcc: None,
                text_signature: String::new(),
                html_signature: String::new(),
                may_delete: false,
            }],
            sent: Vec::new(),
            calls: Vec::new(),
            oversized: BTreeSet::new(),
            query_clamp: None,
            flat_mailbox_filters: false,
            refused: BTreeSet::new(),
            move_before_set: None,
            submission_refusal: None,
            lose_submission_answer: false,
            unclear_submission_answer: false,
            one_copy: false,
            total_override: None,
            vanish_after_query: None,
            refuse_anchors: false,
            refuse_from_anchor: None,
            refusing: None,
            anchored_queries: 0,
            refuse_mailbox_roles: false,
            refuse_mailbox_creates: false,
            vacation: VacationResponse::default(),
            cards: BTreeMap::new(),
            books: vec![AddressBook {
                id: "AB1".into(),
                name: "Personal".into(),
                is_default: true,
            }],
            scripts: BTreeMap::new(),
            masked: BTreeMap::new(),
            holds: BTreeMap::new(),
            quotas: Vec::new(),
            mdns: Vec::new(),
            mdn_failure: None,
            submission_get: true,
            hold_failure: None,
            submission_query_failure: None,
            cancel_races_send: false,
            upload_delay: std::time::Duration::ZERO,
            calendars: Vec::new(),
            events: BTreeMap::new(),
            schedules: true,
            scheduled: Vec::new(),
            state_mismatches: 0,
            lose_event_answer: false,
            failing_event_gets: 0,
            log: Vec::new(),
            state: 0,
            forgotten: 0,
            next: 0,
            push: true,
            push_ends: None,
            push_paused_until: None,
        };
        for (name, role) in [
            ("Inbox", "inbox"),
            ("Drafts", "drafts"),
            ("Sent", "sent"),
            ("Trash", "trash"),
            ("Junk Mail", "junk"),
            ("Archive", "archive"),
        ] {
            let id = format!("mb-{role}");
            state.mailboxes.insert(
                id,
                FakeMailbox {
                    name: name.into(),
                    parent_id: None,
                    role: Some(role.into()),
                },
            );
        }
        FakeJmap {
            state: Mutex::new(state),
            pushed: Notify::new(),
        }
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut JmapState) -> T) -> T {
        f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn called(&self, name: &str) {
        self.with(|s| s.calls.push(name.to_string()));
    }

    /// A server that takes one mailbox condition a filter, never under
    /// `OR` or `NOT`, as Apache James does.
    pub fn one_mailbox_per_filter(self) -> FakeJmap {
        self.with(|s| s.flat_mailbox_filters = true);
        self
    }

    /// Apache James 3.9: one mailbox condition a filter, and no `anchor`.
    pub fn like_james(self) -> FakeJmap {
        self.with(|s| {
            s.flat_mailbox_filters = true;
            s.refuse_anchors = true;
        });
        self
    }

    /// A server that answers at most `most` hits to a query.
    pub fn clamping_queries(self, most: usize) -> FakeJmap {
        self.with(|s| s.query_clamp = Some(most));
        self
    }

    /// A server whose `maxObjectsInSet` is `most`; a bigger `Email/set`
    /// answers `requestTooLarge`, as a server does.
    pub fn objects_in_set(self, most: usize) -> FakeJmap {
        self.with(|s| {
            if let Some(core) = s.session.capabilities.get_mut(CORE) {
                core["maxObjectsInSet"] = json!(most);
            }
        });
        self
    }

    /// A server that refuses every change to the message `id`.
    pub fn refusing(self, id: &str) -> FakeJmap {
        self.with(|s| s.refused.insert(id.to_string()));
        self
    }

    /// A server that refuses every submission as `kind`, as
    /// `EmailSubmission/set` does in `notCreated`.
    pub fn refusing_submissions(self, kind: &str) -> FakeJmap {
        self.with(|s| s.submission_refusal = Some(kind.to_string()));
        self
    }

    /// A server whose event stream closes as soon as it opens.
    pub fn with_push_that_ends_at_once(self) -> FakeJmap {
        self.with(|s| s.push_ends = Some(std::time::Duration::ZERO));
        self
    }

    /// A server whose event stream closes after it has been open for `held`.
    pub fn with_push_that_ends_after(self, held: std::time::Duration) -> FakeJmap {
        self.with(|s| s.push_ends = Some(held));
        self
    }

    /// Changes how long the stream stays open before it closes.
    pub fn set_push_ends_after(&self, held: std::time::Duration) {
        self.with(|s| s.push_ends = Some(held));
    }

    /// Pauses push for `long`, as a call stalled behind the stream does.
    pub fn pause_push_for(&self, long: std::time::Duration) {
        self.with(|s| s.push_paused_until = Some(tokio::time::Instant::now() + long));
    }

    /// How many times the event stream was opened.
    pub fn push_opens(&self) -> usize {
        self.with(|s| s.calls.iter().filter(|c| *c == "push").count())
    }

    /// A server that holds mail for up to `seconds`, as its submission
    /// capability's `maxDelayedSend` says.
    pub fn with_delayed_send(self, seconds: u64) -> FakeJmap {
        self.with(|s| {
            if let Some(account) = s.session.accounts.get_mut(ACCOUNT) {
                account
                    .account_capabilities
                    .insert(SUBMISSION.into(), json!({ "maxDelayedSend": seconds }));
            }
        });
        self
    }

    /// A server whose `EmailSubmission/get` answers `unknownMethod`.
    pub fn without_submission_get(self) -> FakeJmap {
        self.with(|s| s.submission_get = false);
        self
    }

    /// Sends every pending hold due by `now`, as the server would at its
    /// hour, and answers how many went. The copy stays where it waited.
    pub fn release_due(&self, now: i64) -> usize {
        self.with(|s| {
            let due: Vec<String> = s
                .holds
                .iter()
                .filter(|(_, h)| h.status == "pending" && h.send_at <= now)
                .map(|(id, _)| id.clone())
                .collect();
            for id in &due {
                let Some(hold) = s.holds.get_mut(id) else {
                    continue;
                };
                hold.status = "final".into();
                let (email_id, rcpt_to) = (hold.email_id.clone(), hold.rcpt_to.clone());
                let raw = s
                    .emails
                    .get(&email_id)
                    .map(|e| e.raw.clone())
                    .unwrap_or_default();
                s.sent.push(Sent {
                    identity_id: "I1".into(),
                    mail_from: "me@example.com".into(),
                    rcpt_to,
                    raw,
                });
            }
            due.len()
        })
    }

    pub fn without_push(self) -> FakeJmap {
        self.with(|s| {
            s.push = false;
            s.session.event_source_url = None;
        });
        self
    }

    /// A server offering JMAP Calendars in draft 32's shape, with one
    /// calendar, `cal-1`, the account's default.
    pub fn with_calendars(self) -> FakeJmap {
        let fake = self.with_capability(CALENDARS);
        fake.with(|s| {
            if let Some(account) = s.session.accounts.get_mut(ACCOUNT) {
                account.account_capabilities.insert(CALENDARS.into(), json!({
                    "maxCalendarsPerEvent": null, "minDateTime": "1900-01-01T00:00:00Z", "maxDateTime": "2200-01-01T00:00:00Z",
                    "maxExpandedQueryDuration": "P366D", "maxParticipantsPerEvent": null, "mayCreateCalendar": true
                }));
            }
            let rights = CalendarRights { may_read_items: true, may_write_all: true, may_write_own: true, may_rsvp: true, may_admin: true, may_delete: true };
            s.calendars.push(JmapCalendar {
                id: "cal-1".into(),
                name: "Personal".into(),
                color: Some("#e66100".into()),
                is_default: true,
                is_visible: true,
                time_zone: Some("Europe/Lisbon".into()),
                my_rights: rights,
                default_alerts_with_time: None,
                default_alerts_without_time: None,
            });
        });
        fake
    }

    /// Stores `object` as a new event under a fresh id, as another client
    /// would, and answers the id.
    pub fn add_event(&self, mut object: Value) -> String {
        self.with(|s| {
            let id = s.id("ev-");
            object["id"] = json!(id);
            object["updated"] = json!(format!("rev-{}", s.state + 1));
            s.events.insert(id.clone(), object);
            s.record("CalendarEvent", &id, Op::Created);
            id
        })
    }

    pub fn event(&self, id: &str) -> Option<Value> {
        self.with(|s| s.events.get(id).cloned())
    }

    pub fn with_capability(self, capability: &str) -> FakeJmap {
        self.with(|s| {
            s.session.capabilities.insert(capability.into(), json!({}));
            s.session
                .primary_accounts
                .insert(capability.into(), ACCOUNT.into());
            if let Some(account) = s.session.accounts.get_mut(ACCOUNT) {
                account
                    .account_capabilities
                    .insert(capability.into(), json!({}));
            }
        });
        self
    }

    /// Offers Sieve over JMAP with `extensions`.
    pub fn with_sieve(self, extensions: &[&str]) -> FakeJmap {
        let fake = self.with_capability(mailrs_jmap::SIEVE);
        fake.with(|s| {
            if let Some(account) = s.session.accounts.get_mut(ACCOUNT) {
                account.account_capabilities.insert(
                    mailrs_jmap::SIEVE.into(),
                    json!({ "sieveExtensions": extensions }),
                );
            }
        });
        fake
    }

    /// A card another client made.
    pub fn add_card(&self, name: &str, email: &str) -> String {
        self.with(|s| {
            let id = s.id("K");
            s.cards.insert(
                id.clone(),
                json!({
                    "id": id, "@type": "Card", "addressBookIds": { "AB1": true },
                    "name": { "full": name }, "emails": { "e1": { "address": email } }
                }),
            );
            s.record("ContactCard", &id, Op::Created);
            id
        })
    }

    pub fn without_capability(self, capability: &str) -> FakeJmap {
        self.with(|s| {
            s.session.capabilities.remove(capability);
            s.session.primary_accounts.remove(capability);
        });
        self
    }

    /// Keeps one copy of the same bytes per account, as Cyrus does.
    pub fn keeping_one_copy(self) -> FakeJmap {
        self.with(|s| s.one_copy = true);
        self
    }

    pub fn mailboxes_per_email(self, limit: Option<u64>) -> FakeJmap {
        self.with(|s| {
            if let Some(account) = s.session.accounts.get_mut(ACCOUNT) {
                account
                    .account_capabilities
                    .insert(MAIL.into(), json!({ "maxMailboxesPerEmail": limit }));
            }
        });
        self
    }

    pub fn mailbox(&self, role: &str) -> String {
        self.with(|s| s.role_id(role))
            .unwrap_or_else(|| panic!("the fake has no {role} mailbox"))
    }

    pub fn add_mailbox(&self, name: &str, parent: Option<&str>) -> String {
        self.with(|s| {
            let id = s.id("mb-");
            s.mailboxes.insert(
                id.clone(),
                FakeMailbox {
                    name: name.into(),
                    parent_id: parent.map(str::to_string),
                    role: None,
                },
            );
            s.record("Mailbox", &id, Op::Created);
            id
        })
    }

    /// Puts `raw` in the mailbox with `role`, received at `received_at`
    /// milliseconds, threaded with the stored message its `In-Reply-To`
    /// names.
    pub fn deliver(&self, role: &str, raw: &[u8], received_at: i64) -> String {
        self.with(|s| {
            let mailbox = s.role_id(role).expect("the role's mailbox");
            let (id, blob) = (s.id("M"), s.id("G"));
            let parts = mailrs_mime::parts(raw).unwrap_or_default();
            let parent = parts.header("In-Reply-To").map(|v| v.trim().to_string());
            let thread = parent
                .and_then(|wanted| {
                    s.emails.iter().find_map(|(_, e)| {
                        let p = mailrs_mime::parts(&e.raw).unwrap_or_default();
                        (p.header("Message-ID").map(str::trim) == Some(wanted.as_str()))
                            .then(|| e.thread_id.clone())
                    })
                })
                .unwrap_or_else(|| format!("T{id}"));
            s.blobs.insert(blob.clone(), raw.to_vec());
            s.emails.insert(
                id.clone(),
                FakeEmail {
                    raw: raw.to_vec(),
                    blob_id: blob,
                    thread_id: thread,
                    mailbox_ids: BTreeSet::from([mailbox.clone()]),
                    keywords: BTreeSet::new(),
                    received_at,
                },
            );
            s.record("Email", &id, Op::Created);
            let made = s.emails.get(&id).cloned();
            s.settle(None, made.as_ref());
            id
        })
    }

    pub fn set_keyword(&self, id: &str, keyword: &str, on: bool) {
        self.with(|s| {
            let email = s.emails.get_mut(id).expect("the message");
            let before = email.clone();
            match on {
                true => email.keywords.insert(keyword.to_string()),
                false => email.keywords.remove(keyword),
            };
            let after = email.clone();
            s.record("Email", id, Op::Updated);
            s.settle(Some(&before), Some(&after));
        });
    }

    pub fn move_to(&self, id: &str, mailbox: &str) {
        self.with(|s| {
            let email = s.emails.get_mut(id).expect("the message");
            let before = email.clone();
            email.mailbox_ids = BTreeSet::from([mailbox.to_string()]);
            let after = email.clone();
            s.record("Email", id, Op::Updated);
            s.settle(Some(&before), Some(&after));
        });
    }

    pub fn destroy(&self, id: &str) {
        self.with(|s| {
            let gone = s.emails.remove(id);
            s.record("Email", id, Op::Destroyed);
            s.settle(gone.as_ref(), None);
        });
    }

    /// Forgets every change before now, so an older state cannot be read
    /// while the current one reads as empty.
    pub fn forget_changes(&self) {
        self.with(|s| s.forgotten = s.state);
    }

    /// Tells a waiting push that something changed.
    pub fn poke(&self) {
        self.pushed.notify_one();
    }

    /// The query's page: from `position`, or right after `anchor` when
    /// given (`anchorOffset` 1), which a server that no longer lists it
    /// answers with `anchorNotFound`.
    fn listing(&self, query: &EmailQuery, anchor: Option<&str>) -> Answer<Queried> {
        self.with(|s| {
            if s.flat_mailbox_filters
                && query
                    .filter
                    .as_ref()
                    .is_some_and(mailbox_filter_james_refuses)
            {
                return Err(JmapError::Method {
                    kind: "unsupportedFilter".into(),
                    description: "Nested inMailbox filters are not supported".into(),
                });
            }
            let mut found: Vec<(&String, &FakeEmail)> = Vec::new();
            for (id, e) in &s.emails {
                if query
                    .filter
                    .as_ref()
                    .map_or(Ok(true), |f| s.matches(e, f))?
                {
                    found.push((id, e));
                }
            }
            found.sort_by(|a, b| b.1.received_at.cmp(&a.1.received_at).then(b.0.cmp(a.0)));
            let total = found.len() as u64;
            let start = match anchor {
                None => query.position as usize,
                Some(anchor) => match found.iter().position(|(id, _)| id.as_str() == anchor) {
                    Some(at) => at + 1,
                    None => {
                        return Err(JmapError::Method {
                            kind: "anchorNotFound".into(),
                            description: String::new(),
                        });
                    }
                },
            };
            let hits = found
                .into_iter()
                .skip(start)
                .take(
                    s.query_clamp
                        .map_or(query.limit, |clamp| clamp.min(query.limit)),
                )
                .map(|(id, e)| Hit {
                    id: id.clone(),
                    thread_id: e.thread_id.clone(),
                })
                .collect();
            // Any change moves the state, as on Cyrus.
            Ok(Queried {
                hits,
                position: start as u64,
                total: query.total.then_some(total),
                query_state: s.state.to_string(),
            })
        })
    }

    /// Refuses the query while `refuse_from_anchor` has refusals left.
    fn refuse_if_asked(&self) -> Answer<()> {
        self.with(|s| match s.refusing.take() {
            Some((left, description)) if left > 0 => {
                s.refusing = Some((left - 1, description.clone()));
                Err(JmapError::Method {
                    kind: "invalidArguments".into(),
                    description,
                })
            }
            _ => Ok(()),
        })
    }

    fn vanish_if_asked(&self) {
        if let Some(id) = self.with(|s| s.vanish_after_query.take()) {
            self.destroy(&id);
        }
    }

    /// Imports the blob. A server that keeps one copy of the same bytes
    /// files the copy already there instead, as the client does after an
    /// `alreadyExists`.
    fn import_now(
        s: &mut JmapState,
        blob_id: &str,
        mailboxes: &[String],
        keywords: &[&str],
    ) -> Answer<Imported> {
        let raw = s.blobs.get(blob_id).cloned().ok_or(JmapError::NotFound)?;
        if let Some((id, email)) = s
            .emails
            .iter_mut()
            .find(|(_, e)| e.raw == raw)
            .filter(|_| s.one_copy)
        {
            let (id, before) = (id.clone(), email.clone());
            email.mailbox_ids.extend(mailboxes.iter().cloned());
            email
                .keywords
                .extend(keywords.iter().map(|k| k.to_string()));
            let after = email.clone();
            s.settle(Some(&before), Some(&after));
            s.record("Email", &id, Op::Updated);
            return Ok(Imported {
                id,
                thread_id: after.thread_id,
            });
        }
        let places: BTreeSet<String> = mailboxes.iter().cloned().collect();
        s.check_places(&places).map_err(|refused| refused.error())?;
        let id = s.id("M");
        let thread = format!("T{id}");
        s.emails.insert(
            id.clone(),
            FakeEmail {
                raw,
                blob_id: blob_id.to_string(),
                thread_id: thread.clone(),
                mailbox_ids: mailboxes.iter().cloned().collect(),
                keywords: keywords.iter().map(|k| k.to_string()).collect(),
                received_at: chrono::Utc::now().timestamp_millis(),
            },
        );
        s.record("Email", &id, Op::Created);
        let made = s.emails.get(&id).cloned();
        s.settle(None, made.as_ref());
        Ok(Imported {
            id,
            thread_id: thread,
        })
    }
}

/// Whether `filter` holds a mailbox condition under an `OR` or a `NOT`,
/// or more than one `inMailbox`: Apache James refuses both.
fn mailbox_filter_james_refuses(filter: &Value) -> bool {
    fn walk(filter: &Value, under: bool, seen: &mut usize) -> bool {
        match filter.get("operator").and_then(Value::as_str) {
            Some(op) => {
                let under = under || op != "AND";
                filter["conditions"]
                    .as_array()
                    .is_some_and(|c| c.iter().any(|c| walk(c, under, seen)))
            }
            None => {
                *seen += usize::from(filter.get("inMailbox").is_some());
                let mailbox =
                    filter.get("inMailbox").is_some() || filter.get("inMailboxOtherThan").is_some();
                (under && mailbox) || *seen > 1
            }
        }
    }
    walk(filter, false, &mut 0)
}

impl JmapApi for FakeJmap {
    async fn session(&self) -> Answer<Arc<Session>> {
        Ok(Arc::new(self.with(|s| s.session.clone())))
    }

    async fn mailboxes(&self) -> Answer<Listed<Mailbox>> {
        self.called("Mailbox/get");
        Ok(self.with(|s| {
            let list = s
                .mailboxes
                .iter()
                .map(|(id, m)| {
                    let inside: Vec<&FakeEmail> = s
                        .emails
                        .values()
                        .filter(|e| e.mailbox_ids.contains(id))
                        .collect();
                    let unseen: Vec<&&FakeEmail> = inside
                        .iter()
                        .filter(|e| !e.keywords.contains("$seen"))
                        .collect();
                    let unread = unseen.len() as u64;
                    Mailbox {
                        id: id.clone(),
                        name: m.name.clone(),
                        parent_id: m.parent_id.clone(),
                        role: m.role.clone(),
                        total_emails: s
                            .total_override
                            .as_ref()
                            .filter(|(m, _)| m == id)
                            .map_or(inside.len() as u64, |(_, n)| *n),
                        unread_emails: unread,
                        total_threads: inside
                            .iter()
                            .map(|e| &e.thread_id)
                            .collect::<BTreeSet<_>>()
                            .len() as u64,
                        unread_threads: unseen
                            .iter()
                            .map(|e| &e.thread_id)
                            .collect::<BTreeSet<_>>()
                            .len() as u64,
                        is_subscribed: true,
                        ..Mailbox::default()
                    }
                })
                .collect();
            Listed {
                state: s.state.to_string(),
                list,
            }
        }))
    }

    async fn mail_changes(&self, email: &str, mailbox: &str, max: usize) -> Answer<MailChanges> {
        self.called("Email/changes");
        self.with(|s| {
            let changes = s.changes("Email", email, max)?;
            let changed = changes
                .created
                .iter()
                .chain(&changes.updated)
                .filter_map(|id| s.email(id, false))
                .collect();
            let mailbox = s.changes("Mailbox", mailbox, 10_000)?;
            Ok(MailChanges {
                email: changes,
                mailbox,
                changed,
            })
        })
    }

    async fn email_state(&self) -> Answer<String> {
        Ok(self.with(|s| s.state.to_string()))
    }

    async fn query(&self, query: &EmailQuery) -> Answer<Queried> {
        self.called("Email/query");
        self.refuse_if_asked()?;
        let answer = self.listing(query, None);
        self.vanish_if_asked();
        answer
    }

    async fn query_after(&self, query: &EmailQuery, anchor: &str) -> Answer<Queried> {
        self.called("Email/query");
        if self.with(|s| s.refuse_anchors) {
            return Err(JmapError::Method {
                kind: "invalidArguments".into(),
                description: "anchor is not supported".into(),
            });
        }
        self.with(|s| {
            s.anchored_queries += 1;
            s.refusing = s.refusing.take().or(s.refuse_from_anchor.take());
        });
        self.refuse_if_asked()?;
        let answer = self.listing(query, Some(anchor));
        self.vanish_if_asked();
        answer
    }

    async fn emails(&self, ids: &[String], properties: &[&str]) -> Answer<Got<Email>> {
        self.called("Email/get");
        Ok(self.with(|s| {
            let (mut list, mut not_found) = (Vec::new(), Vec::new());
            for id in ids {
                match s.email(id, properties.contains(&"bodyStructure")) {
                    Some(email) => list.push(email),
                    None => not_found.push(id.clone()),
                }
            }
            Got {
                state: s.state.to_string(),
                list,
                not_found,
            }
        }))
    }

    async fn threads(&self, ids: &[String]) -> Answer<Threads> {
        self.called("Thread/get");
        Ok(self.with(|s| {
            let mut out = Threads::default();
            for thread in ids {
                let mut members: Vec<(&String, &FakeEmail)> = s
                    .emails
                    .iter()
                    .filter(|(_, e)| &e.thread_id == thread)
                    .collect();
                if members.is_empty() {
                    out.not_found.push(thread.clone());
                    continue;
                }
                members.sort_by_key(|(_, e)| e.received_at);
                out.threads.push(Thread {
                    id: thread.clone(),
                    email_ids: members.iter().map(|(id, _)| (*id).clone()).collect(),
                });
                out.emails
                    .extend(members.iter().filter_map(|(id, _)| s.email(id, false)));
            }
            out
        }))
    }

    async fn set_emails(
        &self,
        update: &Map<String, Value>,
        destroy: &[String],
        state: Option<&str>,
    ) -> Answer<SetResponse> {
        self.called("Email/set");
        let too_many =
            self.with(|s| update.len() + destroy.len() > s.session.limits().max_objects_in_set);
        if too_many {
            return Err(JmapError::RequestTooLarge);
        }
        if let Some((id, mailbox)) = self.with(|s| s.move_before_set.take()) {
            self.move_to(&id, &mailbox);
        }
        self.with(|s| {
            if state.is_some_and(|state| state != s.state.to_string()) {
                return Err(JmapError::StateMismatch);
            }
            let mut answer = SetResponse::default();
            let (mut updated, mut not_updated) = (BTreeMap::new(), BTreeMap::new());
            for (id, patch) in update {
                let Some(email) = s.emails.get(id).cloned() else {
                    not_updated.insert(
                        id.clone(),
                        SetError {
                            kind: "notFound".into(),
                            description: None,
                        },
                    );
                    continue;
                };
                if s.refused.contains(id) {
                    not_updated.insert(
                        id.clone(),
                        SetError {
                            kind: "forbidden".into(),
                            description: None,
                        },
                    );
                    continue;
                }
                let mut next = email.clone();
                let invalid = |why: &str| SetError {
                    kind: "invalidProperties".into(),
                    description: Some(why.into()),
                };
                let mut refusal = None;
                for (path, value) in patch.as_object().into_iter().flatten() {
                    let unescaped = |name: &str| name.replace("~1", "/").replace("~0", "~");
                    let flag = |v: &Value| match v {
                        Value::Bool(true) => Some(true),
                        Value::Null => Some(false),
                        _ => None,
                    };
                    if path == "mailboxIds" {
                        match value
                            .as_object()
                            .filter(|m| m.values().all(|v| v == &Value::Bool(true)))
                        {
                            Some(m) => next.mailbox_ids = m.keys().cloned().collect(),
                            None => refusal = Some(invalid("mailboxIds must map ids to true")),
                        }
                    } else if let Some(mailbox) = path.strip_prefix("mailboxIds/") {
                        match flag(value) {
                            Some(true) => drop(next.mailbox_ids.insert(unescaped(mailbox))),
                            Some(false) => drop(next.mailbox_ids.remove(&unescaped(mailbox))),
                            None => {
                                refusal =
                                    Some(invalid("a mailbox is set to true or removed with null"))
                            }
                        }
                    } else if let Some(keyword) = path.strip_prefix("keywords/") {
                        match flag(value) {
                            Some(true) => drop(next.keywords.insert(unescaped(keyword))),
                            Some(false) => drop(next.keywords.remove(&unescaped(keyword))),
                            None => {
                                refusal =
                                    Some(invalid("a keyword is set to true or removed with null"))
                            }
                        }
                    } else {
                        refusal = Some(invalid("an unknown property"));
                    }
                }
                if let Some(refused) = refusal.or_else(|| s.check_places(&next.mailbox_ids).err()) {
                    not_updated.insert(id.clone(), refused);
                    continue;
                }
                s.emails.insert(id.clone(), next.clone());
                s.record("Email", id, Op::Updated);
                s.settle(Some(&email), Some(&next));
                updated.insert(id.clone(), None);
            }
            let (mut destroyed, mut not_destroyed) = (Vec::new(), BTreeMap::new());
            for id in destroy {
                if s.refused.contains(id) {
                    not_destroyed.insert(
                        id.clone(),
                        SetError {
                            kind: "forbidden".into(),
                            description: None,
                        },
                    );
                    continue;
                }
                match s.emails.remove(id) {
                    Some(gone) => {
                        s.record("Email", id, Op::Destroyed);
                        s.settle(Some(&gone), None);
                        destroyed.push(id.clone());
                    }
                    None => {
                        not_destroyed.insert(
                            id.clone(),
                            SetError {
                                kind: "notFound".into(),
                                description: None,
                            },
                        );
                    }
                }
            }
            answer.not_destroyed = (!not_destroyed.is_empty()).then_some(not_destroyed);
            answer.updated = Some(updated);
            answer.not_updated = (!not_updated.is_empty()).then_some(not_updated);
            answer.destroyed = Some(destroyed);
            answer.new_state = Some(s.state.to_string());
            Ok(answer)
        })
    }

    async fn set_mailbox(&self, write: &MailboxWrite) -> Answer<String> {
        self.called("Mailbox/set");
        self.with(|s| match write {
            MailboxWrite::Create {
                name,
                parent_id,
                role,
            } => {
                if s.refuse_mailbox_creates {
                    return Err(JmapError::Forbidden(String::new()));
                }
                if role.is_some() && s.refuse_mailbox_roles {
                    return Err(JmapError::Method {
                        kind: "invalidProperties".into(),
                        description: "role".into(),
                    });
                }
                let id = match role {
                    Some(role) => format!("mb-{role}"),
                    None => s.id("mb-"),
                };
                s.mailboxes.insert(
                    id.clone(),
                    FakeMailbox {
                        name: name.clone(),
                        parent_id: parent_id.clone(),
                        role: role.clone(),
                    },
                );
                s.record("Mailbox", &id, Op::Created);
                Ok(id)
            }
            MailboxWrite::Update {
                id,
                name,
                parent_id,
            } => {
                let mailbox = s.mailboxes.get_mut(id).ok_or(JmapError::NotFound)?;
                mailbox.name = name.clone();
                mailbox.parent_id = parent_id.clone();
                s.record("Mailbox", id, Op::Updated);
                Ok(id.clone())
            }
            MailboxWrite::Destroy { id } => {
                s.mailboxes.remove(id).ok_or(JmapError::NotFound)?;
                s.record("Mailbox", id, Op::Destroyed);
                Ok(id.clone())
            }
        })
    }

    async fn download(&self, blob_id: &str, limit: usize) -> Answer<Vec<u8>> {
        self.called("download");
        if self.with(|s| s.oversized.contains(blob_id)) {
            return Err(JmapError::AnswerTooLarge { limit });
        }
        let bytes = self.with(|s| s.blob(blob_id)).ok_or(JmapError::NotFound)?;
        match bytes.len() > limit {
            true => Err(JmapError::AnswerTooLarge { limit }),
            false => Ok(bytes),
        }
    }

    async fn upload(&self, bytes: Vec<u8>) -> Answer<String> {
        self.called("upload");
        let delay = self.with(|s| s.upload_delay);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        Ok(self.with(|s| {
            let id = s.id("G");
            s.blobs.insert(id.clone(), bytes);
            id
        }))
    }

    async fn import(
        &self,
        blob_id: &str,
        mailbox_ids: &[String],
        keywords: &[&str],
        destroy: Option<&str>,
    ) -> Answer<Saved> {
        self.called("Email/import");
        self.with(|s| {
            // As the client does: the same bytes held by a message that is
            // no draft, and not the one being replaced, are not taken over.
            let raw = s.blobs.get(blob_id);
            let taken = s
                .emails
                .iter()
                .find(|(_, e)| s.one_copy && Some(&e.raw) == raw);
            if let Some((id, email)) = taken
                && destroy != Some(id.as_str())
                && !email.keywords.contains("$draft")
            {
                return Err(JmapError::Method {
                    kind: "alreadyExists".into(),
                    description: String::new(),
                });
            }
            let made = FakeJmap::import_now(s, blob_id, mailbox_ids, keywords)?;
            // As the client does: the old draft goes in a second request,
            // and a refusal there comes back beside the new message.
            let leftover = destroy.filter(|old| *old != made.id).and_then(|old| {
                if s.refused.contains(old) {
                    return Some(JmapError::Forbidden(String::new()));
                }
                let Some(gone) = s.emails.remove(old) else {
                    return Some(JmapError::NotFound);
                };
                s.record("Email", old, Op::Destroyed);
                s.settle(Some(&gone), None);
                None
            });
            Ok(Saved {
                email: made,
                leftover,
            })
        })
    }

    async fn submit(&self, submission: &Submission) -> Answer<Imported> {
        self.called("EmailSubmission/set");
        self.with(|s| {
            // A server sends only from the identity's address, or from any
            // address in its domain for a `*@domain` identity.
            let from = submission.mail_from.to_ascii_lowercase();
            let owns = |i: &Identity| {
                let email = i.email.to_ascii_lowercase();
                match email.strip_prefix("*@") {
                    Some(domain) => from.rsplit_once('@').is_some_and(|(_, d)| d == domain),
                    None => email == from,
                }
            };
            if !s
                .identities
                .iter()
                .any(|i| i.id == submission.identity_id && owns(i))
            {
                return Err(JmapError::Method {
                    kind: "forbiddenFrom".into(),
                    description: String::new(),
                });
            }
            let home = submission
                .drafts
                .clone()
                .or(submission.sent.clone())
                .ok_or_else(|| {
                    JmapError::Forbidden("the account has neither Drafts nor Sent".into())
                })?;
            let keywords: &[&str] = match submission.drafts {
                Some(_) => &["$seen", "$draft"],
                None => &["$seen"],
            };
            let had = s.one_copy
                && s.blobs
                    .get(&submission.blob_id)
                    .is_some_and(|raw| s.emails.values().any(|e| &e.raw == raw));
            // As the client does: a copy the server keeps that is no draft
            // any more went out on an earlier try. It is filed again and
            // not sent twice.
            let raw = s.blobs.get(&submission.blob_id).cloned();
            let went = s.emails.iter().find(|(_, e)| {
                had && Some(&e.raw) == raw.as_ref() && !e.keywords.contains("$draft")
            });
            if let (Some((id, _)), Some(sent)) = (went, submission.sent.clone()) {
                let id = id.clone();
                if let Some(email) = s.emails.get_mut(&id) {
                    let before = email.clone();
                    email.mailbox_ids = BTreeSet::from([sent]);
                    let after = email.clone();
                    s.settle(Some(&before), Some(&after));
                    s.record("Email", &id, Op::Updated);
                }
                let thread_id = s.emails[&id].thread_id.clone();
                return Ok(Imported { id, thread_id });
            }
            let made = FakeJmap::import_now(s, &submission.blob_id, &[home], keywords)?;
            if let Some(kind) = s.submission_refusal.clone() {
                // The client destroys the copy when the submission fails,
                // unless the server had it before.
                if let Some(gone) = (!had).then(|| s.emails.remove(&made.id)).flatten() {
                    s.record("Email", &made.id, Op::Destroyed);
                    s.settle(Some(&gone), None);
                }
                return Err(mailrs_jmap::error::method_error(&kind, None));
            }
            let raw = s
                .blobs
                .get(&submission.blob_id)
                .cloned()
                .unwrap_or_default();
            s.sent.push(Sent {
                identity_id: submission.identity_id.clone(),
                mail_from: submission.mail_from.clone(),
                rcpt_to: submission.rcpt_to.clone(),
                raw,
            });
            // The client moves the copy only when it names both mailboxes.
            if let (Some(_), Some(sent)) = (&submission.drafts, &submission.sent)
                && let Some(email) = s.emails.get_mut(&made.id)
            {
                let before = email.clone();
                email.keywords.remove("$draft");
                email.mailbox_ids = BTreeSet::from([sent.clone()]);
                let after = email.clone();
                s.settle(Some(&before), Some(&after));
                s.record("Email", &made.id, Op::Updated);
            }
            if std::mem::take(&mut s.lose_submission_answer) {
                return Err(JmapError::Network("the answer was lost".into()));
            }
            if std::mem::take(&mut s.unclear_submission_answer) {
                return Err(JmapError::SubmitUnknown);
            }
            Ok(made)
        })
    }

    async fn calendars(&self) -> Answer<Vec<JmapCalendar>> {
        self.called("Calendar/get");
        self.with(|s| match s.session.offers(CALENDARS) {
            true => Ok(s.calendars.clone()),
            false => Err(JmapError::UnknownCapability(CALENDARS.into())),
        })
    }

    async fn calendar_event_changes(&self, since: &str, max: usize) -> Answer<ChangesResponse> {
        self.called("CalendarEvent/changes");
        self.with(|s| s.changes("CalendarEvent", since, max))
    }

    async fn calendar_events(&self, ids: &[String]) -> Answer<CalendarEvents> {
        self.called("CalendarEvent/get");
        self.with(|s| {
            if s.failing_event_gets > 0 {
                s.failing_event_gets -= 1;
                return Err(JmapError::Network("the read failed".into()));
            }
            let (found, missing): (Vec<&String>, Vec<&String>) =
                ids.iter().partition(|id| s.events.contains_key(*id));
            Ok(CalendarEvents {
                state: s.state.to_string(),
                list: found
                    .into_iter()
                    .filter_map(|id| s.events.get(id).cloned())
                    .collect(),
                not_found: missing.into_iter().cloned().collect(),
            })
        })
    }

    /// Filters on `inCalendar` and `uid`; `after` and `before` match
    /// everything here, which the adapter's tests do not lean on. Both must
    /// be LocalDateTime, as the drafts define them and Cyrus checks: a UTC
    /// time with `Z` is `invalidArguments`.
    async fn calendar_events_page(
        &self,
        filter: &Value,
        position: u64,
        limit: usize,
    ) -> Answer<EventsPage> {
        self.called("CalendarEvent/query");
        let local = |key: &str| {
            filter.get(key).and_then(Value::as_str).is_none_or(|t| {
                chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S").is_ok()
            })
        };
        if !local("after") || !local("before") {
            return Err(JmapError::Method {
                kind: "invalidArguments".into(),
                description: String::new(),
            });
        }
        self.with(|s| {
            let calendar = filter.get("inCalendar").and_then(Value::as_str);
            let uid = filter.get("uid").and_then(Value::as_str);
            let matching: Vec<Value> = s
                .events
                .values()
                .filter(|e| calendar.is_none_or(|c| e["calendarIds"][c] == json!(true)))
                .filter(|e| uid.is_none_or(|u| e["uid"] == json!(u)))
                .cloned()
                .collect();
            let total = matching.len() as u64;
            let events = matching
                .into_iter()
                .skip(usize::try_from(position).unwrap_or(usize::MAX))
                .take(limit)
                .collect();
            Ok(EventsPage {
                state: s.state.to_string(),
                events,
                total: Some(total),
            })
        })
    }

    async fn set_calendar_event(
        &self,
        write: &EventWrite,
        send: bool,
        state: Option<&str>,
    ) -> Answer<Option<Value>> {
        self.called("CalendarEvent/set");
        self.with(|s| {
            if state.is_some_and(|state| state != s.state.to_string()) {
                return Err(JmapError::StateMismatch);
            }
            if s.state_mismatches > 0 {
                s.state_mismatches -= 1;
                return Err(JmapError::StateMismatch);
            }
            let has_guests = |o: &Value| {
                o.get("participants")
                    .and_then(Value::as_object)
                    .is_some_and(|p| !p.is_empty())
            };
            let touched = match write {
                EventWrite::Create(object) => Some(Value::Object(object.clone())),
                EventWrite::Update { id, .. } | EventWrite::Destroy(id) => {
                    s.events.get(id).cloned()
                }
            };
            if send && !s.schedules && touched.as_ref().is_some_and(has_guests) {
                return Err(JmapError::Method {
                    kind: "noSupportedScheduleMethods".into(),
                    description: String::new(),
                });
            }
            let stamp = format!("rev-{}", s.state + 1);
            let (id, answer) = match write {
                EventWrite::Create(object) => {
                    let id = s.id("ev-");
                    let mut object = Value::Object(object.clone());
                    object["id"] = json!(id);
                    object["updated"] = json!(stamp);
                    s.events.insert(id.clone(), object);
                    s.record("CalendarEvent", &id, Op::Created);
                    (id.clone(), Some(json!({ "id": id })))
                }
                EventWrite::Update { id, patch } => {
                    let event = s.events.get_mut(id).ok_or(JmapError::NotFound)?;
                    for (path, value) in patch {
                        set_path(event, path, value.clone());
                    }
                    event["updated"] = json!(stamp);
                    s.record("CalendarEvent", id, Op::Updated);
                    (id.clone(), None)
                }
                EventWrite::Destroy(id) => {
                    s.events.remove(id).ok_or(JmapError::NotFound)?;
                    s.record("CalendarEvent", id, Op::Destroyed);
                    (id.clone(), None)
                }
            };
            if send {
                s.scheduled.push(id);
            }
            if std::mem::take(&mut s.lose_event_answer) {
                return Err(JmapError::Network("the answer was lost".into()));
            }
            Ok(answer)
        })
    }

    async fn identities(&self) -> Answer<Vec<Identity>> {
        Ok(self.with(|s| s.identities.clone()))
    }

    async fn vacation(&self) -> Answer<VacationResponse> {
        Ok(self.with(|s| s.vacation.clone()))
    }

    async fn set_vacation(&self, reply: &mailrs_jmap::VacationPatch) -> Answer<()> {
        self.called("VacationResponse/set");
        self.with(|s| s.vacation = reply.applied_to(&s.vacation));
        Ok(())
    }

    async fn address_books(&self) -> Answer<Vec<AddressBook>> {
        Ok(self.with(|s| s.books.clone()))
    }

    async fn cards_page(&self, position: u64, limit: usize) -> Answer<CardsPage> {
        self.called("ContactCard/query");
        Ok(self.with(|s| CardsPage {
            state: s.state.to_string(),
            cards: s
                .cards
                .values()
                .skip(position as usize)
                .take(limit)
                .cloned()
                .collect(),
            position,
            total: Some(s.cards.len() as u64),
        }))
    }

    async fn card_changes(&self, since: &str, max: usize) -> Answer<CardChanges> {
        self.called("ContactCard/changes");
        self.with(|s| {
            let changes = s.changes("ContactCard", since, max)?;
            let cards = changes
                .created
                .iter()
                .chain(&changes.updated)
                .filter_map(|id| s.cards.get(id).cloned())
                .collect();
            Ok(CardChanges { changes, cards })
        })
    }

    async fn card(&self, id: &str) -> Answer<Option<Value>> {
        Ok(self.with(|s| s.cards.get(id).cloned()))
    }

    async fn set_card(&self, write: &CardWrite) -> Answer<Value> {
        self.called("ContactCard/set");
        self.with(|s| match write {
            CardWrite::Create(card) => {
                let id = s.id("K");
                let mut card = card.clone();
                card.insert("id".into(), json!(id));
                s.cards.insert(id.clone(), Value::Object(card));
                s.record("ContactCard", &id, Op::Created);
                Ok(s.cards[&id].clone())
            }
            CardWrite::Update { id, patch } => {
                let card = s.cards.get_mut(id).ok_or(JmapError::NotFound)?;
                for (path, value) in patch {
                    patch_at(card, path, value)?;
                }
                let card = card.clone();
                s.record("ContactCard", id, Op::Updated);
                Ok(card)
            }
        })
    }

    async fn sieve_scripts(&self) -> Answer<Vec<SieveScript>> {
        Ok(self.with(|s| {
            s.scripts
                .iter()
                .map(|(id, (name, _, active))| SieveScript {
                    id: id.clone(),
                    name: Some(name.clone()),
                    blob_id: format!("blob-{id}"),
                    is_active: *active,
                })
                .collect()
        }))
    }

    async fn sieve_text(&self, blob_id: &str) -> Answer<String> {
        let id = blob_id.strip_prefix("blob-").ok_or(JmapError::NotFound)?;
        self.with(|s| s.scripts.get(id).map(|(_, text, _)| text.clone()))
            .ok_or(JmapError::NotFound)
    }

    async fn put_sieve(&self, name: &str, existing: Option<&str>, text: &str) -> Answer<String> {
        self.called("SieveScript/set");
        Ok(self.with(|s| {
            let id = existing.map(str::to_string).unwrap_or_else(|| s.id("S"));
            let active = s.scripts.get(&id).is_some_and(|(_, _, a)| *a);
            s.scripts
                .insert(id.clone(), (name.to_string(), text.to_string(), active));
            id
        }))
    }

    async fn activate_sieve(&self, id: &str) -> Answer<()> {
        self.with(|s| {
            for (script, (_, _, active)) in s.scripts.iter_mut() {
                *active = script == id;
            }
        });
        Ok(())
    }

    async fn sieve_extensions(&self) -> Answer<Vec<String>> {
        Ok(self.with(|s| {
            let listed = s
                .session
                .account_capability(mailrs_jmap::SIEVE)
                .and_then(|c| c.get("sieveExtensions"))
                .and_then(Value::as_array);
            listed
                .into_iter()
                .flatten()
                .filter_map(|e| e.as_str().map(str::to_string))
                .collect()
        }))
    }

    async fn hold(&self, hold: &Hold) -> Answer<HeldSubmission> {
        self.called("EmailSubmission/set");
        self.with(|s| {
            let failure = s.hold_failure.take();
            if let Some(err) = failure.clone().filter(|e| *e != JmapError::HoldUnknown) {
                return Err(err);
            }
            if hold.hold_for > max_delayed_send(&s.session) {
                return Err(JmapError::Method {
                    kind: "invalidArguments".into(),
                    description: "HOLDFOR".into(),
                });
            }
            if !s.identities.iter().any(|i| i.id == hold.identity_id) {
                return Err(JmapError::Method {
                    kind: "forbiddenFrom".into(),
                    description: String::new(),
                });
            }
            let made = FakeJmap::import_now(
                s,
                &hold.blob_id,
                std::slice::from_ref(&hold.drafts),
                &["$draft", "$seen", HELD],
            )?;
            let id = s.id("S");
            let wait = i64::try_from(hold.hold_for.saturating_mul(1000)).unwrap_or(i64::MAX / 2);
            let send_at = chrono::Utc::now().timestamp_millis().saturating_add(wait);
            s.holds.insert(
                id.clone(),
                FakeHold {
                    email_id: made.id.clone(),
                    send_at,
                    status: "pending".into(),
                    rcpt_to: hold.rcpt_to.clone(),
                },
            );
            match failure {
                Some(lost) => Err(lost),
                None => Ok(HeldSubmission {
                    id,
                    email_id: made.id,
                    thread_id: made.thread_id,
                }),
            }
        })
    }

    async fn submissions(&self, ids: &[String]) -> Answer<Vec<SubmissionState>> {
        self.called("EmailSubmission/get");
        self.with(|s| {
            if !s.submission_get {
                return Err(JmapError::UnknownCapability("unknownMethod".into()));
            }
            Ok(ids
                .iter()
                .filter_map(|id| {
                    let hold = s.holds.get(id)?;
                    let send_at = chrono::DateTime::from_timestamp_millis(hold.send_at)
                        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
                    let envelope = Some(mailrs_jmap::Envelope {
                        rcpt_to: hold
                            .rcpt_to
                            .iter()
                            .map(|email| mailrs_jmap::EnvelopeAddress {
                                email: email.clone(),
                            })
                            .collect(),
                    });
                    Some(SubmissionState {
                        id: id.clone(),
                        email_id: hold.email_id.clone(),
                        undo_status: hold.status.clone(),
                        send_at,
                        envelope,
                    })
                })
                .collect())
        })
    }

    async fn answers_submission_get(&self) -> Answer<bool> {
        self.called("EmailSubmission/get ids=[]");
        Ok(self.with(|s| s.submission_get))
    }

    async fn submissions_for(&self, email_id: &str) -> Answer<Vec<String>> {
        self.called("EmailSubmission/query");
        self.with(|s| match s.submission_query_failure.take() {
            Some(err) => Err(err),
            None => Ok(s
                .holds
                .iter()
                .filter(|(_, hold)| hold.email_id == email_id)
                .map(|(id, _)| id.clone())
                .collect()),
        })
    }

    async fn cancel_submission(&self, id: &str) -> Answer<Canceled> {
        self.called("EmailSubmission/set");
        self.with(|s| {
            if std::mem::take(&mut s.cancel_races_send)
                && let Some(hold) = s.holds.remove(id)
            {
                let raw = s
                    .emails
                    .get(&hold.email_id)
                    .map(|e| e.raw.clone())
                    .unwrap_or_default();
                s.sent.push(Sent {
                    identity_id: "I1".into(),
                    mail_from: "me@example.com".into(),
                    rcpt_to: hold.rcpt_to,
                    raw,
                });
                return Ok(Canceled::Gone);
            }
            // The client reads `notFound` as gone.
            let Some(hold) = s.holds.get_mut(id) else {
                return Ok(Canceled::Gone);
            };
            match hold.status.as_str() {
                "final" => Ok(Canceled::TooLate),
                _ => {
                    hold.status = "canceled".into();
                    Ok(Canceled::Canceled)
                }
            }
        })
    }

    async fn file_held(&self, email_id: &str, drafts: &str, sent: &str) -> Answer<()> {
        self.called("Email/set");
        self.with(|s| {
            let email = s.emails.get_mut(email_id).ok_or(JmapError::NotFound)?;
            email.mailbox_ids.remove(drafts);
            email.mailbox_ids.insert(sent.to_string());
            email.keywords.remove("$draft");
            email.keywords.remove(HELD);
            s.record("Email", email_id, Op::Updated);
            Ok(())
        })
    }

    async fn unhold(&self, email_id: &str) -> Answer<()> {
        self.called("Email/set");
        self.with(|s| {
            let email = s.emails.get_mut(email_id).ok_or(JmapError::NotFound)?;
            email.keywords.remove(HELD);
            s.record("Email", email_id, Op::Updated);
            Ok(())
        })
    }

    async fn quotas(&self) -> Answer<Vec<Quota>> {
        self.called("Quota/get");
        self.with(|s| match s.session.mail_quota_account().is_some() {
            true => Ok(s.quotas.clone()),
            false => Err(JmapError::UnknownCapability(QUOTA.into())),
        })
    }

    async fn send_mdn(&self, mdn: &MdnOut) -> Answer<()> {
        self.called("MDN/send");
        self.with(|s| {
            if !s.session.offers(MDN) {
                return Err(JmapError::UnknownCapability(MDN.into()));
            }
            let failure = s.mdn_failure.take();
            if let Some(refused) = failure
                .clone()
                .filter(|e| !matches!(e, JmapError::Network(_)))
            {
                return Err(refused);
            }
            let email = s
                .emails
                .get_mut(&mdn.for_email_id)
                .ok_or(JmapError::NotFound)?;
            email.keywords.insert("$mdnsent".into());
            s.mdns.push(mdn.clone());
            s.record("Email", &mdn.for_email_id, Op::Updated);
            failure.map_or(Ok(()), Err)
        })
    }

    async fn masked_emails(&self) -> Answer<Vec<MaskedEmail>> {
        Ok(self.with(|s| s.masked.values().cloned().collect()))
    }

    async fn create_masked(&self, for_domain: &str, description: &str) -> Answer<MaskedEmail> {
        self.called("MaskedEmail/set");
        Ok(self.with(|s| {
            let id = s.id("ME");
            let made = MaskedEmail {
                id: id.clone(),
                email: format!("masked.{id}@example.com").to_lowercase(),
                state: "enabled".into(),
                for_domain: for_domain.into(),
                description: description.into(),
                created_at: Some("2026-10-11T10:00:00Z".into()),
                last_message_at: None,
            };
            s.masked.insert(id, made.clone());
            made
        }))
    }

    async fn set_masked_state(&self, id: &str, state: MaskedState) -> Answer<()> {
        self.with(|s| {
            s.masked.get_mut(id).ok_or(JmapError::NotFound)?.state = state.as_str().to_string();
            Ok(())
        })
    }

    async fn push(&self) -> Answer<Pushed> {
        // The client waits out a pause before it opens a stream.
        if let Some(until) = self.with(|s| s.push_paused_until) {
            tokio::time::sleep_until(until).await;
        }
        self.called("push");
        if let Some(held) = self.with(|s| s.push_ends) {
            tokio::select! {
                () = tokio::time::sleep(held) => {}
                () = self.pushed.notified() => return Ok(Pushed::Changed),
            }
            return Err(JmapError::Network("the stream ended".into()));
        }
        if !self.with(|s| s.push) {
            return Ok(Pushed::NoPush);
        }
        self.pushed.notified().await;
        Ok(Pushed::Changed)
    }

    fn push_paused(&self) -> Option<tokio::time::Instant> {
        self.with(|s| {
            s.push_paused_until
                .filter(|until| *until > tokio::time::Instant::now())
        })
    }
}

/// Applies one PatchObject entry (RFC 8620 section 5.3): `path` is a JSON
/// pointer without its leading slash, and `null` removes what it names. A
/// path through a property the object lacks is an `invalidPatch`.
fn patch_at(object: &mut Value, path: &str, value: &Value) -> Result<(), JmapError> {
    let invalid = || JmapError::Method {
        kind: "invalidPatch".into(),
        description: String::new(),
    };
    let mut parts: Vec<&str> = path.split('/').collect();
    let last = parts.pop().ok_or_else(invalid)?;
    let mut at = object;
    for part in parts {
        at = at
            .get_mut(part)
            .filter(|v| v.is_object())
            .ok_or_else(invalid)?;
    }
    let map = at.as_object_mut().ok_or_else(invalid)?;
    match value {
        Value::Null => {
            map.remove(last);
        }
        _ => {
            map.insert(last.to_string(), value.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use mailrs_jmap::{EmailQuery, JmapError, MailboxWrite, PLACES, Pushed};
    use serde_json::{Map, json};

    use super::*;
    use crate::services::jmap::JmapApi;

    const RAW: &[u8] = b"From: Ada <ada@example.com>\r\nTo: me@example.com\r\nSubject: Hi\r\nMessage-ID: <a1@example.com>\r\n\r\nHello\r\n";

    #[tokio::test]
    async fn delivered_mail_shows_in_the_changes_since_the_last_state() {
        let fake = FakeJmap::new();
        let (email, mailbox) = (
            fake.email_state().await.unwrap(),
            fake.mailboxes().await.unwrap().state,
        );
        let id = fake.deliver("inbox", RAW, 1_000);
        let changes = fake.mail_changes(&email, &mailbox, 100).await.unwrap();
        assert_eq!(changes.email.created, std::slice::from_ref(&id));
        assert_eq!(
            changes.changed[0].mailbox_ids.keys().next(),
            Some(&fake.mailbox("inbox"))
        );
        let again = fake
            .mail_changes(&changes.email.new_state, &mailbox, 100)
            .await
            .unwrap();
        assert!(again.email.created.is_empty() && again.changed.is_empty());
    }

    #[tokio::test]
    async fn a_forgotten_state_cannot_be_calculated() {
        let fake = FakeJmap::new();
        let email = fake.email_state().await.unwrap();
        fake.deliver("inbox", RAW, 1_000);
        fake.forget_changes();
        assert_eq!(
            fake.mail_changes(&email, "0", 100).await.unwrap_err(),
            JmapError::CannotCalculateChanges
        );
    }

    #[tokio::test]
    async fn a_patch_that_empties_mailbox_ids_is_refused_like_a_server_would() {
        let fake = FakeJmap::new();
        let id = fake.deliver("inbox", RAW, 1_000);
        let mut update = Map::new();
        update.insert(
            id.clone(),
            json!({ format!("mailboxIds/{}", fake.mailbox("inbox")): null }),
        );
        let set = fake.set_emails(&update, &[], None).await.unwrap();
        assert_eq!(set.refusal(&id).unwrap().kind, "invalidProperties");
    }

    #[tokio::test]
    async fn a_query_filters_and_sorts_newest_first() {
        let fake = FakeJmap::new();
        let old = fake.deliver("inbox", RAW, 1_000);
        let new = fake.deliver("archive", RAW, 2_000);
        let all = fake
            .query(&EmailQuery {
                filter: None,
                position: 0,
                limit: 10,
                total: true,
            })
            .await
            .unwrap();
        assert_eq!(
            all.hits.iter().map(|h| h.id.clone()).collect::<Vec<_>>(),
            [new, old.clone()]
        );
        let inbox = json!({ "inMailbox": fake.mailbox("inbox") });
        let only = fake
            .query(&EmailQuery {
                filter: Some(inbox),
                position: 0,
                limit: 10,
                total: false,
            })
            .await
            .unwrap();
        assert_eq!(only.hits.len(), 1);
        let got = fake.emails(&[old], PLACES).await.unwrap();
        assert!(got.list[0].mailbox_ids.contains_key(&fake.mailbox("inbox")));
    }

    #[tokio::test]
    async fn an_unknown_filter_key_or_operator_is_unsupported_like_a_server_says() {
        let fake = FakeJmap::new();
        fake.deliver("inbox", RAW, 1_000);
        for filter in [
            json!({ "colour": "red" }),
            json!({ "operator": "XOR", "conditions": [] }),
        ] {
            let asked = fake
                .query(&EmailQuery {
                    filter: Some(filter),
                    position: 0,
                    limit: 10,
                    total: false,
                })
                .await;
            assert!(
                matches!(asked, Err(JmapError::Method { kind, .. }) if kind == "unsupportedFilter")
            );
        }
    }

    #[tokio::test]
    async fn a_submission_files_the_copy_in_sent_and_keeps_the_envelope() {
        let fake = FakeJmap::new();
        let blob = fake.upload(RAW.to_vec()).await.unwrap();
        let identity = fake.identities().await.unwrap()[0].id.clone();
        let submission = mailrs_jmap::Submission {
            blob_id: blob,
            identity_id: identity,
            mail_from: "me@example.com".into(),
            rcpt_to: vec!["ada@example.com".into()],
            drafts: Some(fake.mailbox("drafts")),
            sent: Some(fake.mailbox("sent")),
        };
        let sent = fake.submit(&submission).await.unwrap();
        fake.with(|s| {
            let email = &s.emails[&sent.id];
            assert!(email.mailbox_ids.contains(&s.role_id("sent").unwrap()));
            assert!(!email.keywords.contains("$draft"));
            assert_eq!(s.sent[0].rcpt_to, ["ada@example.com"]);
        });
    }

    #[tokio::test]
    async fn a_poke_wakes_push_and_no_push_says_so() {
        let fake = std::sync::Arc::new(FakeJmap::new());
        let waiting = tokio::spawn({
            let fake = std::sync::Arc::clone(&fake);
            async move { fake.push().await }
        });
        tokio::task::yield_now().await;
        fake.poke();
        assert_eq!(waiting.await.unwrap().unwrap(), Pushed::Changed);
        assert_eq!(
            FakeJmap::new().without_push().push().await.unwrap(),
            Pushed::NoPush
        );
    }

    #[tokio::test]
    async fn a_mailbox_made_under_a_parent_lists_with_it() {
        let fake = FakeJmap::new();
        let parent = fake.add_mailbox("Trips", None);
        let id = fake
            .set_mailbox(&MailboxWrite::Create {
                name: "2026".into(),
                parent_id: Some(parent.clone()),
                role: None,
            })
            .await
            .unwrap();
        let listed = fake.mailboxes().await.unwrap().list;
        assert!(
            listed
                .iter()
                .any(|m| m.id == id && m.parent_id.as_deref() == Some(parent.as_str()))
        );
    }

    async fn set(fake: &FakeJmap, id: &str, patch: Value) -> SetResponse {
        let mut update = Map::new();
        update.insert(id.to_string(), patch);
        fake.set_emails(&update, &[], None).await.unwrap()
    }

    #[tokio::test]
    async fn after_forgetting_the_current_state_reads_as_empty() {
        let fake = FakeJmap::new();
        fake.deliver("inbox", RAW, 1_000);
        fake.forget_changes();
        let (email, mailbox) = (
            fake.email_state().await.unwrap(),
            fake.mailboxes().await.unwrap().state,
        );
        let changes = fake.mail_changes(&email, &mailbox, 100).await.unwrap();
        assert!(changes.email.created.is_empty() && changes.changed.is_empty());
    }

    #[tokio::test]
    async fn a_state_from_the_future_cannot_be_calculated() {
        let fake = FakeJmap::new();
        assert_eq!(
            fake.mail_changes("99", "0", 100).await.unwrap_err(),
            JmapError::CannotCalculateChanges
        );
    }

    #[tokio::test]
    async fn every_write_that_moves_a_count_moves_the_mailbox_state() {
        let fake = FakeJmap::new();
        let id = fake.deliver("inbox", RAW, 1_000);
        let mut since = fake.mailboxes().await.unwrap().state;
        let step = |fake: &FakeJmap, since: &mut String| {
            let changes = fake.with(|s| s.changes("Mailbox", since, 100)).unwrap();
            *since = changes.new_state.clone();
            changes.updated
        };
        let inbox = fake.mailbox("inbox");
        fake.set_keyword(&id, "$seen", true);
        assert_eq!(step(&fake, &mut since), std::slice::from_ref(&inbox));
        fake.move_to(&id, &fake.mailbox("archive"));
        let mut both = step(&fake, &mut since);
        both.sort();
        let mut expected = [inbox.clone(), fake.mailbox("archive")];
        expected.sort();
        assert_eq!(both, expected);
        set(&fake, &id, json!({ "keywords/$seen": null })).await;
        assert_eq!(step(&fake, &mut since), [fake.mailbox("archive")]);
        let blob = fake.upload(RAW.to_vec()).await.unwrap();
        let made = fake
            .import(&blob, std::slice::from_ref(&inbox), &[], None)
            .await
            .unwrap()
            .email;
        assert_eq!(step(&fake, &mut since), std::slice::from_ref(&inbox));
        fake.destroy(&made.id);
        assert_eq!(step(&fake, &mut since), std::slice::from_ref(&inbox));
    }

    #[tokio::test]
    async fn a_patch_with_an_unknown_path_is_refused() {
        let fake = FakeJmap::new();
        let id = fake.deliver("inbox", RAW, 1_000);
        let set = set(&fake, &id, json!({ "subject": "New" })).await;
        assert_eq!(set.refusal(&id).unwrap().kind, "invalidProperties");
    }

    #[tokio::test]
    async fn a_keyword_set_to_anything_but_true_or_null_is_refused() {
        let fake = FakeJmap::new();
        let id = fake.deliver("inbox", RAW, 1_000);
        let set = set(&fake, &id, json!({ "keywords/$seen": false })).await;
        assert_eq!(set.refusal(&id).unwrap().kind, "invalidProperties");
    }

    #[tokio::test]
    async fn a_patch_into_a_mailbox_that_does_not_exist_is_refused() {
        let fake = FakeJmap::new();
        let id = fake.deliver("inbox", RAW, 1_000);
        let set = set(&fake, &id, json!({ "mailboxIds/nowhere": true })).await;
        assert_eq!(set.refusal(&id).unwrap().kind, "invalidProperties");
    }

    #[tokio::test]
    async fn a_patch_past_the_mailbox_limit_is_refused() {
        let fake = FakeJmap::new().mailboxes_per_email(Some(1));
        let id = fake.deliver("inbox", RAW, 1_000);
        let archive = fake.mailbox("archive");
        let set = set(&fake, &id, json!({ format!("mailboxIds/{archive}"): true })).await;
        assert_eq!(set.refusal(&id).unwrap().kind, "invalidProperties");
    }

    #[tokio::test]
    async fn an_import_into_no_mailbox_or_a_missing_one_is_refused() {
        let fake = FakeJmap::new();
        let blob = fake.upload(RAW.to_vec()).await.unwrap();
        let kind = |e: JmapError| match e {
            JmapError::Method { kind, .. } => kind,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            kind(fake.import(&blob, &[], &[], None).await.unwrap_err()),
            "invalidProperties"
        );
        assert_eq!(
            kind(
                fake.import(&blob, &["nowhere".into()], &[], None)
                    .await
                    .unwrap_err()
            ),
            "invalidProperties"
        );
        assert!(fake.with(|s| s.emails.is_empty()));
    }

    #[tokio::test]
    async fn destroying_an_unknown_id_answers_not_found() {
        let fake = FakeJmap::new();
        let set = fake
            .set_emails(&Map::new(), &["gone".into()], None)
            .await
            .unwrap();
        assert_eq!(set.refusal("gone").unwrap().kind, "notFound");
    }

    #[tokio::test]
    async fn cards_change_and_the_changes_name_them() {
        let fake = FakeJmap::new().with_capability(mailrs_jmap::CONTACTS);
        let first = fake.cards_page(0, 10).await.unwrap();
        let id = fake.add_card("Ada", "ada@example.com");
        let changed = fake.card_changes(&first.state, 100).await.unwrap();
        assert_eq!(changed.changes.created, [id]);
        assert_eq!(changed.cards[0]["name"]["full"], "Ada");
    }

    #[tokio::test]
    async fn a_sieve_script_is_kept_and_activated() {
        let fake = FakeJmap::new().with_sieve(&["fileinto", "vacation"]);
        let id = fake.put_sieve("penguin-mail", None, "keep;").await.unwrap();
        fake.activate_sieve(&id).await.unwrap();
        let scripts = fake.sieve_scripts().await.unwrap();
        assert!(scripts.iter().any(|s| s.id == id && s.is_active));
        assert_eq!(fake.sieve_text(&scripts[0].blob_id).await.unwrap(), "keep;");
        assert_eq!(
            fake.sieve_extensions().await.unwrap(),
            ["fileinto", "vacation"]
        );
    }

    #[tokio::test]
    async fn a_masked_address_turns_off_and_comes_back() {
        let fake = FakeJmap::new().with_capability(mailrs_jmap::MASKED_EMAIL);
        let made = fake
            .create_masked("https://shop.example", "Shop")
            .await
            .unwrap();
        fake.set_masked_state(&made.id, MaskedState::Disabled)
            .await
            .unwrap();
        assert_eq!(fake.masked_emails().await.unwrap()[0].state, "disabled");
        fake.set_masked_state(&made.id, MaskedState::Enabled)
            .await
            .unwrap();
        assert_eq!(fake.masked_emails().await.unwrap()[0].state, "enabled");
    }

    #[tokio::test]
    async fn changes_page_by_max_and_say_there_is_more() {
        let fake = FakeJmap::new();
        let since = fake.email_state().await.unwrap();
        let ids: Vec<String> = (0..3)
            .map(|n| fake.deliver("inbox", RAW, 1_000 + n))
            .collect();
        let first = fake.with(|s| s.changes("Email", &since, 2)).unwrap();
        assert!(first.has_more_changes);
        assert_eq!(first.created, ids[..2]);
        let rest = fake
            .with(|s| s.changes("Email", &first.new_state, 2))
            .unwrap();
        assert!(!rest.has_more_changes);
        assert_eq!(rest.created, ids[2..]);
    }

    fn hold_for(seconds: u64, blob: &str) -> mailrs_jmap::Hold {
        mailrs_jmap::Hold {
            blob_id: blob.into(),
            identity_id: "I1".into(),
            mail_from: "me@example.com".into(),
            rcpt_to: vec!["ada@example.com".into()],
            drafts: "mb-drafts".into(),
            hold_for: seconds,
        }
    }

    #[tokio::test]
    async fn a_hold_waits_until_released_and_then_cannot_be_called_back() {
        let fake = FakeJmap::new().with_delayed_send(86_400);
        let blob = fake.upload(RAW.to_vec()).await.unwrap();
        let held = fake.hold(&hold_for(3600, &blob)).await.unwrap();
        assert_eq!(
            fake.submissions(std::slice::from_ref(&held.id))
                .await
                .unwrap()[0]
                .undo_status,
            "pending"
        );
        assert_eq!(fake.release_due(i64::MAX), 1);
        assert_eq!(
            fake.with(|s| s.sent.len()),
            1,
            "released mail goes out once"
        );
        assert_eq!(
            fake.cancel_submission(&held.id).await.unwrap(),
            mailrs_jmap::Canceled::TooLate
        );
    }

    #[tokio::test]
    async fn a_hold_past_the_servers_reach_is_refused() {
        let fake = FakeJmap::new().with_delayed_send(60);
        let blob = fake.upload(RAW.to_vec()).await.unwrap();
        assert!(matches!(
            fake.hold(&hold_for(3600, &blob)).await,
            Err(JmapError::Method { .. })
        ));
    }

    #[tokio::test]
    async fn a_server_without_submission_get_answers_unknown_method() {
        let fake = FakeJmap::new()
            .with_delayed_send(86_400)
            .without_submission_get();
        assert!(!fake.answers_submission_get().await.unwrap());
        assert!(matches!(
            fake.submissions(&[]).await,
            Err(JmapError::UnknownCapability(_))
        ));
    }

    #[tokio::test]
    async fn a_receipt_needs_the_capability_and_marks_the_message() {
        let fake = FakeJmap::new();
        let id = fake.deliver("inbox", RAW, 1);
        let mdn = mailrs_jmap::MdnOut {
            for_email_id: id,
            identity_id: "I1".into(),
            subject: "s".into(),
            text_body: "t".into(),
        };
        assert!(matches!(
            fake.send_mdn(&mdn).await,
            Err(JmapError::UnknownCapability(_))
        ));
        let fake = FakeJmap::new().with_capability(mailrs_jmap::MDN);
        let id = fake.deliver("inbox", RAW, 1);
        let mdn = mailrs_jmap::MdnOut {
            for_email_id: id.clone(),
            ..mdn
        };
        fake.send_mdn(&mdn).await.unwrap();
        assert!(fake.with(|s| s.emails[&id].keywords.contains("$mdnsent")));
    }

    #[tokio::test]
    async fn a_refused_hold_leaves_nothing_in_drafts() {
        let fake = FakeJmap::new().with_delayed_send(86_400);
        let blob = fake.upload(RAW.to_vec()).await.unwrap();
        let before = fake.with(|s| s.emails.len());
        fake.with(|s| {
            s.hold_failure = Some(JmapError::Method {
                kind: "forbiddenFrom".into(),
                description: String::new(),
            })
        });
        assert!(matches!(
            fake.hold(&hold_for(3600, &blob)).await,
            Err(JmapError::Method { .. })
        ));
        assert_eq!(fake.with(|s| (s.emails.len(), s.holds.len())), (before, 0));
        assert!(
            fake.hold(&hold_for(3600, &blob)).await.is_ok(),
            "the failure is once"
        );
    }

    #[tokio::test]
    async fn a_hold_whose_answer_is_lost_is_still_kept_and_findable_in_drafts() {
        let fake = FakeJmap::new().with_delayed_send(86_400);
        let blob = fake.upload(RAW.to_vec()).await.unwrap();
        let drafts = "mb-drafts".to_string();
        fake.with(|s| s.hold_failure = Some(JmapError::HoldUnknown));
        let hold = hold_for(3600, &blob);
        assert_eq!(fake.hold(&hold).await, Err(JmapError::HoldUnknown));
        assert_eq!(fake.with(|s| s.holds.len()), 1, "the server holds it");
        let found = fake.with(|s| {
            s.emails
                .values()
                .filter(|e| {
                    e.mailbox_ids.contains(&drafts) && e.keywords.contains(mailrs_jmap::HELD)
                })
                .count()
        });
        assert_eq!(found, 1);
        let query = EmailQuery {
            filter: Some(
                json!({ "inMailbox": drafts, "hasKeyword": mailrs_jmap::HELD, "header": ["Message-ID", "<a1@example.com>"] }),
            ),
            position: 0,
            limit: 10,
            total: false,
        };
        assert_eq!(fake.query(&query).await.unwrap().hits.len(), 1);
    }

    #[tokio::test]
    async fn canceling_a_hold_that_is_gone_says_so() {
        let fake = FakeJmap::new().with_delayed_send(86_400);
        assert_eq!(
            fake.cancel_submission("S404").await.unwrap(),
            mailrs_jmap::Canceled::Gone
        );
    }

    #[tokio::test]
    async fn a_clamped_server_answers_at_most_its_limit_to_query_and_query_after() {
        let fake = FakeJmap::new().clamping_queries(2);
        let ids: Vec<String> = (1..=5).map(|n| fake.deliver("inbox", RAW, n)).collect();
        let query = EmailQuery {
            filter: None,
            position: 0,
            limit: 10,
            total: false,
        };
        let first = fake.query(&query).await.unwrap();
        assert_eq!(first.hits.len(), 2);
        let next = fake.query_after(&query, &first.hits[1].id).await.unwrap();
        assert_eq!(next.hits.len(), 2);
        assert!(ids.contains(&next.hits[0].id));
    }
    #[tokio::test]
    async fn calendars_need_the_capability_in_its_shape() {
        assert!(!FakeJmap::new().with(|s| s.session.offers(mailrs_jmap::CALENDARS)));
        let fake = FakeJmap::new().with_calendars();
        assert!(fake.with(|s| s.session.offers(mailrs_jmap::CALENDARS)));
        assert_eq!(fake.calendars().await.unwrap()[0].id, "cal-1");
    }

    #[tokio::test]
    async fn an_event_write_patches_and_logs_a_change() {
        let fake = FakeJmap::new().with_calendars();
        let state = fake.with(|s| s.state.to_string());
        let id = fake.add_event(json!({"title": "Rota", "calendarIds": {"cal-1": true}, "start": "2026-10-06T09:00:00"}));
        let mut patch = Map::new();
        patch.insert(
            "participants/p1/participationStatus".into(),
            json!("accepted"),
        );
        fake.set_calendar_event(
            &mailrs_jmap::EventWrite::Update {
                id: id.clone(),
                patch,
            },
            false,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            fake.event(&id).unwrap()["participants"]["p1"]["participationStatus"],
            "accepted"
        );
        let changes = fake.calendar_event_changes(&state, 50).await.unwrap();
        assert_eq!(changes.created, [id]);
    }

    #[tokio::test]
    async fn a_server_that_cannot_schedule_refuses_to_mail_guests() {
        let fake = FakeJmap::new().with_calendars();
        fake.with(|s| s.schedules = false);
        let id = fake.add_event(json!({"title": "Rota", "participants": {"p1": {"calendarAddress": "mailto:a@x.example"}}}));
        let write = mailrs_jmap::EventWrite::Update {
            id,
            patch: Map::new(),
        };
        assert!(
            matches!(fake.set_calendar_event(&write, true, None).await, Err(JmapError::Method { ref kind, .. }) if kind == "noSupportedScheduleMethods")
        );
        assert!(fake.set_calendar_event(&write, false, None).await.is_ok());
    }

    #[tokio::test]
    async fn a_stale_state_answers_state_mismatch_and_changes_nothing() {
        let fake = FakeJmap::new().with_calendars();
        let id = fake.add_event(json!({"title": "Rota"}));
        fake.with(|s| s.state_mismatches = 1);
        let mut patch = Map::new();
        patch.insert("title".into(), json!("Changed"));
        let write = mailrs_jmap::EventWrite::Update {
            id: id.clone(),
            patch,
        };
        assert_eq!(
            fake.set_calendar_event(&write, false, None).await,
            Err(JmapError::StateMismatch)
        );
        assert_eq!(fake.event(&id).unwrap()["title"], "Rota");
        fake.set_calendar_event(&write, false, None).await.unwrap();
        assert_eq!(fake.event(&id).unwrap()["title"], "Changed");
    }

    #[tokio::test]
    async fn a_write_from_an_older_state_answers_state_mismatch() {
        let fake = FakeJmap::new().with_calendars();
        let id = fake.add_event(json!({"title": "Rota"}));
        let old = fake.calendar_events(&[]).await.unwrap().state;
        fake.add_event(json!({"title": "Another"}));
        let mut patch = Map::new();
        patch.insert("title".into(), json!("Changed"));
        let write = mailrs_jmap::EventWrite::Update {
            id: id.clone(),
            patch,
        };
        assert_eq!(
            fake.set_calendar_event(&write, false, Some(&old)).await,
            Err(JmapError::StateMismatch)
        );
        assert_eq!(fake.event(&id).unwrap()["title"], "Rota");
        let now = fake.calendar_events(&[]).await.unwrap().state;
        fake.set_calendar_event(&write, false, Some(&now))
            .await
            .unwrap();
        assert_eq!(fake.event(&id).unwrap()["title"], "Changed");
    }
}
