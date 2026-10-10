//! TLS to a mail server, as implicit TLS or as STARTTLS on a plain
//! connection first. TLS 1.2 or 1.3, the only versions rustls speaks,
//! with the certificate checked against the platform's roots and the
//! host name. A certificate explicitly pinned for a local mail bridge
//! is accepted only on 127.0.0.1.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use mailrs_discover::{Security, Server};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime, pem::PemObject};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_platform_verifier::BuilderVerifierExt;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::ImapError;
use crate::refusal::{Doing, Refusal, clipped, refusal};

/// A connection to a mail server once TLS runs.
pub type Tls = TlsStream<TcpStream>;

/// The longest line read before TLS starts. A greeting is one short line.
const LINE_LIMIT: u64 = 8192;

/// Connects to `server` and starts TLS. The flag says the greeting was
/// read already, as STARTTLS reads it on the plain connection.
pub(crate) async fn dial(server: &Server) -> Result<(Tls, bool), ImapError> {
    let host = server.host.as_str();
    let tcp = connect(server).await?;
    let (tcp, greeted) = match server.security {
        Security::Tls => (tcp, false),
        Security::StartTls => (starttls(tcp, host).await?, true),
    };
    Ok((handshake(host, tcp).await?, greeted))
}

/// A plain TCP connection to `server`'s host and port.
pub(crate) async fn connect(server: &Server) -> Result<TcpStream, ImapError> {
    TcpStream::connect((server.host.as_str(), server.port))
        .await
        .map_err(|err| ImapError::Network(format!("{}:{}: {err}", server.host, server.port)))
}

/// `host` as rustls wants it, which also refuses anything that is not a
/// host name or an IP address.
pub(crate) fn server_name(host: &str) -> Result<ServerName<'static>, ImapError> {
    ServerName::try_from(host.to_string()).map_err(|_| ImapError::Tls {
        host: host.to_string(),
        detail: "not a host name".into(),
    })
}

/// Runs the TLS handshake on `tcp` and checks the certificate for `host`.
pub(crate) async fn handshake(host: &str, tcp: TcpStream) -> Result<Tls, ImapError> {
    TlsConnector::from(config(host)?)
        .connect(server_name(host)?, tcp)
        .await
        .map_err(|err| handshake_error(host, err))
}

/// TLS 1.2 and 1.3 with the platform verifier or an exact local pin.
fn config(host: &str) -> Result<Arc<ClientConfig>, ImapError> {
    let setup = |err: rustls::Error| ImapError::Tls {
        host: host.to_string(),
        detail: err.to_string(),
    };
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(rustls::DEFAULT_VERSIONS)
        .map_err(setup)?;
    let config = if host == "127.0.0.1" {
        if let Some(cert) = local_pin(host)? {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(
                    PinnedLocalCert::new(cert, provider).map_err(setup)?,
                ))
                .with_no_client_auth()
        } else {
            builder
                .with_platform_verifier()
                .map_err(setup)?
                .with_no_client_auth()
        }
    } else {
        builder
            .with_platform_verifier()
            .map_err(setup)?
            .with_no_client_auth()
    };
    Ok(Arc::new(config))
}

/// A bridge export in this file is an exact pin, never a general trust root.
fn local_pin(host: &str) -> Result<Option<CertificateDer<'static>>, ImapError> {
    let Some(dir) = dirs::config_dir() else {
        return Ok(None);
    };
    let path = dir.join("penguin-mail/proton-bridge-cert.pem");
    read_pin(&path, host)
}

fn read_pin(path: &Path, host: &str) -> Result<Option<CertificateDer<'static>>, ImapError> {
    let pem = match std::fs::read(path) {
        Ok(pem) => pem,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(ImapError::Tls {
                host: host.into(),
                detail: format!("could not read {}: {err}", path.display()),
            });
        }
    };
    let mut certs = CertificateDer::pem_slice_iter(&pem);
    let cert = certs
        .next()
        .transpose()
        .map_err(|err| ImapError::Tls {
            host: host.into(),
            detail: format!("invalid Bridge certificate: {err}"),
        })?
        .ok_or_else(|| ImapError::Tls {
            host: host.into(),
            detail: "the Bridge certificate file has no certificate".into(),
        })?;
    if certs.next().is_some()
        || pem
            .windows(b"PRIVATE KEY".len())
            .any(|part| part == b"PRIVATE KEY")
    {
        return Err(ImapError::Tls {
            host: host.into(),
            detail: "the Bridge certificate file must contain one public certificate".into(),
        });
    }
    Ok(Some(cert))
}

struct PinnedLocalCert {
    cert: CertificateDer<'static>,
    fallback: Arc<dyn ServerCertVerifier>,
}

impl fmt::Debug for PinnedLocalCert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PinnedLocalCert").finish_non_exhaustive()
    }
}

impl PinnedLocalCert {
    fn new(cert: CertificateDer<'static>, provider: Arc<CryptoProvider>) -> Result<Self, rustls::Error> {
        let fallback = Arc::new(rustls_platform_verifier::Verifier::new(provider)?);
        Ok(Self::with_fallback(cert, fallback))
    }

    fn with_fallback(
        cert: CertificateDer<'static>,
        fallback: Arc<dyn ServerCertVerifier>,
    ) -> Self {
        Self { cert, fallback }
    }
}

impl ServerCertVerifier for PinnedLocalCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if *server_name == ServerName::try_from("127.0.0.1").expect("a valid loopback address")
            && end_entity.as_ref() == self.cert.as_ref()
        {
            return Ok(ServerCertVerified::assertion());
        }
        self.fallback.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.fallback.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.fallback.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.fallback.supported_verify_schemes()
    }
}

/// A failed handshake: a certificate or protocol failure is the server's
/// TLS, anything else the network. tokio-rustls hands rustls's own error
/// inside the I/O error.
pub(crate) fn handshake_error(host: &str, err: std::io::Error) -> ImapError {
    let tls = err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        .map(ToString::to_string);
    match (tls, err.kind()) {
        (Some(detail), _) => ImapError::Tls {
            host: host.to_string(),
            detail,
        },
        (None, std::io::ErrorKind::InvalidData) => ImapError::Tls {
            host: host.to_string(),
            detail: err.to_string(),
        },
        (None, _) => ImapError::Network(format!("{host}: {err}")),
    }
}

/// Reads the greeting on a plain connection and asks for STARTTLS. Hands
/// the stream back ready for the handshake.
pub(crate) async fn starttls<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    host: &str,
) -> Result<S, ImapError> {
    let mut reader = BufReader::new(stream);
    let greeting = line(&mut reader).await?;
    if let Some(text) = greeting.strip_prefix("* BYE") {
        return Err(refusal(Doing::Greeting, Refusal::Bye, false, text.trim()));
    }
    // PREAUTH on a plain connection would mean working without TLS.
    if !greeting.starts_with("* OK") {
        return Err(ImapError::Protocol(format!(
            "the greeting is not one this client accepts: {}",
            clipped(greeting)
        )));
    }
    let stream = reader.get_mut();
    stream
        .write_all(b"PM0 STARTTLS\r\n")
        .await
        .map_err(network)?;
    stream.flush().await.map_err(network)?;
    loop {
        let answer = line(&mut reader).await?;
        let Some(status) = answer.strip_prefix("PM0 ") else {
            continue;
        };
        if !status.to_ascii_uppercase().starts_with("OK") {
            return Err(ImapError::Tls {
                host: host.to_string(),
                detail: format!("the server refused STARTTLS: {}", clipped(status.into())),
            });
        }
        break;
    }
    // Bytes after the OK and before the handshake travel in the clear,
    // where anyone on the path could have put them; a server sends none.
    if !reader.buffer().is_empty() {
        return Err(ImapError::Protocol(
            "the server sent data before TLS started".into(),
        ));
    }
    Ok(reader.into_inner())
}

pub(crate) async fn line<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> Result<String, ImapError> {
    let mut bytes = Vec::new();
    let read = (&mut *reader)
        .take(LINE_LIMIT)
        .read_until(b'\n', &mut bytes)
        .await
        .map_err(network)?;
    if read == 0 {
        return Err(ImapError::Network(
            "the server closed the connection".into(),
        ));
    }
    if !bytes.ends_with(b"\n") {
        return Err(ImapError::Protocol("a line before TLS ran too long".into()));
    }
    Ok(String::from_utf8_lossy(&bytes).trim_end().to_string())
}

fn network(err: std::io::Error) -> ImapError {
    ImapError::Network(err.to_string())
}

#[cfg(test)]
mod tests {
    use rustls::client::danger::ServerCertVerifier;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime, pem::PemObject};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_rustls::{TlsAcceptor, TlsConnector};

    use super::{handshake_error, starttls};
    use crate::ImapError;

    #[test]
    fn local_pin_accepts_only_the_exact_certificate_and_loopback_name() {
        let cert = CertificateDer::from(vec![1, 2, 3]);
        let other = CertificateDer::from(vec![1, 2, 4]);
        let pin = super::PinnedLocalCert::new(
            cert.clone(),
            std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
        )
        .unwrap();
        let loopback = ServerName::try_from("127.0.0.1").unwrap();
        let remote = ServerName::try_from("mail.example.com").unwrap();
        let now = UnixTime::now();
        assert!(
            pin.verify_server_cert(&cert, &[], &loopback, &[], now)
                .is_ok()
        );
        assert!(
            pin.verify_server_cert(&other, &[], &loopback, &[], now)
                .is_err()
        );
        assert!(
            pin.verify_server_cert(&cert, &[], &remote, &[], now)
                .is_err()
        );
    }

    #[test]
    fn bridge_pin_file_reads_only_the_public_certificate() {
        let Some(certs) = mailrs_testmail::Certs::make() else {
            return;
        };
        let expected = CertificateDer::from_pem_file(certs.root()).unwrap();
        let loaded = super::read_pin(&certs.root(), "127.0.0.1")
            .unwrap()
            .unwrap();
        assert_eq!(loaded, expected);
        assert!(super::read_pin(&certs.root_key(), "127.0.0.1").is_err());
    }

    #[test]
    fn a_different_local_certificate_can_still_use_normal_trust() {
        let Some(certs) = mailrs_testmail::Certs::make() else {
            return;
        };
        let root = CertificateDer::from_pem_file(certs.root()).unwrap();
        let server = CertificateDer::from_pem_file(certs.root().with_file_name("tls.crt")).unwrap();
        let pin = CertificateDer::from_pem_file(certs.stranger()).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(root).unwrap();
        let fallback = rustls::client::WebPkiServerVerifier::builder(std::sync::Arc::new(roots))
            .build()
            .unwrap();
        let verifier = super::PinnedLocalCert::with_fallback(
            pin,
            fallback,
        );
        let name = ServerName::try_from("localhost").unwrap();
        assert!(
            verifier
                .verify_server_cert(&server, &[], &name, &[], UnixTime::now())
                .is_ok()
        );
    }

    #[tokio::test]
    async fn pinned_ca_certificate_completes_a_real_tls_handshake() {
        let Some(certs) = mailrs_testmail::Certs::make() else {
            return;
        };
        let cert = CertificateDer::from_pem_file(certs.root()).unwrap();
        let key = PrivateKeyDer::from_pem_file(certs.root_key()).unwrap();
        let server = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert.clone()], key)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let serving = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            TlsAcceptor::from(std::sync::Arc::new(server))
                .accept(tcp)
                .await
        });
        let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let client = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(rustls::DEFAULT_VERSIONS)
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(std::sync::Arc::new(
                super::PinnedLocalCert::new(cert, provider).unwrap(),
            ))
            .with_no_client_auth();
        let tcp = TcpStream::connect(address).await.unwrap();
        let name = ServerName::try_from("127.0.0.1").unwrap();
        assert!(
            TlsConnector::from(std::sync::Arc::new(client))
                .connect(name, tcp)
                .await
                .is_ok()
        );
        assert!(serving.await.unwrap().is_ok());
    }

    /// A plain server that greets with `greeting`, reads one command and
    /// answers `answer` in a single write.
    fn plain(greeting: &'static str, answer: &'static str) -> tokio::io::DuplexStream {
        let (client, server) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server);
            let mut read = BufReader::new(read);
            let _ = write.write_all(greeting.as_bytes()).await;
            let mut command = String::new();
            let _ = read.read_line(&mut command).await;
            let _ = write.write_all(answer.as_bytes()).await;
            // Keep the pipe open until the client is done with it.
            let _ = read.read_line(&mut command).await;
        });
        client
    }

    #[tokio::test]
    async fn starttls_hands_the_stream_back_after_ok() {
        let stream = plain("* OK ready\r\n", "PM0 OK Begin TLS\r\n");
        assert!(starttls(stream, "imap.example.com").await.is_ok());
    }

    #[tokio::test]
    async fn starttls_refused_is_a_tls_error_and_never_falls_back() {
        let stream = plain("* OK ready\r\n", "PM0 BAD STARTTLS unknown\r\n");
        let err = starttls(stream, "imap.example.com").await.err();
        assert!(
            matches!(err, Some(ImapError::Tls { ref host, .. }) if host == "imap.example.com"),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn data_after_the_ok_and_before_tls_is_refused() {
        let stream = plain("* OK ready\r\n", "PM0 OK Begin TLS\r\n* 1 EXISTS\r\n");
        let err = starttls(stream, "imap.example.com").await.err();
        assert!(matches!(err, Some(ImapError::Protocol(_))), "{err:?}");
    }

    #[tokio::test]
    async fn preauth_on_a_plain_connection_is_refused() {
        let stream = plain("* PREAUTH welcome\r\n", "");
        let err = starttls(stream, "imap.example.com").await.err();
        assert!(matches!(err, Some(ImapError::Protocol(_))), "{err:?}");
    }

    #[test]
    fn a_certificate_for_another_name_is_a_tls_error_naming_the_host() {
        let bad = rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidForName);
        let err = handshake_error(
            "imap.example.com",
            std::io::Error::new(std::io::ErrorKind::InvalidData, bad),
        );
        assert!(
            matches!(err, ImapError::Tls { ref host, .. } if host == "imap.example.com"),
            "{err:?}"
        );
    }

    #[test]
    fn a_reset_during_the_handshake_is_a_network_error() {
        let err = handshake_error(
            "imap.example.com",
            std::io::ErrorKind::ConnectionReset.into(),
        );
        assert!(matches!(err, ImapError::Network(_)));
    }
}
