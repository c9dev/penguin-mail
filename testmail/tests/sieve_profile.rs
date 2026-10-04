//! The Sieve profile serves ManageSieve and runs the active script when a
//! message is delivered.

use mailrs_testmail::{Certs, Dovecot, Profile};
use tokio::io::AsyncReadExt;

#[test]
fn the_sieve_profile_greets_on_managesieve() {
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
        let Some(dovecot) = Dovecot::start(&certs, Profile::Sieve, &password).await else {
            return;
        };
        let port = dovecot.sieve.expect("the Sieve profile maps ManageSieve");
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connects");
        let mut greeting = vec![0u8; 512];
        let read = stream
            .read(&mut greeting)
            .await
            .expect("reads the greeting");
        let text = String::from_utf8_lossy(&greeting[..read]).to_string();
        assert!(text.contains("\"SIEVE\""), "{text}");
        assert!(text.contains("\"STARTTLS\""), "{text}");
        dovecot
            .deliver(
                "me@example.test",
                b"Subject: Hi\r\nFrom: a@example.test\r\n\r\nHi\r\n",
            )
            .await;
        assert_eq!(dovecot.messages("me@example.test", "INBOX").await.len(), 1);
    });
}
