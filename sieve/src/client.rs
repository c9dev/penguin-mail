//! A ManageSieve client (RFC 5804) behind [`ManageSieveApi`]. Each call
//! opens its own connection: rules change a few times a day, and a
//! connection kept open would hold a socket and a TLS session for
//! nothing. STARTTLS is required, the login is SASL PLAIN over TLS, and
//! a script is read up to [`crate::MOST_SCRIPT_BYTES`].

use std::time::Duration;

use base64::Engine;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

use crate::MOST_SCRIPT_BYTES;
use crate::protocol::{self, Kind, literal, quote};
use crate::script::Extensions;

#[derive(Debug, Clone, thiserror::Error)]
pub enum SieveError {
    #[error("network error: {0}")]
    Network(String),
    #[error("TLS failed: {0}")]
    Tls(String),
    #[error("the server refused the login: {0}")]
    Auth(String),
    /// A NO, with the server's own words.
    #[error("{0}")]
    Refused(String),
    #[error("no such script")]
    NotFound,
    #[error("the server said something Penguin Mail cannot read: {0}")]
    Protocol(String),
    #[error("the script is larger than Penguin Mail reads")]
    TooLarge,
    #[error("the server offers no STARTTLS, so Penguin Mail will not send the password")]
    NoStartTls,
}

impl SieveError {
    pub fn is_transient(&self) -> bool {
        matches!(self, SieveError::Network(_))
    }
}

#[derive(Clone)]
pub struct Login {
    pub user: String,
    pub password: String,
}

impl Login {
    pub fn new(user: impl Into<String>, password: impl Into<String>) -> Login {
        Login {
            user: user.into(),
            password: password.into(),
        }
    }
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login")
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub implementation: Option<String>,
    pub sieve: Extensions,
    pub sasl: Vec<String>,
    pub starttls: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub name: String,
    pub active: bool,
}

pub trait ManageSieveApi: Send + Sync + 'static {
    fn capabilities(&self) -> impl Future<Output = Result<Capabilities, SieveError>> + Send;
    fn scripts(&self) -> impl Future<Output = Result<Vec<Listed>, SieveError>> + Send;
    fn get(&self, name: &str) -> impl Future<Output = Result<String, SieveError>> + Send;
    fn put(&self, name: &str, script: &str) -> impl Future<Output = Result<(), SieveError>> + Send;
    fn activate(&self, name: &str) -> impl Future<Output = Result<(), SieveError>> + Send;
}

/// The longest line read; a capability line is short.
const LINE_LIMIT: u64 = 8192;
const REQUEST_LIMIT: Duration = Duration::from_secs(20);
/// How long LOGOUT waits for its answer. The work already went through,
/// so a server slow to say goodbye only gets its connection dropped.
const LOGOUT_LIMIT: Duration = Duration::from_secs(2);

fn network(err: std::io::Error) -> SieveError {
    SieveError::Network(err.to_string())
}

/// One logged-in connection over `S`.
pub struct Session<S> {
    reader: BufReader<S>,
    caps: Capabilities,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Session<S> {
    fn new(stream: S) -> Session<S> {
        Session {
            reader: BufReader::new(stream),
            caps: Capabilities::default(),
        }
    }

    async fn line(&mut self) -> Result<String, SieveError> {
        let mut line = String::new();
        let read = (&mut self.reader)
            .take(LINE_LIMIT)
            .read_line(&mut line)
            .await
            .map_err(network)?;
        if read == 0 {
            return Err(SieveError::Network(
                "the server closed the connection".into(),
            ));
        }
        Ok(line)
    }

    async fn send(&mut self, text: &str) -> Result<(), SieveError> {
        let stream = self.reader.get_mut();
        stream.write_all(text.as_bytes()).await.map_err(network)?;
        stream.flush().await.map_err(network)
    }

    /// Data lines until the status line; a literal's bytes come back as
    /// one data line of their own.
    async fn answer(&mut self) -> Result<(Vec<String>, protocol::Status), SieveError> {
        let mut data = Vec::new();
        loop {
            let line = self.line().await?;
            if let Some(mut status) = protocol::status(&line) {
                if let Some(len) = status.literal {
                    status.text = self.literal(len).await?;
                }
                return Ok((data, status));
            }
            if let Some(len) = literal(&line) {
                let text = self.literal(len).await?;
                data.push(text);
                continue;
            }
            data.push(line);
        }
    }

    /// A literal's bytes, and the CRLF that ends the line they finish.
    async fn literal(&mut self, len: usize) -> Result<String, SieveError> {
        if len > MOST_SCRIPT_BYTES {
            return Err(SieveError::TooLarge);
        }
        let mut bytes = vec![0u8; len];
        self.reader.read_exact(&mut bytes).await.map_err(network)?;
        self.line().await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn read_capabilities(&mut self) -> Result<Capabilities, SieveError> {
        let (lines, status) = self.answer().await?;
        if status.kind != Kind::Ok {
            return Err(SieveError::Protocol(status.text));
        }
        let mut caps = Capabilities::default();
        for line in lines {
            let (quoted, _) = protocol::strings(&line);
            let Some(name) = quoted.first().map(|q| q.to_ascii_uppercase()) else {
                continue;
            };
            let value = quoted.get(1);
            match (name.as_str(), value) {
                ("IMPLEMENTATION", value) => caps.implementation = value.cloned(),
                ("SIEVE", Some(value)) => caps.sieve = Extensions::parse(value),
                ("SASL", Some(value)) => {
                    caps.sasl = value.split_whitespace().map(str::to_string).collect();
                }
                ("STARTTLS", _) => caps.starttls = true,
                _ => {}
            }
        }
        Ok(caps)
    }

    async fn authenticate(&mut self, login: &Login) -> Result<(), SieveError> {
        let plain = format!("\0{}\0{}", login.user, login.password);
        let encoded = base64::engine::general_purpose::STANDARD.encode(plain);
        self.send(&format!("AUTHENTICATE \"PLAIN\" {}\r\n", quote(&encoded)))
            .await?;
        let (_, status) = self.answer().await?;
        match status.kind {
            Kind::Ok => Ok(()),
            _ => Err(SieveError::Auth(status.text)),
        }
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    async fn simple(&mut self, command: String) -> Result<Vec<String>, SieveError> {
        self.send(&command).await?;
        let (data, status) = self.answer().await?;
        match status.kind {
            Kind::Ok => Ok(data),
            Kind::No if status.code.as_deref() == Some("NONEXISTENT") => Err(SieveError::NotFound),
            Kind::No => Err(SieveError::Refused(status.text)),
            Kind::Bye => Err(SieveError::Network(status.text)),
        }
    }

    pub async fn scripts(&mut self) -> Result<Vec<Listed>, SieveError> {
        let lines = self.simple("LISTSCRIPTS\r\n".into()).await?;
        Ok(lines
            .iter()
            .filter_map(|line| {
                let (quoted, atoms) = protocol::strings(line);
                Some(Listed {
                    name: quoted.into_iter().next()?,
                    active: atoms.iter().any(|a| a.eq_ignore_ascii_case("ACTIVE")),
                })
            })
            .collect())
    }

    pub async fn get(&mut self, name: &str) -> Result<String, SieveError> {
        let lines = self
            .simple(format!("GETSCRIPT {}\r\n", quote(name)))
            .await?;
        let script = lines.into_iter().next().unwrap_or_default();
        Ok(script.trim_end_matches("\r\n").to_string())
    }

    pub async fn put(&mut self, name: &str, script: &str) -> Result<(), SieveError> {
        self.simple(format!(
            "PUTSCRIPT {} {{{}+}}\r\n{script}\r\n",
            quote(name),
            script.len()
        ))
        .await
        .map(drop)
    }

    pub async fn activate(&mut self, name: &str) -> Result<(), SieveError> {
        self.simple(format!("SETACTIVE {}\r\n", quote(name)))
            .await
            .map(drop)
    }

    pub async fn logout(mut self) {
        let _ = tokio::time::timeout(LOGOUT_LIMIT, self.simple("LOGOUT\r\n".into())).await;
    }
}

pub struct ManageSieveClient {
    host: String,
    port: u16,
    login: Login,
}

impl ManageSieveClient {
    pub fn new(host: &str, port: u16, login: Login) -> ManageSieveClient {
        ManageSieveClient {
            host: host.to_string(),
            port,
            login,
        }
    }

    /// Connects, reads the greeting, asks for STARTTLS, and logs in over
    /// TLS. A server that offers no STARTTLS never sees the password.
    async fn open(&self) -> Result<Session<TlsStream<TcpStream>>, SieveError> {
        let run = async {
            let tcp = TcpStream::connect((self.host.as_str(), self.port))
                .await
                .map_err(network)?;
            let mut plain = Session::new(tcp);
            let greeting = plain.read_capabilities().await?;
            if !greeting.starttls {
                return Err(SieveError::NoStartTls);
            }
            plain.simple("STARTTLS\r\n".into()).await?;
            let tcp = plain.reader.into_inner();
            let tls = crate::tls::handshake(&self.host, tcp).await?;
            let mut session = Session::new(tls);
            session.caps = session.read_capabilities().await?;
            session.authenticate(&self.login).await?;
            Ok(session)
        };
        within_limit(run).await
    }
}

/// Cuts a request off after [`REQUEST_LIMIT`].
async fn within_limit<T>(
    run: impl Future<Output = Result<T, SieveError>>,
) -> Result<T, SieveError> {
    tokio::time::timeout(REQUEST_LIMIT, run)
        .await
        .map_err(|_| SieveError::Network("the server took too long".into()))?
}

impl ManageSieveApi for ManageSieveClient {
    async fn capabilities(&self) -> Result<Capabilities, SieveError> {
        let session = self.open().await?;
        let caps = session.caps.clone();
        session.logout().await;
        Ok(caps)
    }

    async fn scripts(&self) -> Result<Vec<Listed>, SieveError> {
        let mut session = self.open().await?;
        let out = within_limit(session.scripts()).await;
        session.logout().await;
        out
    }

    async fn get(&self, name: &str) -> Result<String, SieveError> {
        let mut session = self.open().await?;
        let out = within_limit(session.get(name)).await;
        session.logout().await;
        out
    }

    async fn put(&self, name: &str, script: &str) -> Result<(), SieveError> {
        let mut session = self.open().await?;
        let out = within_limit(session.put(name, script)).await;
        session.logout().await;
        out
    }

    async fn activate(&self, name: &str) -> Result<(), SieveError> {
        let mut session = self.open().await?;
        let out = within_limit(session.activate(name)).await;
        session.logout().await;
        out
    }
}

/// A session over any stream from the point TLS has started, for tests.
#[cfg(any(test, feature = "fake"))]
pub mod testing {
    use super::*;

    pub async fn session_over<S: AsyncRead + AsyncWrite + Unpin>(
        stream: S,
        login: &Login,
    ) -> Result<Session<S>, SieveError> {
        let mut session = Session::new(stream);
        session.caps = session.read_capabilities().await?;
        session.authenticate(login).await?;
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_send<T: Send>(_: &T) {}

    #[allow(dead_code)]
    async fn every_future_a_trait_method_returns_is_send(client: &ManageSieveClient) {
        is_send(&client.capabilities());
        is_send(&client.scripts());
        is_send(&client.get("x"));
        is_send(&client.put("x", "keep;"));
        is_send(&client.activate("x"));
    }

    #[tokio::test]
    async fn a_refusal_sent_as_a_literal_keeps_its_words() {
        let words = "line 1: unknown command 'fileintoo'.\r\n";
        let (ours, mut server) = tokio::io::duplex(1024);
        server
            .write_all(format!("NO {{{}}}\r\n{words}\r\nOK\r\n", words.len()).as_bytes())
            .await
            .unwrap();
        let mut session = Session::new(ours);
        let refused = session.simple("PUTSCRIPT \"x\" {1+}\r\nx\r\n".into()).await.unwrap_err();
        assert!(matches!(refused, SieveError::Refused(ref w) if w.contains("unknown command")), "{refused:?}");
        // The literal's closing CRLF is read too, so the next answer
        // starts on its own status line.
        assert!(session.simple("NOOP\r\n".into()).await.is_ok());
    }

    #[tokio::test]
    async fn a_logout_the_server_never_answers_ends_on_its_own() {
        let (ours, _silent) = tokio::io::duplex(1024);
        let ended = tokio::time::timeout(LOGOUT_LIMIT + Duration::from_secs(5), Session::new(ours).logout()).await;
        assert!(ended.is_ok(), "logout waited past its limit");
    }

    #[test]
    fn a_login_leaves_its_password_out_of_debug() {
        let shown = format!("{:?}", Login::new("me", "hunter2"));
        assert!(!shown.contains("hunter2"), "{shown}");
    }
}
