//! POP3's lines (RFC 1939 section 3): a status line that starts `+OK` or
//! `-ERR`, and for some commands a multiline answer whose lines carry an
//! extra leading dot where they start with one, ended by a lone `.`.

use crate::Pop3Error;
use crate::client::{Capabilities, ListItem, Stat, Uidl};

/// A line of a multiline answer with the dot the server added taken off.
pub(crate) fn undot(line: &[u8]) -> &[u8] {
    match line.strip_prefix(b"..") {
        Some(_) => &line[1..],
        None => line,
    }
}

/// The text after `+OK`, or the error a `-ERR` line carries. RFC 2449's
/// response codes say whether the login or the mailbox lock refused.
pub(crate) fn status(line: &[u8]) -> Result<String, Pop3Error> {
    let text = String::from_utf8_lossy(line);
    if let Some(rest) = text.strip_prefix("+OK") {
        return Ok(rest.trim_start().to_string());
    }
    let Some(rest) = text.strip_prefix("-ERR") else {
        return Err(Pop3Error::Protocol(text.into_owned()));
    };
    let rest = rest.trim_start();
    if rest.starts_with("[AUTH]") {
        return Err(Pop3Error::Auth { text: rest.to_string() });
    }
    if rest.starts_with("[IN-USE]") {
        return Err(Pop3Error::InUse(rest.to_string()));
    }
    Err(Pop3Error::Refused(rest.to_string()))
}

/// `n uidl`, one line of a `UIDL` answer.
pub(crate) fn uidl_line(line: &str) -> Option<Uidl> {
    let (id, uidl) = line.trim().split_once(' ')?;
    let uidl = uidl.trim();
    if uidl.is_empty() {
        return None;
    }
    Some(Uidl { id: id.parse().ok()?, uidl: uidl.to_string() })
}

/// `n octets`, one line of a `LIST` answer.
pub(crate) fn list_line(line: &str) -> Option<ListItem> {
    let mut words = line.split_whitespace();
    Some(ListItem { id: words.next()?.parse().ok()?, octets: words.next()?.parse().ok()? })
}

/// `count octets`, the text of a `STAT` answer.
pub(crate) fn stat_text(text: &str) -> Option<Stat> {
    let mut words = text.split_whitespace();
    Some(Stat { count: words.next()?.parse().ok()?, octets: words.next()?.parse().ok()? })
}

/// What a `CAPA` answer's lines offer.
pub(crate) fn capabilities<S: AsRef<str>>(lines: &[S]) -> Capabilities {
    let mut caps = Capabilities::default();
    for line in lines {
        let mut words = line.as_ref().split_whitespace();
        match words.next().map(str::to_ascii_uppercase).as_deref() {
            Some("UIDL") => caps.uidl = true,
            Some("STLS") => caps.stls = true,
            Some("TOP") => caps.top = true,
            Some("SASL") => caps.sasl_plain = words.any(|w| w.eq_ignore_ascii_case("PLAIN")),
            _ => {}
        }
    }
    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_doubled_leading_dot_loses_one_dot() {
        assert_eq!(undot(b"..dot at the start"), b".dot at the start");
        assert_eq!(undot(b"plain"), b"plain");
        assert_eq!(undot(b"."), b".", "a lone dot ends the answer before undot sees it");
    }

    #[test]
    fn status_lines_read_as_the_server_meant_them() {
        assert_eq!(status(b"+OK 2 320"), Ok("2 320".to_string()));
        assert_eq!(status(b"+OK"), Ok(String::new()));
        assert_eq!(status(b"-ERR no such message"), Err(Pop3Error::Refused("no such message".into())));
        assert!(matches!(status(b"-ERR [AUTH] wrong password"), Err(Pop3Error::Auth { text }) if text.contains("wrong password")));
        assert!(matches!(status(b"-ERR [IN-USE] locked"), Err(Pop3Error::InUse(_))));
        assert!(matches!(status(b"* OK imap"), Err(Pop3Error::Protocol(_))));
    }

    #[test]
    fn listings_and_stat_parse_and_bad_lines_do_not() {
        assert_eq!(uidl_line("1 whqtswO00WBw418f9t5JxYwZ"), Some(Uidl { id: 1, uidl: "whqtswO00WBw418f9t5JxYwZ".into() }));
        assert_eq!(uidl_line("x abc"), None);
        assert_eq!(uidl_line("2"), None);
        assert_eq!(list_line("2 4096"), Some(ListItem { id: 2, octets: 4096 }));
        assert_eq!(stat_text("2 320"), Some(Stat { count: 2, octets: 320 }));
        assert_eq!(stat_text("two"), None);
    }

    #[test]
    fn capa_lines_name_what_the_server_offers() {
        let caps = capabilities(&["TOP", "UIDL", "SASL PLAIN LOGIN", "STLS", "USER"]);
        assert_eq!(caps, Capabilities { uidl: true, stls: true, sasl_plain: true, top: true });
        assert_eq!(capabilities(&["USER"]), Capabilities::default());
    }
}
