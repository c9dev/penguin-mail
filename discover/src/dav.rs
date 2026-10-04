//! Where an account's CalDAV, CardDAV and ManageSieve servers may be: the
//! provider table first, then RFC 6764's SRV and TXT records on the
//! address's domain, then the well-known URLs on the domain and on the
//! IMAP host. These are hints. Whether a server takes the account's login
//! is for the DAV and ManageSieve clients to find out, with the password,
//! and only the domain goes out here.

use crate::name::{domain_of, host, is_within};
use crate::table::services_of;
use crate::{Net, Source, SrvRecord};

/// Places to try, best first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DavHints {
    pub caldav: Vec<Hint>,
    pub carddav: Vec<Hint>,
    pub sieve: Vec<SieveHint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    pub url: String,
    pub source: Source,
    /// The host is outside the address's domain and did not come from the
    /// table, so the person says yes before the password goes there.
    pub confirm: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SieveHint {
    pub host: String,
    pub port: u16,
    pub source: Source,
    pub confirm: bool,
}

/// ManageSieve's port (RFC 5804).
const SIEVE_PORT: u16 = 4190;

pub async fn dav_hints<N: Net>(
    net: &N,
    address: &str,
    // The table is read by IMAP host now, which tells GMX's families and
    // Zoho's data centers apart; the name stays for the callers' sake.
    _provider_name: &str,
    imap_host: &str,
) -> DavHints {
    let listed = services_of(imap_host);
    let mut hints = DavHints::default();
    let table = |url: Option<String>| {
        url.map(|url| Hint {
            url,
            source: Source::Table,
            confirm: false,
        })
    };
    hints.caldav.extend(table(listed.caldav));
    hints.carddav.extend(table(listed.carddav));
    if let Some((host, port)) = listed.sieve {
        hints.sieve.push(SieveHint {
            host,
            port,
            source: Source::Table,
            confirm: false,
        });
    }
    let imap_host = host(imap_host);
    let Some(domain) = domain_of(address) else {
        return hints;
    };
    if hints.caldav.is_empty() {
        hints.caldav = rfc6764(net, &domain, imap_host.as_deref(), "caldav").await;
    }
    if hints.carddav.is_empty() {
        hints.carddav = rfc6764(net, &domain, imap_host.as_deref(), "carddav").await;
    }
    if hints.sieve.is_empty()
        && let Some(imap) = imap_host
    {
        let confirm = !is_within(&imap, &domain);
        hints.sieve.push(SieveHint {
            host: imap,
            port: SIEVE_PORT,
            source: Source::Probe,
            confirm,
        });
    }
    hints
}

/// RFC 6764 for `service` (`caldav` or `carddav`): SRV `_<service>s._tcp`
/// with TXT `path=`, then `/.well-known/<service>` on the domain and on
/// the IMAP host. Plain `_caldav._tcp` is never asked: it may name a
/// server without TLS.
async fn rfc6764<N: Net>(
    net: &N,
    domain: &str,
    imap_host: Option<&str>,
    service: &str,
) -> Vec<Hint> {
    let name = format!("_{service}s._tcp.{domain}");
    let (records, txt) = tokio::join!(net.srv(&name), net.txt(&name));
    let mut hints = Vec::new();
    if let Some(found) = from_srv(records, &txt, domain) {
        hints.push(found);
    }
    for at in std::iter::once(domain).chain(imap_host) {
        let url = format!("https://{at}/.well-known/{service}");
        if let Some(target) = net.locate(&url).await {
            let confirm = url::Url::parse(&target)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .is_none_or(|h| !is_within(&h, domain) && Some(h.as_str()) != imap_host);
            hints.push(Hint {
                url: target,
                source: Source::WellKnown,
                confirm,
            });
        }
    }
    hints
}

fn from_srv(mut records: Vec<SrvRecord>, txt: &[String], domain: &str) -> Option<Hint> {
    // A target of "." says the domain offers no such service.
    if records
        .iter()
        .any(|r| r.target.trim_end_matches('.').is_empty())
    {
        return None;
    }
    records.sort_by(|a, b| a.priority.cmp(&b.priority).then(b.weight.cmp(&a.weight)));
    let best = records.into_iter().find(|r| r.port != 0)?;
    let target = host(&best.target)?;
    let path = txt
        .iter()
        .find_map(|t| t.strip_prefix("path="))
        .map(|p| format!("/{}", p.trim_start_matches('/')))
        .unwrap_or_else(|| "/".to_string());
    let port = if best.port == 443 {
        String::new()
    } else {
        format!(":{}", best.port)
    };
    Some(Hint {
        url: format!("https://{target}{port}{path}"),
        source: Source::Srv,
        confirm: !is_within(&target, domain),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeNet, Request};

    #[tokio::test]
    async fn a_listed_provider_answers_from_the_table_and_asks_nobody() {
        let net = FakeNet::default();
        let hints = dav_hints(&net, "me@fastmail.com", "Fastmail", "imap.fastmail.com").await;
        assert_eq!(hints.caldav[0].url, "https://caldav.fastmail.com/");
        assert_eq!(hints.carddav[0].url, "https://carddav.fastmail.com/");
        assert_eq!(hints.caldav[0].source, Source::Table);
        assert!(!hints.caldav[0].confirm);
        assert!(
            hints.sieve.iter().all(|s| s.source != Source::Table),
            "Fastmail runs no ManageSieve"
        );
        assert!(
            net.requests()
                .iter()
                .all(|r| !matches!(r, Request::Srv(_) | Request::Locate(_)))
        );
    }

    #[tokio::test]
    async fn a_custom_domain_finds_its_servers_by_srv_and_txt() {
        let net = FakeNet::default()
            .answer_srv(
                "_caldavs._tcp.example.org",
                vec![SrvRecord {
                    priority: 0,
                    weight: 0,
                    port: 443,
                    target: "dav.example.org.".into(),
                }],
            )
            .answer_txt("_caldavs._tcp.example.org", &["path=/dav/cal/"]);
        let hints = dav_hints(&net, "me@example.org", "example.org", "mail.example.org").await;
        assert_eq!(hints.caldav[0].url, "https://dav.example.org/dav/cal/");
        assert_eq!(hints.caldav[0].source, Source::Srv);
        assert!(!hints.caldav[0].confirm);
    }

    #[tokio::test]
    async fn an_srv_target_outside_the_domain_needs_a_yes() {
        let net = FakeNet::default().answer_srv(
            "_carddavs._tcp.example.org",
            vec![SrvRecord {
                priority: 0,
                weight: 0,
                port: 443,
                target: "dav.hoster.net.".into(),
            }],
        );
        let hints = dav_hints(&net, "me@example.org", "example.org", "mail.example.org").await;
        assert!(hints.carddav[0].confirm);
    }

    #[tokio::test]
    async fn the_well_known_urls_follow_their_redirect_on_the_domain_and_the_imap_host() {
        let net = FakeNet::default()
            .locate_at(
                "https://example.org/.well-known/caldav",
                "https://example.org/remote.php/dav/",
            )
            .locate_at(
                "https://mail.example.org/.well-known/carddav",
                "https://mail.example.org/dav/",
            );
        let hints = dav_hints(&net, "me@example.org", "example.org", "mail.example.org").await;
        assert_eq!(hints.caldav[0].url, "https://example.org/remote.php/dav/");
        assert_eq!(hints.caldav[0].source, Source::WellKnown);
        assert_eq!(hints.carddav[0].url, "https://mail.example.org/dav/");
    }

    #[tokio::test]
    async fn only_the_domain_goes_out_and_nothing_above_it() {
        let net = FakeNet::default();
        dav_hints(
            &net,
            "me@dept.example.org",
            "dept.example.org",
            "imap.example.org",
        )
        .await;
        for request in net.requests() {
            let name = request.name();
            assert!(!name.contains("me@"), "the address went out in {name}");
            assert!(
                name.contains("dept.example.org") || name.contains("imap.example.org"),
                "{name} is neither the domain nor the IMAP host"
            );
        }
    }

    #[tokio::test]
    async fn sieve_is_tried_on_4190_of_the_imap_host_after_the_table() {
        let net = FakeNet::default();
        let hints = dav_hints(&net, "me@mailbox.org", "mailbox.org", "imap.mailbox.org").await;
        assert_eq!(hints.sieve[0].host, "imap.mailbox.org");
        assert_eq!(hints.sieve[0].source, Source::Table);
        let custom = dav_hints(&net, "me@example.org", "example.org", "mail.example.org").await;
        assert_eq!(
            (custom.sieve[0].host.as_str(), custom.sieve[0].port),
            ("mail.example.org", 4190)
        );
        assert_eq!(custom.sieve[0].source, Source::Probe);
    }
}
