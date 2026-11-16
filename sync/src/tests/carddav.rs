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
