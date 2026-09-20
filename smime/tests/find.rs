use mailrs_smime::Smime;

#[test]
fn finds_gpgsm_on_the_path() {
    let Ok(smime) = Smime::find() else {
        eprintln!("skipping: no gpgsm on PATH");
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
