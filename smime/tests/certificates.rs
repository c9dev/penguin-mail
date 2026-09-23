use mailrs_smime::certificates::{Job, usable};

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
    let found = usable(MINE, "ada@example.test", Job::Encrypt).expect("a certificate");
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
    assert!(usable(&signing_only, "ada@example.test", Job::Encrypt).is_none());
    assert!(usable(&signing_only, "ada@example.test", Job::Sign).is_some());
}

#[test]
fn an_expired_or_revoked_certificate_is_no_answer_either() {
    for state in ["e", "r", "d", "i"] {
        let listing = MINE.replacen("crt:u:", &format!("crt:{state}:"), 1);
        assert!(
            usable(&listing, "ada@example.test", Job::Encrypt).is_none(),
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
        usable(&both, "work@example.test", Job::Encrypt)
            .expect("a certificate")
            .email,
        "work@example.test"
    );
}

#[test]
fn a_listing_with_nothing_in_it_holds_no_certificate() {
    assert!(usable("", "ada@example.test", Job::Encrypt).is_none());
    // A record with no fingerprint beside it names nothing gpgsm can use.
    assert!(
        usable(
            "crt:u:2048:1:6611EDFE381C75B2:::::CN=Ada::esES::::::23:\n",
            "ada@example.test",
            Job::Encrypt
        )
        .is_none()
    );
}

/// The same certificate as gpgsm lists it when nothing here trusts it: the
/// root it names is not in the trust list.
fn stranger(validity: &str) -> String {
    MINE.replace("crt:u:", &format!("crt:{validity}:"))
        .replace("uid:u:", &format!("uid:{validity}:"))
}

#[test]
fn only_a_certificate_whose_chain_this_computer_trusts_takes_a_message() {
    // `n` is a root nobody put in the trust list, `i` a chain that did not
    // validate, and a blank one that was never checked.
    for validity in ["n", "i", "", "-", "q"] {
        assert!(
            usable(&stranger(validity), "ada@example.test", Job::Encrypt).is_none(),
            "{validity:?}"
        );
    }
    assert!(usable(&stranger("f"), "ada@example.test", Job::Encrypt).is_some());
    assert!(usable(MINE, "ada@example.test", Job::Encrypt).is_some());
}

#[test]
fn signing_needs_no_trust_in_the_chain() {
    assert!(usable(&stranger("n"), "ada@example.test", Job::Sign).is_some());
}

#[test]
fn a_trusted_certificate_wins_over_one_seen_in_mail_whatever_the_order() {
    let impostor = stranger("n")
        .replace("13303E1996309763B67A7C5E6611EDFE381C75B2", "AAAA")
        .replace("CN=Ada Lovelace,O=Example", "CN=Not Ada");
    let listing = format!("{impostor}{MINE}");
    let found = usable(&listing, "ada@example.test", Job::Encrypt).expect("the trusted one");
    assert_eq!(found.subject, "CN=Ada Lovelace,O=Example");
}

#[test]
fn one_listing_answers_for_every_address_in_it() {
    let work = MINE
        .replace("13303E1996309763B67A7C5E6611EDFE381C75B2", "BBBB")
        .replace("<ada@example.test>", "<bo@example.test>");
    let listing = format!("{MINE}{work}");
    assert_eq!(
        usable(&listing, "bo@example.test", Job::Encrypt)
            .expect("Bo's")
            .fingerprint,
        "BBBB"
    );
    assert_eq!(
        usable(&listing, "ADA@example.test", Job::Encrypt)
            .expect("Ada's")
            .email,
        "ada@example.test"
    );
    assert!(usable(&listing, "cy@example.test", Job::Encrypt).is_none());
}
