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
                    [
                        "",
                        "",
                        "1 Demo Street",
                        "Lisbon",
                        "",
                        "1000-001",
                        "Portugal",
                    ]
                    .map(String::from),
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
        assert_eq!(read.details.groups, ["Work", "Friends", "family"]);
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
fn invalid_dates_and_photos_are_rejected_and_labels_stay_text() {
    use mailrs_domain::contact::{Changes, Field};
    let original = new_card("id", &CardFields::default());
    // A label that is no safe property name is written as a label's text,
    // so it can neither add a line nor become a property.
    for label in [
        "X-FOO\r\nFN",
        "FN",
        "X-ABDATE",
        "X-ADDRESSBOOKSERVER-MEMBER",
        "X-FOO:bar",
        "Badge",
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
        let saved = patch_card(&original, &fields).unwrap();
        let names: Vec<_> = saved
            .lines()
            .map(|line| line.split([':', ';']).next().unwrap_or_default())
            .collect();
        for forbidden in ["X-FOO", "X-ABDATE", "X-ADDRESSBOOKSERVER-MEMBER", "Badge"] {
            assert!(!names.contains(&forbidden), "{saved}");
        }
        assert_eq!(
            saved.matches("\nFN").count(),
            original.matches("\nFN").count(),
            "{saved}"
        );
        let read = read_card(&saved).unwrap().details.custom;
        assert_eq!(read.len(), 1, "{saved}");
        assert_eq!(read[0].value, "bad");
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

#[test]
fn version_three_cards_keep_multiple_groups_phones_and_a_birthday() {
    let groups = read_card("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:demo-groups\r\nFN:Demo Groups\r\nEMAIL;TYPE=HOME:groups@example.org\r\nTEL;TYPE=CELL:+1 202 555 0100\r\nCATEGORIES:Friends,Colleagues\r\nX-ABLABEL:Personal\r\nPHOTO;TYPE=JPEG;ENCODING=BASE64:/9j/2Q==\r\nEND:VCARD\r\n").unwrap();
    assert_eq!(groups.details.groups, ["Friends", "Colleagues"]);
    let phones = read_card("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:demo-phones\r\nFN:Demo Phones\r\nTEL;TYPE=cell,pref:+1 202 555 0101\r\nTEL;TYPE=cell:+1 202 555 0102\r\nBDAY:1985-04-23\r\nPHOTO;ENCODING=b;TYPE=jpeg:/9j/2Q==\r\nEND:VCARD\r\n").unwrap();
    assert_eq!(
        phones.details.phones,
        ["+1 202 555 0101", "+1 202 555 0102"]
    );
    assert_eq!(phones.details.birthday, "1985-04-23");
    assert!(matches!(phones.photo, Some(Photo::Inline(_))));
}

#[test]
fn category_lists_split_escaped_commas_and_drop_empty_or_duplicate_groups() {
    for version in ["3.0", "4.0"] {
        let text = format!(
            "BEGIN:VCARD\r\nVERSION:{version}\r\nFN:Demo\r\nCATEGORIES:Friends\\,Work\\,starred\r\nCATEGORIES:Work, ,Friends\r\nBDAY:--02-29\r\nEND:VCARD\r\n"
        );
        let card = read_card(&text).unwrap();
        assert_eq!(card.details.groups, ["Friends", "Work", "starred"]);
        assert_eq!(card.details.birthday, "--02-29");
        let saved = patch_card(
            &text,
            &CardFields {
                details: mailrs_domain::contact::Changes {
                    groups: Some(card.details.groups.clone()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            read_card(&saved).unwrap().details.groups,
            card.details.groups
        );
    }
}

#[test]
fn editing_another_field_preserves_full_and_yearless_birthdays() {
    for version in ["3.0", "4.0"] {
        for (source, expected) in [
            ("1985-04-23", "1985-04-23"),
            ("19850423", "1985-04-23"),
            ("--02-29", "--02-29"),
            ("--0229", "--02-29"),
            ("--02-\r\n 29", "--02-29"),
        ] {
            let text = format!(
                "BEGIN:VCARD\r\nVERSION:{version}\r\nFN:Demo\r\nitem1.BDAY;VALUE=date:{source}\r\nEND:VCARD\r\n"
            );
            assert_eq!(read_card(&text).unwrap().details.birthday, expected);
            let saved = patch_card(
                &text,
                &CardFields {
                    name: Some("Changed".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(read_card(&saved).unwrap().details.birthday, expected);
            let saved = patch_card(
                &saved,
                &CardFields {
                    details: mailrs_domain::contact::Changes {
                        birthday: Some(expected.into()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(read_card(&saved).unwrap().details.birthday, expected);
        }
    }
}

#[test]
fn a_street_with_a_newline_keeps_every_address_part_in_place() {
    use mailrs_domain::contact::Changes;
    let text = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Demo\r\nADR;TYPE=HOME:;;12 Main St\\nFlat 4;Lisbon;;1000-001;Portugal\r\nEND:VCARD\r\n";
    let home = [
        "",
        "",
        "12 Main St\nFlat 4",
        "Lisbon",
        "",
        "1000-001",
        "Portugal",
    ]
    .map(String::from);
    assert_eq!(
        read_card(text).unwrap().details.addresses,
        std::slice::from_ref(&home)
    );
    let work = ["", "", "1 Work Road", "Porto", "", "", "Portugal"].map(String::from);
    let saved = patch_card(
        text,
        &CardFields {
            details: Changes {
                addresses: Some(vec![home.clone(), work.clone()]),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(read_card(&saved).unwrap().details.addresses, [home, work]);
}

#[test]
fn an_apple_birthday_without_a_year_reads_as_yearless_and_stays_as_written() {
    let text = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Demo\r\nBDAY;X-APPLE-OMIT-YEAR=1604:1604-03-15\r\nEND:VCARD\r\n";
    assert_eq!(read_card(text).unwrap().details.birthday, "--03-15");
    let saved = patch_card(
        text,
        &CardFields {
            name: Some("Changed".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        saved.contains("BDAY;X-APPLE-OMIT-YEAR=1604:1604-03-15"),
        "{saved}"
    );
}

#[test]
fn custom_fields_save_on_a_card_with_two_social_profiles() {
    use mailrs_domain::contact::{Changes, Field};
    let text = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Demo\r\nX-SOCIALPROFILE;TYPE=twitter:x-apple:demo\r\nX-SOCIALPROFILE;TYPE=mastodon:x-apple:demo2\r\nEND:VCARD\r\n";
    let mut custom = read_card(text).unwrap().details.custom;
    assert_eq!(custom.len(), 2);
    custom.push(Field {
        label: "X-CUSTOMER-ID".into(),
        value: "7".into(),
    });
    let saved = patch_card(
        text,
        &CardFields {
            details: Changes {
                custom: Some(custom.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(read_card(&saved).unwrap().details.custom, custom);
    assert!(saved.contains("TYPE=twitter"), "{saved}");
}

#[test]
fn a_custom_field_under_a_typed_label_saves_edits_and_goes() {
    use mailrs_domain::contact::{Changes, Field};
    let text = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Demo\r\nX-SOCIALPROFILE:demo\r\nitem1.X-ABDATE:2015-06-20\r\nitem1.X-ABLABEL:Met\r\nEND:VCARD\r\n";
    let field = |label: &str, value: &str| Field {
        label: label.into(),
        value: value.into(),
    };
    let with = |custom: Vec<Field>| CardFields {
        details: Changes {
            custom: Some(custom),
            ..Default::default()
        },
        ..Default::default()
    };
    let social = field("X-SOCIALPROFILE", "demo");
    let added = patch_card(text, &with(vec![social.clone(), field("Badge", "42")])).unwrap();
    let read = read_card(&added).unwrap().details;
    assert_eq!(
        read.custom,
        [social.clone(), field("Badge", "42")],
        "{added}"
    );
    assert_eq!(read.dates, [field("Met", "2015-06-20")], "{added}");

    let edited = patch_card(&added, &with(vec![social.clone(), field("Badge", "43")])).unwrap();
    assert_eq!(
        read_card(&edited).unwrap().details.custom,
        [social.clone(), field("Badge", "43")],
        "{edited}"
    );

    let removed = patch_card(&edited, &with(vec![social.clone()])).unwrap();
    let read = read_card(&removed).unwrap().details;
    assert_eq!(read.custom, [social], "{removed}");
    assert_eq!(
        read.dates,
        [field("Met", "2015-06-20")],
        "the date keeps its label: {removed}"
    );
    assert_eq!(removed.matches("X-ABLABEL").count(), 1, "{removed}");
}

#[test]
fn removing_a_custom_field_with_a_reserved_label_keeps_the_dates() {
    use mailrs_domain::contact::{Changes, Field};
    let text = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Demo\r\nitem1.X-ABDATE:2015-06-20\r\nitem1.X-ABLABEL:Met\r\nEND:VCARD\r\n";
    let with = |custom: Vec<Field>| CardFields {
        details: Changes {
            custom: Some(custom),
            ..Default::default()
        },
        ..Default::default()
    };
    let added = patch_card(
        text,
        &with(vec![Field {
            label: "X-ABDATE".into(),
            value: "odd".into(),
        }]),
    )
    .unwrap();
    assert_eq!(
        read_card(&added).unwrap().details.custom.len(),
        1,
        "{added}"
    );
    let removed = patch_card(&added, &with(Vec::new())).unwrap();
    let read = read_card(&removed).unwrap().details;
    assert!(read.custom.is_empty(), "{removed}");
    assert_eq!(read.dates.len(), 1, "the date stays: {removed}");
    assert_eq!(read.dates[0].label, "Met", "{removed}");
}

#[test]
fn google_s_loose_labels_pair_in_order_and_go_with_their_values() {
    use mailrs_domain::contact::{Changes, Field};
    // Google's answer (2026-11-28) to two grouped labelled fields: the
    // groups are gone and each label sits before its value.
    let text = fixture("google-loose-labels.vcf");
    let field = |label: &str, value: &str| Field {
        label: label.into(),
        value: value.into(),
    };
    assert_eq!(
        read_card(&text).unwrap().details.custom,
        [field("One", "1"), field("Two", "2")]
    );
    let saved = patch_card(
        &text,
        &CardFields {
            details: Changes {
                custom: Some(vec![field("Two", "2")]),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        read_card(&saved).unwrap().details.custom,
        [field("Two", "2")],
        "{saved}"
    );
    assert_eq!(
        saved.to_uppercase().matches("X-ABLABEL").count(),
        1,
        "{saved}"
    );
}

#[test]
fn an_anniversary_is_a_labelled_date_in_version_3_and_its_own_line_in_4() {
    use mailrs_domain::contact::{Changes, Field};
    let anniversary = CardFields {
        details: Changes {
            dates: Some(vec![Field {
                label: "Anniversary".into(),
                value: "2015-06-20".into(),
            }]),
            ..Default::default()
        },
        ..Default::default()
    };
    let v3 = new_card("id", &anniversary);
    assert!(
        !v3.contains("ANNIVERSARY:"),
        "Google drops it from version 3: {v3}"
    );
    assert_eq!(
        read_card(&v3).unwrap().details.dates[0].label,
        "Anniversary"
    );
    let v4 = patch_card(&fixture("card-v4.vcf"), &anniversary).unwrap();
    assert!(v4.contains("ANNIVERSARY:"), "{v4}");
    assert_eq!(read_card(&v4).unwrap().details.dates[0].value, "2015-06-20");
    // Google keeps Apple's form and answers with its own group and spelling.
    let google = read_card(&fixture("google-abdate.vcf"))
        .unwrap()
        .details
        .dates;
    assert_eq!(google[0].label, "Anniversary");
    assert_eq!(google[0].value, "2015-06-20");
    let apple = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:A\r\nitem1.X-ABDATE:2015-06-20\r\nitem1.X-ABLABEL:_$!<Anniversary>!$_\r\nEND:VCARD\r\n";
    assert_eq!(
        read_card(apple).unwrap().details.dates[0].label,
        "Anniversary"
    );
}
