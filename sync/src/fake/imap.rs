//! An in-memory IMAP server and SMTP submission server, for sync's tests
//! and the demo. Mailboxes hold raw messages with UIDs, UIDVALIDITY,
//! flags and MODSEQs; a test turns the server's extensions on and off and
//! changes mail behind the app's back. Both answer through `ImapApi` and
//! `Submit`, keeping the promises the real clients make there, and log
//! every call.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use mailrs_domain::EpochMillis;
use mailrs_imap::{
    AppendUid, BodyStructure, Capabilities, CopyUid, Fetched, FlagsOf, IDLE_LIMIT, ImapError,
    Listed, Selected, Since, SpecialUse, UidSet, Woke,
};
use mailrs_mime::Part;
use tokio::sync::Notify;

use crate::services::imap::{ImapApi, Submit};

/// An IMAP server for one account.
pub struct FakeImap {
    state: Mutex<ImapState>,
    /// Wakes an IDLE when any mailbox changes.
    changed: Notify,
}

pub struct ImapState {
    pub capabilities: Capabilities,
    /// What separates levels in mailbox names.
    pub delimiter: char,
    pub mailboxes: BTreeMap<String, FakeMailbox>,
    /// Errors the next calls return, one per call, after the call is logged.
    pub failures: VecDeque<ImapError>,
    /// The latest calls, at most [`MAX_CALLS`], oldest first, such as
    /// `select INBOX` or `store INBOX 1:3 + \Seen`.
    pub calls: VecDeque<String>,
    /// Errors aimed at one method, such as `select`: each answers the next
    /// call to that method and no other.
    pub aimed: Vec<(String, ImapError)>,
    /// The next IDLE ends at once as `Woke::Changed`, as the real client
    /// answers when the server sends more during an IDLE than the guard
    /// lets through and the connection is dropped.
    pub overflow_idle: bool,
    /// The UIDVALIDITY the next new or reset mailbox gets.
    next_uidvalidity: u32,
}

pub struct FakeMailbox {
    pub special_use: Option<SpecialUse>,
    /// A parent that holds no mail and cannot be selected.
    pub no_select: bool,
    pub uidvalidity: u32,
    pub uidnext: u32,
    /// Goes up by one with every change to the mailbox.
    pub highestmodseq: u64,
    pub permanent_flags: Vec<String>,
    pub messages: BTreeMap<u32, FakeMessage>,
    /// The latest expunged UIDs, at most [`MAX_EXPUNGED`], each with the
    /// MODSEQ it went at, for QRESYNC.
    pub expunged: VecDeque<(u32, u64)>,
    /// The MODSEQ of the newest expunge dropped from `expunged`: a `Since`
    /// below it may miss expunges, so SELECT answers the whole mailbox.
    pub forgotten: u64,
    /// Listed but not yet made, as Dovecot 2.4 lists a special-use mailbox
    /// before its first message. The first APPEND makes it and answers an
    /// APPENDUID whose UIDVALIDITY is one higher than SELECT reports; later
    /// APPENDs agree with SELECT.
    pub unmade: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeMessage {
    pub raw: Vec<u8>,
    pub flags: BTreeSet<String>,
    pub internal_date: EpochMillis,
    pub modseq: u64,
}

/// The most calls the log keeps. The demo runs on this fake for a whole
/// session, so the log keeps the latest calls and drops older ones.
const MAX_CALLS: usize = 1_000;

/// The most messages `FakeSmtp` keeps, the latest ones.
const MAX_SENT: usize = 100;

/// The most expunges a mailbox remembers for QRESYNC. A `Since` older than
/// the oldest one kept gets the whole mailbox, as from a server that no
/// longer knows what went since that MODSEQ.
const MAX_EXPUNGED: usize = 1_000;

/// The flags a mailbox keeps unless a test says otherwise: the system
/// flags and any keyword a client makes up.
const PERMANENT_FLAGS: [&str; 6] = [
    "\\Answered",
    "\\Flagged",
    "\\Deleted",
    "\\Seen",
    "\\Draft",
    "\\*",
];

impl FakeMailbox {
    fn new(uidvalidity: u32, special_use: Option<SpecialUse>) -> Self {
        FakeMailbox {
            special_use,
            no_select: false,
            uidvalidity,
            uidnext: 1,
            highestmodseq: 1,
            permanent_flags: PERMANENT_FLAGS.map(String::from).to_vec(),
            messages: BTreeMap::new(),
            expunged: VecDeque::new(),
            forgotten: 0,
            unmade: false,
        }
    }

    /// Whether the mailbox keeps `flag` on its messages.
    fn keeps(&self, flag: &str) -> bool {
        self.permanent_flags
            .iter()
            .any(|f| (f == "\\*" && !flag.starts_with('\\')) || f.eq_ignore_ascii_case(flag))
    }

    /// Files `raw` under the next UID and returns it.
    fn add(&mut self, raw: Vec<u8>, flags: BTreeSet<String>, internal_date: EpochMillis) -> u32 {
        let uid = self.uidnext;
        self.unmade = false;
        self.uidnext += 1;
        self.highestmodseq += 1;
        let flags = flags.into_iter().filter(|f| self.keeps(f)).collect();
        self.messages.insert(
            uid,
            FakeMessage {
                raw,
                flags,
                internal_date,
                modseq: self.highestmodseq,
            },
        );
        uid
    }

    fn remove(&mut self, uid: u32) -> Option<FakeMessage> {
        let message = self.messages.remove(&uid)?;
        self.highestmodseq += 1;
        self.expunged.push_back((uid, self.highestmodseq));
        if self.expunged.len() > MAX_EXPUNGED
            && let Some((_, modseq)) = self.expunged.pop_front()
        {
            self.forgotten = modseq;
        }
        Some(message)
    }

    /// Adds or takes away `flag` on message `uid`, giving it a new MODSEQ
    /// when that changed anything.
    fn flag(&mut self, uid: u32, flag: &str, add: bool) {
        if !self.keeps(flag) {
            return;
        }
        let Some(message) = self.messages.get_mut(&uid) else {
            return;
        };
        let had = message.flags.iter().any(|f| f.eq_ignore_ascii_case(flag));
        match (add, had) {
            (true, false) => {
                message.flags.insert(flag.to_string());
            }
            (false, true) => message.flags.retain(|f| !f.eq_ignore_ascii_case(flag)),
            _ => return,
        }
        self.highestmodseq += 1;
        message.modseq = self.highestmodseq;
    }
}

impl Default for FakeImap {
    fn default() -> Self {
        FakeImap::new()
    }
}

impl FakeImap {
    /// A server with every extension on and the mailboxes most providers
    /// start an account with: INBOX, Sent, Drafts, Trash, Junk and
    /// Archive, each with its special use.
    pub fn new() -> Self {
        let fake = FakeImap {
            state: Mutex::new(ImapState {
                capabilities: Capabilities::all(),
                delimiter: '/',
                mailboxes: BTreeMap::new(),
                failures: VecDeque::new(),
                calls: VecDeque::new(),
                aimed: Vec::new(),
                overflow_idle: false,
                next_uidvalidity: 1000,
            }),
            changed: Notify::new(),
        };
        fake.add_mailbox("INBOX", None);
        for (name, use_) in [
            ("Sent", SpecialUse::Sent),
            ("Drafts", SpecialUse::Drafts),
            ("Trash", SpecialUse::Trash),
            ("Junk", SpecialUse::Junk),
            ("Archive", SpecialUse::Archive),
        ] {
            fake.add_mailbox(name, Some(use_));
        }
        fake
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut ImapState) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Adds an empty mailbox, or does nothing when one by that name exists.
    pub fn add_mailbox(&self, name: &str, special_use: Option<SpecialUse>) {
        self.with(|s| {
            let fresh = !s.mailboxes.contains_key(name);
            let mailbox = s.mailbox_mut(name);
            if fresh {
                mailbox.special_use = special_use;
            }
        });
    }

    /// Mail arriving in `mailbox`, unread, received at `date`. Returns its
    /// UID. Wakes an IDLE.
    pub fn deliver(&self, mailbox: &str, raw: Vec<u8>, date: EpochMillis) -> u32 {
        let uid = self.with(|s| s.mailbox_mut(mailbox).add(raw, BTreeSet::new(), date));
        self.changed.notify_waiters();
        uid
    }

    /// Another client adds or takes away `flag` on a message.
    pub fn remote_flag(&self, mailbox: &str, uid: u32, flag: &str, add: bool) {
        self.with(|s| s.mailbox_mut(mailbox).flag(uid, flag, add));
        self.changed.notify_waiters();
    }

    /// Another client expunges a message.
    pub fn remote_expunge(&self, mailbox: &str, uid: u32) {
        self.with(|s| s.mailbox_mut(mailbox).remove(uid));
        self.changed.notify_waiters();
    }

    /// The server renumbers `mailbox`: a new UIDVALIDITY, and UIDs from 1
    /// in the old order. Every UID the app held there is void.
    pub fn reset_uidvalidity(&self, mailbox: &str) {
        self.with(|s| {
            let uidvalidity = s.fresh_uidvalidity();
            let target = s.mailbox_mut(mailbox);
            let messages = std::mem::take(&mut target.messages);
            target.uidvalidity = uidvalidity;
            target.expunged.clear();
            target.forgotten = 0;
            target.highestmodseq += 1;
            for (i, (_, mut message)) in messages.into_iter().enumerate() {
                message.modseq = target.highestmodseq;
                let uid = u32::try_from(i + 1).unwrap_or(u32::MAX);
                target.messages.insert(uid, message);
            }
            target.uidnext = u32::try_from(target.messages.len() + 1).unwrap_or(u32::MAX);
        });
        self.changed.notify_waiters();
    }

    pub fn fail_next(&self, err: ImapError) {
        self.with(|s| s.failures.push_back(err));
    }

    /// The next call to `method`, a word of the call log such as
    /// `"select"`, answers `err`. Calls to other methods pass.
    pub fn fail_on(&self, method: &str, err: ImapError) {
        self.with(|s| s.aimed.push((method.to_string(), err)));
    }

    /// The next IDLE ends at once as `Woke::Changed`, as when the guard
    /// stops an IDLE that brought more than its budget.
    pub fn overflow_next_idle(&self) {
        self.with(|s| s.overflow_idle = true);
    }

    /// The latest calls, at most [`MAX_CALLS`], oldest first.
    pub fn calls(&self) -> Vec<String> {
        self.with(|s| s.calls.iter().cloned().collect())
    }

    /// How many calls went to `method`, such as `"select"`.
    pub fn calls_to(&self, method: &str) -> usize {
        self.with(|s| {
            s.calls
                .iter()
                .filter(|c| c.split(' ').next() == Some(method))
                .count()
        })
    }

    /// A copy of one message.
    pub fn message(&self, mailbox: &str, uid: u32) -> Option<FakeMessage> {
        self.with(|s| s.mailboxes.get(mailbox)?.messages.get(&uid).cloned())
    }

    /// Logs `line` as a call, then answers with the next planned failure,
    /// else the next one aimed at the call's method, else `f` on the state.
    /// What the real client refuses or answers without sending a command
    /// (a bad name, an empty UID set, an extension the server lacks) is
    /// decided before this and never logged.
    fn call<T>(
        &self,
        line: String,
        f: impl FnOnce(&mut ImapState) -> Result<T, ImapError>,
    ) -> Result<T, ImapError> {
        self.with(|s| {
            let method = line.split(' ').next().unwrap_or_default();
            let aimed = s.aimed.iter().position(|(m, _)| m == method);
            s.calls.push_back(line);
            if s.calls.len() > MAX_CALLS {
                s.calls.pop_front();
            }
            if let Some(err) = s.failures.pop_front() {
                return Err(err);
            }
            match aimed {
                Some(at) => Err(s.aimed.remove(at).1),
                None => f(s),
            }
        })
    }

    /// As [`FakeImap::call`], for a call that changes a mailbox: wakes an
    /// IDLE afterwards.
    fn change<T>(
        &self,
        line: String,
        f: impl FnOnce(&mut ImapState) -> Result<T, ImapError>,
    ) -> Result<T, ImapError> {
        let result = self.call(line, f);
        self.changed.notify_waiters();
        result
    }
}

impl ImapState {
    fn fresh_uidvalidity(&mut self) -> u32 {
        self.next_uidvalidity += 1;
        self.next_uidvalidity
    }

    /// The mailbox `name`, made empty when a test names one that is not
    /// there yet.
    pub fn mailbox_mut(&mut self, name: &str) -> &mut FakeMailbox {
        let counter = &mut self.next_uidvalidity;
        self.mailboxes.entry(name.to_string()).or_insert_with(|| {
            *counter += 1;
            FakeMailbox::new(*counter, None)
        })
    }

    /// A mailbox the app names, which must exist and hold mail.
    fn selectable(&mut self, name: &str) -> Result<&mut FakeMailbox, ImapError> {
        match self.mailboxes.get_mut(name) {
            Some(mailbox) if !mailbox.no_select => Ok(mailbox),
            _ => Err(ImapError::NoMailbox(name.to_string())),
        }
    }
}

/// The client's rule for names, which it applies before sending: modified
/// UTF-7, so ASCII only, and no line break or NUL.
fn check_name(name: &str) -> Result<(), ImapError> {
    match name.is_ascii() && !name.contains(['\r', '\n', '\0']) {
        true => Ok(()),
        false => Err(ImapError::Protocol(format!(
            "{name:?} is not a mailbox name in modified UTF-7"
        ))),
    }
}

/// The client's rule for flags: each an IMAP atom, with an optional
/// leading backslash.
fn check_flags(flags: &[String]) -> Result<(), ImapError> {
    for flag in flags {
        let atom = flag.strip_prefix('\\').unwrap_or(flag);
        let bad = |c: char| !c.is_ascii_graphic() || "(){%*\"\\]".contains(c);
        if atom.is_empty() || atom.chars().any(bad) {
            return Err(ImapError::Protocol(format!("{flag:?} is not a flag")));
        }
    }
    Ok(())
}

/// The client's rule for search text: a line break outside quotes would
/// end the command and start another, and no IMAP string carries NUL.
/// Inside quotes the client sends such text as a literal, so it passes.
fn check_search(keys: &str) -> Result<(), ImapError> {
    if keys.contains('\0') {
        return Err(ImapError::Protocol("a search holds a NUL".into()));
    }
    let mut quoted = false;
    let mut chars = keys.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' if quoted => {
                chars.next();
            }
            '\r' | '\n' if !quoted => {
                return Err(ImapError::Protocol(
                    "a search holds a line break outside quotes".into(),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

impl ImapApi for FakeImap {
    async fn capabilities(&self) -> Result<Capabilities, ImapError> {
        self.call("capabilities".into(), |s| Ok(s.capabilities))
    }

    async fn list(&self) -> Result<Vec<Listed>, ImapError> {
        self.call("list".into(), |s| {
            let special_use = s.capabilities.special_use;
            Ok(s.mailboxes
                .iter()
                .map(|(name, m)| {
                    Listed::new(
                        name.clone(),
                        Some(s.delimiter),
                        m.special_use.filter(|_| special_use),
                        m.no_select,
                    )
                })
                .collect())
        })
    }

    async fn select(&self, mailbox: &str, since: Option<Since>) -> Result<Selected, ImapError> {
        check_name(mailbox)?;
        // The client sends QRESYNC parameters only with the extension on
        // and a MODSEQ of at least 1, as RFC 7162 wants.
        let since = since.filter(|since| self.with(|s| s.capabilities.qresync) && since.modseq > 0);
        let line = match &since {
            Some(since) => format!(
                "select {mailbox} qresync {} {}",
                since.uidvalidity, since.modseq
            ),
            None => format!("select {mailbox}"),
        };
        self.call(line, |s| {
            let caps = s.capabilities;
            let m = s.selectable(mailbox)?;
            let mut selected = Selected {
                uidvalidity: m.uidvalidity,
                uidnext: Some(m.uidnext),
                highestmodseq: caps.condstore.then_some(m.highestmodseq),
                exists: u32::try_from(m.messages.len()).unwrap_or(u32::MAX),
                permanent_flags: m.permanent_flags.clone(),
                ..Selected::default()
            };
            // A server reports changes only against the UIDVALIDITY it has.
            if let Some(since) = since.filter(|since| since.uidvalidity == m.uidvalidity) {
                let known = |uid: u32| since.known.as_ref().is_none_or(|k| k.contains(uid));
                match since.modseq < m.forgotten {
                    // Some expunges since that MODSEQ are forgotten, so a
                    // list of the ones kept would be partial. Answer as a
                    // server that cannot tell what changed: every UID below
                    // UIDNEXT that is not here vanished, and every message
                    // changed.
                    true => {
                        selected.vanished = (1..m.uidnext)
                            .filter(|uid| !m.messages.contains_key(uid) && known(*uid))
                            .collect();
                        selected.changed = m
                            .messages
                            .iter()
                            .map(|(uid, message)| flags_of(*uid, message, true))
                            .collect();
                    }
                    false => {
                        selected.vanished = m
                            .expunged
                            .iter()
                            .filter(|(uid, modseq)| *modseq > since.modseq && known(*uid))
                            .map(|(uid, _)| *uid)
                            .collect();
                        selected.changed = m
                            .messages
                            .iter()
                            .filter(|(_, message)| message.modseq > since.modseq)
                            .map(|(uid, message)| flags_of(*uid, message, true))
                            .collect();
                    }
                }
            }
            Ok(selected)
        })
    }

    async fn flags(
        &self,
        mailbox: &str,
        uids: &UidSet,
        changed_since: Option<u64>,
    ) -> Result<Vec<FlagsOf>, ImapError> {
        let condstore = self.with(|s| s.capabilities.condstore);
        if changed_since.is_some() && !condstore {
            return Err(ImapError::Unsupported("CONDSTORE"));
        }
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        check_name(mailbox)?;
        let since = changed_since
            .map(|m| format!(" changedsince {m}"))
            .unwrap_or_default();
        self.call(format!("flags {mailbox} {uids}{since}"), |s| {
            let m = s.selectable(mailbox)?;
            Ok(m.messages
                .iter()
                .filter(|(uid, message)| {
                    uids.contains(**uid) && changed_since.is_none_or(|c| message.modseq > c)
                })
                .map(|(uid, message)| flags_of(*uid, message, condstore))
                .collect())
        })
    }

    async fn search(&self, mailbox: &str, keys: &str) -> Result<Vec<u32>, ImapError> {
        check_name(mailbox)?;
        check_search(keys)?;
        self.call(format!("search {mailbox} {keys}"), |s| {
            let m = s.selectable(mailbox)?;
            let query = search::parse(keys)?;
            Ok(m.messages
                .iter()
                .filter(|(uid, message)| query.matches(**uid, message))
                .map(|(uid, _)| *uid)
                .collect())
        })
    }

    async fn headers(&self, mailbox: &str, uids: &UidSet) -> Result<Vec<Fetched>, ImapError> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        check_name(mailbox)?;
        self.call(format!("headers {mailbox} {uids}"), |s| {
            let condstore = s.capabilities.condstore;
            let m = s.selectable(mailbox)?;
            Ok(m.messages
                .iter()
                .filter(|(uid, _)| uids.contains(**uid))
                .map(|(uid, message)| Fetched {
                    flags: message.flags.iter().cloned().collect(),
                    internal_date: Some(message.internal_date),
                    size: Some(message.raw.len() as u64),
                    modseq: condstore.then_some(message.modseq),
                    ..Fetched::from_header(*uid, &message.raw)
                })
                .collect())
        })
    }

    async fn body(
        &self,
        mailbox: &str,
        uid: u32,
        section: &str,
    ) -> Result<Option<Vec<u8>>, ImapError> {
        if !section
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.')
        {
            return Err(ImapError::Protocol(format!(
                "{section:?} is not a section this client fetches"
            )));
        }
        check_name(mailbox)?;
        self.call(format!("body {mailbox} {uid} {section}"), |s| {
            let m = s.selectable(mailbox)?;
            let Some(message) = m.messages.get(&uid) else {
                return Ok(None);
            };
            let raw = &message.raw;
            let split = header_end(raw);
            Ok(Some(match section {
                "" => raw.clone(),
                "HEADER" => raw[..split].to_vec(),
                "TEXT" => raw[split..].to_vec(),
                // The structure below calls every part 8bit, so a part's
                // bytes go out already decoded.
                path => mailrs_mime::part(raw, path).unwrap_or_default(),
            }))
        })
    }

    async fn structure(&self, mailbox: &str, uid: u32) -> Result<Option<BodyStructure>, ImapError> {
        check_name(mailbox)?;
        self.call(format!("structure {mailbox} {uid}"), |s| {
            let m = s.selectable(mailbox)?;
            let Some(message) = m.messages.get(&uid) else {
                return Ok(None);
            };
            let Some(parts) = mailrs_mime::parts(&message.raw) else {
                return Ok(None);
            };
            let mut structure = BodyStructure {
                root: parts.root,
                ..BodyStructure::default()
            };
            strip(&mut structure.root, &mut structure.encodings);
            Ok(Some(structure))
        })
    }

    async fn store(
        &self,
        mailbox: &str,
        uids: &UidSet,
        add: bool,
        flags: &[String],
    ) -> Result<(), ImapError> {
        if uids.is_empty() || flags.is_empty() {
            return Ok(());
        }
        check_flags(flags)?;
        check_name(mailbox)?;
        let sign = if add { '+' } else { '-' };
        self.change(
            format!("store {mailbox} {uids} {sign} {}", flags.join(" ")),
            |s| {
                let m = s.selectable(mailbox)?;
                let targets: Vec<u32> = m
                    .messages
                    .keys()
                    .copied()
                    .filter(|uid| uids.contains(*uid))
                    .collect();
                for uid in targets {
                    for flag in flags {
                        m.flag(uid, flag, add);
                    }
                }
                Ok(())
            },
        )
    }

    async fn move_to(
        &self,
        mailbox: &str,
        uids: &UidSet,
        to: &str,
    ) -> Result<Option<CopyUid>, ImapError> {
        if !self.with(|s| s.capabilities.moves) {
            return Err(ImapError::Unsupported("MOVE"));
        }
        if uids.is_empty() {
            return Ok(None);
        }
        check_name(mailbox)?;
        check_name(to)?;
        self.change(format!("move {mailbox} {uids} {to}"), |s| {
            s.transfer(mailbox, uids, to, true)
        })
    }

    async fn copy_to(
        &self,
        mailbox: &str,
        uids: &UidSet,
        to: &str,
    ) -> Result<Option<CopyUid>, ImapError> {
        if uids.is_empty() {
            return Ok(None);
        }
        check_name(mailbox)?;
        check_name(to)?;
        self.change(format!("copy {mailbox} {uids} {to}"), |s| {
            s.transfer(mailbox, uids, to, false)
        })
    }

    async fn expunge(&self, mailbox: &str, uids: &UidSet) -> Result<(), ImapError> {
        if !self.with(|s| s.capabilities.uidplus) {
            return Err(ImapError::Unsupported("UIDPLUS"));
        }
        if uids.is_empty() {
            return Ok(());
        }
        check_name(mailbox)?;
        self.change(format!("expunge {mailbox} {uids}"), |s| {
            let m = s.selectable(mailbox)?;
            let deleted: Vec<u32> = m
                .messages
                .iter()
                .filter(|(uid, message)| {
                    uids.contains(**uid)
                        && message
                            .flags
                            .iter()
                            .any(|f| f.eq_ignore_ascii_case("\\Deleted"))
                })
                .map(|(uid, _)| *uid)
                .collect();
            for uid in deleted {
                m.remove(uid);
            }
            Ok(())
        })
    }

    async fn append(
        &self,
        mailbox: &str,
        flags: &[String],
        raw: &[u8],
    ) -> Result<Option<AppendUid>, ImapError> {
        check_name(mailbox)?;
        check_flags(flags)?;
        self.change(format!("append {mailbox} {}", flags.join(" ")), |s| {
            let uidplus = s.capabilities.uidplus;
            let m = s.selectable(mailbox)?;
            // Dovecot's first APPEND into a mailbox it listed but had not
            // made answers a UIDVALIDITY one past what SELECT reports.
            let ahead = u32::from(m.unmade);
            let uid = m.add(
                raw.to_vec(),
                flags.iter().cloned().collect(),
                Utc::now().timestamp_millis(),
            );
            Ok(uidplus.then_some(AppendUid {
                uidvalidity: m.uidvalidity.wrapping_add(ahead),
                uid,
            }))
        })
    }

    async fn create(&self, mailbox: &str) -> Result<(), ImapError> {
        check_name(mailbox)?;
        self.change(format!("create {mailbox}"), |s| {
            if s.mailboxes.contains_key(mailbox) {
                return Err(ImapError::Refused(
                    "[ALREADYEXISTS] Mailbox already exists".into(),
                ));
            }
            let uidvalidity = s.fresh_uidvalidity();
            s.mailboxes
                .insert(mailbox.to_string(), FakeMailbox::new(uidvalidity, None));
            Ok(())
        })
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), ImapError> {
        check_name(from)?;
        check_name(to)?;
        self.change(format!("rename {from} {to}"), |s| {
            if !s.mailboxes.contains_key(from) {
                return Err(ImapError::NoMailbox(from.to_string()));
            }
            if s.mailboxes.contains_key(to) {
                return Err(ImapError::Refused(
                    "[ALREADYEXISTS] Mailbox already exists".into(),
                ));
            }
            // Children move with their parent.
            let prefix = format!("{from}{}", s.delimiter);
            let names: Vec<String> = s
                .mailboxes
                .keys()
                .filter(|name| *name == from || name.starts_with(&prefix))
                .cloned()
                .collect();
            for name in names {
                if let Some(mailbox) = s.mailboxes.remove(&name) {
                    s.mailboxes
                        .insert(format!("{to}{}", &name[from.len()..]), mailbox);
                }
            }
            Ok(())
        })
    }

    async fn delete(&self, mailbox: &str) -> Result<(), ImapError> {
        check_name(mailbox)?;
        self.change(format!("delete {mailbox}"), |s| {
            match s.mailboxes.remove(mailbox) {
                Some(_) => Ok(()),
                None => Err(ImapError::NoMailbox(mailbox.to_string())),
            }
        })
    }

    async fn idle(&self, mailbox: &str, limit: Duration) -> Result<Woke, ImapError> {
        if !self.with(|s| s.capabilities.idle) {
            return Err(ImapError::Unsupported("IDLE"));
        }
        check_name(mailbox)?;
        let limit = limit.min(IDLE_LIMIT);
        let start = self.call(format!("idle {mailbox}"), |s| {
            let start = s.selectable(mailbox)?.highestmodseq;
            Ok((!std::mem::take(&mut s.overflow_idle)).then_some(start))
        })?;
        let Some(start) = start else {
            return Ok(Woke::Changed);
        };
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            // Registered before the check, so a change between the check and
            // the wait still wakes it.
            notified.as_mut().enable();
            let now = self.with(|s| s.mailboxes.get(mailbox).map(|m| m.highestmodseq));
            if now != Some(start) {
                return Ok(Woke::Changed);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Ok(Woke::TimedOut);
            }
        }
    }
}

impl ImapState {
    /// Copies the messages in `uids` from `from` to `to`, taking them out
    /// of `from` when `take` says so. The pairs of UIDs under UIDPLUS.
    fn transfer(
        &mut self,
        from: &str,
        uids: &UidSet,
        to: &str,
        take: bool,
    ) -> Result<Option<CopyUid>, ImapError> {
        let uidplus = self.capabilities.uidplus;
        if !self.mailboxes.get(to).is_some_and(|m| !m.no_select) {
            return Err(ImapError::NoMailbox(to.to_string()));
        }
        let source = self.selectable(from)?;
        let chosen: Vec<u32> = source
            .messages
            .keys()
            .copied()
            .filter(|uid| uids.contains(*uid))
            .collect();
        let mut moving = Vec::new();
        for uid in chosen {
            let message = match take {
                true => source.remove(uid),
                false => source.messages.get(&uid).cloned(),
            };
            if let Some(message) = message {
                moving.push((uid, message));
            }
        }
        let target = self.selectable(to)?;
        let pairs: Vec<(u32, u32)> = moving
            .into_iter()
            .map(|(uid, message)| {
                (
                    uid,
                    target.add(message.raw, message.flags, message.internal_date),
                )
            })
            .collect();
        let uidvalidity = target.uidvalidity;
        Ok((uidplus && !pairs.is_empty()).then_some(CopyUid { uidvalidity, pairs }))
    }
}

fn flags_of(uid: u32, message: &FakeMessage, condstore: bool) -> FlagsOf {
    FlagsOf {
        uid,
        flags: message.flags.iter().cloned().collect(),
        modseq: condstore.then_some(message.modseq),
    }
}

/// Where the header block of `raw` ends, its blank line included.
fn header_end(raw: &[u8]) -> usize {
    raw.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|at| at + 4)
        .or_else(|| raw.windows(2).position(|w| w == b"\n\n").map(|at| at + 2))
        .unwrap_or(raw.len())
}

/// Clears each part's bytes, as a server's structure has none, and calls
/// every leaf 8bit, since the fake's parts go out decoded.
fn strip(part: &mut Part, encodings: &mut BTreeMap<String, String>) {
    part.data = None;
    if part.children.is_empty() {
        encodings.insert(part.path.clone(), "8bit".into());
    }
    for child in &mut part.children {
        strip(child, encodings);
    }
}

/// A plain message for a test: from Ann to me, with `id` in its
/// Message-ID and body, sent at `date`.
pub fn raw_message(
    id: &str,
    subject: &str,
    date: EpochMillis,
    in_reply_to: Option<&str>,
) -> Vec<u8> {
    let sent = DateTime::from_timestamp_millis(date)
        .unwrap_or_default()
        .to_rfc2822();
    let mut raw = format!(
        "From: Ann <ann@example.com>\r\nTo: me@example.com\r\nSubject: {subject}\r\nDate: {sent}\r\nMessage-ID: <{id}@example.com>\r\n"
    );
    if let Some(parent) = in_reply_to {
        raw.push_str(&format!(
            "In-Reply-To: <{parent}@example.com>\r\nReferences: <{parent}@example.com>\r\n"
        ));
    }
    raw.push_str(&format!(
        "Content-Type: text/plain; charset=utf-8\r\n\r\nBody of {id}\r\n"
    ));
    raw.into_bytes()
}

/// The SEARCH keys the fake understands: what the adapter prints from a
/// query tree, and the few it sends on its own.
mod search {
    use super::*;

    pub(super) enum Key {
        All,
        Flag(String, bool),
        Field(&'static str, String),
        Header(String, String),
        Text(bool, String),
        Since(NaiveDate),
        Before(NaiveDate),
        On(NaiveDate),
        Larger(u64),
        Smaller(u64),
        Uid(UidSet),
        Not(Box<Key>),
        Or(Box<Key>, Box<Key>),
        And(Vec<Key>),
    }

    pub(super) fn parse(keys: &str) -> Result<Key, ImapError> {
        let mut tokens = tokenize(keys)?.into_iter().peekable();
        if tokens
            .peek()
            .is_some_and(|t| t.eq_ignore_ascii_case("CHARSET"))
        {
            tokens.next();
            tokens.next();
        }
        let mut all = Vec::new();
        while tokens.peek().is_some() {
            all.push(key(&mut tokens)?);
        }
        Ok(Key::And(all))
    }

    type Tokens = std::iter::Peekable<std::vec::IntoIter<String>>;

    fn key(tokens: &mut Tokens) -> Result<Key, ImapError> {
        let token = tokens
            .next()
            .ok_or_else(|| bad("a search that ends early"))?;
        let mut arg = || {
            tokens
                .next()
                .ok_or_else(|| bad(&format!("{token} without its argument")))
        };
        let date = |text: String| {
            NaiveDate::parse_from_str(&text, "%d-%b-%Y")
                .map_err(|_| bad(&format!("the date {text}")))
        };
        let number = |text: String| {
            text.parse::<u64>()
                .map_err(|_| bad(&format!("the number {text}")))
        };
        Ok(match token.to_ascii_uppercase().as_str() {
            "ALL" => Key::All,
            "SEEN" => Key::Flag("\\Seen".into(), true),
            "UNSEEN" => Key::Flag("\\Seen".into(), false),
            "FLAGGED" => Key::Flag("\\Flagged".into(), true),
            "UNFLAGGED" => Key::Flag("\\Flagged".into(), false),
            "ANSWERED" => Key::Flag("\\Answered".into(), true),
            "UNANSWERED" => Key::Flag("\\Answered".into(), false),
            "DELETED" => Key::Flag("\\Deleted".into(), true),
            "UNDELETED" => Key::Flag("\\Deleted".into(), false),
            "DRAFT" => Key::Flag("\\Draft".into(), true),
            "KEYWORD" => Key::Flag(arg()?, true),
            "UNKEYWORD" => Key::Flag(arg()?, false),
            "FROM" => Key::Field("from", arg()?),
            "TO" => Key::Field("to", arg()?),
            "CC" => Key::Field("cc", arg()?),
            "SUBJECT" => Key::Field("subject", arg()?),
            "HEADER" => {
                let name = arg()?;
                Key::Header(name, arg()?)
            }
            "BODY" => Key::Text(false, arg()?),
            "TEXT" => Key::Text(true, arg()?),
            "SINCE" => Key::Since(date(arg()?)?),
            "BEFORE" => Key::Before(date(arg()?)?),
            "ON" => Key::On(date(arg()?)?),
            "LARGER" => Key::Larger(number(arg()?)?),
            "SMALLER" => Key::Smaller(number(arg()?)?),
            "UID" => Key::Uid(uid_set(&arg()?)?),
            "NOT" => Key::Not(Box::new(key(tokens)?)),
            "OR" => {
                let left = key(tokens)?;
                Key::Or(Box::new(left), Box::new(key(tokens)?))
            }
            "(" => {
                let mut inner = Vec::new();
                while tokens.peek().is_some_and(|t| t != ")") {
                    inner.push(key(tokens)?);
                }
                tokens
                    .next()
                    .ok_or_else(|| bad("an unclosed parenthesis"))?;
                Key::And(inner)
            }
            other => return Err(bad(other)),
        })
    }

    fn bad(what: &str) -> ImapError {
        ImapError::Protocol(format!("the fake cannot search {what}"))
    }

    fn uid_set(text: &str) -> Result<UidSet, ImapError> {
        let mut set = UidSet::new();
        for piece in text.split(',') {
            let bound = |b: &str| match b {
                "*" => Ok(u32::MAX),
                b => b
                    .parse::<u32>()
                    .map_err(|_| bad(&format!("the UID set {text}"))),
            };
            match piece.split_once(':') {
                Some((from, to)) => set.insert(bound(from)?, bound(to)?),
                None => {
                    let uid = bound(piece)?;
                    set.insert(uid, uid);
                }
            }
        }
        Ok(set)
    }

    /// Atoms, quoted strings with their escapes undone, and parentheses.
    fn tokenize(keys: &str) -> Result<Vec<String>, ImapError> {
        let mut tokens = Vec::new();
        let mut chars = keys.chars().peekable();
        while let Some(&c) = chars.peek() {
            match c {
                ' ' => {
                    chars.next();
                }
                '(' | ')' => {
                    tokens.push(c.to_string());
                    chars.next();
                }
                '"' => {
                    chars.next();
                    let mut value = String::new();
                    loop {
                        match chars.next() {
                            Some('\\') => value.extend(chars.next()),
                            Some('"') => break,
                            Some(c) => value.push(c),
                            None => return Err(bad("an unclosed string")),
                        }
                    }
                    tokens.push(value);
                }
                _ => {
                    let mut atom = String::new();
                    while let Some(&c) = chars.peek() {
                        if c == ' ' || c == '(' || c == ')' {
                            break;
                        }
                        atom.push(c);
                        chars.next();
                    }
                    tokens.push(atom);
                }
            }
        }
        Ok(tokens)
    }

    impl Key {
        pub(super) fn matches(&self, uid: u32, message: &FakeMessage) -> bool {
            let has = |needle: &str, hay: &str| hay.to_lowercase().contains(&needle.to_lowercase());
            let day =
                || DateTime::from_timestamp_millis(message.internal_date).map(|d| d.date_naive());
            match self {
                Key::All => true,
                Key::Flag(flag, want) => {
                    message.flags.iter().any(|f| f.eq_ignore_ascii_case(flag)) == *want
                }
                Key::Field(field, needle) => {
                    let fetched = Fetched::from_header(uid, &message.raw);
                    let people = |list: &[mailrs_domain::Address]| {
                        list.iter().any(|a| {
                            has(needle, &a.email)
                                || a.name.as_deref().is_some_and(|n| has(needle, n))
                        })
                    };
                    match *field {
                        "from" => people(fetched.from.as_slice()),
                        "to" => people(&fetched.to),
                        "cc" => people(&fetched.cc),
                        _ => has(needle, &fetched.subject),
                    }
                }
                Key::Header(name, needle) => {
                    let head = String::from_utf8_lossy(&message.raw[..header_end(&message.raw)])
                        .replace("\r\n ", " ");
                    head.lines().any(|line| {
                        line.split_once(':').is_some_and(|(n, v)| {
                            n.trim().eq_ignore_ascii_case(name) && has(needle, v)
                        })
                    })
                }
                Key::Text(whole, needle) => {
                    let start = if *whole { 0 } else { header_end(&message.raw) };
                    has(needle, &String::from_utf8_lossy(&message.raw[start..]))
                }
                Key::Since(date) => day().is_some_and(|d| d >= *date),
                Key::Before(date) => day().is_some_and(|d| d < *date),
                Key::On(date) => day() == Some(*date),
                Key::Larger(n) => message.raw.len() as u64 > *n,
                Key::Smaller(n) => (message.raw.len() as u64) < *n,
                Key::Uid(set) => set.contains(uid),
                Key::Not(inner) => !inner.matches(uid, message),
                Key::Or(left, right) => left.matches(uid, message) || right.matches(uid, message),
                Key::And(all) => all.iter().all(|k| k.matches(uid, message)),
            }
        }
    }
}

/// An SMTP submission server that keeps what it is handed.
#[derive(Default)]
pub struct FakeSmtp {
    state: Mutex<SmtpState>,
}

#[derive(Default)]
pub struct SmtpState {
    /// The latest messages handed over, at most [`MAX_SENT`], oldest first.
    pub sent: VecDeque<Submitted>,
    /// Errors the next submissions return, one each.
    pub failures: VecDeque<ImapError>,
}

/// One message handed to the fake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submitted {
    pub from: String,
    pub to: Vec<String>,
    pub raw: Vec<u8>,
}

impl FakeSmtp {
    pub fn new() -> Self {
        FakeSmtp::default()
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut SmtpState) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub fn fail_next(&self, err: ImapError) {
        self.with(|s| s.failures.push_back(err));
    }

    /// The latest messages handed over, oldest first.
    pub fn sent(&self) -> Vec<Submitted> {
        self.with(|s| s.sent.iter().cloned().collect())
    }
}

impl Submit for FakeSmtp {
    async fn submit(&self, from: &str, to: &[String], raw: &[u8]) -> Result<(), ImapError> {
        // The real client checks the envelope before it connects.
        for address in to.iter().map(String::as_str).chain([from]) {
            let address = address.trim();
            let parts = address.split_once('@');
            if parts.is_none_or(|(local, domain)| local.is_empty() || domain.is_empty())
                || address.contains(|c: char| c.is_whitespace() || c.is_control())
            {
                return Err(ImapError::Refused(format!(
                    "{address} is not an email address"
                )));
            }
        }
        if to.is_empty() {
            return Err(ImapError::Refused(
                "a message needs at least one recipient".into(),
            ));
        }
        self.with(|s| {
            if let Some(err) = s.failures.pop_front() {
                return Err(err);
            }
            if s.sent.len() == MAX_SENT {
                s.sent.pop_front();
            }
            s.sent.push_back(Submitted {
                from: from.to_string(),
                to: to.to_vec(),
                raw: raw.to_vec(),
            });
            Ok(())
        })
    }
}
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// 1 March 2026, noon UTC.
    const MARCH: EpochMillis = 1_772_366_400_000;
    const DAY: EpochMillis = 24 * 60 * 60 * 1000;

    fn inbox_with(count: u32) -> FakeImap {
        let fake = FakeImap::new();
        for i in 1..=count {
            let at = MARCH + EpochMillis::from(i) * DAY;
            fake.deliver(
                "INBOX",
                raw_message(&format!("m{i}"), &format!("Subject {i}"), at, None),
                at,
            );
        }
        fake
    }

    fn flags(list: &[&str]) -> Vec<String> {
        list.iter().map(|f| f.to_string()).collect()
    }

    #[tokio::test]
    async fn a_new_server_lists_its_mailboxes_with_special_use() {
        let fake = FakeImap::new();
        let listed = fake.list().await.unwrap();
        let sent = listed.iter().find(|l| l.name == "Sent").unwrap();
        assert_eq!(sent.special_use, Some(SpecialUse::Sent));
        fake.with(|s| s.capabilities.special_use = false);
        let listed = fake.list().await.unwrap();
        assert!(listed.iter().all(|l| l.special_use.is_none()));
    }

    #[tokio::test]
    async fn select_reports_uidvalidity_uidnext_and_modseq() {
        let fake = inbox_with(3);
        let selected = fake.select("INBOX", None).await.unwrap();
        assert_eq!(selected.uidnext, Some(4));
        assert_eq!(selected.exists, 3);
        assert!(selected.highestmodseq.is_some());
        assert!(selected.keeps("$muted"));
        fake.with(|s| s.capabilities.condstore = false);
        assert_eq!(
            fake.select("INBOX", None).await.unwrap().highestmodseq,
            None
        );
    }

    #[tokio::test]
    async fn qresync_select_reports_what_changed_since() {
        let fake = inbox_with(3);
        let first = fake.select("INBOX", None).await.unwrap();
        fake.remote_flag("INBOX", 1, "\\Seen", true);
        fake.remote_expunge("INBOX", 2);
        let since = Since {
            uidvalidity: first.uidvalidity,
            modseq: first.highestmodseq.unwrap(),
            known: None,
        };
        let again = fake.select("INBOX", Some(since.clone())).await.unwrap();
        assert_eq!(again.vanished, UidSet::from_uids([2]));
        assert_eq!(again.changed.iter().map(|f| f.uid).collect::<Vec<_>>(), [1]);
        fake.with(|s| s.capabilities.qresync = false);
        let plain = fake.select("INBOX", Some(since)).await.unwrap();
        assert!(plain.vanished.is_empty() && plain.changed.is_empty());
    }

    #[tokio::test]
    async fn a_new_uidvalidity_renumbers_and_ignores_the_old_since() {
        let fake = inbox_with(2);
        fake.remote_expunge("INBOX", 1);
        let before = fake.select("INBOX", None).await.unwrap();
        fake.reset_uidvalidity("INBOX");
        let since = Since {
            uidvalidity: before.uidvalidity,
            modseq: 1,
            known: None,
        };
        let after = fake.select("INBOX", Some(since)).await.unwrap();
        assert_ne!(after.uidvalidity, before.uidvalidity);
        assert!(after.vanished.is_empty());
        assert_eq!(
            fake.with(|s| s.mailboxes["INBOX"]
                .messages
                .keys()
                .copied()
                .collect::<Vec<_>>()),
            [1]
        );
        assert_eq!(after.uidnext, Some(2));
    }

    #[tokio::test]
    async fn flags_changed_since_need_condstore() {
        let fake = inbox_with(2);
        let modseq = fake
            .select("INBOX", None)
            .await
            .unwrap()
            .highestmodseq
            .unwrap();
        fake.remote_flag("INBOX", 2, "\\Flagged", true);
        let changed = fake
            .flags("INBOX", &UidSet::from_uid(1), Some(modseq))
            .await
            .unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].flags, ["\\Flagged"]);
        fake.with(|s| s.capabilities.condstore = false);
        let err = fake
            .flags("INBOX", &UidSet::from_uid(1), Some(modseq))
            .await
            .err();
        assert_eq!(err, Some(ImapError::Unsupported("CONDSTORE")));
    }

    #[tokio::test]
    async fn headers_read_the_raw_message() {
        let fake = FakeImap::new();
        fake.deliver(
            "INBOX",
            raw_message("r", "Re: plans", MARCH, Some("p")),
            MARCH,
        );
        let fetched = fake.headers("INBOX", &UidSet::from_uid(1)).await.unwrap();
        assert_eq!(fetched[0].subject, "Re: plans");
        assert_eq!(fetched[0].in_reply_to.as_deref(), Some("<p@example.com>"));
        assert_eq!(fetched[0].internal_date, Some(MARCH));
        assert!(fetched[0].size.is_some());
    }

    #[tokio::test]
    async fn a_keyword_the_mailbox_does_not_keep_is_dropped() {
        let fake = inbox_with(1);
        fake.with(|s| s.mailbox_mut("INBOX").permanent_flags = flags(&["\\Seen", "\\Flagged"]));
        fake.store(
            "INBOX",
            &UidSet::from_uids([1]),
            true,
            &flags(&["\\Seen", "$muted"]),
        )
        .await
        .unwrap();
        assert_eq!(
            fake.message("INBOX", 1)
                .unwrap()
                .flags
                .into_iter()
                .collect::<Vec<_>>(),
            ["\\Seen"]
        );
    }

    #[tokio::test]
    async fn move_hands_back_new_uids_under_uidplus_and_none_without() {
        let fake = inbox_with(2);
        let copied = fake
            .move_to("INBOX", &UidSet::from_uids([1, 2]), "Archive")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(copied.pairs, [(1, 1), (2, 2)]);
        assert!(fake.with(|s| s.mailboxes["INBOX"].messages.is_empty()));
        fake.with(|s| s.capabilities.uidplus = false);
        fake.deliver("INBOX", raw_message("m3", "x", MARCH, None), MARCH);
        assert_eq!(
            fake.move_to("INBOX", &UidSet::from_uids([3]), "Archive")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn move_without_the_extension_is_unsupported() {
        let fake = inbox_with(1);
        fake.with(|s| s.capabilities.moves = false);
        let err = fake
            .move_to("INBOX", &UidSet::from_uids([1]), "Archive")
            .await
            .err();
        assert_eq!(err, Some(ImapError::Unsupported("MOVE")));
        let err = fake
            .copy_to("INBOX", &UidSet::from_uids([1]), "Gone")
            .await
            .err();
        assert_eq!(err, Some(ImapError::NoMailbox("Gone".into())));
    }

    #[tokio::test]
    async fn expunge_takes_only_the_named_deleted_messages() {
        let fake = inbox_with(3);
        fake.store(
            "INBOX",
            &UidSet::from_uids([1, 2]),
            true,
            &flags(&["\\Deleted"]),
        )
        .await
        .unwrap();
        fake.expunge("INBOX", &UidSet::from_uids([1, 3]))
            .await
            .unwrap();
        let left: Vec<u32> = fake.with(|s| s.mailboxes["INBOX"].messages.keys().copied().collect());
        assert_eq!(left, [2, 3]);
    }

    #[tokio::test]
    async fn append_files_the_message_with_its_flags() {
        let fake = FakeImap::new();
        let uid = fake
            .append(
                "Sent",
                &flags(&["\\Seen"]),
                &raw_message("s", "sent", MARCH, None),
            )
            .await
            .unwrap()
            .unwrap();
        let stored = fake.message("Sent", uid.uid).unwrap();
        assert!(stored.flags.contains("\\Seen"));
        assert_eq!(
            uid.uidvalidity,
            fake.with(|s| s.mailboxes["Sent"].uidvalidity)
        );
    }

    #[tokio::test]
    async fn search_understands_the_keys_the_adapter_prints() {
        let fake = inbox_with(3);
        fake.remote_flag("INBOX", 2, "\\Seen", true);
        let search = |keys: &'static str| {
            let fake = &fake;
            async move { fake.search("INBOX", keys).await.unwrap() }
        };
        assert_eq!(search("UNSEEN").await, [1, 3]);
        assert_eq!(search("SUBJECT \"subject 2\"").await, [2]);
        assert_eq!(search("SINCE 3-Mar-2026").await, [2, 3]);
        assert_eq!(search("OR UID 1 NOT SEEN").await, [1, 3]);
        assert_eq!(search("HEADER Message-ID \"<m3@example.com>\"").await, [3]);
        assert_eq!(
            search("CHARSET UTF-8 FROM \"ann\" (BEFORE 3-Mar-2026)").await,
            [1]
        );
        assert!(matches!(
            fake.search("INBOX", "FUZZY x").await,
            Err(ImapError::Protocol(_))
        ));
    }

    #[tokio::test]
    async fn body_and_structure_agree_on_part_paths() {
        let fake = FakeImap::new();
        let raw = b"Subject: files\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nhello\r\n--b\r\nContent-Type: application/pdf\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=a.pdf\r\n\r\naGk=\r\n--b--\r\n";
        fake.deliver("INBOX", raw.to_vec(), MARCH);
        let structure = fake.structure("INBOX", 1).await.unwrap().unwrap();
        let pdf = &structure.root.children[1];
        assert_eq!(
            (pdf.path.as_str(), pdf.filename.as_deref()),
            ("2", Some("a.pdf"))
        );
        assert_eq!(pdf.data, None);
        let bytes = fake.body("INBOX", 1, "2").await.unwrap().unwrap();
        assert_eq!(structure.decode("2", &bytes).as_deref(), Some(&b"hi"[..]));
        assert!(
            fake.body("INBOX", 1, "HEADER")
                .await
                .unwrap()
                .unwrap()
                .ends_with(b"\r\n\r\n")
        );
        assert_eq!(fake.body("INBOX", 9, "").await.unwrap(), None);
    }

    #[tokio::test]
    async fn mailbox_names_must_be_modified_utf7() {
        let fake = FakeImap::new();
        assert!(matches!(
            fake.create("Envoyés").await,
            Err(ImapError::Protocol(_))
        ));
        fake.create(&mailrs_imap::utf7::encode("Envoyés"))
            .await
            .unwrap();
        fake.create("Work").await.unwrap();
        fake.create("Work/Q3").await.unwrap();
        fake.rename("Work", "Job").await.unwrap();
        assert!(fake.with(|s| s.mailboxes.contains_key("Job/Q3")));
        assert_eq!(
            fake.delete("Work").await.err(),
            Some(ImapError::NoMailbox("Work".into()))
        );
    }

    #[tokio::test]
    async fn a_planned_failure_answers_the_next_call_and_is_logged() {
        let fake = FakeImap::new();
        fake.fail_next(ImapError::Network("reset".into()));
        assert_eq!(
            fake.list().await.err(),
            Some(ImapError::Network("reset".into()))
        );
        assert!(fake.list().await.is_ok());
        assert_eq!(fake.calls_to("list"), 2);
    }

    #[tokio::test]
    async fn idle_wakes_on_delivery() {
        let fake = std::sync::Arc::new(FakeImap::new());
        let waiting = tokio::spawn({
            let fake = fake.clone();
            async move { fake.idle("INBOX", Duration::from_secs(600)).await }
        });
        tokio::task::yield_now().await;
        while fake.calls_to("idle") == 0 {
            tokio::task::yield_now().await;
        }
        fake.deliver("INBOX", raw_message("n", "new", MARCH, None), MARCH);
        assert_eq!(waiting.await.unwrap(), Ok(Woke::Changed));
    }

    #[tokio::test(start_paused = true)]
    async fn idle_times_out_and_ignores_other_mailboxes() {
        let fake = std::sync::Arc::new(FakeImap::new());
        let waiting = tokio::spawn({
            let fake = fake.clone();
            async move { fake.idle("INBOX", Duration::from_secs(60)).await }
        });
        while fake.calls_to("idle") == 0 {
            tokio::task::yield_now().await;
        }
        fake.deliver("Archive", raw_message("n", "new", MARCH, None), MARCH);
        assert_eq!(waiting.await.unwrap(), Ok(Woke::TimedOut));
    }

    #[tokio::test]
    async fn idle_without_the_extension_is_unsupported() {
        let fake = FakeImap::new();
        fake.with(|s| s.capabilities.idle = false);
        let err = fake.idle("INBOX", Duration::from_secs(1)).await.err();
        assert_eq!(err, Some(ImapError::Unsupported("IDLE")));
    }

    #[tokio::test]
    async fn smtp_keeps_what_it_is_handed_and_fails_on_request() {
        let smtp = FakeSmtp::new();
        smtp.submit("me@example.com", &["ann@example.com".into()], b"raw")
            .await
            .unwrap();
        smtp.fail_next(ImapError::Auth { text: "535".into() });
        assert!(
            smtp.submit("me@example.com", &["ann@example.com".into()], b"raw")
                .await
                .is_err()
        );
        assert!(matches!(
            smtp.submit("me@example.com", &[], b"raw").await,
            Err(ImapError::Refused(_))
        ));
        assert_eq!(
            smtp.sent(),
            [Submitted {
                from: "me@example.com".into(),
                to: vec!["ann@example.com".into()],
                raw: b"raw".to_vec()
            }]
        );
    }

    #[tokio::test]
    async fn smtp_refuses_a_bad_envelope_before_a_planned_failure() {
        let smtp = FakeSmtp::new();
        smtp.fail_next(ImapError::Network("reset".into()));
        assert!(matches!(
            smtp.submit("me@example.com", &["ann\r\nRCPT TO:<x@y>".into()], b"raw")
                .await,
            Err(ImapError::Refused(_))
        ));
        assert!(matches!(
            smtp.submit("me@example.com", &[], b"raw").await,
            Err(ImapError::Refused(_))
        ));
        assert_eq!(
            smtp.submit("me@example.com", &["ann@example.com".into()], b"raw")
                .await,
            Err(ImapError::Network("reset".into()))
        );
    }

    #[tokio::test]
    async fn flags_since_without_condstore_is_unsupported_even_for_no_uids() {
        let fake = inbox_with(1);
        fake.with(|s| s.capabilities.condstore = false);
        assert_eq!(
            fake.flags("INBOX", &UidSet::new(), Some(1)).await,
            Err(ImapError::Unsupported("CONDSTORE"))
        );
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn move_without_the_extension_is_unsupported_even_for_no_uids() {
        let fake = inbox_with(1);
        fake.with(|s| s.capabilities.moves = false);
        assert_eq!(
            fake.move_to("INBOX", &UidSet::new(), "Archive").await,
            Err(ImapError::Unsupported("MOVE"))
        );
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn expunge_without_uidplus_is_unsupported_even_for_no_uids() {
        let fake = inbox_with(1);
        fake.with(|s| s.capabilities.uidplus = false);
        assert_eq!(
            fake.expunge("INBOX", &UidSet::new()).await,
            Err(ImapError::Unsupported("UIDPLUS"))
        );
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn the_call_log_keeps_only_the_latest_calls() {
        let fake = FakeImap::new();
        for _ in 0..MAX_CALLS {
            fake.list().await.unwrap();
        }
        fake.capabilities().await.unwrap();
        let calls = fake.calls();
        assert_eq!(calls.len(), MAX_CALLS);
        assert_eq!(calls.last().map(String::as_str), Some("capabilities"));
        assert_eq!(fake.calls_to("list"), MAX_CALLS - 1);
    }

    #[tokio::test]
    async fn smtp_keeps_only_the_latest_messages() {
        let smtp = FakeSmtp::new();
        for i in 0..=MAX_SENT {
            smtp.submit(
                "me@example.com",
                &["ann@example.com".into()],
                i.to_string().as_bytes(),
            )
            .await
            .unwrap();
        }
        let sent = smtp.sent();
        assert_eq!(sent.len(), MAX_SENT);
        assert_eq!(sent[0].raw, b"1");
    }

    /// A mailbox of `count` messages, with the state a sync took after
    /// they arrived, and the first `gone` of them expunged since.
    async fn expunged_after_since(count: u32, gone: u32) -> (FakeImap, Since) {
        let fake = FakeImap::new();
        for i in 1..=count {
            fake.deliver(
                "INBOX",
                raw_message(&format!("m{i}"), "x", MARCH, None),
                MARCH,
            );
        }
        let first = fake.select("INBOX", None).await.unwrap();
        for uid in 1..=gone {
            fake.remote_expunge("INBOX", uid);
        }
        let since = Since {
            uidvalidity: first.uidvalidity,
            modseq: first.highestmodseq.unwrap(),
            known: None,
        };
        (fake, since)
    }

    #[tokio::test]
    async fn a_mailbox_remembers_a_bounded_number_of_expunges() {
        let gone = u32::try_from(MAX_EXPUNGED).unwrap() + 1;
        let (fake, _) = expunged_after_since(gone + 1, gone).await;
        assert_eq!(
            fake.with(|s| s.mailboxes["INBOX"].expunged.len()),
            MAX_EXPUNGED
        );
    }

    #[tokio::test]
    async fn a_since_within_the_expunges_kept_gets_only_what_changed() {
        let gone = u32::try_from(MAX_EXPUNGED).unwrap();
        let (fake, since) = expunged_after_since(gone + 2, gone).await;
        fake.remote_flag("INBOX", gone + 2, "\\Seen", true);
        let selected = fake.select("INBOX", Some(since)).await.unwrap();
        assert_eq!(selected.vanished, UidSet::range(1, gone));
        assert_eq!(
            selected.changed.iter().map(|f| f.uid).collect::<Vec<_>>(),
            [gone + 2]
        );
    }

    #[tokio::test]
    async fn a_since_older_than_the_expunges_kept_gets_the_whole_mailbox() {
        let gone = u32::try_from(MAX_EXPUNGED).unwrap() + 1;
        let (fake, since) = expunged_after_since(gone + 2, gone).await;
        let selected = fake.select("INBOX", Some(since.clone())).await.unwrap();
        assert_eq!(selected.vanished, UidSet::range(1, gone));
        assert_eq!(
            selected.changed.iter().map(|f| f.uid).collect::<Vec<_>>(),
            [gone + 1, gone + 2]
        );
        let known = Since {
            known: Some(UidSet::from_uids([1, gone + 1])),
            ..since
        };
        let selected = fake.select("INBOX", Some(known)).await.unwrap();
        assert_eq!(selected.vanished, UidSet::from_uids([1]));
    }

    #[tokio::test]
    async fn an_empty_uid_set_answers_empty_without_reaching_the_server() {
        let fake = inbox_with(2);
        let none = UidSet::new();
        fake.fail_next(ImapError::Network("reset".into()));
        assert!(fake.flags("INBOX", &none, None).await.unwrap().is_empty());
        assert!(fake.headers("INBOX", &none).await.unwrap().is_empty());
        fake.store("INBOX", &none, true, &flags(&["\\Seen"]))
            .await
            .unwrap();
        assert_eq!(fake.move_to("INBOX", &none, "Archive").await, Ok(None));
        assert_eq!(fake.copy_to("INBOX", &none, "Archive").await, Ok(None));
        fake.expunge("INBOX", &none).await.unwrap();
        assert!(fake.calls().is_empty());
        assert!(fake.list().await.is_err());
    }

    #[tokio::test]
    async fn what_the_client_refuses_itself_never_reaches_the_server() {
        let fake = inbox_with(1);
        assert!(matches!(
            fake.search("INBOX", "FROM ann\r\nA1 DELETE INBOX").await,
            Err(ImapError::Protocol(_))
        ));
        assert!(matches!(
            fake.store("INBOX", &UidSet::from_uid(1), true, &flags(&["bad flag"]))
                .await,
            Err(ImapError::Protocol(_))
        ));
        assert!(matches!(
            fake.body("INBOX", 1, "1]").await,
            Err(ImapError::Protocol(_))
        ));
        assert!(matches!(
            fake.select("Envoyés", None).await,
            Err(ImapError::Protocol(_))
        ));
        assert!(fake.calls().is_empty());
        assert_eq!(
            fake.search("INBOX", "SUBJECT \"line\r\nbreak\"").await,
            Ok(Vec::new())
        );
    }

    #[tokio::test]
    async fn a_failure_aimed_at_a_method_waits_for_that_method() {
        let fake = inbox_with(1);
        fake.fail_on("select", ImapError::Protocol("QRESYNC garbled".into()));
        assert!(fake.list().await.is_ok());
        let since = Since {
            uidvalidity: 1,
            modseq: 1,
            known: None,
        };
        assert_eq!(
            fake.select("INBOX", Some(since)).await.err(),
            Some(ImapError::Protocol("QRESYNC garbled".into()))
        );
        assert!(fake.select("INBOX", None).await.is_ok());
    }

    #[tokio::test]
    async fn the_log_tells_a_qresync_select_from_a_plain_one() {
        let fake = inbox_with(1);
        let first = fake.select("INBOX", None).await.unwrap();
        let since = Since {
            uidvalidity: first.uidvalidity,
            modseq: 3,
            known: None,
        };
        fake.select("INBOX", Some(since.clone())).await.unwrap();
        fake.with(|s| s.capabilities.qresync = false);
        fake.select("INBOX", Some(since)).await.unwrap();
        let qresync = format!("select INBOX qresync {} 3", first.uidvalidity);
        assert_eq!(
            fake.calls(),
            ["select INBOX", qresync.as_str(), "select INBOX"]
        );
    }

    #[tokio::test]
    async fn the_first_append_into_an_unmade_mailbox_answers_a_uidvalidity_one_ahead() {
        let fake = FakeImap::new();
        fake.with(|s| s.mailbox_mut("Drafts").unmade = true);
        let selected = fake.select("Drafts", None).await.unwrap().uidvalidity;
        let raw = raw_message("d", "draft", MARCH, None);
        let first = fake.append("Drafts", &[], &raw).await.unwrap().unwrap();
        assert_eq!(first.uidvalidity, selected + 1);
        assert_eq!(
            fake.select("Drafts", None).await.unwrap().uidvalidity,
            selected
        );
        let second = fake.append("Drafts", &[], &raw).await.unwrap().unwrap();
        assert_eq!(second.uidvalidity, selected);
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_past_its_budget_wakes_at_once_as_changed() {
        let fake = FakeImap::new();
        fake.overflow_next_idle();
        assert_eq!(
            fake.idle("INBOX", Duration::from_secs(600)).await,
            Ok(Woke::Changed)
        );
        assert_eq!(fake.calls_to("idle"), 1);
    }

    #[tokio::test]
    async fn idle_on_a_missing_mailbox_is_no_mailbox() {
        let fake = FakeImap::new();
        assert_eq!(
            fake.idle("Gone", Duration::from_secs(1)).await,
            Err(ImapError::NoMailbox("Gone".into()))
        );
    }
}
