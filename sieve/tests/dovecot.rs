//! ManageSieveClient against Dovecot's Pigeonhole: STARTTLS, PLAIN,
//! capabilities, PUTSCRIPT, SETACTIVE, GETSCRIPT, LISTSCRIPTS, a NO with
//! the server's words for a script it cannot compile, and a wrong password.
//!
//! One test, because it points `SSL_CERT_FILE` at a root made for the
//! run, and the environment belongs to the whole process.

use mailrs_sieve::client::{Login, ManageSieveApi, ManageSieveClient, SieveError};
use mailrs_testmail::{Certs, Dovecot, Profile};

const USER: &str = "me@example.test";

#[test]
fn the_client_keeps_a_script_on_dovecot() {
    if mailrs_testmail::docker().is_none() {
        return;
    }
    let Some(certs) = Certs::make() else { return };
    // SAFETY: the only test in this binary, before any thread starts.
    unsafe { mailrs_testmail::trust(&certs.root()) };
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("a runtime");
    runtime.block_on(async {
        let password = mailrs_testmail::password();
        let Some(dovecot) = Dovecot::start(&certs, Profile::Sieve, &password).await else { return };
        let port = dovecot.sieve.expect("the Sieve profile maps ManageSieve");
        let client = ManageSieveClient::new("localhost", port, Login::new(USER, &password));
        let caps = client.capabilities().await.expect("capabilities after STARTTLS");
        assert!(caps.sieve.usable() && caps.sieve.has("include") && caps.sieve.has("body"), "{caps:?}");
        client.put("penguin-mail", "require \"fileinto\";\nfileinto \"Archive\";\n").await.expect("PUTSCRIPT");
        client.activate("penguin-mail").await.expect("SETACTIVE");
        let listed = client.scripts().await.expect("LISTSCRIPTS");
        assert!(listed.iter().any(|l| l.name == "penguin-mail" && l.active), "{listed:?}");
        assert!(client.get("penguin-mail").await.expect("GETSCRIPT").contains("fileinto \"Archive\""));
        let refused = client.put("penguin-mail", "fileintoo \"x\";\n").await.expect_err("a script that does not compile");
        assert!(matches!(refused, SieveError::Refused(ref words) if !words.is_empty()), "{refused:?}");
        let wrong = ManageSieveClient::new("localhost", port, Login::new(USER, "wrong"));
        assert!(matches!(wrong.scripts().await, Err(SieveError::Auth(_))));
    });
}
