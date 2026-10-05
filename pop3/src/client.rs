//! The session behind [`Pop3Api`]. A client holds at most one connection,
//! from `connect` to `quit`; each call waits for the one before it. A
//! failure that leaves the connection unreadable drops it, and the next
//! check connects again.

use std::future::Future;
use std::time::Duration;

use base64::Engine;
use mailrs_discover::{Security, Server};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::{MOST_MESSAGE_BYTES, Pop3Error, wire};

/// The longest status line read. RFC 1939 allows 512 octets.
const LINE_LIMIT: u64 = 4096;
/// How long a command waits for its status line.
const COMMAND_LIMIT: Duration = Duration::from_secs(30);
/// How long a multiline answer may go silent between two lines.
const STALL_LIMIT: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct Login {
    pub user: String,
    pub password: String,
}

impl Login {
    pub fn new(user: impl Into<String>, password: impl Into<String>) -> Login {
        Login { user: user.into(), password: password.into() }
    }
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login").field("user", &self.user).finish_non_exhaustive()
    }
}

/// What the server offers, from `CAPA`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub uidl: bool,
    pub stls: bool,
    pub sasl_plain: bool,
    pub top: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub count: u32,
    pub octets: u64,
}

/// A message number, valid for one session, and the name that holds
/// across sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uidl {
    pub id: u32,
    pub uidl: String,
}

/// A `UIDL` answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UidlListing {
    /// Each message's number and name, in the server's order.
    pub messages: Vec<Uidl>,
    /// Lines that did not read as a number and a name, each logged. The
    /// messages they stood for are missing from `messages`.
    pub unreadable: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListItem {
    pub id: u32,
    pub octets: u64,
}

/// A POP3 server, one session at a time.
pub trait Pop3Api: Send + Sync + 'static {
    /// Opens a session: greeting, `CAPA`, `STLS` where the connection is
    /// plain, then `AUTH PLAIN` or `USER` and `PASS`. Refuses a server
    /// that cannot answer `UIDL` with `Unsupported("UIDL")`.
    fn connect(&self) -> impl Future<Output = Result<Capabilities, Pop3Error>> + Send;
    fn stat(&self) -> impl Future<Output = Result<Stat, Pop3Error>> + Send;
    fn uidl(&self) -> impl Future<Output = Result<UidlListing, Pop3Error>> + Send;
    fn list(&self) -> impl Future<Output = Result<Vec<ListItem>, Pop3Error>> + Send;
    /// The whole message, its dots undone, up to [`MOST_MESSAGE_BYTES`].
    /// `octets` is the size `LIST` gave, which sizes the buffer; a longer
    /// answer still reads, up to the cap.
    fn retr(&self, id: u32, octets: u64) -> impl Future<Output = Result<Vec<u8>, Pop3Error>> + Send;
    fn top(&self, id: u32, lines: u32) -> impl Future<Output = Result<Vec<u8>, Pop3Error>> + Send;
    /// Marks message `id` for deletion at a clean `QUIT`.
    fn dele(&self, id: u32) -> impl Future<Output = Result<(), Pop3Error>> + Send;
    /// Ends the session; the server deletes what `DELE` marked.
    fn quit(&self) -> impl Future<Output = Result<(), Pop3Error>> + Send;
}

/// A connection the session reads and writes.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
pub type Stream = Box<dyn Io>;

/// How a session reaches its server.
pub trait Connect: Send + Sync + 'static {
    /// The connection, and whether it is still plain and needs `STLS`
    /// before the sign-in.
    fn open(&self) -> impl Future<Output = Result<(Stream, bool), Pop3Error>> + Send;
    /// TLS over a plain connection that just answered `STLS`.
    fn upgrade(&self, plain: Stream) -> impl Future<Output = Result<Stream, Pop3Error>> + Send;
}

/// TCP to the server, with TLS from the first byte or after `STLS`.
pub struct Pop3Tls {
    host: String,
    port: u16,
    security: Security,
}

impl Pop3Tls {
    pub fn new(server: &Server) -> Pop3Tls {
        Pop3Tls { host: server.host.clone(), port: server.port, security: server.security }
    }
}

impl Connect for Pop3Tls {
    async fn open(&self) -> Result<(Stream, bool), Pop3Error> {
        let tcp = within(COMMAND_LIMIT, async {
            TcpStream::connect((self.host.as_str(), self.port)).await.map_err(network)
        })
        .await?;
        match self.security {
            Security::Tls => Ok((Box::new(crate::tls::handshake(&self.host, tcp).await?), false)),
            Security::StartTls => Ok((Box::new(tcp), true)),
        }
    }

    async fn upgrade(&self, plain: Stream) -> Result<Stream, Pop3Error> {
        Ok(Box::new(crate::tls::handshake(&self.host, plain).await?))
    }
}

fn network(err: std::io::Error) -> Pop3Error {
    Pop3Error::Network(err.to_string())
}

fn closed() -> Pop3Error {
    Pop3Error::Network("the server closed the connection".into())
}

async fn within<T>(
    limit: Duration,
    run: impl Future<Output = Result<T, Pop3Error>>,
) -> Result<T, Pop3Error> {
    tokio::time::timeout(limit, run)
        .await
        .map_err(|_| Pop3Error::Network("the server took too long".into()))?
}

/// One connection, from its greeting on.
struct Session {
    reader: BufReader<Stream>,
}

impl Session {
    fn new(stream: Stream) -> Session {
        Session { reader: BufReader::new(stream) }
    }

    async fn send(&mut self, command: &str) -> Result<(), Pop3Error> {
        let stream = self.reader.get_mut();
        stream.write_all(command.as_bytes()).await.map_err(network)?;
        stream.flush().await.map_err(network)
    }

    /// One status line without its line end.
    async fn line(&mut self) -> Result<Vec<u8>, Pop3Error> {
        let mut line = Vec::new();
        let read = (&mut self.reader)
            .take(LINE_LIMIT)
            .read_until(b'\n', &mut line)
            .await
            .map_err(network)?;
        if read == 0 {
            return Err(closed());
        }
        if !line.ends_with(b"\n") {
            return Err(Pop3Error::Protocol("a status line longer than POP3 allows".into()));
        }
        Ok(trimmed(&line).to_vec())
    }

    async fn status(&mut self) -> Result<String, Pop3Error> {
        let line = within(COMMAND_LIMIT, self.line()).await?;
        wire::status(&line)
    }

    async fn command(&mut self, command: &str) -> Result<String, Pop3Error> {
        self.send(command).await?;
        self.status().await
    }

    /// A multiline answer's body, its dots undone, its lines ending CRLF,
    /// up to `cap` bytes. `expected` bytes are set aside at the start, so
    /// an answer of the size the server announced takes one allocation,
    /// where growing by doubling could hold twice the message at the end.
    async fn multiline(&mut self, cap: u64, expected: u64) -> Result<Vec<u8>, Pop3Error> {
        let room = usize::try_from(expected.min(cap)).unwrap_or(0);
        let mut body = Vec::with_capacity(room);
        let mut line = Vec::new();
        loop {
            line.clear();
            // Room for the line end and the terminator past the cap.
            let room = cap.saturating_sub(body.len() as u64) + 3;
            let read = within(STALL_LIMIT, async {
                (&mut self.reader).take(room).read_until(b'\n', &mut line).await.map_err(network)
            })
            .await?;
            if read == 0 {
                return Err(closed());
            }
            if !line.ends_with(b"\n") {
                return Err(Pop3Error::TooLarge);
            }
            let text = trimmed(&line);
            if text == b"." {
                // A server that announced more than it sent leaves room to
                // give back.
                if body.capacity() - body.len() > body.len() / 8 {
                    body.shrink_to_fit();
                }
                return Ok(body);
            }
            body.extend_from_slice(wire::undot(text));
            body.extend_from_slice(b"\r\n");
            if body.len() as u64 > cap {
                return Err(Pop3Error::TooLarge);
            }
        }
    }

    /// A multiline answer read as text lines.
    async fn listing(&mut self, command: &str) -> Result<Vec<String>, Pop3Error> {
        self.command(command).await?;
        let body = self.multiline(MOST_MESSAGE_BYTES, 0).await?;
        Ok(String::from_utf8_lossy(&body).lines().map(str::to_string).collect())
    }

    /// `CAPA`, or `None` from a server that answers it with `-ERR`.
    async fn capa(&mut self) -> Result<Option<Capabilities>, Pop3Error> {
        match self.listing("CAPA\r\n").await {
            Ok(lines) => Ok(Some(wire::capabilities(&lines))),
            Err(Pop3Error::Refused(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// SASL PLAIN where the server offers it, else `USER` and `PASS`. Only
    /// ever called over TLS. Any refusal is the login's.
    async fn sign_in(&mut self, caps: &Capabilities, login: &Login) -> Result<(), Pop3Error> {
        // A line break in either would end the command and start another.
        if login.user.contains(['\r', '\n']) || login.password.contains(['\r', '\n']) {
            return Err(Pop3Error::Auth { text: "the user name or password holds a line break".into() });
        }
        let refused = |err: Pop3Error| match err {
            Pop3Error::Refused(text) => Pop3Error::Auth { text },
            other => other,
        };
        if caps.sasl_plain {
            self.send("AUTH PLAIN\r\n").await?;
            let go_on = within(COMMAND_LIMIT, self.line()).await?;
            if !go_on.starts_with(b"+") || go_on.starts_with(b"+OK") {
                return Err(wire::status(&go_on).err().map(refused).unwrap_or_else(|| {
                    Pop3Error::Protocol("the server skipped the PLAIN challenge".into())
                }));
            }
            let plain = format!("\0{}\0{}", login.user, login.password);
            let encoded = base64::engine::general_purpose::STANDARD.encode(plain);
            return self.command(&format!("{encoded}\r\n")).await.map(drop).map_err(refused);
        }
        self.command(&format!("USER {}\r\n", login.user)).await.map_err(refused)?;
        self.command(&format!("PASS {}\r\n", login.password)).await.map(drop).map_err(refused)
    }

    /// Whether `UIDL` works, for a server whose `CAPA` did not say.
    async fn answers_uidl(&mut self) -> Result<bool, Pop3Error> {
        match self.listing("UIDL\r\n").await {
            Ok(_) => Ok(true),
            Err(Pop3Error::Refused(_)) => Ok(false),
            Err(err) => Err(err),
        }
    }
}

fn trimmed(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// The answer, keeping the session for a refusal and dropping it for a
/// failure that leaves the connection unreadable.
fn kept<T>(held: &mut Option<Session>, answer: Result<T, Pop3Error>) -> Result<T, Pop3Error> {
    if matches!(&answer, Err(err) if !matches!(err, Pop3Error::Refused(_))) {
        *held = None;
    }
    answer
}

fn no_session() -> Pop3Error {
    Pop3Error::Protocol("no POP3 session is open".into())
}

pub struct Pop3Client<C = Pop3Tls> {
    connect: C,
    login: Login,
    session: tokio::sync::Mutex<Option<Session>>,
}

impl Pop3Client {
    pub fn new(server: &Server, login: Login) -> Pop3Client {
        Pop3Client::with_connect(Pop3Tls::new(server), login)
    }
}

impl<C: Connect> Pop3Client<C> {
    pub fn with_connect(connect: C, login: Login) -> Pop3Client<C> {
        Pop3Client { connect, login, session: tokio::sync::Mutex::new(None) }
    }

    async fn open_session(&self) -> Result<(Session, Capabilities), Pop3Error> {
        let (stream, plain) = self.connect.open().await?;
        let mut session = Session::new(stream);
        session.status().await?;
        let mut caps = session.capa().await?;
        if plain {
            if !caps.is_some_and(|c| c.stls) {
                return Err(Pop3Error::Unsupported("STLS"));
            }
            session.command("STLS\r\n").await?;
            let stream = self.connect.upgrade(session.reader.into_inner()).await?;
            session = Session::new(stream);
            // RFC 2449 section 6.1: what the server offers can change after STLS.
            caps = session.capa().await?;
        }
        let mut offered = caps.unwrap_or_default();
        session.sign_in(&offered, &self.login).await?;
        if !offered.uidl {
            offered.uidl = session.answers_uidl().await?;
        }
        if !offered.uidl {
            return Err(Pop3Error::Unsupported("UIDL"));
        }
        Ok((session, offered))
    }
}

impl<C: Connect> Pop3Api for Pop3Client<C> {
    async fn connect(&self) -> Result<Capabilities, Pop3Error> {
        let mut held = self.session.lock().await;
        *held = None;
        let (session, caps) = self.open_session().await?;
        *held = Some(session);
        Ok(caps)
    }

    async fn stat(&self) -> Result<Stat, Pop3Error> {
        let mut held = self.session.lock().await;
        let session = held.as_mut().ok_or_else(no_session)?;
        let answer = session.command("STAT\r\n").await.and_then(|text| {
            wire::stat_text(&text).ok_or(Pop3Error::Protocol(format!("STAT answered {text}")))
        });
        kept(&mut held, answer)
    }

    async fn uidl(&self) -> Result<UidlListing, Pop3Error> {
        let mut held = self.session.lock().await;
        let session = held.as_mut().ok_or_else(no_session)?;
        let answer = session.listing("UIDL\r\n").await.map(|lines| {
            let mut listing = UidlListing::default();
            for line in &lines {
                match wire::uidl_line(line) {
                    Some(uidl) => listing.messages.push(uidl),
                    None => {
                        tracing::warn!(line, "a UIDL line that does not read");
                        listing.unreadable += 1;
                    }
                }
            }
            listing
        });
        kept(&mut held, answer)
    }

    async fn list(&self) -> Result<Vec<ListItem>, Pop3Error> {
        let mut held = self.session.lock().await;
        let session = held.as_mut().ok_or_else(no_session)?;
        let answer = session
            .listing("LIST\r\n")
            .await
            .map(|lines| lines.iter().filter_map(|l| wire::list_line(l)).collect());
        kept(&mut held, answer)
    }

    async fn retr(&self, id: u32, octets: u64) -> Result<Vec<u8>, Pop3Error> {
        let mut held = self.session.lock().await;
        let session = held.as_mut().ok_or_else(no_session)?;
        let answer = match session.command(&format!("RETR {id}\r\n")).await {
            Ok(_) => session.multiline(MOST_MESSAGE_BYTES, octets).await,
            Err(err) => Err(err),
        };
        kept(&mut held, answer)
    }

    async fn top(&self, id: u32, lines: u32) -> Result<Vec<u8>, Pop3Error> {
        let mut held = self.session.lock().await;
        let session = held.as_mut().ok_or_else(no_session)?;
        let answer = match session.command(&format!("TOP {id} {lines}\r\n")).await {
            Ok(_) => session.multiline(MOST_MESSAGE_BYTES, 0).await,
            Err(err) => Err(err),
        };
        kept(&mut held, answer)
    }

    async fn dele(&self, id: u32) -> Result<(), Pop3Error> {
        let mut held = self.session.lock().await;
        let session = held.as_mut().ok_or_else(no_session)?;
        let answer = session.command(&format!("DELE {id}\r\n")).await.map(drop);
        kept(&mut held, answer)
    }

    async fn quit(&self) -> Result<(), Pop3Error> {
        let mut held = self.session.lock().await;
        let Some(mut session) = held.take() else { return Err(no_session()) };
        session.command("QUIT\r\n").await.map(drop)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, DuplexStream};

    use super::*;

    /// A connection to a scripted server over a duplex pipe. `upgrade`
    /// hands the plain stream back: these tests check the command
    /// sequence, and the Dovecot tests check TLS.
    struct Scripted {
        stream: Mutex<Option<DuplexStream>>,
        stls: bool,
    }

    impl Connect for Scripted {
        async fn open(&self) -> Result<(Stream, bool), Pop3Error> {
            let stream = self.stream.lock().unwrap().take().expect("one connection");
            Ok((Box::new(stream), self.stls))
        }

        async fn upgrade(&self, plain: Stream) -> Result<Stream, Pop3Error> {
            Ok(plain)
        }
    }

    /// A client over a pipe whose far end greets and then answers each
    /// expected command with its answer, in order. The task answers the
    /// commands it heard.
    fn scripted(
        stls: bool,
        script: Vec<(&'static str, &'static str)>,
    ) -> (Pop3Client<Scripted>, tokio::task::JoinHandle<Vec<String>>) {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(theirs);
            let mut lines = tokio::io::BufReader::new(read).lines();
            write.write_all(b"+OK POP3 ready\r\n").await.unwrap();
            let mut heard = Vec::new();
            for (expect, answer) in script {
                let Ok(Some(line)) = lines.next_line().await else { break };
                assert_eq!(line, expect);
                heard.push(line);
                write.write_all(answer.as_bytes()).await.unwrap();
            }
            heard
        });
        let connect = Scripted { stream: Mutex::new(Some(ours)), stls };
        (Pop3Client::with_connect(connect, Login::new("me", "pw")), server)
    }

    #[tokio::test]
    async fn a_session_signs_in_lists_retrieves_and_deletes() {
        let (client, server) = scripted(
            false,
            vec![
                ("CAPA", "+OK\r\nUIDL\r\nTOP\r\nSASL PLAIN\r\n.\r\n"),
                ("AUTH PLAIN", "+ \r\n"),
                // base64 of "\0me\0pw".
                ("AG1lAHB3", "+OK signed in\r\n"),
                ("STAT", "+OK 2 150\r\n"),
                ("UIDL", "+OK\r\n1 a1\r\n2 a2\r\n.\r\n"),
                ("LIST", "+OK\r\n1 120\r\n2 30\r\n.\r\n"),
                ("RETR 1", "+OK 120 octets\r\nSubject: test\r\n\r\n..dot line\r\n.\r\n"),
                ("TOP 2 0", "+OK\r\nSubject: two\r\n\r\n.\r\n"),
                ("DELE 1", "+OK marked\r\n"),
                ("QUIT", "+OK bye\r\n"),
            ],
        );
        let caps = client.connect().await.unwrap();
        assert!(caps.uidl && caps.sasl_plain && caps.top && !caps.stls);
        assert_eq!(client.stat().await.unwrap(), Stat { count: 2, octets: 150 });
        assert_eq!(
            client.uidl().await.unwrap().messages,
            [Uidl { id: 1, uidl: "a1".into() }, Uidl { id: 2, uidl: "a2".into() }]
        );
        assert_eq!(
            client.list().await.unwrap(),
            [ListItem { id: 1, octets: 120 }, ListItem { id: 2, octets: 30 }]
        );
        assert_eq!(client.retr(1, 31).await.unwrap(), b"Subject: test\r\n\r\n.dot line\r\n");
        assert_eq!(client.top(2, 0).await.unwrap(), b"Subject: two\r\n\r\n");
        client.dele(1).await.unwrap();
        client.quit().await.unwrap();
        assert_eq!(server.await.unwrap().len(), 10);
        assert!(matches!(client.stat().await, Err(Pop3Error::Protocol(_))), "no session after QUIT");
    }

    #[tokio::test]
    async fn stls_comes_before_the_password_and_capa_is_asked_again() {
        let (client, server) = scripted(
            true,
            vec![
                ("CAPA", "+OK\r\nSTLS\r\nUIDL\r\nUSER\r\n.\r\n"),
                ("STLS", "+OK begin TLS\r\n"),
                ("CAPA", "+OK\r\nUIDL\r\nUSER\r\n.\r\n"),
                ("USER me", "+OK\r\n"),
                ("PASS pw", "+OK\r\n"),
            ],
        );
        let caps = client.connect().await.unwrap();
        assert!(caps.uidl && !caps.sasl_plain);
        assert_eq!(server.await.unwrap(), ["CAPA", "STLS", "CAPA", "USER me", "PASS pw"]);
    }

    #[tokio::test]
    async fn a_server_without_stls_never_sees_the_password() {
        let (client, server) = scripted(true, vec![("CAPA", "+OK\r\nUIDL\r\nUSER\r\n.\r\n")]);
        assert_eq!(client.connect().await, Err(Pop3Error::Unsupported("STLS")));
        assert_eq!(server.await.unwrap(), ["CAPA"]);
    }

    #[tokio::test]
    async fn a_server_without_uidl_is_refused() {
        let (client, _server) = scripted(
            false,
            vec![
                ("CAPA", "+OK\r\nUSER\r\n.\r\n"),
                ("USER me", "+OK\r\n"),
                ("PASS pw", "+OK\r\n"),
                ("UIDL", "-ERR unknown command\r\n"),
            ],
        );
        assert_eq!(client.connect().await, Err(Pop3Error::Unsupported("UIDL")));
    }

    #[tokio::test]
    async fn a_server_that_lists_no_capa_but_answers_uidl_is_used() {
        let (client, _server) = scripted(
            false,
            vec![
                ("CAPA", "-ERR unknown command\r\n"),
                ("USER me", "+OK\r\n"),
                ("PASS pw", "+OK\r\n"),
                ("UIDL", "+OK\r\n1 a1\r\n.\r\n"),
            ],
        );
        assert!(client.connect().await.unwrap().uidl);
    }

    #[tokio::test]
    async fn a_wrong_password_is_an_auth_refusal_in_the_servers_words() {
        let (client, _server) = scripted(
            false,
            vec![
                ("CAPA", "+OK\r\nUIDL\r\nUSER\r\n.\r\n"),
                ("USER me", "+OK\r\n"),
                ("PASS pw", "-ERR invalid login\r\n"),
            ],
        );
        assert_eq!(client.connect().await, Err(Pop3Error::Auth { text: "invalid login".into() }));
    }

    #[tokio::test]
    async fn a_refused_retr_keeps_the_session() {
        let (client, _server) = scripted(
            false,
            vec![
                ("CAPA", "+OK\r\nUIDL\r\nUSER\r\n.\r\n"),
                ("USER me", "+OK\r\n"),
                ("PASS pw", "+OK\r\n"),
                ("RETR 3", "-ERR no such message\r\n"),
                ("DELE 2", "+OK\r\n"),
            ],
        );
        client.connect().await.unwrap();
        assert_eq!(client.retr(3, 0).await, Err(Pop3Error::Refused("no such message".into())));
        client.dele(2).await.unwrap();
    }

    /// A buffer grown by doubling can hold twice the message at its last
    /// step. LIST says how large the message is, so one allocation holds it.
    #[tokio::test]
    async fn a_retr_answer_fills_one_buffer_of_the_size_list_gave() {
        let body = format!("{}\r\n", "x".repeat(98)).repeat(6_000);
        let answer: &'static str = Box::leak(format!("+OK\r\n{body}.\r\n").into_boxed_str());
        let (client, _server) = scripted(
            false,
            vec![
                ("CAPA", "+OK\r\nUIDL\r\nUSER\r\n.\r\n"),
                ("USER me", "+OK\r\n"),
                ("PASS pw", "+OK\r\n"),
                ("RETR 1", answer),
            ],
        );
        client.connect().await.unwrap();
        let raw = client.retr(1, body.len() as u64).await.unwrap();
        assert_eq!(raw.len(), body.len());
        assert_eq!(raw.capacity(), body.len(), "no room past the message");
    }

    #[tokio::test]
    async fn a_uidl_line_that_does_not_read_is_counted() {
        let (client, _server) = scripted(
            false,
            vec![
                ("CAPA", "+OK\r\nUIDL\r\nUSER\r\n.\r\n"),
                ("USER me", "+OK\r\n"),
                ("PASS pw", "+OK\r\n"),
                ("UIDL", "+OK\r\n1 a1\r\n2\r\nx a3\r\n4 a4\r\n.\r\n"),
            ],
        );
        client.connect().await.unwrap();
        assert_eq!(
            client.uidl().await.unwrap(),
            UidlListing {
                messages: vec![Uidl { id: 1, uidl: "a1".into() }, Uidl { id: 4, uidl: "a4".into() }],
                unreadable: 2,
            }
        );
    }

    #[tokio::test]
    async fn an_answer_over_the_cap_is_too_large_and_ends_the_session() {
        let (ours, mut theirs) = tokio::io::duplex(1024);
        theirs.write_all(b"line one\r\nline two\r\n.\r\n").await.unwrap();
        let mut session = Session::new(Box::new(ours));
        assert_eq!(session.multiline(12, 0).await, Err(Pop3Error::TooLarge));
    }

    #[tokio::test]
    async fn a_password_with_a_line_break_is_never_sent() {
        let (ours, theirs) = tokio::io::duplex(1024);
        let mut session = Session::new(Box::new(ours));
        let caps = Capabilities::default();
        let login = Login::new("me", "pw\r\nDELE 1");
        assert!(matches!(session.sign_in(&caps, &login).await, Err(Pop3Error::Auth { .. })));
        drop(session);
        let mut heard = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut { theirs }, &mut heard).await.unwrap();
        assert!(heard.is_empty(), "nothing went out");
    }

    #[tokio::test]
    async fn a_call_before_connect_has_no_session() {
        let (client, _server) = scripted(false, vec![]);
        assert!(matches!(client.retr(1, 0).await, Err(Pop3Error::Protocol(_))));
    }

    #[test]
    fn a_login_leaves_its_password_out_of_debug() {
        let shown = format!("{:?}", Login::new("me", "hunter2"));
        assert!(!shown.contains("hunter2"), "{shown}");
    }

    fn is_send<T: Send>(_: &T) {}

    #[allow(dead_code)]
    fn every_future_is_send(client: &Pop3Client) {
        is_send(&client.connect());
        is_send(&client.uidl());
        is_send(&client.retr(1, 0));
        is_send(&client.quit());
    }
}
