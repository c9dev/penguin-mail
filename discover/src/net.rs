//! The network discovery reads: DNS, HTTPS and TCP, behind one trait so
//! tests answer from memory.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::RData;
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use rustls_platform_verifier::BuilderVerifierExt;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::Security;

/// DNS, HTTPS and TCP, so tests answer from fakes.
pub trait Net: Send + Sync {
    /// The domain's mail exchangers, lowest preference first, as lower
    /// case names without the root dot. Empty when there are none.
    fn mx(&self, domain: &str) -> impl Future<Output = Vec<String>> + Send;
    /// The SRV records at `name`, as the server gave them, targets as
    /// they came (a target of `.` means the service is not offered).
    fn srv(&self, name: &str) -> impl Future<Output = Vec<SrvRecord>> + Send;
    /// The TXT strings at `name`, each record's strings joined. Empty when
    /// there are none. Answers nothing unless an implementation says
    /// otherwise, so a network that asks nobody need not.
    fn txt(&self, name: &str) -> impl Future<Output = Vec<String>> + Send {
        let _ = name;
        async { Vec::new() }
    }
    /// Where an HTTPS URL leads once its redirects are followed, when the
    /// server there answers anything but "not found" or a server error: a
    /// well-known DAV URL answers 401 at its target, which still says the
    /// service is there. No credentials go with it.
    fn locate(&self, url: &str) -> impl Future<Output = Option<String>> + Send {
        let _ = url;
        async { None }
    }
    /// The body of an HTTPS page, or `None` for anything but a success.
    fn get(&self, url: &str) -> impl Future<Output = Option<String>> + Send;
    /// Whether a connection to host:port reaches TLS with a valid
    /// certificate. `Security::Tls` starts TLS at once; `Security::StartTls`
    /// speaks SMTP first, since discovery probes STARTTLS only on SMTP
    /// submission, and asks for STARTTLS after EHLO.
    fn reaches(
        &self,
        host: &str,
        port: u16,
        security: Security,
    ) -> impl Future<Output = bool> + Send;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SrvRecord {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    pub target: String,
}

/// How long one request may take. Discovery drops a whole step after
/// `STEP_LIMIT`; this keeps one slow server from using all of it.
const REQUEST_LIMIT: Duration = Duration::from_secs(8);

/// An autoconfig file is a few kilobytes; a server sending more is not
/// sending one.
const MOST_BYTES: usize = 256 * 1024;

/// What the SMTP probe says after EHLO. An address literal names no
/// host, so the server learns nothing it did not see already.
const EHLO: &[u8] = b"EHLO [127.0.0.1]\r\n";

#[derive(Debug, thiserror::Error)]
pub enum RealNetError {
    #[error("cannot read the system's DNS settings: {0}")]
    Dns(#[from] hickory_resolver::net::NetError),
    #[error("cannot set up HTTPS: {0}")]
    Http(#[from] reqwest::Error),
    #[error("cannot set up TLS: {0}")]
    Tls(#[from] rustls::Error),
}

/// The real network: the system's DNS servers through hickory, HTTPS
/// through reqwest, and TLS through rustls with the platform's
/// certificate store.
pub struct RealNet {
    resolver: TokioResolver,
    http: reqwest::Client,
    tls: TlsConnector,
}

impl RealNet {
    pub fn new() -> Result<RealNet, RealNetError> {
        let resolver = TokioResolver::builder_tokio()?.build()?;
        let http = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::limited(3))
            .referer(false)
            .timeout(REQUEST_LIMIT)
            .user_agent(concat!("Penguin Mail/", env!("CARGO_PKG_VERSION")))
            .build()?;
        // The provider is named here rather than read from the process
        // default, so another crate turning on a second one cannot leave
        // rustls without a choice.
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_platform_verifier()?
            .with_no_client_auth();
        Ok(RealNet {
            resolver,
            http,
            tls: TlsConnector::from(Arc::new(config)),
        })
    }

    async fn tls_handshake<S>(&self, stream: S, host: &str) -> bool
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let Ok(name) = ServerName::try_from(host.to_string()) else {
            return false;
        };
        self.tls.connect(name, stream).await.is_ok()
    }
}

impl Net for RealNet {
    async fn mx(&self, domain: &str) -> Vec<String> {
        let Ok(lookup) = self.resolver.mx_lookup(rooted(domain)).await else {
            return Vec::new();
        };
        by_preference(
            lookup
                .answers()
                .iter()
                .filter_map(|record| match &record.data {
                    RData::MX(mx) => Some((mx.preference, mx.exchange.to_ascii())),
                    _ => None,
                })
                .collect(),
        )
    }

    async fn srv(&self, name: &str) -> Vec<SrvRecord> {
        let Ok(lookup) = self.resolver.srv_lookup(rooted(name)).await else {
            return Vec::new();
        };
        // A domain with wildcard DNS answers a name it has no SRV for with
        // a CNAME. Only SRV records count.
        lookup
            .answers()
            .iter()
            .filter_map(|record| match &record.data {
                RData::SRV(srv) => Some(SrvRecord {
                    priority: srv.priority,
                    weight: srv.weight,
                    port: srv.port,
                    target: srv.target.to_ascii(),
                }),
                _ => None,
            })
            .collect()
    }

    async fn txt(&self, name: &str) -> Vec<String> {
        let Ok(lookup) = self.resolver.txt_lookup(rooted(name)).await else {
            return Vec::new();
        };
        lookup
            .answers()
            .iter()
            .filter_map(|record| match &record.data {
                RData::TXT(txt) => Some(
                    txt.txt_data
                        .iter()
                        .map(|part| String::from_utf8_lossy(part))
                        .collect::<String>(),
                ),
                _ => None,
            })
            .collect()
    }

    async fn locate(&self, url: &str) -> Option<String> {
        let response = self.http.get(url).send().await.ok()?;
        let status = response.status().as_u16();
        (status != 404 && status != 410 && status < 500).then(|| response.url().to_string())
    }

    async fn get(&self, url: &str) -> Option<String> {
        let mut response = self
            .http
            .get(url)
            .header(reqwest::header::ACCEPT, "application/xml, text/xml")
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            body.extend_from_slice(&chunk);
            if body.len() > MOST_BYTES {
                return None;
            }
        }
        String::from_utf8(body).ok()
    }

    async fn reaches(&self, host: &str, port: u16, security: Security) -> bool {
        let attempt = async {
            // The TLS name below stays `host`, without the dot.
            let Ok(tcp) = TcpStream::connect((rooted(host), port)).await else {
                return false;
            };
            match security {
                Security::Tls => self.tls_handshake(tcp, host).await,
                Security::StartTls => {
                    let mut tcp = BufReader::new(tcp);
                    if !smtp_starttls(&mut tcp).await {
                        return false;
                    }
                    self.tls_handshake(tcp.into_inner(), host).await
                }
            }
        };
        tokio::time::timeout(REQUEST_LIMIT, attempt)
            .await
            .unwrap_or(false)
    }
}

/// `host` with the root dot, so the resolver never tries the system's
/// search domains and asks about a name the person never typed.
pub(crate) fn rooted(host: &str) -> String {
    format!("{}.", host.strip_suffix('.').unwrap_or(host))
}

/// MX hosts by numeric preference, lowest first, lower case and without
/// the root dot. The null MX of RFC 7505, a lone `.`, means the domain
/// takes no mail, and drops out.
pub(crate) fn by_preference(mut records: Vec<(u16, String)>) -> Vec<String> {
    records.sort_by_key(|(preference, _)| *preference);
    records
        .into_iter()
        .map(|(_, host)| host.trim_end_matches('.').to_ascii_lowercase())
        .filter(|host| !host.is_empty())
        .collect()
}

/// Asks an SMTP submission server to start TLS: the greeting, EHLO, and
/// STARTTLS when the server offers it. Discovery probes STARTTLS on port
/// 587 alone, so the dialogue is SMTP's.
pub(crate) async fn smtp_starttls<S>(stream: &mut S) -> bool
where
    S: AsyncBufRead + AsyncWrite + Unpin,
{
    let Some(greeting) = reply(stream).await else {
        return false;
    };
    if !greeting.starts_with("220") || stream.write_all(EHLO).await.is_err() {
        return false;
    }
    let Some(ehlo) = reply(stream).await else {
        return false;
    };
    let offers = ehlo.lines().any(|line| {
        line.starts_with("250")
            && line
                .get(4..)
                .is_some_and(|k| k.eq_ignore_ascii_case("STARTTLS"))
    });
    if !offers || stream.write_all(b"STARTTLS\r\n").await.is_err() {
        return false;
    }
    reply(stream).await.is_some_and(|r| r.starts_with("220"))
}

/// The most one SMTP reply may hold, all its lines together.
const REPLY_LIMIT: usize = 4096;

/// One SMTP reply, all its lines, or `None` when the server hangs up or
/// sends more than a reply can hold.
async fn reply<S>(stream: &mut S) -> Option<String>
where
    S: AsyncBufRead + Unpin,
{
    let mut all = String::new();
    loop {
        // The limit applies while the line is read, so a server that never
        // sends a newline cannot make the probe buffer more than a reply
        // may hold.
        let budget = REPLY_LIMIT.checked_sub(all.len())? as u64;
        let mut bytes = Vec::new();
        let read = (&mut *stream).take(budget).read_until(b'\n', &mut bytes).await.ok()?;
        if read == 0 || !bytes.ends_with(b"\n") {
            return None;
        }
        let line = String::from_utf8(bytes).ok()?;
        // "250-" continues a reply; "250 " ends it.
        let last = line.as_bytes().get(3) != Some(&b'-');
        all.push_str(&line);
        if last {
            return Some(all);
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, BufReader, duplex, repeat};

    use super::*;

    #[tokio::test]
    async fn a_line_with_no_end_is_cut_off_at_the_reply_limit() {
        let mut stream = BufReader::new(repeat(b'a').take(1 << 20));
        assert_eq!(reply(&mut stream).await, None);
        assert!(
            stream.get_ref().limit() > (1 << 20) - 16 * 1024,
            "the probe read {} bytes of a line that never ended",
            (1u64 << 20) - stream.get_ref().limit()
        );
    }

    #[test]
    fn mx_hosts_sort_by_their_number() {
        let records = vec![
            (20, "In2-SMTP.messagingengine.com.".to_string()),
            (5, "in1-smtp.messagingengine.com.".to_string()),
            (10, "alt.example.net.".to_string()),
        ];
        assert_eq!(
            by_preference(records),
            [
                "in1-smtp.messagingengine.com",
                "alt.example.net",
                "in2-smtp.messagingengine.com"
            ]
        );
    }

    #[test]
    fn a_probe_connects_to_the_name_from_the_root() {
        assert_eq!(rooted("imap.example.org"), "imap.example.org.");
        assert_eq!(rooted("imap.example.org."), "imap.example.org.");
    }

    #[test]
    fn a_null_mx_means_no_mail() {
        assert!(by_preference(vec![(0, ".".to_string())]).is_empty());
    }

    /// Plays an SMTP server that answers `script` in order, and returns
    /// what the probe wrote and whether it asked for TLS.
    async fn talk(script: &'static [&'static str]) -> (bool, String) {
        let (client, mut server) = duplex(4096);
        let serve = async move {
            let mut heard = Vec::new();
            for (i, answer) in script.iter().enumerate() {
                if i > 0 {
                    let mut buffer = [0u8; 256];
                    let n = server.read(&mut buffer).await.unwrap_or(0);
                    heard.extend_from_slice(&buffer[..n]);
                }
                if server.write_all(answer.as_bytes()).await.is_err() {
                    break;
                }
            }
            String::from_utf8_lossy(&heard).into_owned()
        };
        let mut client = BufReader::new(client);
        let (asked, heard) = tokio::join!(smtp_starttls(&mut client), serve);
        (asked, heard)
    }

    #[tokio::test]
    async fn starttls_goes_ahead_when_the_server_offers_it() {
        let (asked, heard) = talk(&[
            "220 smtp.example.org ESMTP\r\n",
            "250-smtp.example.org\r\n250-SIZE 1000\r\n250 STARTTLS\r\n",
            "220 Go ahead\r\n",
        ])
        .await;
        assert!(asked);
        assert_eq!(heard, "EHLO [127.0.0.1]\r\nSTARTTLS\r\n");
    }

    #[tokio::test]
    async fn no_starttls_on_offer_means_no_connection() {
        let (asked, heard) = talk(&[
            "220 smtp.example.org ESMTP\r\n",
            "250-smtp.example.org\r\n250 AUTH PLAIN LOGIN\r\n",
        ])
        .await;
        assert!(!asked);
        assert_eq!(heard, "EHLO [127.0.0.1]\r\n");
    }

    #[tokio::test]
    async fn a_refusing_greeting_ends_the_probe() {
        let (asked, _) = talk(&["554 No service\r\n"]).await;
        assert!(!asked);
    }
}
