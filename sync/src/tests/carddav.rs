//! The CardDAV adapter against FakeDav, and the address book reading it.

use std::sync::Arc;

use mailrs_dav::Kind;
use mailrs_dav::fake::FakeDav;
use mailrs_gmail::ContactFields;

use crate::services::CardDav;
use crate::{BackendError, ContactsService};

fn card(uid: &str, name: &str, email: &str) -> String {
    format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\nEMAIL;TYPE=INTERNET:{email}\r\nEND:VCARD\r\n"
    )
}

fn adapter() -> (Arc<FakeDav>, CardDav<FakeDav>) {
    let dav = Arc::new(FakeDav::new());
    dav.add_collection("/card/default/", Kind::AddressBook, "Contacts", None);
    dav.add_collection("/card/work/", Kind::AddressBook, "Work", None);
    (Arc::clone(&dav), CardDav::new(dav))
}

async fn read_all(
    carddav: &CardDav<FakeDav>,
    token: Option<&str>,
) -> Result<mailrs_gmail::ConnectionsPage, BackendError> {
    let mut all = mailrs_gmail::ConnectionsPage::default();
    let mut page: Option<String> = None;
    loop {
        let got = carddav.connections(page.as_deref(), token).await?;
        all.people.extend(got.people);
        all.deleted.extend(got.deleted);
        match got.next_page_token {
            Some(next) => page = Some(next),
            None => {
                all.next_sync_token = got.next_sync_token;
                return Ok(all);
            }
        }
    }
}

#[tokio::test]
async fn every_address_book_reads_as_one() {
    let (dav, carddav) = adapter();
    dav.put_resource(
        "/card/default/ana.vcf",
        &card("a", "Ana Rocha", "ana@example.pt"),
    );
    dav.put_resource(
        "/card/work/bruno.vcf",
        &card("b", "Bruno Dias", "bruno@example.org"),
    );
    let read = read_all(&carddav, None).await.unwrap();
    let names: Vec<_> = read.people.iter().filter_map(|p| p.name.clone()).collect();
    assert_eq!(names, ["Ana Rocha", "Bruno Dias"]);
    let token: serde_json::Value =
        serde_json::from_str(read.next_sync_token.as_deref().unwrap()).unwrap();
    assert!(token.get("/card/work/").is_some());
}

#[tokio::test]
async fn a_server_that_always_says_more_is_asked_a_bounded_number_of_times() {
    let (dav, carddav) = adapter();
    let first = read_all(&carddav, None).await.unwrap();
    dav.with(|s| s.always_more = true);
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        read_all(&carddav, first.next_sync_token.as_deref()),
    )
    .await
    .expect("the read ends");
    assert!(read.is_ok(), "{read:?}");
    assert!(dav.with(|s| s.syncs) <= 2 * 20, "{}", dav.with(|s| s.syncs));
}

#[tokio::test]
async fn a_later_read_brings_changes_and_deletions_from_each_book() {
    let (dav, carddav) = adapter();
    dav.put_resource(
        "/card/default/ana.vcf",
        &card("a", "Ana Rocha", "ana@example.pt"),
    );
    let first = read_all(&carddav, None).await.unwrap();
    dav.put_resource(
        "/card/work/carla.vcf",
        &card("c", "Carla Nunes", "carla@example.com"),
    );
    dav.remove_resource("/card/default/ana.vcf");
    let next = read_all(&carddav, first.next_sync_token.as_deref())
        .await
        .unwrap();
    assert_eq!(next.people.len(), 1);
    assert_eq!(next.deleted, ["/card/default/ana.vcf"]);
}

#[tokio::test]
async fn an_address_book_that_came_or_went_reads_everything_again() {
    let (dav, carddav) = adapter();
    let first = read_all(&carddav, None).await.unwrap();
    dav.add_collection("/card/family/", Kind::AddressBook, "Family", None);
    assert!(matches!(
        read_all(&carddav, first.next_sync_token.as_deref()).await,
        Err(BackendError::StateLost)
    ));
}

#[tokio::test]
async fn a_new_contact_goes_into_the_first_book_and_an_edit_keeps_its_other_lines() {
    let (dav, carddav) = adapter();
    let made = carddav
        .create_contact(&ContactFields {
            name: Some("Dora Lima".into()),
            emails: Some(vec!["dora@example.com".into()]),
            ..ContactFields::default()
        })
        .await
        .unwrap();
    assert!(
        made.resource.starts_with("/card/default/"),
        "{}",
        made.resource
    );
    dav.put_resource(
        &made.resource,
        &dav.body(&made.resource)
            .unwrap()
            .replace("END:VCARD", "X-KEEP:yes\r\nEND:VCARD"),
    );
    let changed = carddav
        .update_contact(
            &made.resource,
            &ContactFields {
                organization: Some("Lima & Co".into()),
                ..ContactFields::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(changed.organization.as_deref(), Some("Lima & Co"));
    assert!(dav.body(&made.resource).unwrap().contains("X-KEEP:yes"));
}

#[tokio::test]
async fn an_inline_photo_comes_from_the_card_and_a_photo_url_is_never_fetched() {
    let (dav, carddav) = adapter();
    let jpeg = "/9j/4AAQSkZJRgABAQAAAQABAAD/2Q==";
    dav.put_resource("/card/default/e.vcf", &format!("BEGIN:VCARD\r\nVERSION:3.0\r\nFN:E\r\nPHOTO;ENCODING=b;TYPE=JPEG:{jpeg}\r\nEND:VCARD\r\n"));
    dav.put_resource("/card/default/f.vcf", "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:F\r\nPHOTO:https://tracker.example/f.jpg\r\nEND:VCARD\r\n");
    let read = read_all(&carddav, None).await.unwrap();
    let e = read
        .people
        .iter()
        .find(|p| p.name.as_deref() == Some("E"))
        .unwrap();
    let f = read
        .people
        .iter()
        .find(|p| p.name.as_deref() == Some("F"))
        .unwrap();
    assert_eq!(f.photo_url, None);
    let url = e.photo_url.clone().unwrap();
    assert!(url.starts_with("carddav-photo:"));
    assert!(
        carddav
            .contact_photo(&url)
            .await
            .unwrap()
            .starts_with(&[0xff, 0xd8])
    );
}

#[tokio::test]
async fn editing_a_contact_preserves_groups_and_rejects_stale_revisions() {
    use mailrs_domain::contact::Changes;
    let (dav, carddav) = adapter();
    let href = "/card/default/demo.vcf";
    dav.put_resource(href, &card("demo", "Demo Person", "demo@example.org"));
    dav.put_resource(
        "/card/default/group.vcf",
        &mailrs_dav::vcard::new_group("group", "Friends", "demo"),
    );
    let read = read_all(&carddav, None).await.unwrap();
    let person = read.people.iter().find(|p| p.resource == href).unwrap();
    let revision = person.details.revision.clone();
    let changed = carddav
        .update_contact(
            href,
            &ContactFields {
                expected_revision: Some(revision.clone()),
                details: Changes {
                    groups: Some(vec!["Colleagues".into()]),
                    birthday: Some("2000-02-29".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_ne!(changed.details.revision, revision);
    assert_eq!(
        mailrs_dav::vcard::read_card(&dav.body("/card/default/group.vcf").unwrap())
            .unwrap()
            .details
            .members,
        Some(vec![])
    );
    let read = read_all(&carddav, None).await.unwrap();
    assert!(
        read.people
            .iter()
            .any(|p| p.name.as_deref() == Some("Colleagues")
                && p.details.members.as_deref() == Some(&["demo".to_string()]))
    );
    let writes = dav.with(|s| s.puts);
    assert!(
        carddav
            .update_contact(
                href,
                &ContactFields {
                    expected_revision: Some(revision.clone()),
                    name: Some("Stale".into()),
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
    assert!(carddav.delete_contact(href, Some(&revision)).await.is_err());
    assert_eq!(dav.with(|s| s.puts), writes);
    carddav
        .delete_contact(href, Some(&changed.details.revision))
        .await
        .unwrap();
    assert!(dav.body(href).is_none());
}

#[tokio::test]
async fn retrying_contact_creation_reuses_its_resource() {
    let (dav, carddav) = adapter();
    let fields = ContactFields {
        creation_key: Some("12345678-1234-1234-1234-123456789abc".into()),
        name: Some("Demo".into()),
        ..Default::default()
    };
    let first = carddav.create_contact(&fields).await.unwrap();
    let second = carddav.create_contact(&fields).await.unwrap();
    assert_eq!(first.resource, second.resource);
    assert_eq!(read_all(&carddav, None).await.unwrap().people.len(), 1);
    assert!(dav.body(&first.resource).is_some());
    let external = dav
        .body(&first.resource)
        .unwrap()
        .replace("FN:Demo", "FN:Changed Elsewhere");
    dav.put_resource(&first.resource, &external);
    let writes = dav.with(|s| s.puts);
    assert!(carddav.create_contact(&fields).await.is_err());
    assert_eq!(dav.with(|s| s.puts), writes);
    assert_eq!(dav.body(&first.resource).unwrap(), external);
}

#[tokio::test]
async fn new_contacts_go_into_the_chosen_book() {
    let (dav, carddav) = adapter();
    assert_eq!(carddav.address_books().await.unwrap().len(), 2);
    let made = carddav
        .create_contact(&ContactFields {
            name: Some("Demo".into()),
            address_book: Some("/card/work/".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(made.resource.starts_with("/card/work/"));
    assert!(dav.body(&made.resource).is_some());
    let count = dav.with(|s| s.puts);
    assert!(
        carddav
            .create_contact(&ContactFields {
                address_book: Some("https://elsewhere.example/book/".into()),
                ..Default::default()
            })
            .await
            .is_err()
    );
    assert_eq!(dav.with(|s| s.puts), count);
}

#[tokio::test]
async fn a_membership_change_on_another_device_is_not_overwritten() {
    use mailrs_domain::contact::Changes;
    let (dav, carddav) = adapter();
    let href = "/card/default/demo.vcf";
    dav.put_resource(href, &card("demo", "Demo", "demo@example.org"));
    dav.put_resource(
        "/card/default/group.vcf",
        &mailrs_dav::vcard::new_group("group", "Friends", "someone-else"),
    );
    let read = read_all(&carddav, None).await.unwrap();
    let revision = read
        .people
        .iter()
        .find(|p| p.resource == href)
        .unwrap()
        .details
        .revision
        .clone();
    let writes = dav.with(|s| s.puts);
    let result = carddav
        .update_contact(
            href,
            &ContactFields {
                expected_revision: Some(revision),
                expected_groups: Some(vec!["Friends".into()]),
                details: Changes {
                    groups: Some(vec!["Colleagues".into()]),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
    assert!(result.is_err());
    assert_eq!(dav.with(|s| s.puts), writes);
    assert!(!dav.body(href).unwrap().contains("Colleagues"));
}

#[tokio::test]
async fn groups_with_the_same_name_do_not_gain_unwanted_members() {
    use mailrs_domain::contact::Changes;
    let (dav, carddav) = adapter();
    let href = "/card/default/demo.vcf";
    dav.put_resource(href, &card("demo", "Demo", "demo@example.org"));
    dav.put_resource(
        "/card/default/one.vcf",
        &mailrs_dav::vcard::new_group("one", "Friends", "demo"),
    );
    dav.put_resource(
        "/card/default/two.vcf",
        &mailrs_dav::vcard::new_group("two", "Friends", "someone-else"),
    );
    carddav
        .update_contact(
            href,
            &ContactFields {
                expected_groups: Some(vec!["Friends".into()]),
                details: Changes {
                    groups: Some(vec!["Friends".into(), "Colleagues".into()]),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        mailrs_dav::vcard::read_card(&dav.body("/card/default/two.vcf").unwrap())
            .unwrap()
            .details
            .members,
        Some(vec!["someone-else".into()])
    );
    let other = "/card/default/other.vcf";
    dav.put_resource(other, &card("other", "Other", "other@example.org"));
    let writes = dav.with(|s| s.puts);
    assert!(
        carddav
            .update_contact(
                other,
                &ContactFields {
                    expected_groups: Some(vec![]),
                    details: Changes {
                        groups: Some(vec!["Friends".into()]),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
    assert_eq!(dav.with(|s| s.puts), writes);
}

#[tokio::test]
async fn a_refused_contact_write_leaves_its_groups_alone() {
    use mailrs_domain::contact::Changes;
    let (dav, carddav) = adapter();
    let href = "/card/default/demo.vcf";
    dav.put_resource(href, &card("demo", "Demo", "demo@example.org"));
    let group = "/card/default/group.vcf";
    dav.put_resource(
        group,
        &mailrs_dav::vcard::new_group("group", "Friends", "demo"),
    );
    let before = dav.body(group).unwrap();
    dav.with(|s| s.changed_on_put.push(href.into()));
    let result = carddav
        .update_contact(
            href,
            &ContactFields {
                expected_groups: Some(vec!["Friends".into()]),
                details: Changes {
                    groups: Some(vec!["Colleagues".into()]),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
    assert!(result.is_err());
    assert_eq!(dav.body(group).unwrap(), before);
    assert_eq!(dav.with(|s| s.resources.len()), 2);
}

#[tokio::test]
async fn read_only_books_are_not_offered_and_their_cards_say_so() {
    let (dav, carddav) = adapter();
    dav.with(|s| {
        for c in &mut s.collections {
            if c.href == "/card/default/" {
                c.can_write = false;
            }
        }
    });
    dav.put_resource(
        "/card/default/ana.vcf",
        &card("a", "Ana Rocha", "ana@example.pt"),
    );
    dav.put_resource(
        "/card/work/bruno.vcf",
        &card("b", "Bruno Dias", "bruno@example.org"),
    );
    let books = carddav.address_books().await.unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].0, "/card/work/");
    let read = read_all(&carddav, None).await.unwrap();
    let only = |href: &str| {
        read.people
            .iter()
            .find(|p| p.resource == href)
            .unwrap()
            .details
            .read_only
    };
    assert!(only("/card/default/ana.vcf"));
    assert!(!only("/card/work/bruno.vcf"));
    let puts = dav.with(|s| s.puts);
    assert!(
        carddav
            .create_contact(&ContactFields {
                name: Some("Demo".into()),
                address_book: Some("/card/default/".into()),
                ..Default::default()
            })
            .await
            .is_err()
    );
    assert_eq!(dav.with(|s| s.puts), puts);
    let made = carddav
        .create_contact(&ContactFields {
            name: Some("Demo".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        made.resource.starts_with("/card/work/"),
        "{}",
        made.resource
    );
}

#[tokio::test]
async fn a_book_with_group_cards_keeps_group_names_out_of_categories() {
    use mailrs_domain::contact::Changes;
    let (dav, carddav) = adapter();
    let href = "/card/default/demo.vcf";
    dav.put_resource(
        href,
        &card("demo", "Demo", "demo@example.org")
            .replace("END:VCARD", "CATEGORIES:Friends,Old\r\nEND:VCARD"),
    );
    let friends = "/card/default/friends.vcf";
    dav.put_resource(
        friends,
        &mailrs_dav::vcard::new_group("friends", "Friends", "demo"),
    );
    carddav
        .update_contact(
            href,
            &ContactFields {
                expected_groups: Some(vec!["Friends".into(), "Old".into()]),
                details: Changes {
                    groups: Some(vec!["Friends".into(), "Colleagues".into()]),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let body = dav.body(href).unwrap();
    assert!(!body.contains("CATEGORIES"), "{body}");
    let members = |text: String| mailrs_dav::vcard::read_card(&text).unwrap().details.members;
    assert_eq!(
        members(dav.body(friends).unwrap()),
        Some(vec!["demo".into()])
    );
    let colleagues = dav.with(|s| {
        s.resources
            .values()
            .map(|(_, body)| body.clone())
            .find(|body| body.contains("FN:Colleagues"))
    });
    assert_eq!(members(colleagues.unwrap()), Some(vec!["demo".into()]));
}

/// What Google's CardDAV did to a card, from the live check of 2026-11-27
/// on the test account: the files under `golden/google_carddav/`.
mod google {
    use mailrs_dav::{DavApi, DavError, Precondition};
    use mailrs_domain::contact::Changes;

    use super::*;

    const BOOK: &str = "/card/lists/default/";

    fn google() -> (Arc<FakeDav>, CardDav<FakeDav>) {
        let dav = Arc::new(FakeDav::new());
        dav.add_collection(BOOK, Kind::AddressBook, "Address Book", None);
        dav.with(|s| s.google_quirks = true);
        (Arc::clone(&dav), CardDav::new(dav).google())
    }

    fn fields() -> ContactFields {
        ContactFields {
            name: Some("Live Check".into()),
            emails: Some(vec!["live.check@example.org".into()]),
            details: Changes {
                birthday: Some("1990-04-12".into()),
                urls: Some(vec!["https://example.org".into()]),
                groups: Some(vec!["Friends".into()]),
                ..Changes::default()
            },
            ..ContactFields::default()
        }
    }

    #[test]
    fn googles_answer_for_year_1604_reads_as_a_birthday_without_a_year() {
        // Sent as `BDAY:1604-04-12`, read back on 2026-11-28.
        let read = include_str!("golden/google_carddav/yearless_birthday.vcf");
        let card = mailrs_dav::vcard::read_card(read).unwrap();
        assert_eq!(card.details.birthday, "--04-12");
    }

    #[test]
    fn the_saved_answers_read_as_the_adapter_expects() {
        let read = include_str!("golden/google_carddav/read.vcf");
        let card = mailrs_dav::vcard::read_card(read).unwrap();
        assert_eq!(card.name.as_deref(), Some("Live Check"));
        assert_eq!(
            card.details.birthday, "1990-04-12",
            "BDAY;VALUE=DATE reads as a date"
        );
        assert_eq!(
            card.details.urls,
            ["https://example.org"],
            "the `\\:` is unescaped"
        );
        assert!(card.details.groups.is_empty(), "Google keeps no CATEGORIES");
        assert_eq!(card.details.addresses[0][2], "1 Long Street\nSecond line");
        assert!(card.details.notes.contains("live check"));
        assert!(
            card.details
                .custom
                .iter()
                .any(|f| f.label == "X-CUSTOMER-ID" && f.value == "DEMO-42"),
            "{:?}",
            card.details.custom
        );
        assert!(card.photo.is_some(), "the photo comes back as ENCODING=B");
        assert!(include_str!("golden/google_carddav/create.h").contains("location:"));
        assert!(include_str!("golden/google_carddav/edit.h").starts_with("HTTP/2 204"));
        assert!(include_str!("golden/google_carddav/gone.body").contains("NOT_FOUND"));
    }

    #[tokio::test]
    async fn a_new_contact_is_found_at_the_href_google_gave_it() {
        let (dav, carddav) = google();
        let made = carddav.create_contact(&fields()).await.unwrap();
        assert_ne!(made.resource, format!("{BOOK}pm-"), "{}", made.resource);
        assert!(
            !made.resource.contains("pm-"),
            "Google names the card itself"
        );
        assert_eq!(made.details.uid, made.resource.rsplit('/').next().unwrap());
        assert!(dav.body(&made.resource).is_some());
        // A look-up by the UID that was sent finds nothing.
        let sent = dav.body(&made.resource).unwrap();
        assert!(!sent.contains("pm-"));
        assert!(carddav.keeps_groups_here());
        assert!(!sent.contains("CATEGORIES"), "groups stay on this computer");
    }

    #[tokio::test]
    async fn retrying_a_create_updates_the_card_google_already_made() {
        let (dav, carddav) = google();
        let mut fields = fields();
        fields.creation_key = Some("12345678-1234-1234-1234-123456789abc".into());
        let first = carddav.create_contact(&fields).await.unwrap();
        fields.name = Some("Live Check Two".into());
        let second = carddav.create_contact(&fields).await.unwrap();
        assert_eq!(first.resource, second.resource);
        assert_eq!(second.name.as_deref(), Some("Live Check Two"));
        let held = dav.with(|s| s.resources.keys().filter(|k| k.starts_with(BOOK)).count());
        assert_eq!(held, 1, "no second card");
    }

    #[tokio::test]
    async fn an_edit_is_not_refused_when_the_etag_is_stale_and_says_so() {
        let (dav, carddav) = google();
        let made = carddav.create_contact(&fields()).await.unwrap();
        let stale = dav.etag(&made.resource).unwrap();
        assert!(
            stale.starts_with("\"2026-"),
            "the ETag is a timestamp: {stale}"
        );
        dav.put_resource(&made.resource, &dav.body(&made.resource).unwrap());
        // Google ignores If-Match: the stale ETag still overwrites.
        let api = DavApiHandle(&dav);
        assert!(api.put_stale(&made.resource, &stale).await.is_ok());
    }

    struct DavApiHandle<'a>(&'a Arc<FakeDav>);
    impl DavApiHandle<'_> {
        async fn put_stale(&self, href: &str, etag: &str) -> Result<(), DavError> {
            let body = self.0.body(href).unwrap();
            self.0
                .put(
                    href,
                    &body,
                    Kind::AddressBook,
                    Precondition::Match(etag.into()),
                )
                .await
                .map(drop)
        }
    }

    #[tokio::test]
    async fn a_name_edit_is_written_to_n_and_fn() {
        let (dav, carddav) = google();
        let made = carddav.create_contact(&fields()).await.unwrap();
        let edited = carddav
            .update_contact(
                &made.resource,
                &ContactFields {
                    name: Some("Live Check Edited".into()),
                    ..ContactFields::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(edited.name.as_deref(), Some("Live Check Edited"));
        let body = dav.body(&made.resource).unwrap();
        assert!(body.contains("N:Edited;Live Check"), "{body}");
    }

    #[tokio::test]
    async fn a_birthday_without_a_year_reaches_google_as_apples_year_1604() {
        let (dav, carddav) = google();
        let made = carddav.create_contact(&fields()).await.unwrap();
        let edited = carddav
            .update_contact(
                &made.resource,
                &ContactFields {
                    details: Changes {
                        birthday: Some("--04-12".into()),
                        ..Changes::default()
                    },
                    ..ContactFields::default()
                },
            )
            .await
            .unwrap();
        let body = dav.body(&edited.resource).unwrap();
        assert!(body.contains("X-APPLE-OMIT-YEAR=1604:1604-04-12"), "{body}");
        let kept = carddav.connections(None, None).await.unwrap();
        let birthday = &kept.people[0].details.birthday;
        assert_eq!(mailrs_domain::contact::date_for_edit(birthday), "--04-12");
    }

    #[tokio::test]
    async fn a_delete_leaves_nothing_to_read() {
        let (dav, carddav) = google();
        let made = carddav.create_contact(&fields()).await.unwrap();
        carddav.delete_contact(&made.resource, None).await.unwrap();
        assert!(matches!(
            dav.get(&made.resource).await,
            Err(DavError::NotFound)
        ));
    }
}
