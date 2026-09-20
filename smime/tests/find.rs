use mailrs_smime::Smime;

#[test]
fn finds_gpgsm_on_the_path() {
    let Ok(smime) = Smime::find() else {
        eprintln!("skipping: no gpgsm on PATH");
        require_crypto();
        return;
    };
    let name = smime.program().file_name().unwrap().to_string_lossy();
    assert_eq!(name, "gpgsm");
}

#[test]
fn says_so_when_the_path_holds_no_gpgsm() {
    let err = Smime::find_on("").expect_err("an empty PATH holds no gpgsm");
    assert!(err.to_string().contains("gpgsm"), "{err}");
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
