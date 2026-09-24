//! Discovery's steps and the rule that picks one.

use std::time::Duration;

use crate::name::domain_of;
use crate::table::Table;
use crate::{Found, Net, Source, Verdict};

/// How long discovery waits on any one step before it drops it.
pub const STEP_LIMIT: Duration = Duration::from_secs(10);

/// Finds the servers for `address`. The built-in table answers by the
/// exact domain with no network at all; otherwise the domain's MX hosts
/// are matched against the table's patterns. Only the address's domain
/// goes out.
pub async fn find<N: Net>(net: &N, address: &str) -> Found {
    let Some(domain) = domain_of(address) else {
        return Found::nothing();
    };
    let table = Table::built_in();
    if let Some(entry) = table.by_domain(&domain) {
        return entry.found(Source::Table, false);
    }
    let by_mx = limit(async {
        let hosts = net.mx(&domain).await;
        table
            .by_mx(&hosts)
            .map(|entry| entry.found(Source::Mx, true))
            .filter(|found| found.verdict != Verdict::NothingFound)
    });
    by_mx.await.unwrap_or_else(Found::nothing)
}

/// A step that has not answered within `STEP_LIMIT` found nothing.
async fn limit(step: impl Future<Output = Option<Found>>) -> Option<Found> {
    tokio::time::timeout(STEP_LIMIT, step).await.ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Unreachable;
    use crate::fake::{FakeNet, Request};

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
        assert_eq!(net.requests(), [Request::Mx("example.org".into())]);
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
    async fn an_unknown_mx_is_nothing_found() {
        let net = FakeNet::default().answer_mx("example.org", &["mx.example.org"]);
        assert_eq!(find(&net, "ann@example.org").await, Found::nothing());
    }

    #[tokio::test(start_paused = true)]
    async fn a_step_slower_than_the_limit_is_dropped() {
        let net = FakeNet::default()
            .answer_mx("example.org", &["mx01.mail.icloud.com"])
            .delay("example.org", STEP_LIMIT * 2);
        let started = tokio::time::Instant::now();
        assert_eq!(find(&net, "ann@example.org").await, Found::nothing());
        assert_eq!(started.elapsed(), STEP_LIMIT);
    }
}
