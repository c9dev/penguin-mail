use mailrs_smime::status::{Chain, Verdict, signature};

/// The lines gpgsm 2.4 writes after checking a signature whose chain
/// reaches a root in the person's trust list.
const GOOD: &[&str] = &[
    "NEWSIG",
    "GOODSIG 13303E1996309763B67A7C5E6611EDFE381C75B2 /CN=Ada Lovelace/O=Example",
    "VALIDSIG 13303E1996309763B67A7C5E6611EDFE381C75B2 2026-09-20 20260920T135955 \
     20301231T000000 0 0 1 8 00",
    "TRUST_FULLY 0 shell",
];

#[test]
fn reads_the_subject_the_fingerprint_and_the_chain() {
    let found = signature(GOOD).expect("a signature");
    assert_eq!(found.verdict, Verdict::Good);
    assert!(found.is_good());
    assert_eq!(found.subject.as_deref(), Some("/CN=Ada Lovelace/O=Example"));
    assert_eq!(
        found.fingerprint.as_deref(),
        Some("13303E1996309763B67A7C5E6611EDFE381C75B2")
    );
    assert_eq!(found.chain, Chain::Trusted);
    // The address lives in the certificate rather than in these lines.
    assert!(found.emails.is_empty());
}

#[test]
fn a_changed_body_reads_as_bad() {
    let found = signature(&[
        "NEWSIG",
        "BADSIG 13303E1996309763B67A7C5E6611EDFE381C75B2 /CN=Ada Lovelace/O=Example",
    ])
    .expect("a signature");
    assert_eq!(found.verdict, Verdict::Bad);
    assert!(!found.is_good());
    assert_eq!(found.subject.as_deref(), Some("/CN=Ada Lovelace/O=Example"));
}

#[test]
fn an_expired_or_revoked_certificate_still_names_its_owner() {
    for (line, verdict) in [
        (
            "EXPKEYSIG 13303E1996309763B67A7C5E6611EDFE381C75B2 /CN=Ada",
            Verdict::ExpiredCertificate,
        ),
        (
            "REVKEYSIG 13303E1996309763B67A7C5E6611EDFE381C75B2 /CN=Ada",
            Verdict::RevokedCertificate,
        ),
        (
            "EXPSIG 13303E1996309763B67A7C5E6611EDFE381C75B2 /CN=Ada",
            Verdict::Expired,
        ),
    ] {
        let found = signature(&[line]).expect("a signature");
        assert_eq!(found.verdict, verdict, "{line}");
        assert_eq!(found.subject.as_deref(), Some("/CN=Ada"));
        assert!(!found.is_good(), "{line}");
    }
}

#[test]
fn a_chain_that_reached_no_trusted_root_is_a_separate_answer() {
    let found = signature(&[
        "GOODSIG 13303E1996309763B67A7C5E6611EDFE381C75B2 /CN=Ada",
        "TRUST_UNDEFINED 98",
    ])
    .expect("a signature");
    assert_eq!(found.verdict, Verdict::Good);
    assert!(found.is_good());
    assert_eq!(found.chain, Chain::Untrusted);
}

#[test]
fn a_certificate_nobody_here_holds_says_so_rather_than_bad() {
    let found = signature(&["NEWSIG", "ERROR verify.findkey 50331657"]).expect("a signature");
    assert_eq!(found.verdict, Verdict::NoCertificate);
    assert_eq!(found.subject, None);
    assert_eq!(found.fingerprint, None);
    assert_eq!(found.chain, Chain::Unknown);
}

#[test]
fn every_trust_line_says_how_far_the_chain_got() {
    for (line, chain) in [
        ("TRUST_UNDEFINED 98", Chain::Untrusted),
        ("TRUST_NEVER 0 shell", Chain::Untrusted),
        ("TRUST_MARGINAL 0 shell", Chain::Untrusted),
        ("TRUST_FULLY 0 shell", Chain::Trusted),
        ("TRUST_ULTIMATE 0 shell", Chain::Trusted),
    ] {
        let found = signature(&[
            "GOODSIG 13303E1996309763B67A7C5E6611EDFE381C75B2 /CN=Ada",
            line,
        ])
        .expect("a signature");
        assert_eq!(found.chain, chain, "{line}");
    }
}

/// What gpgsm 2.4.8 wrote for a certificate its authority's CRL lists as
/// revoked. It says so on the trust line alone, with error code 94
/// (`GPG_ERR_CERT_REVOKED`), and still writes `GOODSIG`, because the text
/// is the text that certificate signed.
#[test]
fn a_certificate_the_crl_lists_reads_as_revoked() {
    let found = signature(&[
        "NEWSIG",
        "GOODSIG 7D04A7F3425811638828046060ACDA55BD23C6C6 /CN=Ada Lovelace",
        "VALIDSIG 7D04A7F3425811638828046060ACDA55BD23C6C6 2026-09-23 20260923T095715 \
         20370101T000000 0 0 1 8 00",
        "TRUST_NEVER 94",
    ])
    .expect("a signature");
    assert_eq!(found.verdict, Verdict::RevokedCertificate);
    assert!(!found.is_good());
    assert_eq!(found.chain, Chain::Untrusted);
    assert_eq!(found.subject.as_deref(), Some("/CN=Ada Lovelace"));
}

/// What gpgsm 2.4.8 wrote when it could not learn whether a certificate
/// was revoked: the CRL's server refused the connection (32793,
/// `ECONNREFUSED`), dirmngr could not start (92), dirmngr had HTTP turned
/// off (60), or the server had no CRL to give (95, `GPG_ERR_NO_CRL_KNOWN`).
/// On their own these lines look like any chain that failed, so
/// `Smime::verify` asks gpgsm again with CRL checks off to tell the two
/// apart.
#[test]
fn a_crl_that_could_not_be_fetched_reads_as_a_chain_that_failed() {
    for code in ["32793", "92", "60", "95"] {
        let trust = format!("TRUST_UNDEFINED {code}");
        let found = signature(&[
            "NEWSIG",
            "PROGRESS starting_dirmngr ? 0 0",
            "GOODSIG EC940142CFFC5B9852D26BC3143EEDE9072732B2 /CN=Ada Lovelace",
            "VALIDSIG EC940142CFFC5B9852D26BC3143EEDE9072732B2 2026-09-23 20260923T095512 \
             20370101T000000 0 0 1 8 00",
            &trust,
        ])
        .expect("a signature");
        assert_eq!(found.verdict, Verdict::Good, "{code}");
        assert_eq!(found.chain, Chain::Untrusted, "{code}");
    }
}

#[test]
fn lines_about_anything_else_describe_no_signature() {
    assert!(signature(&["NODATA 1", "DECRYPTION_FAILED"]).is_none());
    assert!(signature(&["ERROR verify.leave 150995087"]).is_none());
    assert!(signature::<&str>(&[]).is_none());
}
