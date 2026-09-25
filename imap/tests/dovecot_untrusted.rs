//! `mailrs_imap::check` against a Dovecot whose certificate a root signed
//! that this process does not trust: the check stops at the certificate.
//! Its own binary, because it trusts a different root from `dovecot.rs`,
//! and the environment belongs to the whole process.

use mailrs_discover::{Security, Server, UserName};
use mailrs_imap::{CheckError, ImapError, check};
use mailrs_testmail::{Certs, Dovecot, Mailpit, Profile, Submission};

const ADDRESS: &str = "me@example.test";

#[test]
fn check_refuses_a_certificate_from_a_root_nobody_trusts() {
    if mailrs_testmail::docker().is_none() {
        return;
    }
    let Some(certs) = Certs::make() else {
        return;
    };
    // SAFETY: this is the binary's only test, and no runtime or client has
    // started a thread yet, so nothing else reads the environment.
    unsafe { mailrs_testmail::trust(&certs.stranger()) };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let password = mailrs_testmail::password();
        let Some(dovecot) = Dovecot::start(&certs, Profile::Full, &password).await else {
            return;
        };
        let Some(sink) = Mailpit::start(&certs, Submission::Tls, ADDRESS, &password).await else {
            return;
        };
        let at = |port| Server {
            host: "localhost".to_string(),
            port,
            security: Security::Tls,
            user_name: UserName::Address,
        };
        let refused = check(
            &at(dovecot.imaps),
            &at(sink.smtp),
            ADDRESS,
            ADDRESS,
            &password,
        )
        .await
        .expect_err("a certificate nobody vouches for");
        assert!(
            matches!(refused, CheckError::Imap(ImapError::Tls { .. })),
            "an unknown root reads as a certificate error: {refused:?}"
        );
    });
}
