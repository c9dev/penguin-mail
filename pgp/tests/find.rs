use mailrs_pgp::Pgp;

#[test]
fn finds_gpg_on_the_path() {
    let Ok(pgp) = Pgp::find() else {
        eprintln!("skipping: no gpg on PATH");
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
