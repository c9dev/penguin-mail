use mailrs_pgp::Trust;
use mailrs_pgp::keys::usable;

/// What `gpg --with-colons --list-keys` prints for one ed25519 key with a
/// subkey to encrypt to.
fn listing(validity: &str, capabilities: &str) -> String {
    format!(
        "tru:o:1:1789905014:1:3:1:5\n\
         pub:{validity}:255:22:01E8A2DA011C4521:1789905014:::u:::{capabilities}:::::ed25519:::0:\n\
         fpr:::::::::B40AA3E6E125733C83091B5201E8A2DA011C4521:\n\
         uid:{validity}::::1789905014::8E2B78765D869A66::Ada Lovelace <ada@example.test>::::::::::0:\n\
         sub:{validity}:255:18:A3DE131D5711BFA0:1789905014::::::e:::::cv25519::\n\
         fpr:::::::::BAF9451E1AC1C7C26F2E4C6FA3DE131D5711BFA0:\n"
    )
}

#[test]
fn a_key_that_can_encrypt_comes_back_with_its_fingerprint_and_trust() {
    let key = usable(&listing("u", "scESC"), "ada@example.test").expect("a key");
    assert_eq!(key.fingerprint, "B40AA3E6E125733C83091B5201E8A2DA011C4521");
    assert_eq!(key.user_id, "Ada Lovelace <ada@example.test>");
    assert_eq!(key.trust, Trust::Ultimate);
}

#[test]
fn a_key_nobody_has_vouched_for_is_still_a_key() {
    // Encryption works whatever the trust says. Saying how far the trust
    // goes is the caller's to pass on, not a reason to hold the key back.
    let key = usable(&listing("-", "scESC"), "ada@example.test").expect("a key");
    assert_eq!(key.trust, Trust::Unknown);
    let key = usable(&listing("m", "scESC"), "ada@example.test").expect("a key");
    assert_eq!(key.trust, Trust::Marginal);
    let key = usable(&listing("f", "scESC"), "ada@example.test").expect("a key");
    assert_eq!(key.trust, Trust::Full);
    let key = usable(&listing("n", "scESC"), "ada@example.test").expect("a key");
    assert_eq!(key.trust, Trust::Never);
}

#[test]
fn a_key_that_cannot_encrypt_is_no_use_here() {
    assert!(usable(&listing("u", "scSC"), "ada@example.test").is_none());
}

#[test]
fn an_expired_revoked_or_disabled_key_is_no_use_either() {
    for validity in ["e", "r", "d", "i"] {
        assert!(
            usable(&listing(validity, "scESC"), "ada@example.test").is_none(),
            "a key listed as {validity}"
        );
    }
}

#[test]
fn a_listing_with_no_key_in_it_gives_nothing() {
    assert!(usable("tru:o:1:1789905014:1:3:1:5\n", "ada@example.test").is_none());
    assert!(usable("", "ada@example.test").is_none());
}

#[test]
fn the_trust_comes_from_the_user_id_that_matches_the_address() {
    // A key can carry several addresses, each vouched for on its own. The
    // one being written to is the one that counts.
    let listing = "pub:f:255:22:01E8A2DA011C4521:1789905014:::u:::scESC:::::ed25519:::0:\n\
         fpr:::::::::B40AA3E6E125733C83091B5201E8A2DA011C4521:\n\
         uid:f::::1789905014::8E2B::Ada Lovelace <ada@work.test>::::::::::0:\n\
         uid:m::::1789905014::8E2C::Ada Lovelace <ada@example.test>::::::::::0:\n";
    let key = usable(listing, "ada@example.test").expect("a key");
    assert_eq!(key.trust, Trust::Marginal);
    assert_eq!(key.user_id, "Ada Lovelace <ada@example.test>");
}

#[test]
fn one_listing_answers_for_every_address_in_it() {
    let bo = listing("f", "scESC")
        .replace("tru:o:1:1789905014:1:3:1:5\n", "")
        .replace("B40AA3E6E125733C83091B5201E8A2DA011C4521", "BBBB")
        .replace(
            "Ada Lovelace <ada@example.test>",
            "Bo Peep <bo@example.test>",
        );
    let both = format!("{}{bo}", listing("u", "scESC"));
    assert_eq!(
        usable(&both, "bo@example.test").expect("Bo's").fingerprint,
        "BBBB"
    );
    assert_eq!(
        usable(&both, "ADA@example.test")
            .expect("Ada's")
            .fingerprint,
        "B40AA3E6E125733C83091B5201E8A2DA011C4521"
    );
    assert!(usable(&both, "cy@example.test").is_none());
    // An address that is only part of another is not that address.
    assert!(usable(&both, "o@example.test").is_none());
}

#[test]
fn every_user_id_on_a_key_comes_back_with_its_own_validity() {
    let listing = "pub:f:255:22:01E8A2DA011C4521:1789905014:::f:::scESC:::::ed25519:::0:\n\
         fpr:::::::::B40AA3E6E125733C83091B5201E8A2DA011C4521:\n\
         uid:f::::1789905014::8E2B::Mallory <mallory@example.test>::::::::::0:\n\
         uid:-::::1789905014::9F3C::The Boss <ceo@example.test>::::::::::0:\n\
         uid:r::::1789905014::AB12::Old <old@example.test>::::::::::0:\n";
    let found = mailrs_pgp::keys::user_ids(listing);
    assert_eq!(
        found,
        [
            mailrs_pgp::UserId {
                user_id: "Mallory <mallory@example.test>".into(),
                trust: Trust::Full,
            },
            mailrs_pgp::UserId {
                user_id: "The Boss <ceo@example.test>".into(),
                trust: Trust::Unknown,
            },
        ],
        "a revoked user id names nobody"
    );
}
