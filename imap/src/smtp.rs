//! SMTP submission for one account, on lettre: implicit TLS (usually
//! port 465) or STARTTLS (usually 587), then AUTH PLAIN or LOGIN. Each
//! send opens its own connection, since an account sends seldom.
//!
//! This crate dials the server and runs TLS itself, with the IMAP side's
//! rustls setup, and hands lettre the encrypted stream. lettre then speaks
//! only SMTP, so it brings no TLS code of its own, and no test needs a
//! socket. STARTTLS runs here too, before lettre sees the stream, and a
//! server that does not offer it gets no password.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use lettre::address::{Address, Envelope};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{AsyncSmtpConnection, AsyncTokioStream};
use lettre::transport::smtp::commands::{Data, Mail, Rcpt};
use lettre::transport::smtp::extension::{ClientId, Extension, MailBodyParameter, MailParameter};
use mailrs_discover::{Security, Server};
use tokio::io::{AsyncBufRead, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};

use crate::client::Dial;
use crate::refusal::clipped;
use crate::tls::{self, Tls};
use crate::{ImapError, Login};

/// Connecting, TLS, EHLO and AUTH together.
const OPEN_LIMIT: Duration = Duration::from_secs(60);

/// One message and QUIT. A message of tens of megabytes on a slow line
/// takes minutes.
const SEND_LIMIT: Duration = Duration::from_secs(10 * 60);

/// How much of the message lettre copies at a time to double its leading
/// dots. Handing it the whole message would copy all of it at once.
const CHUNK: usize = 64 << 10;

/// The name the client gives in EHLO. The machine's host name would tell
/// the server who sends; RFC 5321 section 4.1.4 allows an address literal
/// in its place.
const HELLO: ClientId = ClientId::Ipv4(Ipv4Addr::LOCALHOST);

/// AUTH PLAIN carries UTF-8, so it goes first; LOGIN is for servers
/// without it.
const MECHANISMS: [Mechanism; 2] = [Mechanism::Plain, Mechanism::Login];

/// The most lines one reply before TLS may have. An EHLO reply lists a
/// dozen extensions.
const MAX_REPLY_LINES: usize = 100;

/// Sends mail for one account. Cloning it shares nothing but settings.
#[derive(Clone, Debug)]
pub struct SmtpClient<D: Dial = SmtpTls> {
    dial: D,
    login: Login,
}

/// Dials the account's submission server over implicit TLS or STARTTLS.
#[derive(Clone, Debug)]
pub struct SmtpTls {
    server: Server,
}

impl SmtpTls {
    /// Refuses a host that is not a host name or an IP address at once,
    /// before anything connects.
    pub(crate) fn new(server: &Server) -> Result<SmtpTls, ImapError> {
        tls::server_name(&server.host)?;
        Ok(SmtpTls {
            server: server.clone(),
        })
    }
}

impl Dial for SmtpTls {
    type Stream = Tls;

    async fn dial(&self) -> Result<(Tls, bool), ImapError> {
        let host = self.server.host.as_str();
        let tcp = tls::connect(&self.server).await?;
        let (tcp, greeted) = match self.server.security {
            Security::Tls => (tcp, false),
            Security::StartTls => (starttls(tcp, host).await?, true),
        };
        Ok((tls::handshake(host, tcp).await?, greeted))
    }
}

impl SmtpClient<SmtpTls> {
    /// A client for `server` that signs in with `login`. Connects to
    /// nothing until the first call.
    pub fn new(server: &Server, login: &Login) -> Result<SmtpClient, ImapError> {
        Ok(SmtpClient::with_dial(SmtpTls::new(server)?, login.clone()))
    }
}

impl<D: Dial> SmtpClient<D>
where
    D::Stream: Sync,
{
    /// A client that reaches its server through `dial`, as a test does
    /// with an in-memory pipe.
    pub fn with_dial(dial: D, login: Login) -> Self {
        SmtpClient { dial, login }
    }

    /// Hands `raw` to the server for `to`, with `from` as the envelope
    /// sender. The server adds no Bcc line; the recipients come from `to`
    /// alone, so a Bcc address goes in `to` and not in `raw`.
    pub async fn submit(&self, from: &str, to: &[String], raw: &[u8]) -> Result<(), ImapError> {
        let envelope = envelope(from, to)?;
        let mut conn = within(OPEN_LIMIT, self.open()).await?;
        within(SEND_LIMIT, send(&mut conn, &envelope, raw, &self.login)).await
    }

    /// Connects, starts TLS, signs in and says goodbye.
    pub async fn check(&self) -> Result<(), ImapError> {
        within(OPEN_LIMIT, async {
            let mut conn = self.open().await?;
            let _ = conn.quit().await;
            Ok(())
        })
        .await
    }

    async fn open(&self) -> Result<AsyncSmtpConnection, ImapError> {
        let (stream, greeted) = self.dial.dial().await?;
        let wire = Wire {
            stand_in: if greeted { STAND_IN } else { b"" },
            stream,
        };
        let lettre = |err| smtp_error(&err, &self.login);
        let mut conn = AsyncSmtpConnection::connect_with_transport(Box::new(wire), &HELLO)
            .await
            .map_err(lettre)?;
        if conn.server_info().get_auth_mechanism(&MECHANISMS).is_none() {
            return Err(ImapError::Unsupported("AUTH PLAIN or AUTH LOGIN"));
        }
        let credentials = Credentials::new(self.login.user.clone(), self.login.password.clone());
        conn.auth(&MECHANISMS, &credentials).await.map_err(lettre)?;
        Ok(conn)
    }
}

/// MAIL, RCPT for each recipient, DATA, the message and QUIT. lettre's own
/// `send` copies the whole message before it writes; this hands it the
/// message a chunk at a time.
async fn send(
    conn: &mut AsyncSmtpConnection,
    envelope: &Envelope,
    raw: &[u8],
    login: &Login,
) -> Result<(), ImapError> {
    let info = conn.server_info();
    let mut options = Vec::new();
    // RFC 6531: an address outside ASCII needs SMTPUTF8.
    let addresses = envelope.from().into_iter().chain(envelope.to());
    if addresses
        .into_iter()
        .any(|a| !AsRef::<str>::as_ref(a).is_ascii())
    {
        if !info.supports_feature(Extension::SmtpUtfEight) {
            return Err(ImapError::Unsupported("SMTPUTF8"));
        }
        options.push(MailParameter::SmtpUtfEight);
    }
    // RFC 6152: a message with bytes above 127 needs 8BITMIME.
    if !raw.is_ascii() {
        if !info.supports_feature(Extension::EightBitMime) {
            return Err(ImapError::Unsupported("8BITMIME"));
        }
        options.push(MailParameter::Body(MailBodyParameter::EightBitMime));
    }
    let lettre = |err| smtp_error(&err, login);
    conn.command(Mail::new(envelope.from().cloned(), options))
        .await
        .map_err(lettre)?;
    for to in envelope.to() {
        conn.command(Rcpt::new(to.clone(), Vec::new()))
            .await
            .map_err(lettre)?;
    }
    conn.command(Data).await.map_err(lettre)?;
    // lettre ends the data with CRLF, a dot and CRLF, so a message that
    // ends with its own CRLF would gain an empty line.
    let body = raw.strip_suffix(b"\r\n").unwrap_or(raw);
    conn.message_iter(body.chunks(CHUNK))
        .await
        .map_err(lettre)?;
    // The server took the message; a failed goodbye changes nothing.
    let _ = conn.quit().await;
    Ok(())
}

async fn within<T>(
    limit: Duration,
    work: impl Future<Output = Result<T, ImapError>>,
) -> Result<T, ImapError> {
    tokio::time::timeout(limit, work).await.unwrap_or_else(|_| {
        Err(ImapError::Network(format!(
            "the SMTP server did not answer within {} seconds",
            limit.as_secs()
        )))
    })
}

fn envelope(from: &str, to: &[String]) -> Result<Envelope, ImapError> {
    let address = |text: &str| {
        text.trim()
            .parse::<Address>()
            .map_err(|_| ImapError::Refused(format!("{text} is not an email address")))
    };
    let recipients = to
        .iter()
        .map(|t| address(t))
        .collect::<Result<Vec<_>, _>>()?;
    Envelope::new(Some(address(from)?), recipients)
        .map_err(|_| ImapError::Refused("a message needs at least one recipient".into()))
}

/// Reads the greeting on a plain connection, says EHLO and asks for
/// STARTTLS. Hands the stream back ready for the handshake. A server that
/// does not offer STARTTLS ends the connection here: going on in the clear
/// would send the password where anyone on the path can read it.
pub(crate) async fn starttls<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    host: &str,
) -> Result<S, ImapError> {
    let mut reader = BufReader::new(stream);
    let greeting = reply(&mut reader).await?;
    if greeting.code != 220 {
        return Err(greeting.refused());
    }
    say(&mut reader, &format!("EHLO {HELLO}\r\n")).await?;
    let ehlo = reply(&mut reader).await?;
    if ehlo.code != 250 {
        return Err(ehlo.refused());
    }
    // The first line names the server; each line after it names one
    // extension and its parameters.
    let offered = ehlo.lines.iter().skip(1).any(|line| {
        line.split_ascii_whitespace()
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("STARTTLS"))
    });
    if !offered {
        return Err(ImapError::Tls {
            host: host.to_string(),
            detail: "the server does not offer STARTTLS".into(),
        });
    }
    say(&mut reader, "STARTTLS\r\n").await?;
    let go = reply(&mut reader).await?;
    if go.code != 220 {
        return Err(ImapError::Tls {
            host: host.to_string(),
            detail: format!("the server refused STARTTLS: {}", go.text()),
        });
    }
    // Bytes after the go-ahead and before the handshake travel in the
    // clear, where anyone on the path could have put them.
    if !reader.buffer().is_empty() {
        return Err(ImapError::Protocol(
            "the server sent data before TLS started".into(),
        ));
    }
    Ok(reader.into_inner())
}

async fn say<S: AsyncRead + AsyncWrite + Unpin>(
    reader: &mut BufReader<S>,
    line: &str,
) -> Result<(), ImapError> {
    let stream = reader.get_mut();
    stream.write_all(line.as_bytes()).await.map_err(network)?;
    stream.flush().await.map_err(network)
}

fn network(err: io::Error) -> ImapError {
    ImapError::Network(err.to_string())
}

/// One SMTP reply before TLS. RFC 5321 section 4.2: each line starts with
/// the code, and every line but the last has a hyphen after it.
struct Reply {
    code: u16,
    lines: Vec<String>,
}

impl Reply {
    fn text(&self) -> String {
        clipped(format!("{} {}", self.code, self.lines.join(" ")))
    }

    /// A reply that turns the client away.
    fn refused(&self) -> ImapError {
        let code = self.code.to_string();
        let text = self.text();
        classify(Smtp {
            code: Some(&code),
            transient: code.starts_with('4'),
            permanent: code.starts_with('5'),
            client: false,
            text: &text,
        })
    }
}

async fn reply<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Reply, ImapError> {
    let mut lines = Vec::new();
    loop {
        let line = tls::line(reader).await?;
        let bytes = line.as_bytes();
        let code = line
            .get(..3)
            .and_then(|code| code.parse::<u16>().ok())
            .filter(|_| matches!(bytes.get(3), None | Some(b' ' | b'-')));
        let Some(code) = code else {
            return Err(ImapError::Protocol(format!(
                "not an SMTP reply: {}",
                clipped(line)
            )));
        };
        let last = bytes.get(3) != Some(&b'-');
        lines.push(line.get(4..).unwrap_or_default().to_string());
        if last {
            return Ok(Reply { code, lines });
        }
        if lines.len() >= MAX_REPLY_LINES {
            return Err(ImapError::Protocol("an SMTP reply ran too long".into()));
        }
    }
}

/// What lettre reads first on a STARTTLS connection. The server greeted
/// on the plain connection and sends no second greeting after the
/// handshake, but lettre reads one before its EHLO.
const STAND_IN: &[u8] = b"220 TLS started\r\n";

/// The stream lettre works on: the connection, after a stand-in greeting
/// when the real one came before TLS.
#[derive(Debug)]
struct Wire<S> {
    stand_in: &'static [u8],
    stream: S,
}

impl<S: AsyncRead + Unpin> AsyncRead for Wire<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.stand_in.is_empty() {
            return Pin::new(&mut this.stream).poll_read(cx, buf);
        }
        let n = this.stand_in.len().min(buf.remaining());
        buf.put_slice(&this.stand_in[..n]);
        this.stand_in = &this.stand_in[n..];
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Wire<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

impl<S> AsyncTokioStream for Wire<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + std::fmt::Debug,
{
    /// lettre asks for the address only through its own dialing, which
    /// this client does not use.
    fn peer_addr(&self) -> io::Result<SocketAddr> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the SMTP client does not track the peer address",
        ))
    }
}

fn smtp_error(err: &lettre::transport::smtp::Error, login: &Login) -> ImapError {
    let code = err.status().map(|c| c.to_string());
    let text = hide(err.to_string(), login);
    classify(Smtp {
        code: code.as_deref(),
        transient: err.is_transient(),
        permanent: err.is_permanent(),
        client: err.is_client(),
        text: &text,
    })
}

/// `text` with the password, and the base64 forms AUTH PLAIN and LOGIN
/// send it in, taken out. A server may repeat what it was sent in its
/// refusal, and error text goes to the log.
fn hide(mut text: String, login: &Login) -> String {
    let plain = BASE64.encode(format!("\0{}\0{}", login.user, login.password));
    let alone = BASE64.encode(&login.password);
    // The base64 forms go first: taking the password out first could
    // break a base64 form that happens to contain it.
    for secret in [plain, alone, login.password.clone()] {
        if !secret.is_empty() && text.contains(&secret) {
            text = text.replace(&secret, "<hidden>");
        }
    }
    text
}

/// What lettre says about a failure, taken apart so a test can build one.
/// lettre's error type has no public constructor.
struct Smtp<'a> {
    code: Option<&'a str>,
    transient: bool,
    permanent: bool,
    client: bool,
    text: &'a str,
}

fn classify(e: Smtp<'_>) -> ImapError {
    let text = clipped(e.text.to_string());
    // RFC 4954 section 6: 535 wrong credentials, 534 a stronger mechanism
    // or an app password wanted, 530 sign-in required, 538 TLS required.
    if matches!(e.code, Some("530" | "534" | "535" | "538")) {
        return ImapError::Auth { text };
    }
    let lower = text.to_ascii_lowercase();
    if e.code == Some("421") && (lower.contains("too many") || lower.contains("connection")) {
        return ImapError::TooManyConnections { text };
    }
    match (e.transient, e.permanent, e.client) {
        (true, _, _) => ImapError::Network(text),
        (_, true, _) => ImapError::Refused(text),
        (_, _, true) => ImapError::Protocol(text),
        _ => ImapError::Network(text),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, PoisonError};

    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use tokio::io::DuplexStream;

    use super::{Smtp, SmtpClient, classify, envelope, hide, starttls};
    use crate::client::Dial;
    use crate::refusal::MAX_ERROR_TEXT as MAX_SERVER_TEXT;
    use crate::testing::{HeapMark, SmtpSeen, smtp_pipe, smtp_server};
    use crate::{ImapError, Login};

    fn failure<'a>(code: Option<&'a str>, text: &'a str) -> Smtp<'a> {
        let first = code.and_then(|c| c.chars().next());
        Smtp {
            code,
            transient: first == Some('4'),
            permanent: first == Some('5'),
            client: false,
            text,
        }
    }

    #[test]
    fn a_refused_password_is_an_auth_error() {
        let err = classify(failure(
            Some("535"),
            "permanent error (535): 5.7.8 Username and Password not accepted",
        ));
        assert!(matches!(err, ImapError::Auth { .. }));
    }

    #[test]
    fn an_app_password_wanted_is_an_auth_error_too() {
        let err = classify(failure(
            Some("534"),
            "permanent error (534): 5.7.9 Application-specific password required",
        ));
        assert!(matches!(err, ImapError::Auth { .. }));
    }

    #[test]
    fn a_busy_server_is_worth_retrying_and_a_refused_message_is_not() {
        assert!(classify(failure(Some("451"), "transient error (451): try later")).is_transient());
        let refused = classify(failure(
            Some("552"),
            "permanent error (552): message too large",
        ));
        assert_eq!(
            refused,
            ImapError::Refused("permanent error (552): message too large".into())
        );
        let limited = classify(failure(
            Some("421"),
            "transient error (421): Too many connections",
        ));
        assert!(matches!(limited, ImapError::TooManyConnections { .. }));
    }

    #[test]
    fn the_envelope_takes_every_recipient_and_refuses_an_empty_list() {
        let ok = envelope(
            "me@example.com",
            &["a@example.com".into(), "b@example.com".into()],
        )
        .unwrap();
        assert_eq!(ok.to().len(), 2);
        assert!(matches!(
            envelope("me@example.com", &[]),
            Err(ImapError::Refused(_))
        ));
        assert!(matches!(
            envelope("me@example.com", &["not an address".into()]),
            Err(ImapError::Refused(_))
        ));
    }

    #[test]
    fn server_text_is_clipped() {
        let long = "é".repeat(5_000);
        let ImapError::Refused(kept) = classify(failure(Some("554"), &long)) else {
            panic!("not a refusal");
        };
        assert_eq!(kept.chars().count(), MAX_SERVER_TEXT);
    }

    /// A server that echoes what it was sent would otherwise put the
    /// password in the error, and error text goes to the log.
    #[test]
    fn the_password_and_its_base64_forms_never_reach_an_error() {
        let login = Login::new("ann@example.com", "pässword 1");
        let plain = BASE64.encode("\0ann@example.com\0pässword 1");
        let alone = BASE64.encode("pässword 1");
        let text = format!("535 no: pässword 1 {plain} {alone}");
        let hidden = hide(text, &login);
        assert!(!hidden.contains("pässword"), "{hidden}");
        assert!(!hidden.contains(&plain), "{hidden}");
        assert!(!hidden.contains(&alone), "{hidden}");
        assert!(hidden.starts_with("535 no: "), "{hidden}");
    }

    /// A plain SMTP server that greets and answers through `answer`.
    fn plain(seen: SmtpSeen, answer: fn(&str) -> Vec<String>) -> DuplexStream {
        smtp_pipe("220 smtp.example.com ESMTP", seen, answer)
    }

    fn ehlo_without_starttls(command: &str) -> Vec<String> {
        match command {
            c if c.starts_with("EHLO") => {
                vec!["250-smtp.example.com".into(), "250 AUTH PLAIN".into()]
            }
            _ => vec!["250 OK".into()],
        }
    }

    fn ehlo_with_starttls(command: &str) -> Vec<String> {
        match command {
            c if c.starts_with("EHLO") => vec![
                "250-smtp.example.com".into(),
                "250-starttls".into(),
                "250 SIZE 35882577".into(),
            ],
            "STARTTLS" => vec!["220 2.0.0 Ready to start TLS".into()],
            _ => vec!["502 no".into()],
        }
    }

    #[tokio::test]
    async fn starttls_names_the_machine_by_address_and_hands_the_stream_back() {
        let seen = SmtpSeen::default();
        let stream = plain(seen.clone(), ehlo_with_starttls);
        assert!(starttls(stream, "smtp.example.com").await.is_ok());
        assert_eq!(seen.commands(), ["EHLO [127.0.0.1]", "STARTTLS"]);
    }

    /// Without STARTTLS on offer, going on would send the password in
    /// the clear. The client stops before AUTH.
    #[tokio::test]
    async fn a_server_that_does_not_offer_starttls_is_a_tls_error() {
        let seen = SmtpSeen::default();
        let stream = plain(seen.clone(), ehlo_without_starttls);
        let err = starttls(stream, "smtp.example.com").await.err();
        assert!(
            matches!(err, Some(ImapError::Tls { ref host, .. }) if host == "smtp.example.com"),
            "{err:?}"
        );
        assert_eq!(seen.commands(), ["EHLO [127.0.0.1]"]);
    }

    #[tokio::test]
    async fn starttls_refused_is_a_tls_error() {
        fn refuses(command: &str) -> Vec<String> {
            match command {
                "STARTTLS" => vec!["454 4.7.0 TLS not available".into()],
                other => ehlo_with_starttls(other),
            }
        }
        let err = starttls(plain(SmtpSeen::default(), refuses), "smtp.example.com")
            .await
            .err();
        assert!(matches!(err, Some(ImapError::Tls { .. })), "{err:?}");
    }

    /// Bytes after the 220 and before the handshake travel in the clear,
    /// where anyone on the path could have put them.
    #[tokio::test]
    async fn data_after_the_go_ahead_and_before_tls_is_refused() {
        fn injects(command: &str) -> Vec<String> {
            match command {
                "STARTTLS" => vec!["220 go ahead\r\n250 injected".into()],
                other => ehlo_with_starttls(other),
            }
        }
        let err = starttls(plain(SmtpSeen::default(), injects), "smtp.example.com")
            .await
            .err();
        assert!(matches!(err, Some(ImapError::Protocol(_))), "{err:?}");
    }

    #[tokio::test]
    async fn a_greeting_that_turns_the_client_away_is_not_a_tls_error() {
        let stream = smtp_pipe(
            "421 4.7.0 Too many connections",
            SmtpSeen::default(),
            smtp_server,
        );
        let err = starttls(stream, "smtp.example.com").await.err();
        assert!(
            matches!(err, Some(ImapError::TooManyConnections { .. })),
            "{err:?}"
        );
    }

    /// Hands out one scripted SMTP server per dial, in order.
    #[derive(Clone)]
    struct Pipes {
        next: Arc<Mutex<Vec<(DuplexStream, bool)>>>,
    }

    impl Pipes {
        fn of(pipes: Vec<(DuplexStream, bool)>) -> Self {
            Pipes {
                next: Arc::new(Mutex::new(pipes.into_iter().rev().collect())),
            }
        }
    }

    impl Dial for Pipes {
        type Stream = DuplexStream;

        async fn dial(&self) -> Result<(DuplexStream, bool), ImapError> {
            self.next
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop()
                .ok_or_else(|| ImapError::Network("no more servers".into()))
        }
    }

    fn client(stream: DuplexStream, greeted: bool, password: &str) -> SmtpClient<Pipes> {
        SmtpClient::with_dial(
            Pipes::of(vec![(stream, greeted)]),
            Login::new("ann@example.com", password),
        )
    }

    #[tokio::test]
    async fn a_send_signs_in_names_every_recipient_and_doubles_leading_dots() {
        let seen = SmtpSeen::default();
        let stream = smtp_pipe("220 smtp.example.com ESMTP", seen.clone(), smtp_server);
        let raw = b"Subject: hi\r\n\r\n.starts with a dot\r\nend\r\n";
        client(stream, false, "pässword")
            .submit(
                "ann@example.com",
                &["bob@example.com".into(), "hidden@example.com".into()],
                raw,
            )
            .await
            .unwrap();
        let plain = BASE64.encode("\0ann@example.com\0pässword");
        assert_eq!(
            seen.commands(),
            [
                "EHLO [127.0.0.1]".to_string(),
                format!("AUTH PLAIN {plain}"),
                "MAIL FROM:<ann@example.com>".into(),
                "RCPT TO:<bob@example.com>".into(),
                "RCPT TO:<hidden@example.com>".into(),
                "DATA".into(),
                "QUIT".into(),
            ]
        );
        assert_eq!(
            seen.messages(),
            [b"Subject: hi\r\n\r\n..starts with a dot\r\nend\r\n".to_vec()]
        );
    }

    /// After STARTTLS the server sends no new greeting, and RFC 3207 wants
    /// a fresh EHLO over TLS before AUTH.
    #[tokio::test]
    async fn after_starttls_the_client_says_ehlo_again_before_signing_in() {
        let seen = SmtpSeen::default();
        let stream = smtp_pipe("", seen.clone(), smtp_server);
        client(stream, true, "pw").check().await.unwrap();
        let commands = seen.commands();
        assert_eq!(commands[0], "EHLO [127.0.0.1]");
        assert!(commands[1].starts_with("AUTH PLAIN "), "{commands:?}");
        assert_eq!(commands.last().map(String::as_str), Some("QUIT"));
    }

    #[tokio::test]
    async fn a_refused_password_is_an_auth_error_that_does_not_repeat_it() {
        fn refuses(command: &str) -> Vec<String> {
            match command {
                c if c.starts_with("AUTH") => {
                    vec![format!("535 5.7.8 Bad credentials: {c}")]
                }
                other => smtp_server(other),
            }
        }
        let stream = smtp_pipe("220 ready", SmtpSeen::default(), refuses);
        let err = client(stream, false, "hunter2 secret")
            .check()
            .await
            .unwrap_err();
        let printed = format!("{err} {err:?}");
        assert!(matches!(err, ImapError::Auth { .. }), "{printed}");
        let plain = BASE64.encode("\0ann@example.com\0hunter2 secret");
        assert!(!printed.contains(&plain), "{printed}");
        assert!(!printed.contains("hunter2"), "{printed}");
    }

    #[tokio::test]
    async fn a_server_without_plain_or_login_is_unsupported() {
        fn cram_only(command: &str) -> Vec<String> {
            match command {
                c if c.starts_with("EHLO") => vec!["250 AUTH CRAM-MD5".into()],
                other => smtp_server(other),
            }
        }
        let stream = smtp_pipe("220 ready", SmtpSeen::default(), cram_only);
        let err = client(stream, false, "pw").check().await.unwrap_err();
        assert!(matches!(err, ImapError::Unsupported(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_message_with_eight_bit_text_needs_8bitmime() {
        fn seven_bit(command: &str) -> Vec<String> {
            match command {
                c if c.starts_with("EHLO") => vec!["250 AUTH PLAIN".into()],
                other => smtp_server(other),
            }
        }
        let stream = smtp_pipe("220 ready", SmtpSeen::default(), seven_bit);
        let err = client(stream, false, "pw")
            .submit(
                "ann@example.com",
                &["bob@example.com".into()],
                "Olá".as_bytes(),
            )
            .await
            .unwrap_err();
        assert_eq!(err, ImapError::Unsupported("8BITMIME"));
    }

    #[tokio::test]
    async fn a_refused_recipient_fails_the_send_with_the_servers_words() {
        fn no_bob(command: &str) -> Vec<String> {
            match command {
                c if c.starts_with("RCPT") => vec!["550 5.1.1 No such user".into()],
                other => smtp_server(other),
            }
        }
        let stream = smtp_pipe("220 ready", SmtpSeen::default(), no_bob);
        let err = client(stream, false, "pw")
            .submit("ann@example.com", &["bob@example.com".into()], b"x\r\n")
            .await
            .unwrap_err();
        assert!(
            matches!(err, ImapError::Refused(ref text) if text.contains("No such user")),
            "{err:?}"
        );
    }

    #[test]
    fn a_host_that_is_not_a_host_name_fails_at_once() {
        let server = mailrs_discover::Server {
            host: "smtp example".into(),
            port: 465,
            security: mailrs_discover::Security::Tls,
            user_name: mailrs_discover::UserName::Address,
        };
        let err = SmtpClient::new(&server, &Login::new("ann", "pw")).unwrap_err();
        assert!(matches!(err, ImapError::Tls { .. }), "{err:?}");
    }

    #[test]
    fn debug_hides_the_password() {
        let server = mailrs_discover::Server {
            host: "smtp.example.com".into(),
            port: 587,
            security: mailrs_discover::Security::StartTls,
            user_name: mailrs_discover::UserName::Address,
        };
        let client = SmtpClient::new(&server, &Login::new("ann", "hunter2 secret")).unwrap();
        assert!(!format!("{client:?}").contains("hunter2"));
    }

    /// The message goes out in chunks, so a send holds the caller's copy
    /// and one chunk, not a second copy of the whole message. The server
    /// runs on the worker thread, so the count sees the client alone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_send_holds_one_chunk_beside_the_message() {
        let raw = b"a line of text\r\n".repeat(1 << 19);
        let stream = smtp_pipe("220 ready", SmtpSeen::default(), smtp_server);
        let client = client(stream, false, "pw");
        let to = ["bob@example.com".to_string()];
        let mark = HeapMark::start();
        client.submit("ann@example.com", &to, &raw).await.unwrap();
        let peak = mark.peak();
        eprintln!("an 8 MiB send held {peak} bytes at its peak");
        assert!(peak < 512 << 10, "{peak} bytes");
    }
}
