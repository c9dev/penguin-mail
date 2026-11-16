//! vCards read into the fields Penguin Mail shows, and patched with only
//! the fields it edits.

use mailrs_dav::vcard::{CardFields, Photo, new_card, patch_card, read_card};

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(path)
        .unwrap()
        .replace("\r\n", "\n")
        .replace('\n', "\r\n")
}

#[test]
fn an_apple_card_reads_its_preferred_address_first_and_its_inline_photo() {
    let card = read_card(&fixture("apple-card.vcf")).unwrap();
    assert_eq!(card.name.as_deref(), Some("Ana Rocha"));
    assert_eq!(card.emails, ["ana@example.pt", "ana.rocha@example.com"]);
    assert_eq!(card.phones, ["+351 912 345 678"]);
    assert_eq!(card.organization.as_deref(), Some("Penguin Studio"));
    assert!(
        matches!(card.photo, Some(Photo::Inline(ref bytes)) if bytes.starts_with(&[0xff, 0xd8]))
    );
    assert_eq!(
        card.uid.as_deref(),
        Some("5E1F2A3B-4C5D-6E7F-8091-A2B3C4D5E6F7")
    );
}

#[test]
fn a_v4_card_reads_its_pref_address_first_and_its_photo_url() {
    let card = read_card(&fixture("card-v4.vcf")).unwrap();
    assert_eq!(card.emails[0], "bruno@example.org");
    assert_eq!(
        card.photo,
        Some(Photo::Uri(
            "https://dav.example.org/photos/bruno.jpg".into()
        ))
    );
}

#[test]
fn a_patched_card_keeps_apple_s_grouped_labels() {
    let text = fixture("apple-card.vcf");
    let patched = patch_card(
        &text,
        &CardFields {
            organization: Some("Penguin Press".into()),
            ..CardFields::default()
        },
    )
    .unwrap();
    for kept in [
        "item1.X-ABLabel:_$!<Work>!$_",
        "x-user=anarocha",
        "VERSION:3.0",
        "item1.EMAIL",
    ] {
        assert!(
            patched.to_lowercase().contains(&kept.to_lowercase()),
            "lost {kept}:\n{patched}"
        );
    }
    assert_eq!(
        read_card(&patched).unwrap().organization.as_deref(),
        Some("Penguin Press")
    );
}

#[test]
fn replacing_the_addresses_keeps_every_other_line() {
    let text = fixture("card-v4.vcf");
    let patched = patch_card(
        &text,
        &CardFields {
            emails: Some(vec!["bruno@new.example".into()]),
            ..CardFields::default()
        },
    )
    .unwrap();
    let card = read_card(&patched).unwrap();
    assert_eq!(card.emails, ["bruno@new.example"]);
    assert!(patched.contains("X-FOO;X-BAR=baz:kept"), "{patched}");
    assert!(patched.contains("PHOTO:https://dav.example.org/photos/bruno.jpg"));
}

#[test]
fn a_new_card_is_version_3_with_its_uid_and_fields() {
    let fields = CardFields {
        name: Some("Carla Nunes".into()),
        emails: Some(vec!["carla@example.com".into()]),
        phones: Some(vec!["+351 213 000 000".into()]),
        organization: Some("Nunes, Lda".into()),
        ..Default::default()
    };
    let text = new_card("pm-7f3a", &fields);
    assert!(text.contains("VERSION:3.0"));
    assert!(text.contains("UID:pm-7f3a"));
    assert!(text.contains("N:Nunes;Carla;;;"));
    let card = read_card(&text).unwrap();
    assert_eq!(card.name.as_deref(), Some("Carla Nunes"));
    assert_eq!(card.organization.as_deref(), Some("Nunes, Lda"));
}

#[test]
fn a_photo_past_the_limit_is_left_out() {
    // 800,000 base64 characters decode to 600,000 bytes: over the photo
    // limit, under the limit on a whole card.
    let big = "A".repeat(800_000);
    let text = format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:X\r\nPHOTO;ENCODING=b;TYPE=JPEG:{big}\r\nEND:VCARD\r\n"
    );
    assert_eq!(read_card(&text).unwrap().photo, None);
}

#[test]
fn a_photo_the_card_names_by_a_non_web_address_is_ignored() {
    let text = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:X\r\nPHOTO:file:///etc/passwd\r\nEND:VCARD\r\n";
    assert_eq!(read_card(text).unwrap().photo, None);
}

#[test]
fn extended_fields_and_photos_round_trip_in_both_versions() {
    use mailrs_domain::contact::{Changes, Field};
    for version in ["3.0", "4.0"] {
        let original = format!(
            "BEGIN:VCARD\r\nVERSION:{version}\r\nUID:test\r\nFN:Demo Person\r\nitem1.EMAIL;TYPE=WORK:demo@example.org\r\nitem1.X-ABLabel:Office\r\nX-KEEP;X-PARAM=untouched:original\r\nEND:VCARD\r\n"
        );
        let fields = CardFields {
            emails: Some(vec!["new@example.org".into(), "other@example.org".into()]),
            details: Changes {
                birthday: Some("1988-02-29".into()),
                notes: Some("line one\nline two; comma, slash\\".into()),
                groups: Some(vec!["Work".into(), "Friends, family".into()]),
                addresses: Some(vec![
                    "\n\n1 Demo Street\nLisbon\n\n1000-001\nPortugal".into(),
                ]),
                urls: Some(vec!["https://example.org/profile".into()]),
                dates: Some(vec![Field {
                    label: "Graduation".into(),
                    value: "2010-06-04".into(),
                }]),
                custom: Some(vec![
                    Field {
                        label: "X-KEEP".into(),
                        value: "original".into(),
                    },
                    Field {
                        label: "X-CUSTOMER-ID".into(),
                        value: "123".into(),
                    },
                ]),
            },
            photo: Some(b"\x89PNG\r\n\x1a\ndemo".to_vec()),
            ..Default::default()
        };
        let text = patch_card(&original, &fields).unwrap();
        let read = read_card(&text).unwrap();
        assert_eq!(read.details.notes, fields.details.notes.clone().unwrap());
        assert_eq!(read.details.groups, fields.details.groups.clone().unwrap());
        assert_eq!(
            read.details.addresses,
            fields.details.addresses.clone().unwrap()
        );
        assert_eq!(read.details.dates, fields.details.dates.clone().unwrap());
        assert_eq!(read.details.custom, fields.details.custom.clone().unwrap());
        assert_eq!(read.photo, fields.photo.clone().map(Photo::Inline));
        assert!(
            text.contains("item1.EMAIL;TYPE=work:new@example.org")
                || text.contains("item1.EMAIL;TYPE=WORK:new@example.org"),
            "{text}"
        );
        assert!(text.contains("item1.X-ABLABEL:Office"));
        assert!(text.contains("X-PARAM=untouched"), "{text}");
        let erased = patch_card(
            &text,
            &CardFields {
                details: Changes {
                    dates: Some(vec![]),
                    custom: Some(vec![]),
                    ..Default::default()
                },
                photo: Some(vec![]),
                ..Default::default()
            },
        )
        .unwrap();
        let read = read_card(&erased).unwrap();
        assert!(
            read.details.dates.is_empty() && read.details.custom.is_empty() && read.photo.is_none()
        );
        assert!(erased.contains("item1.X-ABLABEL:Office"));
        assert!(!erased.contains("Graduation"));
    }
}

#[test]
fn invalid_properties_dates_and_photos_are_rejected_before_writing() {
    use mailrs_domain::contact::{Changes, Field};
    let original = new_card("id", &CardFields::default());
    for label in [
        "X-FOO\r\nFN",
        "FN",
        "X-ABDATE",
        "X-ADDRESSBOOKSERVER-MEMBER",
        "X-FOO:bar",
    ] {
        let fields = CardFields {
            details: Changes {
                custom: Some(vec![Field {
                    label: label.into(),
                    value: "bad".into(),
                }]),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(patch_card(&original, &fields).is_err());
    }
    for date in ["2025-02-29", "2024-13-01", "--éé", "x\r\nFN:bad"] {
        assert!(
            patch_card(
                &original,
                &CardFields {
                    details: Changes {
                        birthday: Some(date.into()),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    for date in ["2024-02-29", "--02-29", "--0229", "20240229"] {
        assert!(
            mailrs_dav::vcard::validate(&CardFields {
                details: Changes {
                    birthday: Some(date.into()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .is_ok()
        );
    }
}

#[test]
fn group_membership_preserves_other_members_and_unknown_fields() {
    use mailrs_dav::vcard::{group_membership, new_group};
    let original =
        new_group("team", "Friends", "first").replace("END:VCARD", "X-KEEP:yes\r\nEND:VCARD");
    let added = group_membership(&original, "second", true).unwrap();
    assert_eq!(
        read_card(&added).unwrap().details.members.unwrap(),
        ["first", "second"]
    );
    let removed = group_membership(&added, "first", false).unwrap();
    assert_eq!(
        read_card(&removed).unwrap().details.members.unwrap(),
        ["second"]
    );
    assert!(removed.contains("X-KEEP:yes"));
}
