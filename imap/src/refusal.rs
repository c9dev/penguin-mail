//! How a server's refusal reads as an [`ImapError`]. Servers put the
//! reason in free text, so the class comes from what the command was doing
//! and from words servers are known to use.

use crate::ImapError;

/// What a command was doing when the server refused it. The same `NO`
/// means a wrong password during login and a missing mailbox during SELECT.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Doing<'a> {
    Greeting,
    Login,
    /// Working in this mailbox: SELECT, RENAME, DELETE, CREATE.
    Mailbox(&'a str),
    /// Putting messages into this mailbox: COPY, MOVE, APPEND.
    Into(&'a str),
    Other,
}

/// How the server refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    No,
    Bad,
    Bye,
}

/// Words servers use when they refuse a connection over a per-user limit.
/// `[LIMIT]` is RFC 5530's code for any limit, so it counts only while
/// greeting or signing in, when the only limit in play is on connections.
/// Dovecot says "Maximum number of connections from user+IP exceeded";
/// others say "Too many connections" or "Too many simultaneous ...".
const LIMIT_WORDS: [&str; 5] = [
    "[limit]",
    "too many connections",
    "simultaneous",
    "maximum number of connections",
    "connection limit",
];

/// Words servers use when they lock a sign-in out after failed attempts.
/// That is a password problem: trying again only makes the lockout longer.
const FAILED_LOGIN_WORDS: [&str; 5] = [
    "failed login",
    "login failures",
    "failed attempts",
    "authentication failures",
    "too many failed",
];

/// Words servers use when IMAP is off for the account rather than the
/// password wrong.
const IMAP_OFF_WORDS: [&str; 8] = [
    "enable imap",
    "imap access",
    "imap is disabled",
    "imap disabled",
    "imap is not enabled",
    "imap not enabled",
    "not enabled for imap",
    "imap is turned off",
];

/// Words servers use for a mailbox that is not there. `[NONEXISTENT]` is
/// RFC 5530's code.
const MISSING_WORDS: [&str; 5] = [
    "[nonexistent]",
    "doesn't exist",
    "does not exist",
    "no such mailbox",
    "unknown mailbox",
];

/// The class of a refusal. `try_create` is the `[TRYCREATE]` code, which
/// imap-proto parses out of the text.
/// The server's text is clipped to [`MAX_ERROR_TEXT`] characters.
pub(crate) fn refusal(doing: Doing<'_>, how: Refusal, try_create: bool, text: &str) -> ImapError {
    let text = clipped(text.to_string());
    let lower = text.to_ascii_lowercase();
    let says = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    let signing_in = matches!(doing, Doing::Greeting | Doing::Login);
    if matches!(doing, Doing::Login) && how == Refusal::No && says(&FAILED_LOGIN_WORDS) {
        return ImapError::Auth { text };
    }
    if signing_in && says(&LIMIT_WORDS) {
        return ImapError::TooManyConnections { text };
    }
    match (how, doing) {
        (Refusal::Bye, _) => ImapError::Network(text),
        (Refusal::Bad, _) => ImapError::Protocol(text),
        (Refusal::No, Doing::Login) if says(&IMAP_OFF_WORDS) => ImapError::ImapDisabled { text },
        (Refusal::No, Doing::Login) => ImapError::Auth { text },
        (Refusal::No, Doing::Mailbox(name)) if says(&MISSING_WORDS) => {
            ImapError::NoMailbox(name.to_string())
        }
        (Refusal::No, Doing::Into(name)) if try_create || says(&MISSING_WORDS) => {
            ImapError::NoMailbox(name.to_string())
        }
        (Refusal::No, _) => ImapError::Refused(text),
    }
}

/// An error from async-imap, which only the login and IDLE paths call.
pub(crate) fn from_async_imap(err: async_imap::error::Error, doing: Doing<'_>) -> ImapError {
    use async_imap::error::Error;
    let error = match err {
        Error::Io(err) => from_io(err),
        Error::ConnectionLost => ImapError::Network("the server closed the connection".into()),
        Error::No(detail) => refusal(doing, Refusal::No, false, &server_words(&detail)),
        Error::Bad(detail) => refusal(doing, Refusal::Bad, false, &server_words(&detail)),
        other => ImapError::Protocol(clipped(other.to_string())),
    };
    match (doing, error) {
        // An answer to LOGIN or AUTHENTICATE that async-imap cannot parse
        // comes with the buffer it read, escaped twice and as a list of
        // byte values. A server that repeats the command puts the password
        // there in forms no search can find, so none of that text is kept.
        (Doing::Login, ImapError::Protocol(_)) => {
            ImapError::Protocol("the server's answer to the sign-in could not be read".into())
        }
        (_, error) => error,
    }
}

/// A read or write that failed under the IMAP layer. async-imap reports
/// an answer it cannot parse, or one too large to buffer, as an error of
/// kind `Other`; the network's own failures come with their own kinds.
pub(crate) fn from_io(err: std::io::Error) -> ImapError {
    let text = clipped(err.to_string());
    match err.kind() {
        std::io::ErrorKind::Other | std::io::ErrorKind::InvalidData => ImapError::Protocol(text),
        _ => ImapError::Network(text),
    }
}

/// The most of an error's text an [`ImapError`] keeps. async-imap puts
/// the whole unparsed buffer in a parse error, mail included, and error
/// text goes to the log.
pub(crate) const MAX_ERROR_TEXT: usize = 200;

pub(crate) fn clipped(mut text: String) -> String {
    if let Some((end, _)) = text.char_indices().nth(MAX_ERROR_TEXT) {
        text.truncate(end);
    }
    text
}

/// The server's own text inside async-imap's `No` and `Bad` errors, which
/// carry it as `code: Some(Alert), info: Some("text")`. Anything else
/// comes back whole.
pub(crate) fn server_words(detail: &str) -> String {
    const START: &str = "info: Some(\"";
    let Some(at) = detail.find(START) else {
        return detail.to_string();
    };
    let quoted = &detail[at + START.len()..];
    let quoted = quoted.strip_suffix("\")").unwrap_or(quoted);
    let mut words = String::with_capacity(quoted.len());
    let mut chars = quoted.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    words.push(next);
                }
            }
            c => words.push(c),
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ImapError;

    #[test]
    fn a_refused_login_is_an_auth_error_with_the_servers_words() {
        let err = refusal(
            Doing::Login,
            Refusal::No,
            false,
            "[AUTHENTICATIONFAILED] Invalid credentials (Failure)",
        );
        assert_eq!(
            err,
            ImapError::Auth {
                text: "[AUTHENTICATIONFAILED] Invalid credentials (Failure)".into()
            }
        );
    }

    #[test]
    fn a_login_refused_for_imap_being_off_says_so() {
        let err = refusal(
            Doing::Login,
            Refusal::No,
            false,
            "[UNAVAILABLE] IMAP access is disabled for your domain.",
        );
        assert!(matches!(err, ImapError::ImapDisabled { .. }));
    }

    #[test]
    fn a_connection_limit_reads_as_too_many_connections_while_greeting_or_signing_in() {
        for (doing, how, text) in [
            (
                Doing::Greeting,
                Refusal::Bye,
                "Maximum number of connections from user+IP exceeded",
            ),
            (
                Doing::Login,
                Refusal::No,
                "[LIMIT] Too many simultaneous connections",
            ),
            (Doing::Greeting, Refusal::Bye, "Too many connections"),
            (Doing::Login, Refusal::No, "Connection limit reached"),
        ] {
            assert!(
                matches!(
                    refusal(doing, how, false, text),
                    ImapError::TooManyConnections { .. }
                ),
                "{text}"
            );
        }
    }

    /// RFC 5530's `[LIMIT]` names any limit, such as flags in a mailbox.
    /// On a command after sign-in it is a refusal, not a reason to retry.
    #[test]
    fn a_limit_on_a_command_after_sign_in_is_a_refusal() {
        for doing in [
            Doing::Other,
            Doing::Mailbox("INBOX"),
            Doing::Into("Archive"),
        ] {
            assert_eq!(
                refusal(doing, Refusal::No, false, "[LIMIT] Too many keywords"),
                ImapError::Refused("[LIMIT] Too many keywords".into()),
            );
        }
    }

    /// Retrying a password the server locks out after failures only makes
    /// the lockout longer.
    #[test]
    fn too_many_failed_sign_ins_is_an_auth_error() {
        for text in [
            "Too many failed login attempts, try again later",
            "[LIMIT] Too many authentication failures",
        ] {
            assert!(
                matches!(
                    refusal(Doing::Login, Refusal::No, false, text),
                    ImapError::Auth { .. }
                ),
                "{text}"
            );
        }
        assert!(matches!(
            refusal(Doing::Greeting, Refusal::Bye, false, "Too many requests"),
            ImapError::Network(_)
        ));
    }

    /// A server's text goes to the log and the dialog. Whatever the class,
    /// an error keeps the first 200 characters of it.
    #[test]
    fn every_refusal_keeps_only_the_start_of_the_servers_text() {
        let long = "é".repeat(10_000);
        for (doing, how, try_create) in [
            (Doing::Greeting, Refusal::Bye, false),
            (Doing::Login, Refusal::No, false),
            (Doing::Other, Refusal::Bad, false),
            (Doing::Other, Refusal::No, false),
            (Doing::Into("Archive"), Refusal::No, true),
        ] {
            let err = refusal(doing, how, try_create, &format!("[LIMIT] {long}"));
            assert!(err.to_string().chars().count() < 300, "{doing:?} {how:?}");
        }
    }

    #[test]
    fn selecting_a_missing_mailbox_names_it() {
        let err = refusal(
            Doing::Mailbox("Archive"),
            Refusal::No,
            false,
            "[NONEXISTENT] Mailbox doesn't exist: Archive",
        );
        assert_eq!(err, ImapError::NoMailbox("Archive".into()));
    }

    #[test]
    fn trycreate_on_a_copy_names_the_destination() {
        let err = refusal(
            Doing::Into("Archive"),
            Refusal::No,
            true,
            "Mailbox doesn't exist",
        );
        assert_eq!(err, ImapError::NoMailbox("Archive".into()));
    }

    #[test]
    fn bad_is_a_protocol_error_and_bye_a_network_one() {
        assert!(matches!(
            refusal(
                Doing::Other,
                Refusal::Bad,
                false,
                "Invalid QRESYNC parameters"
            ),
            ImapError::Protocol(_)
        ));
        assert!(matches!(
            refusal(Doing::Other, Refusal::Bye, false, "Server shutting down"),
            ImapError::Network(_)
        ));
    }

    #[test]
    fn any_other_no_is_refused() {
        let err = refusal(
            Doing::Into("Sent"),
            Refusal::No,
            false,
            "[OVERQUOTA] Quota exceeded",
        );
        assert_eq!(err, ImapError::Refused("[OVERQUOTA] Quota exceeded".into()));
    }

    #[test]
    fn an_answer_async_imap_cannot_parse_is_a_protocol_error() {
        let parse = std::io::Error::other("Error(..) during parsing of \"* 1 FETCH\"");
        assert!(matches!(from_io(parse), ImapError::Protocol(_)));
        let reset = std::io::Error::from(std::io::ErrorKind::ConnectionReset);
        assert!(matches!(from_io(reset), ImapError::Network(_)));
    }

    #[test]
    fn server_words_come_out_of_async_imaps_debug_form() {
        let detail = format!(
            "code: {:?}, info: {:?}",
            None::<()>,
            Some("Say \"hi\" \\ bye")
        );
        assert_eq!(server_words(&detail), "Say \"hi\" \\ bye");
        assert_eq!(server_words("plain words"), "plain words");
    }

    /// async-imap puts the whole unparsed buffer in its parse error, mail
    /// included, and the error text goes to the log.
    #[test]
    fn an_io_error_keeps_only_the_start_of_its_text() {
        let text = format!("Error(..) during parsing of \"{}\"", "é".repeat(10_000));
        let ImapError::Protocol(kept) = from_io(std::io::Error::other(text)) else {
            panic!("not a protocol error");
        };
        assert_eq!(kept.chars().count(), 200);
    }
}
