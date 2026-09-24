//! Servers from SRV records, as RFC 6186 and RFC 8314 name them.

use crate::name::{host, is_within};
use crate::{Candidate, Net, Security, Server, Source, SrvRecord, UserName, pairs};

/// Looks up the domain's IMAP and submission records and pairs what they
/// name, TLS from the first byte ahead of STARTTLS. Only the TLS labels
/// count: a server found through `_imap._tcp` could be one without TLS.
pub(crate) async fn lookup<N: Net>(net: &N, domain: &str) -> Vec<Candidate> {
    let names = [
        format!("_imaps._tcp.{domain}"),
        format!("_submissions._tcp.{domain}"),
        format!("_submission._tcp.{domain}"),
    ];
    let (imaps, submissions, submission) =
        tokio::join!(net.srv(&names[0]), net.srv(&names[1]), net.srv(&names[2]));
    candidates(domain, imaps, submissions, submission)
}

pub(crate) fn candidates(
    domain: &str,
    imaps: Vec<SrvRecord>,
    submissions: Vec<SrvRecord>,
    submission: Vec<SrvRecord>,
) -> Vec<Candidate> {
    let imap = servers(imaps, Security::Tls);
    let smtp: Vec<Server> = servers(submissions, Security::Tls)
        .into_iter()
        .chain(servers(submission, Security::StartTls))
        .collect();
    if imap.is_empty() || smtp.is_empty() {
        return Vec::new();
    }
    // RFC 6186 section 6: DNS without DNSSEC can be forged, so a target
    // outside the domain the person typed goes past them first.
    let confirm = imap
        .iter()
        .chain(&smtp)
        .any(|server| !is_within(&server.host, domain));
    pairs(Source::Srv, None, &imap, &smtp, confirm)
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
        assert_eq!(found[0].imap.host, "imap.fastmail.com");
        assert_eq!(found[0].imap.user_name, UserName::Address);
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
        assert_eq!(found[0].imap.host, "a.example.org");
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
        assert_eq!(found[0].imap.host, "imap2.example.org");
    }
}
