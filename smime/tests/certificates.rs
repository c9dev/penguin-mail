use mailrs_smime::certificates::usable;

/// What `gpgsm --with-colons --list-keys` writes for one certificate that
/// signs and encrypts.
const MINE: &str = "\
crt:u:2048:1:6611EDFE381C75B2:20260920T135931:20301231T000000:5861792F6F2757FA::\
CN=Ada Lovelace,O=Example::esES::::::23:
fpr:::::::::13303E1996309763B67A7C5E6611EDFE381C75B2:::\
13303E1996309763B67A7C5E6611EDFE381C75B2:
uid:u::::::::CN=Ada Lovelace,O=Example:::
uid:u::::::::<ada@example.test>::
";

#[test]
fn reads_the_subject_the_address_and_the_fingerprint() {
    let found = usable(MINE, "ada@example.test", 'e').expect("a certificate");
    assert_eq!(found.subject, "CN=Ada Lovelace,O=Example");
    assert_eq!(found.email, "ada@example.test");
    assert_eq!(
        found.fingerprint,
        "13303E1996309763B67A7C5E6611EDFE381C75B2"
    );
}

#[test]
fn a_certificate_that_cannot_do_the_job_is_no_answer() {
    let signing_only = MINE.replace("esES", "sS");
    assert!(usable(&signing_only, "ada@example.test", 'e').is_none());
    assert!(usable(&signing_only, "ada@example.test", 's').is_some());
}

#[test]
fn an_expired_or_revoked_certificate_is_no_answer_either() {
    for state in ["e", "r", "d", "i"] {
        let listing = MINE.replacen("crt:u:", &format!("crt:{state}:"), 1);
        assert!(
            usable(&listing, "ada@example.test", 'e').is_none(),
            "{state}"
        );
    }
}

#[test]
fn the_address_asked_about_is_the_one_reported() {
    let both = MINE.replace(
        "uid:u::::::::<ada@example.test>::",
        "uid:u::::::::<ada@example.test>::\nuid:u::::::::<work@example.test>::",
    );
    assert_eq!(
        usable(&both, "work@example.test", 'e')
            .expect("a certificate")
            .email,
        "work@example.test"
    );
}

#[test]
fn a_listing_with_nothing_in_it_holds_no_certificate() {
    assert!(usable("", "ada@example.test", 'e').is_none());
    // A record with no fingerprint beside it names nothing gpgsm can use.
    assert!(
        usable(
            "crt:u:2048:1:6611EDFE381C75B2:::::CN=Ada::esES::::::23:\n",
            "ada@example.test",
            'e'
        )
        .is_none()
    );
}
