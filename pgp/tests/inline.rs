use mailrs_pgp::inline::{Armor, armor};

#[test]
fn finds_an_encrypted_block_with_text_around_it() {
    let body = "Sent from my telephone\n\
        -----BEGIN PGP MESSAGE-----\n\
        \n\
        hQIMA0x\n\
        -----END PGP MESSAGE-----\n\
        Please excuse the brevity.\n";
    assert_eq!(armor(body), Some(Armor::Message));
}

#[test]
fn finds_a_clearsigned_block() {
    let body = "-----BEGIN PGP SIGNED MESSAGE-----\n\
        Hash: SHA512\n\
        \n\
        Meet at six.\n\
        -----BEGIN PGP SIGNATURE-----\n\
        \n\
        iHUEARY\n\
        -----END PGP SIGNATURE-----\n";
    assert_eq!(armor(body), Some(Armor::Clearsigned));
}

#[test]
fn a_body_with_nothing_armored_in_it_has_none() {
    assert_eq!(armor("Meet at six. Bring tea.\n"), None);
    assert_eq!(armor(""), None);
}

#[test]
fn a_block_that_never_ends_is_not_one() {
    assert_eq!(armor("-----BEGIN PGP MESSAGE-----\n\nhQIMA0x\n"), None);
    assert_eq!(
        armor("-----BEGIN PGP SIGNED MESSAGE-----\n\nMeet at six.\n"),
        None
    );
}
