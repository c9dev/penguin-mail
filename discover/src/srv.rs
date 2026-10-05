//! Servers from SRV records, as RFC 6186 and RFC 8314 name them.

use crate::name::{host, is_within};
use crate::{Candidate, Net, Security, Server, Source, SrvRecord, UserName, pairs};

/// Looks up the domain's IMAP, submission and POP3 records and pairs what they
/// name, TLS from the first byte ahead of STARTTLS. Only the TLS labels
/// count: a server found through `_imap._tcp` could be one without TLS.
pub(crate) async fn lookup<N: Net>(net: &N, domain: &str) -> Vec<Candidate> {
    let names = [
        format!("_imaps._tcp.{domain}"),
        format!("_submissions._tcp.{domain}"),
        format!("_submission._tcp.{domain}"),
        format!("_pop3s._tcp.{domain}"),
    ];
    let (imaps, submissions, submission, pop3s) = tokio::join!(
        net.srv(&names[0]),
        net.srv(&names[1]),
        net.srv(&names[2]),
        net.srv(&names[3])
    );
    // A POP3 server outside the domain would get the password without the
    // yes that an IMAP or SMTP target outside it asks for, so it is left out.
    let pop3 = servers(pop3s, Security::Tls)
        .into_iter()
        .find(|server| is_within(&server.host, domain));
    paired(domain, imaps, submissions, submission, pop3.as_ref())
}

#[cfg(test)]
pub(crate) fn candidates(
    domain: &str,
    imaps: Vec<SrvRecord>,
    submissions: Vec<SrvRecord>,
    submission: Vec<SrvRecord>,
) -> Vec<Candidate> {
    paired(domain, imaps, submissions, submission, None)
}

/// The records' IMAP servers with their submission servers, each with
/// `pop3` beside it.
fn paired(
    domain: &str,
    imaps: Vec<SrvRecord>,
    submissions: Vec<SrvRecord>,
    submission: Vec<SrvRecord>,
    pop3: Option<&Server>,
) -> Vec<Candidate> {
    let imap = servers(imaps, Security::Tls);
    let smtp: Vec<Server> = servers(submissions, Security::Tls)
        .into_iter()
        .chain(servers(submission, Security::StartTls))
        .collect();
    if smtp.is_empty() || (imap.is_empty() && pop3.is_none()) {
        return Vec::new();
    }
    // RFC 6186 section 6: DNS without DNSSEC can be forged, so a target
    // outside the domain the person typed goes past them first.
    let confirm = imap
        .iter()
        .chain(&smtp)
        .any(|server| !is_within(&server.host, domain));
    pairs(Source::Srv, None, &imap, &smtp, pop3, confirm)
}

/// The records' servers, best first: lower priority, then higher weight.
/// A target of `.` says the service is not offered, so a label holding one
/// offers nothing at all. Ports 465 and 993 speak TLS from the first byte
/// whatever the label says: some domains publish `_submission._tcp` on 465.
fn servers(mut records: Vec<SrvRecord>, security: Security) -> Vec<Server> {
    if records
        .iter()
        .any(|r| r.target.trim_end_matches('.').is_empty())
    {
        return Vec::new();
    }
    records.sort_by(|a, b| a.priority.cmp(&b.priority).then(b.weight.cmp(&a.weight)));
    records
        .into_iter()
        .filter(|r| r.port != 0)
        .filter_map(|r| {
            Some(Server {
                host: host(&r.target)?,
                port: r.port,
                security: if matches!(r.port, 465 | 993) {
                    Security::Tls
                } else {
                    security
                },
                user_name: UserName::Address,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(priority: u16, weight: u16, port: u16, target: &str) -> SrvRecord {
        SrvRecord {
            priority,
            weight,
            port,
            target: target.into(),
        }
    }

    #[tokio::test]
    async fn a_pop3s_record_inside_the_domain_rides_along_and_one_outside_does_not() {
        use crate::fake::FakeNet;
        let net = |target: &str| {
            FakeNet::default()
                .answer_srv("_imaps._tcp.example.org", vec![record(0, 1, 993, "imap.example.org.")])
                .answer_srv("_submissions._tcp.example.org", vec![record(0, 1, 465, "smtp.example.org.")])
                .answer_srv("_pop3s._tcp.example.org", vec![record(0, 1, 995, target)])
        };
        let inside = lookup(&net("pop.example.org."), "example.org").await;
        let pop3 = inside[0].pop3.as_ref().expect("a POP3 server");
        assert_eq!((pop3.host.as_str(), pop3.port, pop3.security), ("pop.example.org", 995, Security::Tls));
        let outside = lookup(&net("pop.hoster.net."), "example.org").await;
        assert_eq!(outside[0].pop3, None, "a host outside the domain never gets the password unasked");
    }

    #[tokio::test]
    async fn records_that_name_pop3_and_no_imap_give_a_pop3_candidate() {
        use crate::fake::FakeNet;
        let net = FakeNet::default()
            .answer_srv("_submissions._tcp.example.org", vec![record(0, 1, 465, "smtp.example.org.")])
            .answer_srv("_pop3s._tcp.example.org", vec![record(0, 1, 995, "pop.example.org.")]);
        let found = lookup(&net, "example.org").await;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].imap, None);
        assert_eq!(found[0].pop3.as_ref().map(|s| s.host.as_str()), Some("pop.example.org"));
        assert_eq!(found[0].smtp.host, "smtp.example.org");
        assert!(!found[0].confirm, "both inside the domain");
    }

    #[test]
    fn records_inside_the_domain_need_no_confirmation() {
        let found = candidates(
            "fastmail.com",
            vec![record(0, 1, 993, "imap.fastmail.com.")],
            vec![record(0, 1, 465, "smtp.fastmail.com.")],
            vec![record(0, 1, 587, "smtp.fastmail.com.")],
        );
        let smtp: Vec<(u16, Security)> = found
            .iter()
            .map(|c| (c.smtp.port, c.smtp.security))
            .collect();
        assert_eq!(smtp, [(465, Security::Tls), (587, Security::StartTls)]);
        assert!(found.iter().all(|c| !c.confirm && c.source == Source::Srv));
        assert_eq!(found[0].imap.as_ref().unwrap().host, "imap.fastmail.com");
        assert_eq!(found[0].imap.as_ref().unwrap().user_name, UserName::Address);
    }

    #[test]
    fn a_target_outside_the_domain_must_be_confirmed() {
        let found = candidates(
            "example.org",
            vec![record(0, 1, 993, "imap.hoster.net.")],
            vec![record(0, 1, 465, "smtp.example.org.")],
            vec![],
        );
        assert!(found.iter().all(|c| c.confirm));
    }

    #[test]
    fn a_dot_target_means_the_service_is_not_offered() {
        let found = candidates(
            "icloud.com",
            vec![record(0, 0, 993, ".")],
            vec![],
            vec![record(0, 1, 587, "smtp.mail.me.com.")],
        );
        assert!(found.is_empty());
    }

    #[test]
    fn a_dot_on_one_submission_label_leaves_the_other() {
        let found = candidates(
            "example.org",
            vec![record(0, 1, 993, "imap.example.org.")],
            vec![record(0, 0, 465, ".")],
            vec![record(0, 1, 587, "mail.example.org.")],
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].smtp.port, 587);
    }

    #[test]
    fn a_submission_record_on_port_465_means_tls() {
        let found = candidates(
            "disroot.org",
            vec![record(5, 0, 993, "disroot.org.")],
            vec![],
            vec![record(5, 0, 465, "disroot.org.")],
        );
        assert_eq!(
            (found[0].smtp.port, found[0].smtp.security),
            (465, Security::Tls)
        );
    }

    #[test]
    fn lower_priority_wins_then_higher_weight() {
        let found = candidates(
            "example.org",
            vec![
                record(10, 5, 993, "b.example.org."),
                record(0, 1, 993, "c.example.org."),
                record(0, 9, 993, "a.example.org."),
            ],
            vec![record(0, 1, 465, "smtp.example.org.")],
            vec![],
        );
        assert_eq!(found[0].imap.as_ref().unwrap().host, "a.example.org");
    }

    #[test]
    fn nothing_without_both_imap_and_submission() {
        assert!(
            candidates(
                "example.org",
                vec![record(0, 1, 993, "imap.example.org.")],
                vec![],
                vec![]
            )
            .is_empty()
        );
        assert!(
            candidates(
                "example.org",
                vec![],
                vec![record(0, 1, 465, "smtp.example.org.")],
                vec![]
            )
            .is_empty()
        );
    }

    #[test]
    fn a_bad_target_or_port_zero_is_skipped() {
        let found = candidates(
            "example.org",
            vec![
                record(0, 1, 993, "127.0.0.1."),
                record(1, 1, 0, "imap.example.org."),
                record(2, 1, 993, "imap2.example.org."),
            ],
            vec![record(0, 1, 465, "smtp.example.org.")],
            vec![],
        );
        assert_eq!(found[0].imap.as_ref().unwrap().host, "imap2.example.org");
    }
}
