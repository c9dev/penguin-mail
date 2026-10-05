//! The Full profile serves POP3 with STLS and over TLS from the first byte.

use mailrs_testmail::{Certs, Dovecot, Profile};
use tokio::io::AsyncReadExt;

/// Whether a POP3 server on `port` greets with `+OK`.
async fn pop3_greets(port: u16) -> bool {
    let Ok(mut stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await else {
        return false;
    };
    let mut greeting = [0u8; 4];
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_exact(&mut greeting),
    )
    .await;
    matches!(read, Ok(Ok(_))) && &greeting == b"+OK "
}

#[test]
fn the_full_profile_greets_on_pop3() {
    if mailrs_testmail::docker().is_none() {
        return;
    }
    let Some(certs) = Certs::make() else { return };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let password = mailrs_testmail::password();
        let Some(dovecot) = Dovecot::start(&certs, Profile::Full, &password).await else {
            return;
        };
        let plain = dovecot.pop3.expect("the Full profile maps POP3");
        assert!(
            dovecot.pop3s.is_some(),
            "the Full profile maps POP3 over TLS"
        );
        assert!(
            pop3_greets(plain).await,
            "POP3 with STLS greets in the clear"
        );
    });
}
