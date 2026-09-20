use mailrs_pgp::Pgp;

#[test]
fn finds_gpg_on_the_path() {
    let Ok(pgp) = Pgp::find() else {
        eprintln!("skipping: no gpg on PATH");
        require_crypto();
        return;
    };
    let name = pgp.program().file_name().unwrap().to_string_lossy();
    assert!(name == "gpg" || name == "gpg2", "found {name}");
}

#[test]
fn says_so_when_the_path_holds_no_gpg() {
    let err = Pgp::find_on("").expect_err("an empty PATH holds no gpg");
    assert!(err.to_string().contains("gpg"), "{err}");
}

/// Stops a run that was meant to exercise the real thing from passing on a
/// computer that cannot. The round trips skip when GnuPG is missing, so a
/// developer without it can still run the suite; that same skip would let
/// a build machine report a green S/MIME and OpenPGP suite having tested
/// nothing. Setting `PENGUIN_MAIL_REQUIRE_CRYPTO` turns the skip into a
/// failure, which is what a build machine should do.
fn require_crypto() {
    if std::env::var_os("PENGUIN_MAIL_REQUIRE_CRYPTO").is_some() {
        panic!(
            "PENGUIN_MAIL_REQUIRE_CRYPTO is set and GnuPG is not on PATH, \
             so these tests would have proved nothing"
        );
    }
}
