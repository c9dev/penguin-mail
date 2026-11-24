//! Switching an IMAP account to JMAP on the same server. The stored mail
//! is matched to its JMAP ids by Message-ID and date, then by a second
//! key for what that leaves out, the mailboxes by role and then by path,
//! and the store rewrites the account in one
//! transaction (`mailrs_store::switch`). OBJECTID would name each
//! message's Email id directly, but the IMAP client cannot read `EMAILID`
//! yet; the Message-ID match works on every server.
//!
//! Nothing in the store changes before the last step, and a failure before
//! it leaves the IMAP account as it was. Drafts the JMAP server lacks go
//! up to it first, from the bytes kept here, so the switch loses none; a
//! draft without a Message-ID gets one first, the one change made earlier.
//! A Send Later message whose draft only the IMAP server holds takes the
//! draft's bytes from it before the swap, since the JMAP send could not
//! find the draft by its IMAP place.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use mailrs_domain::translate::gettext;
use mailrs_domain::{Account, AccountId, Provider};
use mailrs_jmap::{Auth, Email, EmailQuery, JmapClient, JmapError, Mailbox};
use mailrs_store::Db;
use mailrs_store::jmap::{JmapAuth, JmapSaved, SavedOAuth};
use mailrs_store::services::ServiceKind;
use mailrs_store::switch::{
    Adoption, Moved, StoredMailbox, StoredMessage, adopt_jmap, stored_mailboxes, stored_messages,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::account::message_id_header;
use crate::passwords::{PasswordError, PasswordStore, Secrets, first_error};
use crate::services::JmapApi;
use crate::services::jmap::{backend, bare_message_id, has_message_id, received_millis};
use crate::sign_in::BrowserGrant;
use crate::{AccountServices, AccountSync, AnyJmap, AnyRules, BackendError, SyncError};

/// Message-IDs one search asks for.
const IDS_PER_QUERY: usize = 100;

/// How far apart two dates of one message may be: the IMAP INTERNALDATE
/// and JMAP's `receivedAt` come from the same delivery, so a day covers
/// any zone or rounding.
const SAME_MESSAGE_WITHIN: i64 = 86_400_000;

/// Hits one search page asks for. The server may answer fewer.
const PAGE: usize = 500;

/// How many times a listing that changed while it was read starts again
/// before the switch gives up and leaves the account as it was.
const RESTARTS: usize = 8;

/// Second-key conditions one search carries.
const KEYS_PER_QUERY: usize = 20;

/// What the second look reads of each hit.
const SECOND_LOOK: [&str; 7] = [
    "id",
    "threadId",
    "messageId",
    "receivedAt",
    "size",
    "subject",
    "from",
];

/// A mailbox path as one comparable string: IMAP's `.` or `/` separators
/// read as `/`, the `INBOX.` prefix of a dotted namespace drops, case folds.
fn normal(path: &str) -> String {
    let path = path
        .strip_prefix("INBOX.")
        .or_else(|| path.strip_prefix("INBOX/"))
        .unwrap_or(path);
    path.replace('.', "/").to_lowercase()
}

/// Each listed mailbox's path, joined by `/` from its parents' names.
fn paths(listed: &[Mailbox]) -> HashMap<String, String> {
    let by_id: HashMap<&str, &Mailbox> = listed.iter().map(|m| (m.id.as_str(), m)).collect();
    listed
        .iter()
        .map(|mailbox| {
            let mut names = vec![mailbox.name.as_str()];
            let mut at = mailbox.parent_id.as_deref().and_then(|p| by_id.get(p));
            while let Some(parent) = at {
                names.push(parent.name.as_str());
                if names.len() > 32 {
                    break;
                }
                at = parent.parent_id.as_deref().and_then(|p| by_id.get(p));
            }
            names.reverse();
            (names.join("/").to_lowercase(), mailbox.id.clone())
        })
        .collect()
}

/// Each stored mailbox's JMAP id, by role first, then by path. A mailbox
/// the server has no match for is left out; a JMAP mailbox is taken once.
pub fn match_mailboxes(stored: &[StoredMailbox], listed: &[Mailbox]) -> Vec<(String, String)> {
    let by_path = paths(listed);
    let mut taken = BTreeSet::new();
    let mut out = Vec::new();
    for mailbox in stored {
        let by_role = mailbox.role.as_deref().and_then(|role| {
            listed
                .iter()
                .find(|m| m.role.as_deref() == Some(role))
                .map(|m| m.id.clone())
        });
        let found = by_role.or_else(|| {
            by_path
                .get(&normal(&mailbox.id))
                .or_else(|| by_path.get(&normal(&mailbox.name)))
                .cloned()
        });
        if let Some(id) = found
            && taken.insert(id.clone())
        {
            out.push((mailbox.id.clone(), id));
        }
    }
    out
}

/// Every JMAP id `filter` matches. Pages by position, newest first, and
/// starts over when the server's `queryState` moved between two pages: a
/// message gone from an earlier page shifts the next page past a hit.
async fn found_with<J: JmapApi>(api: &J, filter: &Value) -> Result<Vec<String>, BackendError> {
    for _ in 0..=RESTARTS {
        if let Some(ids) = one_listing(api, filter).await? {
            return Ok(ids);
        }
    }
    Err(BackendError::Changed)
}

/// One listing of `filter`, or None when it changed while it was read.
/// Only an empty page ends it: a server that clamps a page to its own
/// limit answers fewer hits than asked while more remain.
async fn one_listing<J: JmapApi>(
    api: &J,
    filter: &Value,
) -> Result<Option<Vec<String>>, BackendError> {
    let (mut ids, mut position, mut state) = (Vec::new(), 0u64, None::<String>);
    loop {
        let query = EmailQuery {
            filter: Some(filter.clone()),
            position,
            limit: PAGE,
            total: false,
        };
        let page = api.query(&query).await.map_err(backend)?;
        if *state.get_or_insert_with(|| page.query_state.clone()) != page.query_state {
            return Ok(None);
        }
        if page.hits.is_empty() {
            return Ok(Some(ids));
        }
        position += page.hits.len() as u64;
        ids.extend(page.hits.into_iter().map(|h| h.id));
    }
}

/// What the server holds under these Message-IDs: each bare Message-ID's
/// JMAP ids, threads and `receivedAt`. Only one chunk's hits are held.
async fn hits<J: JmapApi>(
    api: &J,
    message_ids: &[&str],
) -> Result<HashMap<String, Vec<(String, String, i64)>>, BackendError> {
    let conditions: Vec<_> = message_ids.iter().map(|id| has_message_id(id)).collect();
    let ids = found_with(api, &json!({ "operator": "OR", "conditions": conditions })).await?;
    let mut by_message_id: HashMap<String, Vec<(String, String, i64)>> = HashMap::new();
    for ids in ids.chunks(IDS_PER_QUERY) {
        let got = api
            .emails(ids, &["id", "threadId", "messageId", "receivedAt"])
            .await
            .map_err(backend)?;
        for email in got.list {
            let received = received_millis(&email);
            for id in email.message_id.iter().flatten() {
                by_message_id
                    .entry(bare_message_id(id).to_string())
                    .or_default()
                    .push((email.id.clone(), email.thread_id.clone(), received));
            }
        }
    }
    Ok(by_message_id)
}

/// How a stored message the Message-ID and date left out is looked for
/// again before the switch counts it as gone from the server.
enum SecondKey<'a> {
    /// Arrived within a day of `date`, with exactly this size and
    /// subject, and no other Message-ID than this one when both have one.
    DateAndSize {
        date: i64,
        size: u64,
        subject: &'a str,
        message_id: Option<&'a str>,
    },
    /// The date is unknown: the one copy under this bare Message-ID.
    MessageId(&'a str),
    /// No Message-ID and no date: the subject and the sender.
    SubjectAndFrom {
        subject: &'a str,
        from: Option<&'a str>,
    },
}

impl<'a> SecondKey<'a> {
    fn of(message: &'a StoredMessage) -> Option<SecondKey<'a>> {
        let message_id = message
            .message_id
            .as_deref()
            .map(bare_message_id)
            .filter(|i| !i.is_empty());
        let from = message.from.as_deref().filter(|f| !f.trim().is_empty());
        match (message.date > 0, message_id) {
            (true, _) => u64::try_from(message.size)
                .ok()
                .filter(|s| *s > 0)
                .map(|size| SecondKey::DateAndSize {
                    date: message.date,
                    size,
                    subject: message.subject.trim(),
                    message_id,
                }),
            (false, Some(id)) => Some(SecondKey::MessageId(id)),
            (false, None) if !message.subject.trim().is_empty() || from.is_some() => {
                Some(SecondKey::SubjectAndFrom {
                    subject: message.subject.trim(),
                    from,
                })
            }
            (false, None) => None,
        }
    }

    /// The search condition. It may match more than the key does, since
    /// `subject` and `from` search for words; `holds` decides.
    fn condition(&self) -> Value {
        let stamp = |millis: i64| {
            chrono::DateTime::from_timestamp_millis(millis)
                .unwrap_or_default()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        };
        match self {
            SecondKey::DateAndSize { date, size, .. } => json!({
                "after": stamp(date - SAME_MESSAGE_WITHIN),
                "before": stamp(date + SAME_MESSAGE_WITHIN + 1000),
                "minSize": size,
                "maxSize": size + 1,
            }),
            SecondKey::MessageId(id) => has_message_id(id),
            SecondKey::SubjectAndFrom { subject, from } => {
                let mut condition = serde_json::Map::new();
                if !subject.is_empty() {
                    condition.insert("subject".into(), json!(subject));
                }
                if let Some(from) = from {
                    condition.insert("from".into(), json!(from));
                }
                Value::Object(condition)
            }
        }
    }

    fn holds(&self, email: &Email) -> bool {
        match self {
            SecondKey::DateAndSize {
                date,
                size,
                subject,
                message_id,
            } => {
                // Mail the server no longer holds may share a day and a
                // size with mail the store never had.
                let other_id = message_id.is_some_and(|wanted| {
                    email
                        .message_id
                        .iter()
                        .flatten()
                        .map(|m| bare_message_id(m))
                        .any(|m| !m.is_empty() && m != wanted)
                });
                email.size == *size
                    && (received_millis(email) - date).abs() <= SAME_MESSAGE_WITHIN
                    && email.subject.as_deref().unwrap_or_default().trim() == *subject
                    && !other_id
            }
            SecondKey::MessageId(id) => email
                .message_id
                .iter()
                .flatten()
                .any(|m| bare_message_id(m) == *id),
            SecondKey::SubjectAndFrom { subject, from } => {
                email.subject.as_deref().unwrap_or_default().trim() == *subject
                    && from.is_none_or(|from| {
                        email
                            .from
                            .iter()
                            .flatten()
                            .any(|a| a.email.eq_ignore_ascii_case(from))
                    })
            }
        }
    }
}

/// Pairs each stored message left out of `out` with the one JMAP message
/// its second key finds, when exactly one untaken message answers it and
/// no other stored message the second look reaches picks that one too.
async fn match_again<J: JmapApi>(
    api: &J,
    stored: &[StoredMessage],
    taken: &mut BTreeSet<String>,
    out: &mut Vec<Moved>,
) -> Result<(), BackendError> {
    let matched: BTreeSet<String> = out.iter().map(|m| m.from.clone()).collect();
    let left: Vec<(&StoredMessage, SecondKey)> = stored
        .iter()
        .filter(|m| !matched.contains(&m.id))
        .filter_map(|m| SecondKey::of(m).map(|key| (m, key)))
        .collect();
    let mut picked: Vec<Moved> = Vec::new();
    for chunk in left.chunks(KEYS_PER_QUERY) {
        let conditions: Vec<Value> = chunk.iter().map(|(_, key)| key.condition()).collect();
        let ids = found_with(api, &json!({ "operator": "OR", "conditions": conditions })).await?;
        let mut found: Vec<Email> = Vec::new();
        for ids in ids.chunks(IDS_PER_QUERY) {
            found.extend(api.emails(ids, &SECOND_LOOK).await.map_err(backend)?.list);
        }
        for (message, key) in chunk {
            let mut answers = found
                .iter()
                .filter(|e| !taken.contains(&e.id) && key.holds(e));
            if let (Some(email), None) = (answers.next(), answers.next()) {
                picked.push(Moved {
                    from: message.id.clone(),
                    to: email.id.clone(),
                    thread: email.thread_id.clone(),
                });
            }
        }
    }
    let mut picks: HashMap<String, usize> = HashMap::new();
    for moved in &picked {
        *picks.entry(moved.to.clone()).or_default() += 1;
    }
    for moved in picked {
        if picks.get(&moved.to) == Some(&1) {
            taken.insert(moved.to.clone());
            out.push(moved);
        }
    }
    Ok(())
}

/// Each stored message's JMAP id and thread, by Message-ID and the
/// nearest `receivedAt` within a day. A JMAP id goes to one stored message
/// only, so two stored copies of one Message-ID never share it; the second
/// copy stays unmatched when the server holds just one. A message this
/// leaves out gets a second look by another key (`SecondKey`); one still
/// unmatched after it is mail the server no longer holds.
pub async fn match_messages<J: JmapApi>(
    api: &J,
    stored: &[StoredMessage],
) -> Result<Vec<Moved>, BackendError> {
    let with_id: Vec<&StoredMessage> = stored
        .iter()
        .filter(|m| {
            m.message_id
                .as_deref()
                .is_some_and(|i| !bare_message_id(i).is_empty())
        })
        .collect();
    let mut taken = BTreeSet::new();
    let mut out = Vec::new();
    for chunk in with_id.chunks(IDS_PER_QUERY) {
        let wanted: Vec<&str> = chunk
            .iter()
            .filter_map(|m| m.message_id.as_deref())
            .collect();
        let by_message_id = hits(api, &wanted).await?;
        for message in chunk {
            let Some(wanted) = message.message_id.as_deref().map(bare_message_id) else {
                continue;
            };
            let best = by_message_id
                .get(wanted)
                .into_iter()
                .flatten()
                .filter(|(id, _, received)| {
                    !taken.contains(id) && (received - message.date).abs() <= SAME_MESSAGE_WITHIN
                })
                .min_by_key(|(_, _, received)| (received - message.date).abs());
            if let Some((id, thread, _)) = best {
                taken.insert(id.clone());
                out.push(Moved {
                    from: message.id.clone(),
                    to: id.clone(),
                    thread: thread.clone(),
                });
            }
        }
    }
    match_again(api, stored, &mut taken, &mut out).await?;
    Ok(out)
}

/// The ids of the stored drafts: what sits in Drafts, and what a waiting
/// outbox row names as its draft.
fn stored_drafts(
    conn: &Connection,
    account_id: AccountId,
) -> mailrs_store::Result<BTreeSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT m.id FROM messages m WHERE m.account_id = ?1 AND (\
         EXISTS (SELECT 1 FROM message_mailboxes s CROSS JOIN mailboxes b ON b.key = s.mailbox \
             WHERE s.account_id = m.account_id AND s.message_id = m.id AND b.role = 'drafts') \
         OR m.id IN (SELECT draft_id FROM outbox WHERE account_id = ?1 AND draft_id IS NOT NULL))",
    )?;
    let ids = stmt.query_map(params![account_id], |row| row.get(0))?;
    Ok(ids.collect::<rusqlite::Result<_>>()?)
}

/// A draft's bytes as kept here: its local copy, or the bytes an outbox
/// row waits to send it from. An IMAP draft usually has neither, since
/// its bytes live on the server.
fn draft_bytes(
    conn: &Connection,
    account_id: AccountId,
    id: &str,
) -> mailrs_store::Result<Option<Vec<u8>>> {
    if let Some(raw) = mailrs_store::local_messages::get(conn, account_id, id)? {
        return Ok(Some(raw));
    }
    Ok(conn
        .query_row(
            "SELECT raw FROM outbox WHERE account_id = ?1 AND draft_id = ?2 AND raw IS NOT NULL LIMIT 1",
            params![account_id, id],
            |row| row.get(0),
        )
        .optional()?)
}

/// `raw` with a `Message-ID: <id>` header first, in the line ending the
/// message already uses.
fn with_message_id(raw: &[u8], id: &str) -> Vec<u8> {
    let end: &[u8] = if raw.windows(2).any(|w| w == b"\r\n") {
        b"\r\n"
    } else {
        b"\n"
    };
    let mut out = Vec::with_capacity(raw.len() + id.len() + 16);
    out.extend_from_slice(format!("Message-ID: <{id}>").as_bytes());
    out.extend_from_slice(end);
    out.extend_from_slice(raw);
    out
}

/// Gives the stored draft `id` a Message-ID: the one its kept bytes
/// already carry, else `fresh`. The row takes it, and the bytes kept
/// here that lack one, its local copy and any outbox row that sends it,
/// get the header. The draft's bytes as they now read, if any are kept.
fn stamp_draft(
    conn: &Connection,
    account_id: AccountId,
    id: &str,
    fresh: String,
) -> mailrs_store::Result<Option<Vec<u8>>> {
    let local = mailrs_store::local_messages::get(conn, account_id, id)?;
    let queued: Vec<(i64, Vec<u8>)> = conn
        .prepare("SELECT id, raw FROM outbox WHERE account_id = ?1 AND draft_id = ?2 AND raw IS NOT NULL")?
        .query_map(params![account_id, id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let Some(kept) = local.as_ref().or(queued.first().map(|(_, raw)| raw)) else {
        return Ok(None);
    };
    let message_id = message_id_header(kept).unwrap_or(fresh);
    conn.execute(
        "UPDATE messages SET rfc822_msgid = ?3 WHERE account_id = ?1 AND id = ?2",
        params![account_id, id, message_id],
    )?;
    let stamped = |raw: Vec<u8>| match message_id_header(&raw) {
        Some(_) => (raw, false),
        None => (with_message_id(&raw, &message_id), true),
    };
    let local = match local.map(stamped) {
        Some((raw, true)) => {
            mailrs_store::local_messages::put(conn, account_id, id, &raw)?;
            Some(raw)
        }
        other => other.map(|(raw, _)| raw),
    };
    let mut first = None;
    for (row, raw) in queued {
        let (raw, changed) = stamped(raw);
        if changed {
            conn.execute(
                "UPDATE outbox SET raw = ?2 WHERE id = ?1",
                params![row, raw],
            )?;
        }
        first.get_or_insert(raw);
    }
    Ok(local.or(first))
}

/// A fresh Message-ID in the account's domain, as the composer makes one.
fn new_message_id(address: &str) -> String {
    let domain = address
        .rsplit_once('@')
        .map_or("penguin-mail.local", |(_, d)| d);
    format!(
        "{}.{}@{domain}",
        mailrs_gmail::random_token(12),
        std::process::id()
    )
}

/// The reminders already shown, renamed for the JMAP calendar: each
/// event's CalDAV calendar and id paired with the JMAP calendar and id of
/// the event with the same UID (and the same occurrence, for a changed
/// one). Without it a reminder shown before the switch goes up again once
/// the JMAP calendar is read. A server that does not answer leaves the
/// log as it is: the switch goes on, and at worst a reminder shows twice.
async fn shown_reminders<J: JmapApi>(
    db: &Db,
    id: AccountId,
    api: &J,
) -> Vec<((String, String), (String, String))> {
    let shown = db
        .read(move |c| {
            let mut stmt = c.prepare(
                "SELECT DISTINCT r.calendar, r.event, e.uid, e.original_start FROM event_reminders_shown r \
                 JOIN events e ON e.account_id = r.account_id AND e.calendar = r.calendar AND e.id = r.event \
                 WHERE r.account_id = ?1",
            )?;
            let rows = stmt.query_map(params![id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<(String, String, String, Option<i64>)>>>()?)
        })
        .await;
    let shown = match shown {
        Ok(shown) => shown,
        Err(err) => {
            tracing::warn!(account = id, %err, "could not read the reminders already shown");
            return Vec::new();
        }
    };
    let mut renamed = Vec::new();
    let mut by_uid: HashMap<String, Vec<mailrs_domain::calendar::Event>> = HashMap::new();
    for (calendar, event, uid, original) in shown {
        if !by_uid.contains_key(&uid) {
            let page = match api.calendar_events_page(&json!({ "uid": uid }), 0, 8).await {
                Ok(page) => page,
                Err(err) => {
                    tracing::info!(account = id, %err, "could not look up an event the reminders named");
                    continue;
                }
            };
            let events = page
                .events
                .iter()
                .filter_map(|object| {
                    let calendar = object["calendarIds"]
                        .as_object()?
                        .iter()
                        .find(|(_, on)| on.as_bool() == Some(true))?
                        .0
                        .clone();
                    Some(crate::services::jmap::jscal::events_of(
                        object,
                        &calendar,
                        &[],
                    ))
                })
                .flatten()
                .collect();
            by_uid.insert(uid.clone(), events);
        }
        let mut same = by_uid[&uid].iter().filter(|e| e.original_start == original);
        if let (Some(found), None) = (same.next(), same.next()) {
            renamed.push((
                (calendar, event),
                (found.calendar.clone(), found.id.clone()),
            ));
        }
    }
    renamed
}

/// A Send Later message whose draft only the IMAP server holds: its row,
/// the draft's IMAP id and the bytes read from the server.
struct Waiting {
    row: i64,
    draft_id: String,
    raw: Vec<u8>,
}

/// Reads from the IMAP server the draft of every Send Later message that
/// keeps no bytes here. Scheduled online, such a row names its draft by
/// its IMAP place alone, which means nothing to JMAP. Any draft the
/// server does not give stops the switch.
async fn waiting_drafts(
    db: &Db,
    id: AccountId,
    imap: Option<&AccountSync>,
) -> Result<Vec<Waiting>, SwitchError> {
    let named: Vec<(i64, String)> = db
        .read(move |c| {
            let mut stmt = c.prepare("SELECT id, draft_id FROM outbox WHERE account_id = ?1 AND raw IS NULL AND draft_id IS NOT NULL")?;
            let rows = stmt.query_map(params![id], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await?;
    let mut waiting = Vec::with_capacity(named.len());
    for (row, draft_id) in named {
        let imap = imap.ok_or(SwitchError::Drafts(SyncError::UnknownAccount(id)))?;
        let raw = imap
            .raw(&draft_id)
            .await
            .map_err(SwitchError::Drafts)?
            .to_vec();
        waiting.push(Waiting { row, draft_id, raw });
    }
    Ok(waiting)
}

/// The JMAP id of the one copy in Drafts that carries `raw`'s Message-ID.
async fn in_jmap_drafts<J: JmapApi>(
    api: &J,
    drafts: Option<&str>,
    raw: &[u8],
) -> Result<Option<String>, BackendError> {
    let (Some(drafts), Some(message_id)) = (drafts, message_id_header(raw)) else {
        return Ok(None);
    };
    let filter = json!({ "operator": "AND", "conditions": [has_message_id(&message_id), { "inMailbox": drafts }] });
    let mut found = found_with(api, &filter).await?;
    Ok(match found.len() {
        1 => found.pop(),
        _ => None,
    })
}

/// Pairs each stored draft the date match left out with a JMAP copy:
/// one the server holds under its Message-ID at any date, or one imported
/// now from the bytes kept here. An import dates its copy on arrival, so a
/// copy an earlier, failed switch put there never matches by date; going
/// by Message-ID alone keeps a second switch from importing it again. A
/// draft with no bytes here and no copy there has nothing left to carry.
async fn carry_drafts<J: JmapApi>(
    db: &Db,
    account: &Account,
    api: &J,
    listed: &[Mailbox],
    stored: &[StoredMessage],
    drafts: &BTreeSet<String>,
    moved: &mut Vec<Moved>,
) -> Result<(), SwitchError> {
    let matched: BTreeSet<&str> = moved.iter().map(|m| m.from.as_str()).collect();
    let left: Vec<&StoredMessage> = stored
        .iter()
        .filter(|m| drafts.contains(&m.id) && !matched.contains(m.id.as_str()))
        .collect();
    if left.is_empty() {
        return Ok(());
    }
    let mut taken: BTreeSet<String> = moved.iter().map(|m| m.to.clone()).collect();
    let wanted: Vec<&str> = left
        .iter()
        .filter_map(|m| m.message_id.as_deref())
        .filter(|i| !bare_message_id(i).is_empty())
        .collect();
    let mut held: HashMap<String, Vec<(String, String, i64)>> = HashMap::new();
    for chunk in wanted.chunks(IDS_PER_QUERY) {
        for (message_id, found) in hits(api, chunk).await? {
            held.entry(message_id).or_default().extend(found);
        }
    }
    let drafts_mailbox = listed
        .iter()
        .find(|m| m.role.as_deref() == Some("drafts"))
        .map(|m| m.id.clone());
    for draft in left {
        let copy = draft
            .message_id
            .as_deref()
            .and_then(|i| held.get(bare_message_id(i)))
            .into_iter()
            .flatten()
            .find(|(id, _, _)| !taken.contains(id));
        if let Some((to, thread, _)) = copy {
            taken.insert(to.clone());
            moved.push(Moved {
                from: draft.id.clone(),
                to: to.clone(),
                thread: thread.clone(),
            });
            continue;
        }
        let (from, account_id) = (draft.id.clone(), account.id);
        // A draft whose row has no Message-ID takes the one its bytes
        // carry, or a new one written into them, before it goes up, so a
        // retry after a failed swap finds the copy by it instead of
        // uploading the draft again.
        let Some(raw) = db.read(move |c| draft_bytes(c, account_id, &from)).await? else {
            continue;
        };
        let raw = match draft
            .message_id
            .as_deref()
            .is_some_and(|i| !bare_message_id(i).is_empty())
        {
            true => raw,
            false => {
                let (from, message_id) = (draft.id.clone(), new_message_id(&account.email));
                db.write(move |c| stamp_draft(c, account_id, &from, message_id))
                    .await?
                    .unwrap_or(raw)
            }
        };
        let mailbox = drafts_mailbox
            .clone()
            .ok_or_else(|| BackendError::Refused(gettext("The server has no Drafts mailbox.")))?;
        let blob = api.upload(raw).await.map_err(backend)?;
        let made = api
            .import(&blob, &[mailbox], &["$draft", "$seen"], None, None)
            .await
            .map_err(backend)?
            .email;
        let thread = match made.thread_id.is_empty() {
            false => made.thread_id,
            true => {
                let got = api
                    .emails(std::slice::from_ref(&made.id), &["id", "threadId"])
                    .await
                    .map_err(backend)?;
                got.list
                    .into_iter()
                    .next()
                    .map(|e| e.thread_id)
                    .ok_or(BackendError::NotFound)?
            }
        };
        taken.insert(made.id.clone());
        moved.push(Moved {
            from: draft.id.clone(),
            to: made.id,
            thread,
        });
    }
    Ok(())
}

/// What the person typed for the JMAP sign-in.
pub struct SwitchTo {
    pub session_url: String,
    pub token: bool,
    /// The user name Basic sends.
    pub user: String,
    pub secret: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SwitchError {
    /// The JMAP server refused the sign-in or did not answer it.
    #[error(transparent)]
    SignIn(JmapError),
    #[error("only an IMAP account can switch to JMAP")]
    NotImap,
    /// The IMAP server did not give the draft of a message waiting in
    /// Send Later.
    #[error("could not read a Send Later draft from the IMAP server: {0}")]
    Drafts(SyncError),
    /// The session signs in to another account than this one.
    #[error("the JMAP sign-in reached another account")]
    OtherAccount,
    #[error(transparent)]
    Sync(#[from] SyncError),
}

#[derive(Debug, Clone)]
pub struct Switched {
    pub account: Account,
    pub kept: usize,
    pub dropped: usize,
    /// Local rules turned off because they name a mailbox the server lacks.
    pub rules_off: usize,
}

impl From<mailrs_store::StoreError> for SwitchError {
    fn from(err: mailrs_store::StoreError) -> Self {
        SwitchError::Sync(err.into())
    }
}

impl From<PasswordError> for SwitchError {
    fn from(err: PasswordError) -> Self {
        SwitchError::Sync(err.into())
    }
}

impl From<BackendError> for SwitchError {
    fn from(err: BackendError) -> Self {
        SwitchError::Sync(err.into())
    }
}

async fn keyring<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, PasswordError> + Send + 'static,
) -> Result<T, PasswordError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| PasswordError::Keyring(e.to_string()))?
}

/// The lock one account's switch holds. The map keeps an entry only while
/// a switch holds or waits for it.
fn switch_lock(id: AccountId) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: LazyLock<Mutex<HashMap<AccountId, Arc<tokio::sync::Mutex<()>>>>> =
        LazyLock::new(Mutex::default);
    let mut locks = LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
    locks.retain(|_, lock| Arc::strong_count(lock) > 1);
    Arc::clone(locks.entry(id).or_default())
}

/// Deletes the IMAP and SMTP passwords once the swap is in the database
/// file. With `synchronous = NORMAL` a commit is not synced, and a power
/// cut before the next checkpoint would bring the IMAP account back
/// without its password. The switch is done whatever happens here: a
/// password that stays is deleted when the person removes the account.
async fn forget_imap_passwords(db: &Db, secrets: &Secrets, id: AccountId) {
    match db.checkpoint().await {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(
                account = id,
                "the switch is not in the database file yet, so the IMAP password stays"
            );
            return;
        }
        Err(err) => {
            tracing::warn!(account = id, %err, "could not write the switch to the database file, so the IMAP password stays");
            return;
        }
    }
    let passwords = Arc::clone(&secrets.passwords);
    if let Err(err) =
        keyring(move || first_error([passwords.delete_outgoing(id), passwords.delete(id)])).await
    {
        tracing::warn!(account = id, %err, "the switch is done, and the keyring kept the IMAP password");
    }
}

/// Moves the local rules of an account just switched into its Sieve
/// script, once its JMAP services are up and before they run. A server
/// that will not take them (the person runs a script of their own that it
/// cannot include) leaves them beside the script, running here as before.
/// Answers whether they moved.
pub async fn move_rules(services: &mut AccountServices) -> bool {
    let Some(AnyRules::Beside(beside)) = services.rules.clone() else {
        return false;
    };
    match beside.move_in().await {
        Ok(()) => {
            services.rules = Some(AnyRules::JmapSieve(beside.server().clone()));
            true
        }
        Err(err) => {
            tracing::info!(%err, "the local rules stay on this computer beside the server's");
            false
        }
    }
}

/// How the switch signs in to the JMAP session.
pub enum SwitchSignIn {
    /// A password or a token the person typed.
    Typed(SwitchTo),
    /// A sign-in on the server's own page in the browser.
    Browser {
        session_url: String,
        grant: BrowserGrant,
    },
}

/// What one way of signing in becomes: the sign-in the checks send, and
/// what the account keeps.
pub struct Signed {
    pub session_url: String,
    pub auth: Auth,
    pub kind: JmapAuth,
    pub user_name: Option<String>,
    pub oauth: Option<SavedOAuth>,
    /// What the keyring keeps: the password, the token or the refresh
    /// token.
    pub secret: String,
}

/// What `how` signs in with. A browser grant's checks send its access
/// token as it came: a session that refreshes here would rotate the
/// refresh token before the keyring holds it.
pub fn signed_with(how: SwitchSignIn, address: &str) -> Signed {
    match how {
        SwitchSignIn::Typed(to) => Signed {
            auth: match to.token {
                true => Auth::Bearer(to.secret.clone()),
                false => Auth::Basic {
                    user: to.user.clone(),
                    password: to.secret.clone(),
                },
            },
            kind: if to.token {
                JmapAuth::Bearer
            } else {
                JmapAuth::Basic
            },
            user_name: (!to.token && !to.user.eq_ignore_ascii_case(address)).then_some(to.user),
            oauth: None,
            session_url: to.session_url,
            secret: to.secret,
        },
        SwitchSignIn::Browser { session_url, grant } => Signed {
            session_url,
            auth: Auth::Bearer(grant.access_token),
            kind: JmapAuth::OAuth,
            user_name: None,
            oauth: Some(grant.client),
            secret: grant.refresh_token,
        },
    }
}

/// Signs in to the JMAP session, then switches. `imap` is the IMAP
/// account's sync, which reads the drafts Send Later messages name; None
/// when it is not running.
pub async fn to_jmap(
    db: &Db,
    secrets: &Secrets,
    account: &Account,
    imap: Option<&AccountSync>,
    how: SwitchSignIn,
) -> Result<Switched, SwitchError> {
    if account.provider != Provider::Imap {
        return Err(SwitchError::NotImap);
    }
    // A browser grant the switch does not keep is revoked, so it does not
    // stay valid on the server with nothing here to use it.
    let unkept = match &how {
        SwitchSignIn::Browser { session_url, grant } => Some(crate::JmapGrant {
            session_url: session_url.clone(),
            client_id: grant.client.client_id.clone(),
            refresh_token: grant.refresh_token.clone(),
        }),
        SwitchSignIn::Typed(_) => None,
    };
    let switched = signed_in(db, secrets, account, imap, signed_with(how, &account.email)).await;
    // `NotImap` here means another switch went through first and may sign
    // in with the token saved for this one; it stays.
    if let (Err(err), Some(grant)) = (&switched, unkept)
        && !matches!(err, SwitchError::NotImap)
    {
        tokio::spawn(grant.revoke());
    }
    switched
}

async fn signed_in(
    db: &Db,
    secrets: &Secrets,
    account: &Account,
    imap: Option<&AccountSync>,
    signed: Signed,
) -> Result<Switched, SwitchError> {
    let client = JmapClient::new(&signed.session_url, signed.auth).map_err(SwitchError::SignIn)?;
    let checked = mailrs_jmap::check(&client)
        .await
        .map_err(SwitchError::SignIn)?;
    let session = &checked.session;
    let saved = JmapSaved {
        session_url: signed.session_url,
        auth: signed.kind,
        user_name: signed.user_name,
        labels: session.mailboxes_per_email().is_none_or(|most| most > 1),
        capabilities: session
            .capabilities
            .keys()
            .filter(|c| session.offers(c))
            .cloned()
            .collect(),
        oauth: signed.oauth,
        owner: Some(crate::sign_in::SignedAs::of(&checked).owner),
    };
    to_jmap_over(
        db,
        secrets,
        account,
        imap,
        &AnyJmap::from(client),
        saved,
        signed.secret,
    )
    .await
}

/// Whether the session signs in to the account at `address`: its user name
/// is the address or its local part, or one of its identities sends from
/// it.
async fn same_account<J: JmapApi>(
    api: &J,
    session: &mailrs_jmap::Session,
    address: &str,
) -> Result<bool, SwitchError> {
    let username = session.username.trim();
    let local = address.split_once('@').map(|(local, _)| local);
    // A login name that is the address's local part, as Cyrus and
    // Stalwart name their users, is the same account too.
    if username.eq_ignore_ascii_case(address)
        || local.is_some_and(|local| username.eq_ignore_ascii_case(local))
    {
        return Ok(true);
    }
    let identities = api.identities().await.map_err(backend)?;
    Ok(identities
        .iter()
        .any(|identity| identity.email.eq_ignore_ascii_case(address)))
}

/// The switch over `api`, already signed in. The JMAP secret goes into the
/// keyring just before the store's one transaction and out again when that
/// fails. The IMAP password goes once the swap is in the database file,
/// and stays when a CalDAV, CardDAV or ManageSieve server the session does
/// not replace signs in with it.
pub async fn to_jmap_over<J: JmapApi>(
    db: &Db,
    secrets: &Secrets,
    account: &Account,
    imap: Option<&AccountSync>,
    api: &J,
    saved: JmapSaved,
    secret: String,
) -> Result<Switched, SwitchError> {
    if account.provider != Provider::Imap {
        return Err(SwitchError::NotImap);
    }
    let id = account.id;
    // A second switch of this account waits for the first, then finds it
    // switched. Run side by side, its opening delete could take out the
    // secret the first saved for its swap.
    let lock = switch_lock(id);
    let _one = lock.lock().await;
    // The account shown may be stale: a second click, or a retry after a
    // switch that went through.
    let provider = db
        .read(move |c| Ok(mailrs_store::accounts::account(c, id)?.map(|a| a.provider)))
        .await?;
    if provider != Some(Provider::Imap) {
        return Err(SwitchError::NotImap);
    }
    // A switch that stopped between saving its secret and the swap left
    // the secret here; nothing else keeps a JMAP secret for an IMAP account.
    let jmap = Arc::clone(&secrets.jmap);
    keyring(move || jmap.delete(id)).await?;
    let session = api.session().await.map_err(SwitchError::SignIn)?;
    if !session.offers(mailrs_jmap::MAIL) {
        return Err(SwitchError::SignIn(JmapError::UnknownCapability(
            mailrs_jmap::MAIL.to_string(),
        )));
    }
    // A browser already signed in to a second account at the provider
    // hands back that account. Matched against its mail, most of this
    // account's mail would go.
    if !same_account(api, &session, &account.email).await? {
        return Err(SwitchError::OtherAccount);
    }
    let listed = api.mailboxes().await.map_err(backend)?.list;
    let (stored, boxes, drafts) = db
        .read(move |c| {
            Ok((
                stored_messages(c, id)?,
                stored_mailboxes(c, id)?,
                stored_drafts(c, id)?,
            ))
        })
        .await?;
    let waiting = waiting_drafts(db, id, imap).await?;
    let mut messages = match_messages(api, &stored).await?;
    carry_drafts(db, account, api, &listed, &stored, &drafts, &mut messages).await?;
    drop(stored);
    // A draft the swap gives an Email id keeps it; any other takes the
    // JMAP copy in Drafts, or none, and the message sends from its bytes.
    let drafts_mailbox = listed
        .iter()
        .find(|m| m.role.as_deref() == Some("drafts"))
        .map(|m| m.id.as_str());
    let mut fill = Vec::with_capacity(waiting.len());
    for Waiting { row, draft_id, raw } in waiting {
        let draft = match messages.iter().any(|m| m.from == draft_id) {
            true => None,
            false => Some(in_jmap_drafts(api, drafts_mailbox, &raw).await?),
        };
        fill.push((row, raw, draft));
    }
    let mailboxes = match_mailboxes(&boxes, &listed);
    let found = db
        .read(move |c| mailrs_store::services::load(c, id))
        .await?;
    let contacts_from_jmap = saved.offers(mailrs_jmap::CONTACTS);
    let needs_dav_password = found.iter().filter(|f| f.confirmed).any(|f| match f.kind {
        ServiceKind::CalDav => !saved.offers(mailrs_jmap::CALENDARS),
        ServiceKind::CardDav => !contacts_from_jmap,
        ServiceKind::Sieve => !saved.offers(mailrs_jmap::SIEVE),
    });
    let reminders = match saved.offers(mailrs_jmap::CALENDARS) {
        true => shown_reminders(db, id, api).await,
        false => Vec::new(),
    };
    let jmap = Arc::clone(&secrets.jmap);
    keyring(move || jmap.save(id, &secret)).await?;
    let adoption = Adoption {
        messages,
        mailboxes,
        saved,
        contacts_from_jmap,
    };
    let adopted = db
        .write(move |c| {
            let Some(adopted) = adopt_jmap(c, id, &adoption)? else {
                return Ok(None);
            };
            // An outbox row keeps its bytes and still sends; it lets go of
            // a message or thread the switch dropped.
            c.execute(
                "UPDATE outbox SET message_id = NULL WHERE account_id = ?1 \
                 AND message_id NOT IN (SELECT id FROM messages WHERE account_id = ?1)",
                params![id],
            )?;
            c.execute(
                "UPDATE outbox SET thread_id = NULL WHERE account_id = ?1 \
                 AND thread_id NOT IN (SELECT thread_id FROM messages WHERE account_id = ?1)",
                params![id],
            )?;
            for ((calendar, event), (to_calendar, to_event)) in &reminders {
                c.execute(
                    "UPDATE OR IGNORE event_reminders_shown SET calendar = ?4, event = ?5 \
                     WHERE account_id = ?1 AND calendar = ?2 AND event = ?3",
                    params![id, calendar, event, to_calendar, to_event],
                )?;
            }
            for (row, raw, draft) in &fill {
                c.execute(
                    "UPDATE outbox SET raw = ?2 WHERE id = ?1 AND raw IS NULL",
                    params![row, raw],
                )?;
                if let Some(draft) = draft {
                    c.execute(
                        "UPDATE outbox SET draft_id = ?2 WHERE id = ?1",
                        params![row, draft],
                    )?;
                }
            }
            Ok(Some(adopted))
        })
        .await;
    let adopted = match adopted {
        Ok(Some(adopted)) => adopted,
        // Another switch committed first. The secret saved above signs in
        // the account that switch made, so it stays.
        Ok(None) => return Err(SwitchError::NotImap),
        Err(err) => {
            let jmap = Arc::clone(&secrets.jmap);
            if keyring(move || jmap.delete(id)).await.is_err() {
                tracing::warn!(
                    account = id,
                    "the switch failed, and the keyring kept the JMAP secret"
                );
            }
            return Err(err.into());
        }
    };
    if !needs_dav_password {
        forget_imap_passwords(db, secrets, id).await;
    }
    let account = db
        .read(move |c| mailrs_store::accounts::account(c, id))
        .await?
        .ok_or(SwitchError::NotImap)?;
    Ok(Switched {
        account,
        kept: adopted.kept,
        dropped: adopted.dropped,
        rules_off: adopted.rules_off,
    })
}
