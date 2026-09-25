//! `mailrs_imap::check` against Dovecot and an SMTP sink in Docker, over
//! TLS and over STARTTLS, with the certificate checks the app makes for a
//! person.
//!
//! One test, because it points `SSL_CERT_FILE` at a root made for the run,
//! and the environment belongs to the whole process. The wrong root has
//! its own binary, `dovecot_untrusted.rs`, for the same reason.

use mailrs_discover::{Security, Server, UserName};
use mailrs_imap::{CheckError, ImapError, check};
use mailrs_testmail::{Certs, Dovecot, Mailpit, Profile, Submission};

const ADDRESS: &str = "me@example.test";

fn server(host: &str, port: u16, security: Security) -> Server {
    Server {
        host: host.to_string(),
        port,
        security,
        user_name: UserName::Address,
    }
}

#[test]
fn check_signs_in_to_real_servers_and_refuses_a_certificate_for_another_name() {
    if mailrs_testmail::docker().is_none() {
        return;
    }
    let Some(certs) = Certs::make() else {
        return;
    };
    // SAFETY: this is the binary's only test, and no runtime or client has
    // started a thread yet, so nothing else reads the environment.
    unsafe { mailrs_testmail::trust(&certs.root()) };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let password = mailrs_testmail::password();
        let Some(dovecot) = Dovecot::start(&certs, Profile::Full, &password).await else {
            return;
        };
        let Some(tls) = Mailpit::start(&certs, Submission::Tls, ADDRESS, &password).await else {
            return;
        };
        let Some(starttls) = Mailpit::start(&certs, Submission::StartTls, ADDRESS, &password).await
        else {
            return;
        };

        let imaps = server("localhost", dovecot.imaps, Security::Tls);
        let smtps = server("localhost", tls.smtp, Security::Tls);
        check(&imaps, &smtps, ADDRESS, ADDRESS, &password)
            .await
            .expect("both servers take the password over TLS");

        let imap = server("localhost", dovecot.imap, Security::StartTls);
        let submission = server("localhost", starttls.smtp, Security::StartTls);
        check(&imap, &submission, ADDRESS, ADDRESS, &password)
            .await
            .expect("both servers take the password after STARTTLS");

        let refused = check(&imaps, &smtps, ADDRESS, ADDRESS, "not-the-password")
            .await
            .expect_err("a wrong password");
        // check signs in to IMAP first, so the refusal comes from there.
        assert!(
            matches!(refused, CheckError::Imap(ImapError::Auth { .. })),
            "a wrong password reads as one: {refused:?}"
        );

        // The certificate names localhost alone, so the same server reached
        // by its address has to fail the host name check.
        let by_address = server("127.0.0.1", dovecot.imaps, Security::Tls);
        let mismatch = check(&by_address, &smtps, ADDRESS, ADDRESS, &password)
            .await
            .expect_err("a certificate for another name");
        assert!(
            matches!(mismatch, CheckError::Imap(ImapError::Tls { .. })),
            "a name mismatch reads as a certificate error: {mismatch:?}"
        );
    });
}
