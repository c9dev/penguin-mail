//! TLS for POP3: TLS 1.2 or 1.3 with the platform's roots and the host
//! name checked, over TCP or over a connection that just answered STLS.

use std::sync::Arc;

use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use rustls_platform_verifier::BuilderVerifierExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::Pop3Error;

pub(crate) async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    host: &str,
    stream: S,
) -> Result<TlsStream<S>, Pop3Error> {
    let failed = |detail: String| Pop3Error::Tls { host: host.to_string(), detail };
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|err| failed(err.to_string()))?
        .with_platform_verifier()
        .map_err(|err| failed(err.to_string()))?
        .with_no_client_auth();
    let name = ServerName::try_from(host.to_string()).map_err(|_| failed(format!("{host} is not a host name")))?;
    TlsConnector::from(Arc::new(config))
        .connect(name, stream)
        .await
        .map_err(|err| failed(err.to_string()))
}
