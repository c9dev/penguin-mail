//! Guessing the usual host names and trying them, the last step.

use futures::future::join_all;

use crate::{Candidate, Net, Security, Server, Source, UserName, pairs};

/// Tries `imap.`, `mail.` and the bare domain on 993, and `smtp.` and
/// `mail.` on 465, then on 587 with STARTTLS, all at once. Each answer
/// needs a certificate valid for the host. What answers is a guess, so
/// the person confirms it before the password goes out.
pub(crate) async fn probe<N: Net>(net: &N, domain: &str) -> Vec<Candidate> {
    let imap_tries: Vec<Server> = [
        format!("imap.{domain}"),
        format!("mail.{domain}"),
        domain.to_string(),
    ]
    .into_iter()
    .map(|host| server(host, 993, Security::Tls))
    .collect();
    let smtp_tries: Vec<Server> = [(465, Security::Tls), (587, Security::StartTls)]
        .into_iter()
        .flat_map(|(port, security)| {
            [format!("smtp.{domain}"), format!("mail.{domain}")]
                .into_iter()
                .map(move |host| server(host, port, security))
        })
        .collect();
    let (imap, smtp) = tokio::join!(answering(net, imap_tries), answering(net, smtp_tries));
    // The first IMAP host that answered, with every submission server that
    // answered, in the order tried.
    pairs(Source::Probe, None, &imap[..imap.len().min(1)], &smtp, true)
}

fn server(host: String, port: u16, security: Security) -> Server {
    Server {
        host,
        port,
        security,
        user_name: UserName::Address,
    }
}

/// The servers that answered, in the order given.
async fn answering<N: Net>(net: &N, tries: Vec<Server>) -> Vec<Server> {
    let answers = join_all(
        tries
            .iter()
            .map(|s| net.reaches(&s.host, s.port, s.security)),
    )
    .await;
    tries
        .into_iter()
        .zip(answers)
        .filter_map(|(server, reached)| reached.then_some(server))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeNet, Request};

    #[tokio::test]
    async fn the_first_imap_host_that_answers_wins() {
        let net = FakeNet::default()
            .accept("mail.example.org", 993, Security::Tls)
            .accept("example.org", 993, Security::Tls)
            .accept("smtp.example.org", 587, Security::StartTls);
        let found = probe(&net, "example.org").await;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].imap.host, "mail.example.org");
        assert_eq!(
            (found[0].smtp.host.as_str(), found[0].smtp.port),
            ("smtp.example.org", 587)
        );
        assert!(found[0].confirm);
        assert_eq!(found[0].source, Source::Probe);
    }

    #[tokio::test]
    async fn port_465_comes_before_587() {
        let net = FakeNet::default()
            .accept("imap.example.org", 993, Security::Tls)
            .accept("smtp.example.org", 587, Security::StartTls)
            .accept("mail.example.org", 465, Security::Tls);
        let found = probe(&net, "example.org").await;
        let smtp: Vec<(&str, u16)> = found
            .iter()
            .map(|c| (c.smtp.host.as_str(), c.smtp.port))
            .collect();
        assert_eq!(smtp, [("mail.example.org", 465), ("smtp.example.org", 587)]);
    }

    #[tokio::test]
    async fn nothing_unless_both_sides_answer() {
        let net = FakeNet::default().accept("imap.example.org", 993, Security::Tls);
        assert!(probe(&net, "example.org").await.is_empty());
    }

    #[tokio::test]
    async fn the_probe_tries_seven_servers_and_no_plain_ports() {
        let net = FakeNet::default();
        probe(&net, "example.org").await;
        let mut tried: Vec<(String, u16)> = net
            .requests()
            .into_iter()
            .filter_map(|r| match r {
                Request::Reaches(host, port, _) => Some((host, port)),
                _ => None,
            })
            .collect();
        tried.sort();
        let expected: Vec<(String, u16)> = [
            ("example.org", 993),
            ("imap.example.org", 993),
            ("mail.example.org", 465),
            ("mail.example.org", 587),
            ("mail.example.org", 993),
            ("smtp.example.org", 465),
            ("smtp.example.org", 587),
        ]
        .into_iter()
        .map(|(h, p)| (h.to_string(), p))
        .collect();
        assert_eq!(tried, expected);
    }
}
