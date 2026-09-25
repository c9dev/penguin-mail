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
/// each server's rule allows for its login (`imap_login`, `smtp_login`),
/// and returns what worked. The two logins are the address unless the
/// person typed a user name for that server. IMAP goes first, and SMTP is
/// not tried when IMAP turns the account away.
pub async fn check(
    imap: &Server,
    smtp: &Server,
    imap_login: &str,
    smtp_login: &str,
    password: &str,
) -> Result<Checked, CheckError> {
    let smtp_dial = SmtpTls::new(smtp).map_err(CheckError::Smtp)?;
    check_with(
        (&TlsDial::new(imap.clone()), imap.user_name, imap_login),
        (&smtp_dial, smtp.user_name, smtp_login),
        password,
    )
    .await
}

/// [`check`] with the dialers given, so a test can hand it scripted
/// servers.
async fn check_with<I, S>(
    (imap, imap_rule, imap_login): (&I, UserName, &str),
    (smtp, smtp_rule, smtp_login): (&S, UserName, &str),
    password: &str,
) -> Result<Checked, CheckError>
where
    I: Dial + Clone,
    S: Dial + Clone,
    S::Stream: Sync,
{
    let (imap_user, capabilities) = sign_in_imap((imap, imap_rule), imap_login, password)
        .await
        .map_err(CheckError::Imap)?;
    let smtp_user = sign_in_smtp((smtp, smtp_rule), &imap_user, smtp_login, password)
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
        let login = Login::new(user.as_str(), password);
        match ImapClient::with_dial(dial.clone(), login.clone())
            .capabilities()
            .await
        {
            Ok(capabilities) => return Ok((user, capabilities)),
            Err(err) => refused = Some(refusal_or_stop(err.hidden(&login))?),
        }
    }
    Err(refused.unwrap_or(ImapError::Auth {
        text: String::new(),
    }))
}

/// Signs in to SMTP, trying `imap_user` first when the rule allows it:
/// a provider that counts failed sign-ins should not see one for a name
/// its IMAP server has just taken.
async fn sign_in_smtp<D>(
    (dial, rule): (&D, UserName),
    imap_user: &str,
    address: &str,
    password: &str,
) -> Result<String, ImapError>
where
    D: Dial + Clone,
    D::Stream: Sync,
{
    let mut names = user_names(rule, address);
    if let Some(at) = names.iter().position(|name| name == imap_user) {
        names[..=at].rotate_right(1);
    }
    let mut refused = None;
    for user in names {
        let login = Login::new(user.as_str(), password);
        match SmtpClient::with_dial(dial.clone(), login.clone())
            .check()
            .await
        {
            Ok(()) => return Ok(user),
            Err(err) => refused = Some(refusal_or_stop(err.hidden(&login))?),
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

    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};

    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    use super::{CheckError, check_with, refusal_or_stop, sign_in_imap, user_names};
    use crate::ImapError;
    use crate::client::Dial;
    use crate::testing::{SmtpSeen, forms, pipe, server, smtp_pipe, smtp_server};

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
        sendable(super::check(
            &server,
            &server,
            "ann@example.com",
            "ann@example.com",
            "pw",
        ));
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
            (&imap, UserName::LocalPartFirst, "ann@me.com"),
            (&smtp, UserName::LocalPartFirst, "ann@me.com"),
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
            (&imap, UserName::LocalPartFirst, "ann@me.com"),
            (&smtp, UserName::Address, "ann@me.com"),
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
            (&imap, UserName::LocalPartFirst, "ann@me.com"),
            (&smtp, UserName::LocalPartFirst, "ann@me.com"),
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

    /// Passwords with a quote and a backslash, so each escaped form
    /// differs from the typed one. imap-proto cannot parse a refusal that
    /// repeats the second one, whose letter is outside ASCII.
    const ECHOED: [&str; 2] = ["p\"ss\\word 1", "pä\"ss\\word 1"];

    /// An IMAP server that refuses every sign-in and repeats the command
    /// it was sent, password included.
    fn imap_echoing(greeting: &'static str) -> DuplexStream {
        let mut rest = server("IDLE", Arc::default(), |_| vec!["{tag} OK".into()]);
        pipe(greeting, move |command| {
            if command.starts_with("LOGIN ") || command.starts_with("AUTHENTICATE PLAIN ") {
                return vec![format!(
                    "{{tag}} NO [AUTHENTICATIONFAILED] you sent {command}"
                )];
            }
            rest(command)
        })
    }

    #[tokio::test]
    async fn an_imap_server_that_echoes_the_password_never_gets_it_into_the_error() {
        for password in ECHOED {
            let by_login = Servers::new(|| imap_echoing("* OK [CAPABILITY IMAP4rev1] ready"));
            let by_plain =
                Servers::new(|| imap_echoing("* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN] ready"));
            for imap in [by_login, by_plain] {
                let smtp = Servers::new(|| smtp_taking("ann@me.com"));
                let err = check_with(
                    (&imap, UserName::LocalPartFirst, "ann@me.com"),
                    (&smtp, UserName::Address, "ann@me.com"),
                    password,
                )
                .await
                .unwrap_err();
                let printed = format!("{err} {err:?}");
                assert!(matches!(err, CheckError::Imap(_)), "{printed}");
                if password.is_ascii() {
                    assert!(printed.contains("<hidden>"), "{printed}");
                }
                for user in ["ann", "ann@me.com"] {
                    for form in forms(user, password) {
                        assert!(!printed.contains(&form), "{form}: {printed}");
                    }
                }
                assert!(!printed.contains("ss\\"), "{printed}");
            }
        }
    }

    /// A provider that counts failed sign-ins should not see one for a
    /// name the IMAP server has just taken.
    #[tokio::test]
    async fn smtp_tries_the_name_imap_took_first() {
        let imap = Servers::new(|| imap_taking("ann@me.com"));
        let smtp = Servers::new(|| smtp_taking("ann@me.com"));
        let checked = check_with(
            (&imap, UserName::Address, "ann@me.com"),
            (&smtp, UserName::LocalPartFirst, "ann@me.com"),
            "pw",
        )
        .await
        .unwrap();
        assert_eq!(checked.smtp_user, "ann@me.com");
        assert_eq!(smtp.dials(), 1);
    }

    /// A user name typed for each server in Server Settings goes to that
    /// server alone.
    #[tokio::test]
    async fn each_server_signs_in_with_the_name_typed_for_it() {
        let imap = Servers::new(|| imap_taking("d.santos"));
        let smtp = Servers::new(|| smtp_taking("dana@example.org"));
        let checked = check_with(
            (&imap, UserName::Address, "d.santos"),
            (&smtp, UserName::Address, "dana@example.org"),
            "pw",
        )
        .await
        .unwrap();
        assert_eq!(checked.imap_user, "d.santos");
        assert_eq!(checked.smtp_user, "dana@example.org");
        assert_eq!((imap.dials(), smtp.dials()), (1, 1));
    }

    /// A stream that counts itself while it lives.
    #[derive(Debug)]
    struct Tracked {
        stream: DuplexStream,
        live: Arc<AtomicUsize>,
    }

    impl Drop for Tracked {
        fn drop(&mut self) {
            self.live.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl AsyncRead for Tracked {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for Tracked {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(&mut self.get_mut().stream).poll_write(cx, buf)
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.get_mut().stream).poll_flush(cx)
        }

        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
        }
    }

    /// Dials servers that take only the full address, and counts the
    /// connections made and those still alive.
    #[derive(Clone, Default)]
    struct Counted {
        made: Arc<AtomicUsize>,
        live: Arc<AtomicUsize>,
    }

    impl Dial for Counted {
        type Stream = Tracked;

        async fn dial(&self) -> Result<(Tracked, bool), ImapError> {
            self.made.fetch_add(1, Ordering::SeqCst);
            self.live.fetch_add(1, Ordering::SeqCst);
            let stream = imap_taking("ann@me.com");
            Ok((
                Tracked {
                    stream,
                    live: self.live.clone(),
                },
                false,
            ))
        }
    }

    /// The client that signed in keeps its connection in its pool, and no
    /// task of its own holds one, so dropping it closes every connection.
    #[tokio::test]
    async fn the_imap_sign_in_leaves_no_connection_open() {
        let dial = Counted::default();
        let (user, _) = sign_in_imap((&dial, UserName::LocalPartFirst), "ann@me.com", "pw")
            .await
            .unwrap();
        assert_eq!(user, "ann@me.com");
        assert_eq!(dial.made.load(Ordering::SeqCst), 2);
        assert_eq!(dial.live.load(Ordering::SeqCst), 0);
    }
}
