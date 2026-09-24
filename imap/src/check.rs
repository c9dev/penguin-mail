//! Signing in to both servers before anything is saved, as the account
//! dialog does with what discovery found.
//!
//! The two sign-in loops are written out rather than shared through an
//! `AsyncFn` closure: with one, the future `check` returns stops being
//! `Send`, and the dialog runs it on a worker thread.

use mailrs_discover::{Server, UserName};

use crate::client::{Dial, TlsDial};
use crate::smtp::SmtpTls;
use crate::{Capabilities, ImapClient, ImapError, Login, SmtpClient};

/// What worked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    /// The user name the IMAP server took.
    pub imap_user: String,
    /// The user name the SMTP server took, which can differ.
    pub smtp_user: String,
    /// What the IMAP server offers after login.
    pub capabilities: Capabilities,
}

/// Which server turned the account away, and why.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CheckError {
    #[error("IMAP: {0}")]
    Imap(ImapError),
    #[error("SMTP: {0}")]
    Smtp(ImapError),
}

/// Signs in to `imap` and `smtp` with `password`, trying the user names
/// each server's rule allows for `address`, and returns what worked. IMAP
/// goes first, and SMTP is not tried when IMAP turns the account away.
pub async fn check(
    imap: &Server,
    smtp: &Server,
    address: &str,
    password: &str,
) -> Result<Checked, CheckError> {
    let smtp_dial = SmtpTls::new(smtp).map_err(CheckError::Smtp)?;
    check_with(
        (&TlsDial::new(imap.clone()), imap.user_name),
        (&smtp_dial, smtp.user_name),
        address,
        password,
    )
    .await
}

/// [`check`] with the dialers given, so a test can hand it scripted
/// servers.
async fn check_with<I, S>(
    imap: (&I, UserName),
    smtp: (&S, UserName),
    address: &str,
    password: &str,
) -> Result<Checked, CheckError>
where
    I: Dial + Clone,
    S: Dial + Clone,
    S::Stream: Sync,
{
    let (imap_user, capabilities) = sign_in_imap(imap, address, password)
        .await
        .map_err(CheckError::Imap)?;
    let smtp_user = sign_in_smtp(smtp, address, password)
        .await
        .map_err(CheckError::Smtp)?;
    Ok(Checked {
        imap_user,
        smtp_user,
        capabilities,
    })
}

async fn sign_in_imap<D: Dial + Clone>(
    (dial, rule): (&D, UserName),
    address: &str,
    password: &str,
) -> Result<(String, Capabilities), ImapError> {
    let mut refused = None;
    for user in user_names(rule, address) {
        match ImapClient::with_dial(dial.clone(), Login::new(user.as_str(), password))
            .capabilities()
            .await
        {
            Ok(capabilities) => return Ok((user, capabilities)),
            Err(err) => refused = Some(refusal_or_stop(err)?),
        }
    }
    Err(refused.unwrap_or(ImapError::Auth {
        text: String::new(),
    }))
}

async fn sign_in_smtp<D>(
    (dial, rule): (&D, UserName),
    address: &str,
    password: &str,
) -> Result<String, ImapError>
where
    D: Dial + Clone,
    D::Stream: Sync,
{
    let mut refused = None;
    for user in user_names(rule, address) {
        match SmtpClient::with_dial(dial.clone(), Login::new(user.as_str(), password))
            .check()
            .await
        {
            Ok(()) => return Ok(user),
            Err(err) => refused = Some(refusal_or_stop(err)?),
        }
    }
    Err(refused.unwrap_or(ImapError::Auth {
        text: String::new(),
    }))
}

/// A refused sign-in, kept to report if no other name works. Any other
/// failure ends the check, since the next name would meet it too.
fn refusal_or_stop(err: ImapError) -> Result<ImapError, ImapError> {
    match err {
        ImapError::Auth { .. } => Ok(err),
        other => Err(other),
    }
}

/// The user names to try for `address`, in order. RFC 6186 section 4:
/// the local part first where the provider says so, then the address.
pub fn user_names(rule: UserName, address: &str) -> Vec<String> {
    match (rule, address.split_once('@')) {
        (UserName::LocalPartFirst, Some((local, _))) if !local.is_empty() => {
            vec![local.to_string(), address.to_string()]
        }
        _ => vec![address.to_string()],
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, PoisonError};

    use mailrs_discover::{Security, Server, UserName};
    use tokio::io::DuplexStream;

    use super::{CheckError, check_with, refusal_or_stop, user_names};
    use crate::ImapError;
    use crate::client::Dial;
    use crate::testing::{SmtpSeen, pipe, server, smtp_pipe, smtp_server};

    #[test]
    fn local_part_first_tries_the_local_part_then_the_address() {
        assert_eq!(
            user_names(UserName::LocalPartFirst, "ann@me.com"),
            ["ann", "ann@me.com"]
        );
        assert_eq!(user_names(UserName::Address, "ann@me.com"), ["ann@me.com"]);
        assert_eq!(
            user_names(UserName::LocalPartFirst, "no-at-sign"),
            ["no-at-sign"]
        );
    }

    #[test]
    fn only_a_refused_sign_in_moves_on_to_the_next_name() {
        let refused = ImapError::Auth { text: "no".into() };
        assert_eq!(refusal_or_stop(refused.clone()), Ok(refused));
        let tls = ImapError::Tls {
            host: "imap.me.com".into(),
            detail: "bad".into(),
        };
        assert_eq!(refusal_or_stop(tls.clone()), Err(tls));
    }

    #[test]
    fn a_check_can_run_on_any_worker_thread() {
        fn sendable<T: Send>(_: T) {}
        let server = Server {
            host: "imap.example.com".into(),
            port: 993,
            security: Security::Tls,
            user_name: UserName::Address,
        };
        sendable(super::check(&server, &server, "ann@example.com", "pw"));
    }

    /// Dials a new scripted server each time, built by `make`, and counts
    /// the dials.
    #[derive(Clone)]
    struct Servers {
        make: fn() -> DuplexStream,
        dials: Arc<Mutex<usize>>,
    }

    impl Servers {
        fn new(make: fn() -> DuplexStream) -> Self {
            Servers {
                make,
                dials: Arc::default(),
            }
        }

        fn dials(&self) -> usize {
            *self.dials.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }

    impl Dial for Servers {
        type Stream = DuplexStream;

        async fn dial(&self) -> Result<(DuplexStream, bool), ImapError> {
            *self.dials.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            Ok(((self.make)(), false))
        }
    }

    /// An IMAP server that takes the password only with `user`.
    fn imap_taking(user: &'static str) -> DuplexStream {
        let mut rest = server("IDLE MOVE", Arc::default(), |_| vec!["{tag} OK".into()]);
        pipe(
            "* OK [CAPABILITY IMAP4rev1] ready",
            move |command| match command.strip_prefix("LOGIN ") {
                Some(args) if !args.starts_with(&format!("\"{user}\"")) => {
                    vec!["{tag} NO [AUTHENTICATIONFAILED] Invalid credentials".into()]
                }
                _ => rest(command),
            },
        )
    }

    /// An SMTP server that takes the password only with `user`.
    fn smtp_taking(user: &'static str) -> DuplexStream {
        let taken = base64_plain(user);
        smtp_pipe(
            "220 ready",
            SmtpSeen::default(),
            move |command| match command.strip_prefix("AUTH PLAIN ") {
                Some(given) if given != taken => vec!["535 5.7.8 Bad credentials".into()],
                _ => smtp_server(command),
            },
        )
    }

    fn base64_plain(user: &str) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(format!("\0{user}\0pw"))
    }

    #[tokio::test]
    async fn each_server_keeps_the_first_user_name_it_takes() {
        let imap = Servers::new(|| imap_taking("ann"));
        let smtp = Servers::new(|| smtp_taking("ann@me.com"));
        let checked = check_with(
            (&imap, UserName::LocalPartFirst),
            (&smtp, UserName::LocalPartFirst),
            "ann@me.com",
            "pw",
        )
        .await
        .unwrap();
        assert_eq!(checked.imap_user, "ann");
        assert_eq!(checked.smtp_user, "ann@me.com");
        assert!(checked.capabilities.idle && checked.capabilities.moves);
        assert_eq!((imap.dials(), smtp.dials()), (1, 2));
    }

    #[tokio::test]
    async fn a_password_every_name_refuses_is_an_imap_auth_error() {
        let imap = Servers::new(|| imap_taking("nobody"));
        let smtp = Servers::new(|| smtp_taking("ann@me.com"));
        let err = check_with(
            (&imap, UserName::LocalPartFirst),
            (&smtp, UserName::Address),
            "ann@me.com",
            "pw",
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, CheckError::Imap(ImapError::Auth { .. })),
            "{err:?}"
        );
        assert_eq!((imap.dials(), smtp.dials()), (2, 0));
    }

    /// A server without STARTTLS fails every user name the same way, so
    /// the check stops at the first.
    #[tokio::test]
    async fn a_failure_other_than_a_refusal_ends_the_check_at_once() {
        fn no_auth() -> DuplexStream {
            smtp_pipe("220 ready", SmtpSeen::default(), |command| match command {
                c if c.starts_with("EHLO") => vec!["250 SIZE 1000".into()],
                other => smtp_server(other),
            })
        }
        let imap = Servers::new(|| imap_taking("ann"));
        let smtp = Servers::new(no_auth);
        let err = check_with(
            (&imap, UserName::LocalPartFirst),
            (&smtp, UserName::LocalPartFirst),
            "ann@me.com",
            "pw",
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, CheckError::Smtp(ImapError::Unsupported(_))),
            "{err:?}"
        );
        assert_eq!(smtp.dials(), 1);
    }
}
