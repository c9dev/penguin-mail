use mailrs_pgp::status::{Trust, Verdict, signature};

/// The lines gpg 2.4 writes after checking a signature from a key it holds.
const GOOD: &[&str] = &[
    "NEWSIG ada@example.com",
    "KEY_CONSIDERED 6F8A1C2D3E4F50617283949AB5C6D7E8F9A0B1C2 0",
    "SIG_ID xJ9kQ2 2026-09-20 1758312000",
    "GOODSIG B5C6D7E8F9A0B1C2 Ada Lovelace <ada@example.com>",
    "VALIDSIG 6F8A1C2D3E4F50617283949AB5C6D7E8F9A0B1C2 2026-09-20 1758312000 0 4 0 22 8 00 \
     6F8A1C2D3E4F50617283949AB5C6D7E8F9A0B1C2",
    "TRUST_ULTIMATE 0 pgp",
];

#[test]
fn reads_the_signer_the_fingerprint_and_the_trust() {
    let found = signature(GOOD).expect("a signature");
    assert_eq!(found.verdict, Verdict::Good);
    assert!(found.is_good());
    assert_eq!(
        found.signer.as_deref(),
        Some("Ada Lovelace <ada@example.com>")
    );
    assert_eq!(
        found.fingerprint.as_deref(),
        Some("6F8A1C2D3E4F50617283949AB5C6D7E8F9A0B1C2")
    );
    assert_eq!(found.key_id.as_deref(), Some("B5C6D7E8F9A0B1C2"));
    assert_eq!(found.trust, Trust::Ultimate);
}

#[test]
fn a_changed_body_reads_as_bad() {
    let found = signature(&["BADSIG B5C6D7E8F9A0B1C2 Ada Lovelace <ada@example.com>"])
        .expect("a signature");
    assert_eq!(found.verdict, Verdict::Bad);
    assert!(!found.is_good());
    assert_eq!(
        found.signer.as_deref(),
        Some("Ada Lovelace <ada@example.com>")
    );
}

#[test]
fn an_expired_or_revoked_key_still_names_its_owner() {
    for (line, verdict) in [
        (
            "EXPKEYSIG B5C6D7E8F9A0B1C2 Ada <ada@example.com>",
            Verdict::ExpiredKey,
        ),
        (
            "REVKEYSIG B5C6D7E8F9A0B1C2 Ada <ada@example.com>",
            Verdict::RevokedKey,
        ),
        (
            "EXPSIG B5C6D7E8F9A0B1C2 Ada <ada@example.com>",
            Verdict::Expired,
        ),
    ] {
        let found = signature(&[line]).expect("a signature");
        assert_eq!(found.verdict, verdict, "{line}");
        assert_eq!(found.signer.as_deref(), Some("Ada <ada@example.com>"));
        assert!(!found.is_good(), "{line}");
    }
}

#[test]
fn a_signature_from_a_key_we_lack_says_so_rather_than_bad() {
    let found = signature(&[
        "NO_PUBKEY B5C6D7E8F9A0B1C2",
        "ERRSIG B5C6D7E8F9A0B1C2 22 8 00 1758312000 9 6F8A1C2D3E4F5061",
    ])
    .expect("a signature");
    assert_eq!(found.verdict, Verdict::NoKey);
    assert_eq!(found.key_id.as_deref(), Some("B5C6D7E8F9A0B1C2"));
    assert_eq!(found.signer, None);
    assert_eq!(found.trust, Trust::Unknown);
}

#[test]
fn every_trust_level_has_a_name() {
    for (line, trust) in [
        ("TRUST_UNDEFINED 0 pgp", Trust::Unknown),
        ("TRUST_NEVER 0 pgp", Trust::Never),
        ("TRUST_MARGINAL 0 pgp", Trust::Marginal),
        ("TRUST_FULLY 0 pgp", Trust::Full),
        ("TRUST_ULTIMATE 0 pgp", Trust::Ultimate),
    ] {
        let found = signature(&["GOODSIG B5C6D7E8F9A0B1C2 Ada <ada@example.com>", line])
            .expect("a signature");
        assert_eq!(found.trust, trust, "{line}");
    }
}

#[test]
fn lines_about_anything_else_describe_no_signature() {
    assert!(signature(&["NODATA 1", "DECRYPTION_FAILED"]).is_none());
    assert!(signature::<&str>(&[]).is_none());
}
