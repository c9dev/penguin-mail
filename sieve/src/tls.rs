//! TLS for ManageSieve: a plain connection, STARTTLS, then TLS 1.2 or 1.3
//! with the platform's roots and the host name checked. Nothing here can
//! turn either check off, and nothing is sent in the clear but STARTTLS.

use std::sync::Arc;

use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use rustls_platform_verifier::BuilderVerifierExt;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::client::SieveError;

pub(crate) async fn handshake(
    host: &str,
    tcp: TcpStream,
) -> Result<TlsStream<TcpStream>, SieveError> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|err| SieveError::Tls(err.to_string()))?
        .with_platform_verifier()
        .map_err(|err| SieveError::Tls(err.to_string()))?
        .with_no_client_auth();
    let name = ServerName::try_from(host.to_string())
        .map_err(|_| SieveError::Tls(format!("{host} is not a host name")))?;
    TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
        .map_err(|err| SieveError::Tls(err.to_string()))
}
