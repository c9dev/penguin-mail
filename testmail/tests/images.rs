//! Every image a test pins is one `scripts/test-images.sh` pulls, since
//! the tests themselves never pull.

#[test]
fn the_pull_script_names_every_pinned_image() {
    let script = include_str!("../../scripts/test-images.sh");
    for image in [
        mailrs_testmail::DOVECOT_IMAGE,
        mailrs_testmail::MAILPIT_IMAGE,
        mailrs_testmail::RADICALE_IMAGE,
    ] {
        assert!(
            script.contains(image),
            "scripts/test-images.sh does not pull {image}"
        );
    }
}
