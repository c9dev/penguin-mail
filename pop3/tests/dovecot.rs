//! Pop3Client against Dovecot: TLS from the first byte and STLS, SASL
//! PLAIN, UIDL, RETR with a byte-stuffed line, DELE taking effect at QUIT,
//! and a wrong password.
//!
//! One test, because it points `SSL_CERT_FILE` at a root made for the
//! run, and the environment belongs to the whole process.

use mailrs_discover::{Security, Server, UserName};
use mailrs_pop3::{Login, Pop3Api, Pop3Client, Pop3Error};
use mailrs_testmail::{Certs, Dovecot, Profile};

const USER: &str = "me@example.test";

/// Waits for the server's `+OK` greeting on a plain connection. The test
/// servers start on the IMAP greeting alone, so the POP3 ports are
/// checked here, and a silent one fails with Dovecot's own log.
async fn expect_greeting(port: u16, container: &str) {
    use tokio::io::AsyncReadExt;
    let mut last = String::from("no connection");
    for _ in 0..50 {
        match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            Ok(mut stream) => {
                let mut greeting = [0u8; 3];
                match tokio::time::timeout(std::time::Duration::from_secs(2), stream.read_exact(&mut greeting)).await {
                    Ok(Ok(_)) if &greeting == b"+OK" => return,
                    other => last = format!("read {other:?}, bytes {greeting:?}"),
                }
            }
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let log = std::process::Command::new("docker").args(["logs", container]).output().expect("docker logs");
    panic!(
        "no POP3 greeting on port {port} ({last}). Dovecot said:\n{}{}",
        String::from_utf8_lossy(&log.stdout),
        String::from_utf8_lossy(&log.stderr)
    );
}

fn at(port: u16, security: Security) -> Server {
    Server { host: "localhost".into(), port, security, user_name: UserName::Address }
}

#[test]
fn the_client_downloads_and_deletes_on_dovecot() {
    if mailrs_testmail::docker().is_none() {
        return;
    }
    let Some(certs) = Certs::make() else { return };
    // SAFETY: the only test in this binary, before any thread starts.
    unsafe { mailrs_testmail::trust(&certs.root()) };
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("a runtime");
    runtime.block_on(async {
        let password = mailrs_testmail::password();
        let Some(dovecot) = Dovecot::start(&certs, Profile::Full, &password).await else { return };
        let (pop3s, pop3) = (dovecot.pop3s.expect("POP3 over TLS"), dovecot.pop3.expect("POP3 with STLS"));
        expect_greeting(pop3, dovecot.id()).await;
        dovecot
            .save(USER, "INBOX", b"From: a@example.test\r\nSubject: one\r\nMessage-ID: <p1@example.test>\r\n\r\n.a line that starts with a dot\r\n", None)
            .await;
        dovecot.save(USER, "INBOX", b"From: a@example.test\r\nSubject: two\r\n\r\nbody\r\n", None).await;

        let tls = Pop3Client::new(&at(pop3s, Security::Tls), Login::new(USER, &password));
        let caps = tls.connect().await.expect("TLS from the first byte");
        assert!(caps.uidl && caps.sasl_plain, "{caps:?}");
        let listed = tls.uidl().await.expect("UIDL").messages;
        assert_eq!(listed.len(), 2);
        let raw = tls.retr(listed[0].id, 0).await.expect("RETR");
        assert!(String::from_utf8_lossy(&raw).contains("\r\n.a line that starts with a dot\r\n"), "the dot comes back undone");
        tls.dele(listed[0].id).await.expect("DELE");
        tls.quit().await.expect("QUIT");

        let stls = Pop3Client::new(&at(pop3, Security::StartTls), Login::new(USER, &password));
        stls.connect().await.expect("STLS, then the sign-in");
        let left = stls.uidl().await.expect("UIDL").messages;
        assert_eq!(left.len(), 1, "the DELE took effect at QUIT");
        assert_ne!(left[0].uidl, listed[0].uidl);
        stls.quit().await.expect("QUIT");

        let wrong = Pop3Client::new(&at(pop3s, Security::Tls), Login::new(USER, "wrong"));
        assert!(matches!(wrong.connect().await, Err(Pop3Error::Auth { .. })));
    });
}
