//! The words of Preferences' servers group for one IMAP account: where
//! its calendar, contacts and rules are, a server that waits for the
//! person's yes, and a server that refused the login.

use mailrs_domain::translate::{fill, gettext};
use mailrs_store::services::{FoundService, ServiceKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerLine {
    pub title: String,
    pub subtitle: String,
    /// The row offers "Use It" for this server, found outside the
    /// address's domain.
    pub ask: Option<ServiceKind>,
}

fn host(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| url.to_string())
}

fn line(title: String, found: Option<&FoundService>, refused: Option<&str>) -> ServerLine {
    let (subtitle, ask) = match (found, refused) {
        (None, _) => (gettext("Not found"), None),
        (Some(f), _) if !f.confirmed => (fill(&gettext("Found at {host}. Use it?"), &[("host", &host(&f.url))]), Some(f.kind)),
        (Some(f), Some(why)) => (fill(&gettext("{host} refused the login: {reason}"), &[("host", &host(&f.url)), ("reason", why)]), None),
        (Some(f), None) => (host(&f.url), None),
    };
    ServerLine { title, subtitle, ask }
}

pub fn lines(found: &[FoundService], refused_calendar: Option<&str>, refused_contacts: Option<&str>, rules_here: bool) -> Vec<ServerLine> {
    let of = |kind| found.iter().find(|f| f.kind == kind);
    vec![
        line(gettext("Calendar"), of(ServiceKind::CalDav), refused_calendar),
        line(gettext("Contacts"), of(ServiceKind::CardDav), refused_contacts),
        ServerLine {
            title: gettext("Rules"),
            subtitle: match rules_here {
                true => gettext("On this computer, while Penguin Mail is open"),
                false => gettext("On the server"),
            },
            ask: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use mailrs_store::services::{FoundService, ServiceKind};

    use super::*;

    fn row(kind: ServiceKind, url: &str, confirmed: bool) -> FoundService {
        FoundService { kind, url: url.into(), user_name: "me".into(), confirmed, source: "srv".into() }
    }

    #[test]
    fn found_servers_read_as_their_hosts() {
        let found = [row(ServiceKind::CalDav, "https://caldav.fastmail.com/", true), row(ServiceKind::CardDav, "https://carddav.fastmail.com/", true)];
        let lines = lines(&found, None, None, true);
        assert_eq!(lines[0].title, "Calendar");
        assert_eq!(lines[0].subtitle, "caldav.fastmail.com");
        assert_eq!(lines[1].subtitle, "carddav.fastmail.com");
        assert_eq!(lines[2].title, "Rules");
        assert_eq!(lines[2].subtitle, "On this computer, while Penguin Mail is open");
    }

    #[test]
    fn a_server_waiting_for_a_yes_asks() {
        let lines = lines(&[row(ServiceKind::CalDav, "https://dav.hoster.net/cal/", false)], None, None, true);
        assert_eq!(lines[0].subtitle, "Found at dav.hoster.net. Use it?");
        assert_eq!(lines[0].ask, Some(ServiceKind::CalDav));
    }

    #[test]
    fn a_refused_login_says_so_and_nothing_found_says_that() {
        let lines = lines(&[row(ServiceKind::CalDav, "https://dav.example.org/", true)], Some("the server refused the user name or password"), None, false);
        assert_eq!(lines[0].subtitle, "dav.example.org refused the login: the server refused the user name or password");
        assert_eq!(lines[1].subtitle, "Not found");
        assert_eq!(lines[2].subtitle, "On the server");
    }
}
