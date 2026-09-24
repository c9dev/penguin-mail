//! The IMAP client for one account. It keeps at most three connections:
//! two for sync and actions, and one that waits in IDLE. Servers cap the
//! connections one user may hold, Yahoo lower than most, so when a second
//! worker connection is refused for that reason the client gives the slot
//! up for good and works on one. A connection that fails is dropped, and
//! the next call opens a new one; the failed command is not sent again.

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use mailrs_discover::Server;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::connection::Conn;
use crate::guard::PAST_BUDGET;
use crate::{
    AppendUid, BodyStructure, Capabilities, CopyUid, Fetched, FlagsOf, ImapError, Listed, Login,
    Selected, Since, UidSet, Woke,
};

/// The longest a connection stays in IDLE before the client ends it and
/// starts again. RFC 2177 asks for less than the server's 30-minute
/// inactivity timer.
pub const IDLE_LIMIT: Duration = Duration::from_secs(25 * 60);

/// Worker connections, besides the one kept for IDLE.
const WORKERS: usize = 2;

/// How long a connection may sit unused before a NOOP checks it is still
/// there. Home routers drop quiet TCP connections without telling
/// either end, and a command sent into one waits for a TCP timeout.
const STALE_AFTER: Duration = Duration::from_secs(5 * 60);

const NOOP_LIMIT: Duration = Duration::from_secs(15);

/// Connecting, TLS and login together.
const OPEN_LIMIT: Duration = Duration::from_secs(60);

/// One command and its answer. A large APPEND or a message of tens of
/// megabytes on a slow line takes minutes.
const COMMAND_LIMIT: Duration = Duration::from_secs(10 * 60);

/// Ending IDLE and selecting its mailbox, on top of the IDLE itself.
const IDLE_SLACK: Duration = Duration::from_secs(60);

/// Opens a connection to the server. The app dials TLS through
/// [`TlsDial`]; a test dials an in-memory pipe.
pub trait Dial: Send + Sync + 'static {
    type Stream: AsyncRead + AsyncWrite + Unpin + fmt::Debug + Send + 'static;

    /// A new connection, and whether its greeting was read already.
    fn dial(&self) -> impl Future<Output = Result<(Self::Stream, bool), ImapError>> + Send;
}

/// Dials the account's IMAP server over TLS or STARTTLS.
#[derive(Clone)]
pub struct TlsDial {
    server: Server,
}

impl TlsDial {
    pub(crate) fn new(server: Server) -> Self {
        TlsDial { server }
    }
}

impl Dial for TlsDial {
    type Stream = crate::tls::Tls;

    async fn dial(&self) -> Result<(crate::tls::Tls, bool), ImapError> {
        crate::tls::dial(&self.server).await
    }
}

/// The IMAP client for one account. Cloning it shares its connections.
pub struct ImapClient<D: Dial = TlsDial> {
    inner: Arc<Inner<D>>,
}

impl<D: Dial> Clone for ImapClient<D> {
    fn clone(&self) -> Self {
        ImapClient {
            inner: self.inner.clone(),
        }
    }
}

struct Inner<D: Dial> {
    dial: D,
    login: Login,
    /// One permit per worker connection.
    permits: Arc<Semaphore>,
    /// Worker permits not given up to a connection limit.
    limit: AtomicUsize,
    /// Worker connections open or opening, free or in use.
    open: Arc<AtomicUsize>,
    free: Mutex<Vec<Worker<D::Stream>>>,
    idle: tokio::sync::Mutex<Option<Conn<D::Stream>>>,
    /// What the server offered at the last login.
    capabilities: Mutex<Option<Capabilities>>,
}

/// Counts one worker connection, open or opening, for as long as it
/// lives. The count drops with the token wherever the connection goes:
/// dropped after a failure, or with a call its caller gave up on.
struct Counted(Arc<AtomicUsize>);

impl Counted {
    fn new(open: &Arc<AtomicUsize>) -> Self {
        open.fetch_add(1, Ordering::SeqCst);
        Counted(open.clone())
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A worker connection and its place in the count.
struct Worker<S: crate::connection::Stream> {
    conn: Conn<S>,
    _counted: Counted,
}

/// A worker connection on loan to one call, with its permit.
struct Lease<S: crate::connection::Stream> {
    worker: Worker<S>,
    _permit: OwnedSemaphorePermit,
}

/// Runs one call on a worker connection and puts the connection back, or
/// drops it when the failure leaves it unusable.
macro_rules! on_worker {
    ($self:ident, $conn:ident => $call:expr) => {{
        let mut lease = $self.checkout().await?;
        let $conn = &mut lease.worker.conn;
        let result = within(COMMAND_LIMIT, $call).await;
        $self.checkin(lease, &result);
        result
    }};
}

impl ImapClient<TlsDial> {
    /// A client for `server` that signs in with `login`. Opens nothing
    /// until the first call.
    pub fn new(server: Server, login: Login) -> Self {
        ImapClient::with_dial(TlsDial::new(server), login)
    }
}

impl<D: Dial> ImapClient<D> {
    pub fn with_dial(dial: D, login: Login) -> Self {
        ImapClient {
            inner: Arc::new(Inner {
                dial,
                login,
                permits: Arc::new(Semaphore::new(WORKERS)),
                limit: AtomicUsize::new(WORKERS),
                open: Arc::new(AtomicUsize::new(0)),
                free: Mutex::new(Vec::new()),
                idle: tokio::sync::Mutex::new(None),
                capabilities: Mutex::new(None),
            }),
        }
    }

    /// What the server offers, as it said at the last login. Signs in
    /// when no connection has yet.
    pub async fn capabilities(&self) -> Result<Capabilities, ImapError> {
        let known = *self
            .inner
            .capabilities
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(known) = known {
            return Ok(known);
        }
        let lease = self.checkout().await?;
        let capabilities = lease.worker.conn.capabilities;
        self.checkin(lease, &Ok::<(), ImapError>(()));
        Ok(capabilities)
    }

    pub async fn list(&self) -> Result<Vec<Listed>, ImapError> {
        on_worker!(self, conn => conn.list())
    }

    pub async fn select(
        &self,
        mailbox: &str,
        since: Option<&Since>,
    ) -> Result<Selected, ImapError> {
        on_worker!(self, conn => conn.select(mailbox, since))
    }

    pub async fn flags(
        &self,
        mailbox: &str,
        uids: &UidSet,
        changed_since: Option<u64>,
    ) -> Result<Vec<FlagsOf>, ImapError> {
        on_worker!(self, conn => conn.flags(mailbox, uids, changed_since))
    }

    pub async fn search(&self, mailbox: &str, keys: &str) -> Result<Vec<u32>, ImapError> {
        on_worker!(self, conn => conn.search(mailbox, keys))
    }

    pub async fn headers(&self, mailbox: &str, uids: &UidSet) -> Result<Vec<Fetched>, ImapError> {
        on_worker!(self, conn => conn.headers(mailbox, uids))
    }

    pub async fn body(
        &self,
        mailbox: &str,
        uid: u32,
        section: &str,
    ) -> Result<Option<Vec<u8>>, ImapError> {
        on_worker!(self, conn => conn.body(mailbox, uid, section))
    }

    pub async fn structure(
        &self,
        mailbox: &str,
        uid: u32,
    ) -> Result<Option<BodyStructure>, ImapError> {
        on_worker!(self, conn => conn.structure(mailbox, uid))
    }

    pub async fn store(
        &self,
        mailbox: &str,
        uids: &UidSet,
        add: bool,
        flags: &[String],
    ) -> Result<(), ImapError> {
        on_worker!(self, conn => conn.store(mailbox, uids, add, flags))
    }

    pub async fn move_to(
        &self,
        mailbox: &str,
        uids: &UidSet,
        to: &str,
    ) -> Result<Option<CopyUid>, ImapError> {
        on_worker!(self, conn => conn.move_to(mailbox, uids, to))
    }

    pub async fn copy_to(
        &self,
        mailbox: &str,
        uids: &UidSet,
        to: &str,
    ) -> Result<Option<CopyUid>, ImapError> {
        on_worker!(self, conn => conn.copy_to(mailbox, uids, to))
    }

    pub async fn expunge(&self, mailbox: &str, uids: &UidSet) -> Result<(), ImapError> {
        on_worker!(self, conn => conn.expunge(mailbox, uids))
    }

    pub async fn append(
        &self,
        mailbox: &str,
        flags: &[String],
        raw: &[u8],
    ) -> Result<Option<AppendUid>, ImapError> {
        on_worker!(self, conn => conn.append(mailbox, flags, raw))
    }

    pub async fn create(&self, mailbox: &str) -> Result<(), ImapError> {
        on_worker!(self, conn => conn.create(mailbox))
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<(), ImapError> {
        on_worker!(self, conn => conn.rename(from, to))
    }

    pub async fn delete(&self, mailbox: &str) -> Result<(), ImapError> {
        on_worker!(self, conn => conn.delete(mailbox))
    }

    /// Waits in IDLE on `mailbox`, on the connection kept for it, until
    /// the server reports a change or `limit` passes; `limit` is cut to
    /// [`IDLE_LIMIT`]. One IDLE runs at a time; a second call waits for
    /// the first to end. On a server without IDLE the answer is
    /// [`ImapError::Unsupported`], and no connection stays open for it.
    ///
    /// A server that sends more during one IDLE than the guard lets
    /// through loses the connection, and the answer is
    /// [`Woke::Changed`]: whatever it reported is lost with the
    /// connection, and a sync of the mailbox finds it. Any other failure
    /// is an error, so a SELECT the guard refuses never reads as news.
    pub async fn idle(&self, mailbox: &str, limit: Duration) -> Result<Woke, ImapError> {
        let limit = limit.min(IDLE_LIMIT);
        let known = *self
            .inner
            .capabilities
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if known.is_some_and(|caps| !caps.idle) {
            return Err(ImapError::Unsupported("IDLE"));
        }
        let mut slot = self.inner.idle.lock().await;
        let kept = match slot.take() {
            Some(conn) => self.checked(conn).await,
            None => None,
        };
        let mut conn = match kept {
            Some(conn) => conn,
            None => within(OPEN_LIMIT, self.open()).await?,
        };
        // A connection kept for an IDLE that never runs would hold one of
        // the few connections a provider allows, so it goes.
        if !conn.capabilities.idle {
            return Err(ImapError::Unsupported("IDLE"));
        }
        match within(COMMAND_LIMIT, conn.ensure_selected(mailbox)).await {
            Ok(()) => {}
            Err(err) if err.drops_connection() => return Err(err),
            Err(err) => {
                *slot = Some(conn);
                return Err(err);
            }
        }
        match within(limit + IDLE_SLACK, conn.idle(mailbox, limit)).await {
            Ok((conn, woke)) => {
                *slot = Some(conn);
                Ok(woke)
            }
            Err(ImapError::Protocol(why)) if why == PAST_BUDGET => Ok(Woke::Changed),
            Err(err) => Err(err),
        }
    }

    /// Lends a worker connection: a free one, checked with NOOP when it
    /// sat unused for a while, or a new one.
    async fn checkout(&self) -> Result<Lease<D::Stream>, ImapError> {
        loop {
            let permit = self
                .inner
                .permits
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| ImapError::Network("the client is shutting down".into()))?;
            let pooled = self
                .inner
                .free
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop();
            if let Some(Worker { conn, _counted }) = pooled
                && let Some(conn) = self.checked(conn).await
            {
                return Ok(Lease {
                    worker: Worker { conn, _counted },
                    _permit: permit,
                });
            }
            let counted = Counted::new(&self.inner.open);
            match within(OPEN_LIMIT, self.open()).await {
                Ok(conn) => {
                    return Ok(Lease {
                        worker: Worker {
                            conn,
                            _counted: counted,
                        },
                        _permit: permit,
                    });
                }
                Err(err) => {
                    drop(counted);
                    let others = self.inner.open.load(Ordering::SeqCst);
                    let refused = matches!(err, ImapError::TooManyConnections { .. });
                    if refused && others > 0 && self.give_up_a_slot() {
                        // The connection that exists serves this call
                        // once it is free.
                        permit.forget();
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }

    /// Takes one worker permit away for good, unless it is the last.
    fn give_up_a_slot(&self) -> bool {
        self.inner
            .limit
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n > 1).then(|| n - 1)
            })
            .is_ok()
    }

    /// Puts the connection back for the next call, or drops it when the
    /// failure leaves it unusable.
    fn checkin<T>(&self, lease: Lease<D::Stream>, result: &Result<T, ImapError>) {
        let Lease { worker, _permit } = lease;
        match result {
            Err(err) if err.drops_connection() => drop(worker),
            _ => self
                .inner
                .free
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(worker),
        }
    }

    /// `conn`, or `None` when it sat unused long enough to need a NOOP and
    /// the NOOP failed.
    async fn checked(&self, mut conn: Conn<D::Stream>) -> Option<Conn<D::Stream>> {
        if conn.unused_for() < STALE_AFTER {
            return Some(conn);
        }
        within(NOOP_LIMIT, conn.noop()).await.ok().map(|()| conn)
    }

    async fn open(&self) -> Result<Conn<D::Stream>, ImapError> {
        let (stream, greeted) = self.inner.dial.dial().await?;
        let conn = Conn::login(stream, greeted, &self.inner.login).await?;
        *self
            .inner
            .capabilities
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(conn.capabilities);
        Ok(conn)
    }
}

/// `work`, or a network error once `limit` passes without an answer.
async fn within<T>(
    limit: Duration,
    work: impl Future<Output = Result<T, ImapError>>,
) -> Result<T, ImapError> {
    tokio::time::timeout(limit, work).await.unwrap_or_else(|_| {
        Err(ImapError::Network(format!(
            "no answer in {} seconds",
            limit.as_secs()
        )))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    use tokio::io::DuplexStream;

    use super::{Dial, ImapClient};
    use crate::guard::{IDLE_BYTES, MAX_LINE};
    use crate::testing::{pipe, selected, server};
    use crate::{ImapError, Login, UidSet, Woke};

    type Answer = Box<dyn FnMut(&str) -> Vec<String> + Send>;

    /// Hands out one scripted server per dial, in order, and counts dials.
    #[derive(Default)]
    struct Scripts {
        next: Mutex<VecDeque<(&'static str, Answer)>>,
        dials: AtomicUsize,
    }

    impl Dial for Arc<Scripts> {
        type Stream = DuplexStream;

        async fn dial(&self) -> Result<(DuplexStream, bool), ImapError> {
            self.dials.fetch_add(1, Ordering::SeqCst);
            let next = self
                .next
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front();
            let (greeting, answer) =
                next.ok_or_else(|| ImapError::Network("no more servers".into()))?;
            Ok((pipe(greeting, answer), false))
        }
    }

    const GREETING: &str = "* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN] ready";
    const ALL: &str = "IDLE QRESYNC CONDSTORE MOVE UIDPLUS SPECIAL-USE";

    fn log() -> Arc<Mutex<Vec<String>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn seen(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        log.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// A server that lists one mailbox, pausing first when `slow`.
    fn lister(seen: Arc<Mutex<Vec<String>>>, slow: bool) -> Answer {
        Box::new(server(ALL, seen, move |command| match command {
            c if c.starts_with("LIST") => {
                let mut lines = Vec::new();
                if slow {
                    lines.push("<pause>".to_string());
                }
                lines.push("* LIST () \"/\" INBOX".into());
                lines.push("{tag} OK".into());
                lines
            }
            c if c.starts_with("SELECT") => selected(),
            "IDLE" => vec!["* 5 EXISTS".into()],
            _ => vec!["{tag} OK".into()],
        }))
    }

    fn client(servers: Vec<(&'static str, Answer)>) -> (ImapClient<Arc<Scripts>>, Arc<Scripts>) {
        let scripts = Arc::new(Scripts {
            next: Mutex::new(servers.into()),
            dials: AtomicUsize::new(0),
        });
        (
            ImapClient::with_dial(scripts.clone(), Login::new("ann", "pw")),
            scripts,
        )
    }

    fn dials(scripts: &Scripts) -> usize {
        scripts.dials.load(Ordering::SeqCst)
    }

    #[tokio::test]
    async fn calls_in_turn_share_one_connection() {
        let (client, scripts) = client(vec![(GREETING, lister(log(), false))]);
        client.list().await.unwrap();
        client.list().await.unwrap();
        assert_eq!(scripts.dials.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_connection_that_dropped_is_replaced_on_the_next_call() {
        let dying: Answer = Box::new(server(ALL, log(), |_| vec!["<close>".into()]));
        let (client, scripts) = client(vec![(GREETING, dying), (GREETING, lister(log(), false))]);
        assert!(matches!(client.list().await, Err(ImapError::Network(_))));
        assert_eq!(client.list().await.unwrap().len(), 1);
        assert_eq!(scripts.dials.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_refused_second_connection_leaves_the_client_on_one() {
        let refused: Answer = Box::new(|_: &str| vec![]);
        let (client, scripts) = client(vec![
            (GREETING, lister(log(), true)),
            ("* BYE Too many connections for this user", refused),
        ]);
        let (first, second) = tokio::join!(client.list(), client.list());
        assert!(first.is_ok() && second.is_ok(), "{first:?} {second:?}");
        assert_eq!(scripts.dials.load(Ordering::SeqCst), 2);
        let (third, fourth) = tokio::join!(client.list(), client.list());
        assert!(third.is_ok() && fourth.is_ok());
        assert_eq!(
            scripts.dials.load(Ordering::SeqCst),
            2,
            "no third dial after the limit"
        );
    }

    #[tokio::test]
    async fn a_refusal_with_no_other_connection_is_an_error() {
        let refused: Answer = Box::new(|_: &str| vec![]);
        let (client, _) = client(vec![("* BYE Too many connections for this user", refused)]);
        assert!(matches!(
            client.list().await,
            Err(ImapError::TooManyConnections { .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_connection_unused_for_minutes_is_checked_with_noop() {
        let seen = log();
        let (client, _) = client(vec![(GREETING, lister(seen.clone(), false))]);
        client.list().await.unwrap();
        tokio::time::advance(Duration::from_secs(6 * 60)).await;
        client.list().await.unwrap();
        let commands = seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let last_three: Vec<&str> = commands
            .iter()
            .rev()
            .take(3)
            .rev()
            .map(String::as_str)
            .collect();
        assert_eq!(
            last_three,
            [
                "LIST \"\" \"*\" RETURN (SPECIAL-USE)",
                "NOOP",
                "LIST \"\" \"*\" RETURN (SPECIAL-USE)"
            ]
        );
    }

    #[tokio::test]
    async fn idle_runs_on_its_own_connection_and_keeps_it() {
        let (client, scripts) = client(vec![
            (GREETING, lister(log(), false)),
            (GREETING, lister(log(), false)),
        ]);
        assert_eq!(
            client.idle("INBOX", Duration::from_secs(60)).await.unwrap(),
            Woke::Changed
        );
        client.list().await.unwrap();
        assert_eq!(
            client.idle("INBOX", Duration::from_secs(60)).await.unwrap(),
            Woke::Changed
        );
        assert_eq!(scripts.dials.load(Ordering::SeqCst), 2);
    }

    /// A server without IDLE gets no connection kept for it: each one
    /// holds a slot of the few a provider allows, Yahoo fewer than most.
    #[tokio::test]
    async fn idle_on_a_server_without_it_is_unsupported_and_keeps_no_connection() {
        let bare: Answer = Box::new(server("MOVE", log(), |_| vec!["{tag} OK".into()]));
        let (client, scripts) = client(vec![(GREETING, bare)]);
        let err = client.idle("INBOX", Duration::from_secs(60)).await.err();
        assert_eq!(err, Some(ImapError::Unsupported("IDLE")));
        assert!(
            client.inner.idle.lock().await.is_none(),
            "a connection stayed"
        );
        // The capabilities from that sign-in answer the next call.
        let err = client.idle("INBOX", Duration::from_secs(60)).await.err();
        assert_eq!(err, Some(ImapError::Unsupported("IDLE")));
        assert_eq!(dials(&scripts), 1);
    }

    /// A value the client refuses to send never reached the server, so it
    /// costs no connection: a label that is not an IMAP atom must not cost
    /// a sign-in per STORE.
    #[tokio::test]
    async fn a_value_the_client_refuses_keeps_the_connection() {
        let (client, scripts) = client(vec![(GREETING, lister(log(), false))]);
        client.list().await.unwrap();
        let one = UidSet::from_uids([1]);
        let bad_flag = ["bad flag".to_string()];
        assert!(matches!(
            client.store("INBOX", &one, true, &bad_flag).await,
            Err(ImapError::Invalid(_))
        ));
        assert!(matches!(
            client.select("Envoyés", None).await,
            Err(ImapError::Invalid(_))
        ));
        assert!(matches!(
            client.search("INBOX", "ALL\r\nA9 LOGOUT").await,
            Err(ImapError::Invalid(_))
        ));
        assert!(matches!(
            client.body("INBOX", 1, "1]").await,
            Err(ImapError::Invalid(_))
        ));
        client.list().await.unwrap();
        assert_eq!(dials(&scripts), 1);
    }

    #[tokio::test]
    async fn capabilities_come_from_the_login() {
        let (client, scripts) = client(vec![(GREETING, lister(log(), false))]);
        let caps = client.capabilities().await.unwrap();
        assert!(caps.qresync && caps.idle);
        client.capabilities().await.unwrap();
        assert_eq!(scripts.dials.load(Ordering::SeqCst), 1);
    }

    /// A server that answers LIST with a line nested past the guard's cap.
    fn nesting_lister(seen: Arc<Mutex<Vec<String>>>) -> Answer {
        Box::new(server(ALL, seen, |command| match command {
            c if c.starts_with("LIST") => {
                vec![format!("* 1 FETCH {}", "(".repeat(200)), "{tag} OK".into()]
            }
            _ => vec!["{tag} OK".into()],
        }))
    }

    #[tokio::test]
    async fn an_answer_the_guard_refuses_fails_that_call_once_and_the_next_call_reconnects() {
        let first = log();
        let (client, scripts) = client(vec![
            (GREETING, nesting_lister(first.clone())),
            (GREETING, lister(log(), false)),
        ]);
        let err = client.list().await.err();
        assert!(matches!(err, Some(ImapError::Protocol(_))), "{err:?}");
        let lists = seen(&first)
            .iter()
            .filter(|c| c.starts_with("LIST"))
            .count();
        assert_eq!(lists, 1, "the refused command went out once");
        assert_eq!(dials(&scripts), 1, "nothing reconnects until asked");
        assert_eq!(client.list().await.unwrap().len(), 1);
        assert_eq!(dials(&scripts), 2);
    }

    #[tokio::test]
    async fn a_server_that_keeps_sending_refused_answers_costs_one_connection_a_call() {
        let (client, scripts) = client(vec![
            (GREETING, nesting_lister(log())),
            (GREETING, nesting_lister(log())),
        ]);
        assert!(matches!(client.list().await, Err(ImapError::Protocol(_))));
        assert!(matches!(client.list().await, Err(ImapError::Protocol(_))));
        assert_eq!(dials(&scripts), 2);
    }

    /// A server whose IDLE sends keepalives until they pass IDLE's
    /// budget, then serves as `lister` does.
    fn flooding_idler() -> Answer {
        let line = format!("* OK {}", "x".repeat(MAX_LINE - 16));
        let count = usize::try_from(IDLE_BYTES).unwrap() / line.len() + 2;
        let mut flood = Some(vec![line; count]);
        Box::new(server(ALL, log(), move |command| match command {
            c if c.starts_with("SELECT") => selected(),
            "IDLE" => flood.take().unwrap_or_default(),
            _ => vec!["{tag} OK".into()],
        }))
    }

    #[tokio::test]
    async fn an_idle_past_its_budget_drops_the_connection_and_asks_for_a_sync() {
        let (client, scripts) = client(vec![
            (GREETING, flooding_idler()),
            (GREETING, lister(log(), false)),
        ]);
        let woke = client.idle("INBOX", Duration::from_secs(60)).await;
        assert_eq!(woke, Ok(Woke::Changed));
        assert_eq!(
            client.idle("INBOX", Duration::from_secs(60)).await,
            Ok(Woke::Changed)
        );
        assert_eq!(dials(&scripts), 2, "the flooded connection was dropped");
    }

    #[tokio::test]
    async fn an_idle_the_guard_refuses_for_another_reason_is_an_error_and_the_next_reconnects() {
        let hostile: Answer = Box::new(server(ALL, log(), |command| match command {
            c if c.starts_with("SELECT") => selected(),
            "IDLE" => vec!["* 1 FETCH (UID 1 BODY[] {8388608}".into(), "abc".into()],
            _ => vec!["{tag} OK".into()],
        }));
        let (client, scripts) = client(vec![(GREETING, hostile), (GREETING, lister(log(), false))]);
        let err = client.idle("INBOX", Duration::from_secs(60)).await.err();
        assert!(matches!(err, Some(ImapError::Protocol(_))), "{err:?}");
        assert_eq!(
            client.idle("INBOX", Duration::from_secs(60)).await,
            Ok(Woke::Changed)
        );
        assert_eq!(dials(&scripts), 2);
    }

    #[tokio::test]
    async fn a_select_the_guard_refuses_before_idle_is_an_error_not_news() {
        let hostile: Answer = Box::new(server(ALL, log(), |command| match command {
            c if c.starts_with("SELECT") => {
                vec![format!("* 1 FETCH {}", "(".repeat(200)), "{tag} OK".into()]
            }
            _ => vec!["{tag} OK".into()],
        }));
        let (client, _) = client(vec![(GREETING, hostile)]);
        let err = client.idle("INBOX", Duration::from_secs(60)).await.err();
        assert!(matches!(err, Some(ImapError::Protocol(_))), "{err:?}");
    }

    #[tokio::test]
    async fn an_empty_uid_set_never_reaches_the_server_through_the_client() {
        let commands = log();
        let (client, _) = client(vec![(GREETING, lister(commands.clone(), false))]);
        let none = UidSet::new();
        let flags = ["\\Seen".to_string()];
        assert!(client.flags("INBOX", &none, None).await.unwrap().is_empty());
        assert!(
            client
                .flags("INBOX", &none, Some(9))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(client.headers("INBOX", &none).await.unwrap().is_empty());
        client.store("INBOX", &none, true, &flags).await.unwrap();
        assert_eq!(client.move_to("INBOX", &none, "Archive").await, Ok(None));
        assert_eq!(client.copy_to("INBOX", &none, "Archive").await, Ok(None));
        client.expunge("INBOX", &none).await.unwrap();
        let sent = seen(&commands);
        assert!(
            !sent
                .iter()
                .any(|c| c.starts_with("UID") || c.starts_with("SELECT")),
            "{sent:?}"
        );
    }

    #[tokio::test]
    async fn the_client_never_holds_more_than_three_connections() {
        let (client, scripts) = client(vec![
            (GREETING, lister(log(), true)),
            (GREETING, lister(log(), true)),
            (GREETING, lister(log(), true)),
            (GREETING, lister(log(), true)),
        ]);
        let (a, b, c, d, e, idle) = tokio::join!(
            client.list(),
            client.list(),
            client.list(),
            client.list(),
            client.list(),
            client.idle("INBOX", Duration::from_secs(60)),
        );
        assert!(a.is_ok() && b.is_ok() && c.is_ok() && d.is_ok() && e.is_ok());
        assert_eq!(idle, Ok(Woke::Changed));
        assert_eq!(dials(&scripts), 3);
    }

    #[tokio::test]
    async fn clones_share_their_connections() {
        let (client, scripts) = client(vec![(GREETING, lister(log(), false))]);
        let other = client.clone();
        client.list().await.unwrap();
        other.list().await.unwrap();
        assert_eq!(dials(&scripts), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_call_given_up_midway_leaves_no_connection_counted() {
        let refused: Answer = Box::new(|_: &str| vec![]);
        let (client, scripts) = client(vec![
            (GREETING, lister(log(), true)),
            ("* BYE Too many connections for this user", refused),
        ]);
        let given_up = tokio::time::timeout(Duration::from_millis(50), client.list()).await;
        assert!(given_up.is_err(), "the list should still be waiting");
        // The only connection went with the call, so a refusal now has no
        // other connection to fall back on.
        let err = client.list().await.err();
        assert!(
            matches!(err, Some(ImapError::TooManyConnections { .. })),
            "{err:?}"
        );
        assert_eq!(dials(&scripts), 2);
    }

    #[test]
    fn every_future_the_client_returns_is_send() {
        fn sendable<T: Send>(_: T) {}
        let (client, _) = client(Vec::new());
        let uids = UidSet::from_uid(1);
        sendable(client.capabilities());
        sendable(client.list());
        sendable(client.select("INBOX", None));
        sendable(client.flags("INBOX", &uids, None));
        sendable(client.search("INBOX", "ALL"));
        sendable(client.headers("INBOX", &uids));
        sendable(client.body("INBOX", 1, ""));
        sendable(client.structure("INBOX", 1));
        sendable(client.store("INBOX", &uids, true, &[]));
        sendable(client.move_to("INBOX", &uids, "Archive"));
        sendable(client.copy_to("INBOX", &uids, "Archive"));
        sendable(client.expunge("INBOX", &uids));
        sendable(client.append("INBOX", &[], b"raw"));
        sendable(client.create("INBOX"));
        sendable(client.rename("INBOX", "Old"));
        sendable(client.delete("INBOX"));
        sendable(client.idle("INBOX", Duration::from_secs(1)));
    }
}
