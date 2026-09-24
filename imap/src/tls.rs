//! TLS to a mail server, as implicit TLS or as STARTTLS on a plain
//! connection first. TLS 1.2 or 1.3, the only versions rustls speaks,
//! with the certificate checked against the platform's roots and the
//! host name. Nothing here can turn either check off.

use std::sync::Arc;

use mailrs_discover::{Security, Server};
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use rustls_platform_verifier::BuilderVerifierExt;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::ImapError;
use crate::refusal::{Doing, Refusal, refusal};

/// A connection to a mail server once TLS runs.
pub type Tls = TlsStream<TcpStream>;

/// The longest line read before TLS starts. A greeting is one short line.
const LINE_LIMIT: u64 = 8192;

/// Connects to `server` and starts TLS. The flag says the greeting was
/// read already, as STARTTLS reads it on the plain connection.
pub(crate) async fn dial(server: &Server) -> Result<(Tls, bool), ImapError> {
    let host = server.host.as_str();
    let tcp = TcpStream::connect((host, server.port))
        .await
        .map_err(|err| ImapError::Network(format!("{host}:{}: {err}", server.port)))?;
    let (tcp, greeted) = match server.security {
        Security::Tls => (tcp, false),
        Security::StartTls => (starttls(tcp, host).await?, true),
    };
    let name = ServerName::try_from(host.to_string()).map_err(|_| ImapError::Tls {
        host: host.to_string(),
        detail: "not a host name".into(),
    })?;
    let tls = TlsConnector::from(config(host)?)
        .connect(name, tcp)
        .await
        .map_err(|err| handshake_error(host, err))?;
    Ok((tls, greeted))
}

/// TLS 1.2 and 1.3 with the platform's certificate verifier.
fn config(host: &str) -> Result<Arc<ClientConfig>, ImapError> {
    let setup = |err: rustls::Error| ImapError::Tls {
        host: host.to_string(),
        detail: err.to_string(),
    };
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(rustls::DEFAULT_VERSIONS)
        .map_err(setup)?
        .with_platform_verifier()
        .map_err(setup)?
        .with_no_client_auth();
    Ok(Arc::new(config))
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
            "the greeting is not one this client accepts: {greeting}"
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
                detail: format!("the server refused STARTTLS: {status}"),
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

async fn line<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> Result<String, ImapError> {
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
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::{handshake_error, starttls};
    use crate::ImapError;

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
