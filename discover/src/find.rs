//! Discovery's steps, run side by side, and the rule that picks one.

use std::pin::pin;
use std::time::Duration;

use futures::FutureExt;

use crate::name::{domain_of, is_above};
use crate::table::Table;
use crate::{Found, Net, Source, Verdict, autoconfig, probe, srv};

/// How long discovery waits on any one step before it drops it.
pub const STEP_LIMIT: Duration = Duration::from_secs(10);

/// Mozilla's database of provider settings. It answers 404 for a domain
/// it does not know.
const ISPDB: &str = "https://autoconfig.thunderbird.net/v1.1/";

/// The steps after the table, highest priority first. A step's answer
/// counts once every step above it has finished without one.
const STEPS: usize = 6;

/// Finds the servers for `address`. The built-in table answers with no
/// network at all. Otherwise the MX match, the domain's own autoconfig,
/// the ISPDB, the MX host's autoconfig, SRV records and a probe all start
/// at once, and the answer is the highest-priority step that found
/// something; each step gets `STEP_LIMIT`. Only the address's domain goes
/// out, and no name above it is ever built.
pub async fn find<N: Net>(net: &N, address: &str) -> Found {
    let Some(domain) = domain_of(address) else {
        return Found::nothing();
    };
    let table = Table::built_in();
    if let Some(entry) = table.by_domain(&domain) {
        return entry.found(Source::Table, false);
    }
    let domain = domain.as_str();
    let mx = net.mx(domain).shared();

    let mut by_mx = pin!(limit(async {
        let hosts = mx.clone().await;
        table
            .by_mx(&hosts)
            .map(|entry| entry.found(Source::Mx, true))
            .filter(|found| found.verdict != Verdict::NothingFound)
    }));
    let mut own = pin!(limit(own_autoconfig(net, domain)));
    let mut ispdb = pin!(limit(ispdb(net, domain, domain, Source::Ispdb)));
    let mut mx_derived = pin!(limit(async {
        let hosts = mx.clone().await;
        mx_autoconfig(net, domain, &hosts).await
    }));
    let mut srv = pin!(limit(async {
        Found::servers(srv::lookup(net, domain).await)
    }));
    let mut probe = pin!(limit(async {
        Found::servers(probe::probe(net, domain).await)
    }));
    // One slot per step: `None` while it runs, then what it found.
    let mut answers: [Option<Option<Found>>; STEPS] = Default::default();
    loop {
        tokio::select! {
            found = &mut by_mx, if answers[0].is_none() => answers[0] = Some(found),
            found = &mut own, if answers[1].is_none() => answers[1] = Some(found),
            found = &mut ispdb, if answers[2].is_none() => answers[2] = Some(found),
            found = &mut mx_derived, if answers[3].is_none() => answers[3] = Some(found),
            found = &mut srv, if answers[4].is_none() => answers[4] = Some(found),
            found = &mut probe, if answers[5].is_none() => answers[5] = Some(found),
        }
        if let Some(found) = decided(&answers) {
            return found;
        }
    }
}

/// The answer once it is settled: the first step, in priority order, that
/// found something, provided every step above it has finished. `None`
/// while a step above the best answer so far is still running.
fn decided(answers: &[Option<Option<Found>>; STEPS]) -> Option<Found> {
    for answer in answers {
        match answer {
            None => return None,
            Some(Some(found)) => return Some(found.clone()),
            Some(None) => {}
        }
    }
    Some(Found::nothing())
}

/// A step that has not answered within `STEP_LIMIT` found nothing.
async fn limit(step: impl Future<Output = Option<Found>>) -> Option<Found> {
    tokio::time::timeout(STEP_LIMIT, step).await.ok().flatten()
}

/// The domain's own autoconfig file, over HTTPS only and without the
/// address: first at `autoconfig.<domain>`, then at its well-known path.
async fn own_autoconfig<N: Net>(net: &N, domain: &str) -> Option<Found> {
    for url in [
        format!("https://autoconfig.{domain}/mail/config-v1.1.xml"),
        format!("https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml"),
    ] {
        if let Some(found) = config_at(net, &url, domain, Source::Autoconfig).await {
            return Some(found);
        }
    }
    None
}

/// The ISPDB's entry for `name`, read for an address at `domain`.
async fn ispdb<N: Net>(net: &N, name: &str, domain: &str, source: Source) -> Option<Found> {
    config_at(net, &format!("{ISPDB}{name}"), domain, source).await
}

async fn config_at<N: Net>(net: &N, url: &str, domain: &str, source: Source) -> Option<Found> {
    let text = net.get(url).await?;
    let config = autoconfig::parse(&text, domain)?;
    Found::servers(config.candidates(source, domain))
}

/// The autoconfig and ISPDB entries of the first MX host's parent domain,
/// then of its registrable domain, as Thunderbird does. A hoster's mail
/// exchanger often sits under the domain that publishes its settings. A
/// name that is the address's own domain, or above it, is left out.
async fn mx_autoconfig<N: Net>(net: &N, domain: &str, hosts: &[String]) -> Option<Found> {
    for name in mx_names(domain, hosts) {
        let url = format!("https://autoconfig.{name}/mail/config-v1.1.xml");
        if let Some(found) = config_at(net, &url, domain, Source::MxAutoconfig).await {
            return Some(found);
        }
        if let Some(found) = ispdb(net, &name, domain, Source::MxAutoconfig).await {
            return Some(found);
        }
    }
    None
}

/// The names the MX-derived step asks about, in order.
pub(crate) fn mx_names(domain: &str, hosts: &[String]) -> Vec<String> {
    let Some(host) = hosts.first() else {
        return Vec::new();
    };
    let parent = host.split_once('.').map(|(_, parent)| parent.to_string());
    let base = psl::domain_str(host).map(str::to_string);
    let mut names: Vec<String> = Vec::new();
    for name in [parent, base].into_iter().flatten() {
        // A public suffix is nobody's settings; `psl` names none as a
        // registrable domain, and the parent of `mx.co.uk` is one.
        let registrable = psl::domain_str(&name).is_some();
        if registrable && name != domain && !is_above(&name, domain) && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeNet, Request};
    use crate::{Security, SrvRecord, Unreachable};

    const HOSTER: &str = include_str!("../tests/fixtures/hoster.xml");
    const MAILBOX: &str = include_str!("../tests/fixtures/mailbox.org.xml");

    fn srv(port: u16, target: &str) -> Vec<SrvRecord> {
        vec![SrvRecord {
            priority: 0,
            weight: 1,
            port,
            target: target.into(),
        }]
    }

    #[tokio::test]
    async fn a_table_domain_needs_no_network() {
        let net = FakeNet::default();
        let found = find(&net, "someone@fastmail.com").await;
        assert_eq!(found.verdict, Verdict::Servers);
        assert_eq!(found.candidates[0].source, Source::Table);
        assert!(net.requests().is_empty());
    }

    #[tokio::test]
    async fn an_address_without_a_domain_finds_nothing_and_asks_nobody() {
        let net = FakeNet::default();
        for address in ["someone", "someone@com", "someone@localhost", ""] {
            assert_eq!(find(&net, address).await, Found::nothing(), "{address}");
        }
        assert!(net.requests().is_empty());
    }

    #[tokio::test]
    async fn a_custom_domain_on_icloud_is_found_by_mx() {
        let net = FakeNet::default().answer_mx("example.org", &["mx01.mail.icloud.com"]);
        let found = find(&net, "ann@example.org").await;
        assert_eq!(found.candidates[0].source, Source::Mx);
        assert_eq!(found.candidates[0].imap.host, "imap.mail.me.com");
    }

    #[tokio::test]
    async fn a_workspace_domain_goes_to_google_and_a_proton_one_is_not_reachable_yet() {
        let google = FakeNet::default().answer_mx("example.org", &["smtp.google.com"]);
        assert_eq!(
            find(&google, "ann@example.org").await.verdict,
            Verdict::Google
        );
        let proton = FakeNet::default().answer_mx("example.org", &["mail.protonmail.ch"]);
        assert_eq!(
            find(&proton, "ann@example.org").await.verdict,
            Verdict::Unreachable {
                provider: "Proton Mail".into(),
                reason: Unreachable::NotYet
            }
        );
    }

    #[tokio::test]
    async fn the_domains_own_file_outranks_the_ispdb() {
        let net = FakeNet::default()
            .serve(
                "https://autoconfig.example.org/mail/config-v1.1.xml",
                HOSTER,
            )
            .serve(
                "https://autoconfig.thunderbird.net/v1.1/example.org",
                MAILBOX,
            );
        let found = find(&net, "ann@example.org").await;
        assert_eq!(found.candidates[0].source, Source::Autoconfig);
        assert_eq!(found.candidates[0].imap.host, "mail.example.org");
    }

    #[tokio::test]
    async fn the_well_known_path_serves_when_the_autoconfig_host_does_not() {
        let net = FakeNet::default().serve(
            "https://example.org/.well-known/autoconfig/mail/config-v1.1.xml",
            HOSTER,
        );
        let found = find(&net, "ann@example.org").await;
        assert_eq!(found.candidates[0].source, Source::Autoconfig);
    }

    #[tokio::test]
    async fn the_ispdb_answers_when_the_domain_has_no_file() {
        let net = FakeNet::default().serve(
            "https://autoconfig.thunderbird.net/v1.1/example.org",
            MAILBOX,
        );
        let found = find(&net, "ann@example.org").await;
        assert_eq!(found.candidates[0].source, Source::Ispdb);
    }

    #[tokio::test]
    async fn the_mx_hosts_own_domain_can_name_the_servers() {
        let net = FakeNet::default()
            .answer_mx("example.org", &["mx1.mail.hoster.net"])
            .serve(
                "https://autoconfig.thunderbird.net/v1.1/hoster.net",
                MAILBOX,
            );
        let found = find(&net, "ann@example.org").await;
        assert_eq!(found.candidates[0].source, Source::MxAutoconfig);
        let asked: Vec<Request> = net
            .requests()
            .into_iter()
            .filter(|r| r.name().contains("hoster.net") && matches!(r, Request::Get(_)))
            .collect();
        assert_eq!(
            asked,
            [
                Request::Get("https://autoconfig.mail.hoster.net/mail/config-v1.1.xml".into()),
                Request::Get("https://autoconfig.thunderbird.net/v1.1/mail.hoster.net".into()),
                Request::Get("https://autoconfig.hoster.net/mail/config-v1.1.xml".into()),
                Request::Get("https://autoconfig.thunderbird.net/v1.1/hoster.net".into()),
            ]
        );
    }

    #[tokio::test]
    async fn srv_answers_before_the_probe() {
        let net = FakeNet::default()
            .answer_srv("_imaps._tcp.example.org", srv(993, "imap.example.org."))
            .answer_srv(
                "_submissions._tcp.example.org",
                srv(465, "smtp.example.org."),
            )
            .accept("imap.example.org", 993, Security::Tls)
            .accept("smtp.example.org", 465, Security::Tls);
        let found = find(&net, "ann@example.org").await;
        assert_eq!(found.candidates[0].source, Source::Srv);
        assert!(!found.candidates[0].confirm);
    }

    #[tokio::test]
    async fn a_probed_server_must_be_confirmed() {
        let net = FakeNet::default()
            .accept("mail.example.org", 993, Security::Tls)
            .accept("mail.example.org", 465, Security::Tls);
        let found = find(&net, "ann@example.org").await;
        assert_eq!(found.candidates[0].source, Source::Probe);
        assert!(found.candidates[0].confirm);
    }

    #[tokio::test]
    async fn nothing_anywhere_is_nothing_found() {
        let net = FakeNet::default();
        assert_eq!(find(&net, "ann@example.org").await, Found::nothing());
    }

    #[tokio::test(start_paused = true)]
    async fn a_step_slower_than_the_limit_is_dropped() {
        let net = FakeNet::default()
            .serve(
                "https://autoconfig.example.org/mail/config-v1.1.xml",
                HOSTER,
            )
            .delay(
                "https://autoconfig.example.org/mail/config-v1.1.xml",
                STEP_LIMIT * 2,
            )
            .serve(
                "https://autoconfig.thunderbird.net/v1.1/example.org",
                MAILBOX,
            );
        let started = tokio::time::Instant::now();
        let found = find(&net, "ann@example.org").await;
        assert_eq!(found.candidates[0].source, Source::Ispdb);
        assert_eq!(started.elapsed(), STEP_LIMIT);
    }

    #[tokio::test(start_paused = true)]
    async fn a_higher_step_is_waited_for_but_a_lower_one_is_not() {
        let net = FakeNet::default()
            .answer_mx("example.org", &["mx01.mail.icloud.com"])
            .delay("example.org", Duration::from_secs(3))
            .delay("imap.example.org", Duration::from_secs(9))
            .serve(
                "https://autoconfig.example.org/mail/config-v1.1.xml",
                HOSTER,
            );
        let started = tokio::time::Instant::now();
        let found = find(&net, "ann@example.org").await;
        assert_eq!(found.candidates[0].source, Source::Mx);
        assert_eq!(started.elapsed(), Duration::from_secs(3));
    }

    #[tokio::test]
    async fn only_the_domain_leaves_the_computer() {
        let net = FakeNet::default().answer_mx("example.org", &["mx1.mail.hoster.net"]);
        find(&net, "private.person+tag@example.org").await;
        let requests = net.requests();
        assert!(!requests.is_empty());
        for request in requests {
            let name = request.name();
            assert!(
                !name.contains("private") && !name.contains("tag") && !name.contains('@'),
                "{request:?}"
            );
        }
    }

    /// The domain a request names: the DNS name or host itself, or the
    /// URL's host, or for the ISPDB the domain at the end of its path.
    fn named(request: &Request) -> String {
        let name = request.name();
        let Some(rest) = name.strip_prefix("https://") else {
            return name.to_string();
        };
        if let Some(domain) = rest.strip_prefix("autoconfig.thunderbird.net/v1.1/") {
            return domain.to_string();
        }
        rest.split('/').next().unwrap_or(rest).to_string()
    }

    #[tokio::test]
    async fn no_name_above_the_domain_is_ever_built() {
        // The exchanger sits in the organization's parent domain, whose
        // settings the MX step would otherwise read.
        let net = FakeNet::default().answer_mx("dept.example.ac.uk", &["mx.example.ac.uk"]);
        find(&net, "ann@dept.example.ac.uk").await;
        let requests = net.requests();
        assert!(!requests.is_empty());
        for request in &requests {
            let name = named(request);
            assert!(!is_above(&name, "dept.example.ac.uk"), "{request:?}");
            assert!(name.ends_with("dept.example.ac.uk"), "{request:?}");
        }
    }

    #[test]
    fn mx_names_are_the_parent_then_the_registrable_domain() {
        let hosts = ["mx1.mail.hoster.co.uk".to_string()];
        assert_eq!(
            mx_names("example.org", &hosts),
            ["mail.hoster.co.uk", "hoster.co.uk"]
        );
    }

    #[test]
    fn mx_names_skip_the_domain_itself_and_names_above_it() {
        assert!(mx_names("example.org", &["mx.example.org".to_string()]).is_empty());
        assert!(mx_names("dept.example.org", &["mx.example.org".to_string()]).is_empty());
        assert!(mx_names("example.org", &[]).is_empty());
    }

    #[test]
    fn mx_names_never_include_a_public_suffix() {
        let hosts = ["mx.example.co.uk".to_string()];
        assert_eq!(mx_names("example.org", &hosts), ["example.co.uk"]);
    }
}
