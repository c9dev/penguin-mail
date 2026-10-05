//! An in-memory POP3 server behind `Pop3Api`, for the downloader's tests.
//! It keeps sessions as a real server does: `connect` numbers the
//! messages for the session and locks the maildrop, `dele` marks one, and
//! only a clean `quit` deletes what was marked. A test seeds mail, scripts
//! failures, and reads back what the downloader asked for.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use mailrs_pop3::{Capabilities, ListItem, Pop3Api, Pop3Error, Stat, Uidl};
use tokio::sync::Notify;

/// A small message, the `n`th of a test.
pub fn pop3_mail(n: u32) -> Vec<u8> {
    format!(
        "From: Ana <ana@example.org>\r\nTo: me@example.org\r\nSubject: Hello {n}\r\n\
         Message-ID: <m{n}@example.org>\r\nDate: Mon, 4 Jan 2021 09:00:00 +0000\r\n\r\nBody {n}\r\n"
    )
    .into_bytes()
}

pub struct FakePop3 {
    inner: Mutex<Inner>,
}

struct Inner {
    /// The maildrop: each message's UIDL and bytes, oldest first.
    messages: Vec<(String, Vec<u8>)>,
    /// Sizes `LIST` reports in place of a message's real length.
    claimed: BTreeMap<String, u64>,
    /// This session's numbering: message `n` is `numbered[n - 1]`.
    numbered: Vec<String>,
    marked: BTreeSet<u32>,
    in_session: bool,
    capabilities: Capabilities,
    refuse_sign_in: bool,
    failing: BTreeSet<String>,
    /// Every `quit` fails as a dropped connection would.
    drop_before_quit: bool,
    /// After this many answered `retr`s, the next one fails as a dropped
    /// connection would.
    drop_after_retrs: Option<usize>,
    retr_calls: Vec<u32>,
    deleted: Vec<u32>,
    connects: usize,
    in_flight: usize,
    most_in_flight: usize,
    /// Taken by the first `retr`, which waits until it is notified.
    hold: Option<Arc<Notify>>,
}

impl Default for FakePop3 {
    fn default() -> Self {
        FakePop3 {
            inner: Mutex::new(Inner {
                messages: Vec::new(),
                claimed: BTreeMap::new(),
                numbered: Vec::new(),
                marked: BTreeSet::new(),
                in_session: false,
                capabilities: Capabilities { uidl: true, stls: false, sasl_plain: true, top: true },
                refuse_sign_in: false,
                failing: BTreeSet::new(),
                drop_before_quit: false,
                drop_after_retrs: None,
                retr_calls: Vec::new(),
                deleted: Vec::new(),
                connects: 0,
                in_flight: 0,
                most_in_flight: 0,
                hold: None,
            }),
        }
    }
}

/// Lets a `retr` held by [`FakePop3::holding_retr`] go on.
pub struct Release(Arc<Notify>);

impl Release {
    pub fn release(&self) {
        self.0.notify_one();
    }
}

impl FakePop3 {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn with_message(self, uidl: &str, raw: &[u8]) -> Self {
        self.add(uidl, raw);
        self
    }

    /// Puts a message on the server now, as mail arriving between checks.
    pub fn add(&self, uidl: &str, raw: &[u8]) {
        self.lock().messages.push((uidl.to_string(), raw.to_vec()));
    }

    /// Takes a message off the server, as another client's DELE and QUIT
    /// would.
    pub fn take(&self, uidl: &str) {
        self.lock().messages.retain(|(held, _)| held != uidl);
    }

    /// `retr` of this message answers `-ERR`.
    pub fn failing_retr(self, uidl: &str) -> Self {
        self.lock().failing.insert(uidl.to_string());
        self
    }

    /// `LIST` reports `octets` for this message.
    pub fn claiming_size(self, uidl: &str, octets: u64) -> Self {
        self.lock().claimed.insert(uidl.to_string(), octets);
        self
    }

    pub fn refusing_sign_in(self) -> Self {
        self.lock().refuse_sign_in = true;
        self
    }

    pub fn without_uidl(self) -> Self {
        self.lock().capabilities.uidl = false;
        self
    }

    /// Every `quit` fails as a dropped connection would, for the fake's
    /// whole life, so nothing `dele` marked is deleted.
    pub fn dropping_before_quit(self) -> Self {
        self.lock().drop_before_quit = true;
        self
    }

    /// Once `answered` `retr`s have gone through, the next one fails as a
    /// dropped connection would, and the session ends.
    pub fn dropping_after_retrs(self, answered: usize) -> Self {
        self.lock().drop_after_retrs = Some(answered);
        self
    }

    /// The first `retr` waits until the [`Release`] says go.
    pub fn holding_retr(self) -> (Self, Release) {
        let notify = Arc::new(Notify::new());
        self.lock().hold = Some(Arc::clone(&notify));
        (self, Release(notify))
    }

    /// Every message number `retr` was asked for, in order.
    pub fn retr_calls(&self) -> Vec<u32> {
        self.lock().retr_calls.clone()
    }

    /// Every message number `dele` was asked for, in order.
    pub fn deleted(&self) -> Vec<u32> {
        self.lock().deleted.clone()
    }

    pub fn connects(&self) -> usize {
        self.lock().connects
    }

    /// Sessions open now.
    pub fn in_flight(&self) -> usize {
        self.lock().in_flight
    }

    /// The most sessions ever open at once.
    pub fn most_in_flight(&self) -> usize {
        self.lock().most_in_flight
    }

    /// The UIDLs the server holds now.
    pub fn held(&self) -> Vec<String> {
        self.lock().messages.iter().map(|(uidl, _)| uidl.clone()).collect()
    }
}

impl Inner {
    fn session(&self) -> Result<(), Pop3Error> {
        match self.in_session {
            true => Ok(()),
            false => Err(Pop3Error::Protocol("no POP3 session is open".into())),
        }
    }

    /// The UIDL message `id` names this session, unless `dele` marked it.
    fn uidl_of(&self, id: u32) -> Result<String, Pop3Error> {
        self.session()?;
        let index = usize::try_from(id).ok().and_then(|n| n.checked_sub(1));
        match index.and_then(|i| self.numbered.get(i)) {
            Some(uidl) if !self.marked.contains(&id) => Ok(uidl.clone()),
            _ => Err(Pop3Error::Refused(format!("no such message {id}"))),
        }
    }

    fn raw_of(&self, uidl: &str) -> Vec<u8> {
        self.messages.iter().find(|(u, _)| u == uidl).map(|(_, raw)| raw.clone()).unwrap_or_default()
    }

    fn live(&self) -> impl Iterator<Item = (u32, &String)> {
        self.numbered
            .iter()
            .enumerate()
            .map(|(i, uidl)| (i as u32 + 1, uidl))
            .filter(|(id, _)| !self.marked.contains(id))
    }

    fn end_session(&mut self) {
        self.in_session = false;
        self.in_flight = self.in_flight.saturating_sub(1);
        self.numbered.clear();
        self.marked.clear();
    }
}

impl Pop3Api for FakePop3 {
    async fn connect(&self) -> Result<Capabilities, Pop3Error> {
        let mut inner = self.lock();
        inner.connects += 1;
        if inner.refuse_sign_in {
            return Err(Pop3Error::Auth { text: "invalid login".into() });
        }
        if !inner.capabilities.uidl {
            return Err(Pop3Error::Unsupported("UIDL"));
        }
        if inner.in_session {
            return Err(Pop3Error::InUse("[IN-USE] the maildrop is locked".into()));
        }
        inner.in_session = true;
        inner.in_flight += 1;
        inner.most_in_flight = inner.most_in_flight.max(inner.in_flight);
        inner.numbered = inner.messages.iter().map(|(uidl, _)| uidl.clone()).collect();
        Ok(inner.capabilities)
    }

    async fn stat(&self) -> Result<Stat, Pop3Error> {
        let inner = self.lock();
        inner.session()?;
        let live: Vec<u64> = inner.live().map(|(_, uidl)| inner.raw_of(uidl).len() as u64).collect();
        Ok(Stat { count: live.len() as u32, octets: live.iter().sum() })
    }

    async fn uidl(&self) -> Result<Vec<Uidl>, Pop3Error> {
        let inner = self.lock();
        inner.session()?;
        Ok(inner.live().map(|(id, uidl)| Uidl { id, uidl: uidl.clone() }).collect())
    }

    async fn list(&self) -> Result<Vec<ListItem>, Pop3Error> {
        let inner = self.lock();
        inner.session()?;
        Ok(inner
            .live()
            .map(|(id, uidl)| ListItem {
                id,
                octets: inner.claimed.get(uidl).copied().unwrap_or(inner.raw_of(uidl).len() as u64),
            })
            .collect())
    }

    async fn retr(&self, id: u32) -> Result<Vec<u8>, Pop3Error> {
        let hold = {
            let mut inner = self.lock();
            inner.session()?;
            inner.retr_calls.push(id);
            inner.hold.take()
        };
        if let Some(hold) = hold {
            hold.notified().await;
        }
        let mut inner = self.lock();
        if inner.drop_after_retrs.is_some_and(|n| inner.retr_calls.len() > n) {
            inner.end_session();
            return Err(Pop3Error::Network("the connection dropped".into()));
        }
        let uidl = inner.uidl_of(id)?;
        if inner.failing.contains(&uidl) {
            return Err(Pop3Error::Refused(format!("message {id} cannot be read")));
        }
        Ok(inner.raw_of(&uidl))
    }

    async fn top(&self, id: u32, lines: u32) -> Result<Vec<u8>, Pop3Error> {
        let inner = self.lock();
        let raw = inner.raw_of(&inner.uidl_of(id)?);
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").map_or(raw.len(), |p| p + 4);
        let (head, body) = raw.split_at(split);
        let mut out = head.to_vec();
        for line in body.split_inclusive(|b| *b == b'\n').take(lines as usize) {
            out.extend_from_slice(line);
        }
        Ok(out)
    }

    async fn dele(&self, id: u32) -> Result<(), Pop3Error> {
        let mut inner = self.lock();
        inner.uidl_of(id)?;
        inner.marked.insert(id);
        inner.deleted.push(id);
        Ok(())
    }

    async fn quit(&self) -> Result<(), Pop3Error> {
        let mut inner = self.lock();
        inner.session()?;
        if inner.drop_before_quit {
            inner.end_session();
            return Err(Pop3Error::Network("the connection dropped".into()));
        }
        let gone: BTreeSet<String> = inner.marked.iter().filter_map(|id| inner.numbered.get(*id as usize - 1).cloned()).collect();
        inner.messages.retain(|(uidl, _)| !gone.contains(uidl));
        inner.end_session();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn seeded_messages_answer_within_a_session() {
        let fake = FakePop3::default().with_message("u1", b"Subject: hi\r\n\r\nbody\r\n");
        assert!(matches!(fake.uidl().await, Err(Pop3Error::Protocol(_))), "no session yet");
        fake.connect().await.unwrap();
        assert_eq!(fake.uidl().await.unwrap(), [Uidl { id: 1, uidl: "u1".into() }]);
        assert_eq!(fake.list().await.unwrap(), [ListItem { id: 1, octets: 21 }]);
        assert_eq!(fake.retr(1).await.unwrap(), b"Subject: hi\r\n\r\nbody\r\n");
        assert_eq!(fake.retr_calls(), [1]);
        fake.quit().await.unwrap();
        assert_eq!((fake.connects(), fake.in_flight()), (1, 0));
    }

    #[tokio::test]
    async fn dele_takes_effect_only_at_a_clean_quit() {
        let fake = FakePop3::default().with_message("u1", &pop3_mail(1)).with_message("u2", &pop3_mail(2));
        fake.connect().await.unwrap();
        fake.dele(1).await.unwrap();
        assert_eq!(fake.uidl().await.unwrap(), [Uidl { id: 2, uidl: "u2".into() }], "numbers hold for the session");
        fake.quit().await.unwrap();
        assert_eq!(fake.held(), ["u2"]);

        let dropping = FakePop3::default().with_message("u1", &pop3_mail(1)).dropping_before_quit();
        dropping.connect().await.unwrap();
        dropping.dele(1).await.unwrap();
        assert!(matches!(dropping.quit().await, Err(Pop3Error::Network(_))));
        assert_eq!(dropping.held(), ["u1"], "a dropped session deletes nothing");
        assert_eq!(dropping.deleted(), [1]);
    }

    #[tokio::test]
    async fn a_second_session_meets_the_lock_and_scripted_failures_answer() {
        let fake = FakePop3::default().with_message("u1", &pop3_mail(1)).failing_retr("u1").claiming_size("u1", 1 << 40);
        fake.connect().await.unwrap();
        assert!(matches!(fake.connect().await, Err(Pop3Error::InUse(_))));
        assert_eq!(fake.most_in_flight(), 1);
        assert!(matches!(fake.retr(1).await, Err(Pop3Error::Refused(_))));
        assert_eq!(fake.list().await.unwrap()[0].octets, 1 << 40);
        assert!(matches!(FakePop3::default().refusing_sign_in().connect().await, Err(Pop3Error::Auth { .. })));
        assert_eq!(FakePop3::default().without_uidl().connect().await, Err(Pop3Error::Unsupported("UIDL")));
    }

    #[tokio::test]
    async fn a_held_retr_waits_for_its_release() {
        let (fake, release) = FakePop3::default().with_message("u1", &pop3_mail(1)).holding_retr();
        let fake = Arc::new(fake);
        fake.connect().await.unwrap();
        let waiting = tokio::spawn({
            let fake = Arc::clone(&fake);
            async move { fake.retr(1).await }
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        release.release();
        assert_eq!(waiting.await.unwrap().unwrap(), pop3_mail(1));
    }
}
