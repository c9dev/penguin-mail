//! The CardDAV adapter against FakeDav, and the address book reading it.

use std::sync::Arc;

use mailrs_dav::fake::FakeDav;
use mailrs_dav::Kind;
use mailrs_gmail::ContactFields;

use crate::services::CardDav;
use crate::{BackendError, ContactsService};

fn card(uid: &str, name: &str, email: &str) -> String {
    format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\nEMAIL;TYPE=INTERNET:{email}\r\nEND:VCARD\r\n")
}

fn adapter() -> (Arc<FakeDav>, CardDav<FakeDav>) {
    let dav = Arc::new(FakeDav::new());
    dav.add_collection("/card/default/", Kind::AddressBook, "Contacts", None);
    dav.add_collection("/card/work/", Kind::AddressBook, "Work", None);
    (Arc::clone(&dav), CardDav::new(dav))
}

async fn read_all(carddav: &CardDav<FakeDav>, token: Option<&str>) -> Result<mailrs_gmail::ConnectionsPage, BackendError> {
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
    dav.put_resource("/card/default/ana.vcf", &card("a", "Ana Rocha", "ana@example.pt"));
    dav.put_resource("/card/work/bruno.vcf", &card("b", "Bruno Dias", "bruno@example.org"));
    let read = read_all(&carddav, None).await.unwrap();
    let names: Vec<_> = read.people.iter().filter_map(|p| p.name.clone()).collect();
    assert_eq!(names, ["Ana Rocha", "Bruno Dias"]);
    let token: serde_json::Value = serde_json::from_str(read.next_sync_token.as_deref().unwrap()).unwrap();
    assert!(token.get("/card/work/").is_some());
}

#[tokio::test]
async fn a_server_that_always_says_more_is_asked_a_bounded_number_of_times() {
    let (dav, carddav) = adapter();
    let first = read_all(&carddav, None).await.unwrap();
    dav.with(|s| s.always_more = true);
    let read = tokio::time::timeout(std::time::Duration::from_secs(5), read_all(&carddav, first.next_sync_token.as_deref()))
        .await
        .expect("the read ends");
    assert!(read.is_ok(), "{read:?}");
    assert!(dav.with(|s| s.syncs) <= 2 * 20, "{}", dav.with(|s| s.syncs));
}

#[tokio::test]
async fn a_later_read_brings_changes_and_deletions_from_each_book() {
    let (dav, carddav) = adapter();
    dav.put_resource("/card/default/ana.vcf", &card("a", "Ana Rocha", "ana@example.pt"));
    let first = read_all(&carddav, None).await.unwrap();
    dav.put_resource("/card/work/carla.vcf", &card("c", "Carla Nunes", "carla@example.com"));
    dav.remove_resource("/card/default/ana.vcf");
    let next = read_all(&carddav, first.next_sync_token.as_deref()).await.unwrap();
    assert_eq!(next.people.len(), 1);
    assert_eq!(next.deleted, ["/card/default/ana.vcf"]);
}

#[tokio::test]
async fn an_address_book_that_came_or_went_reads_everything_again() {
    let (dav, carddav) = adapter();
    let first = read_all(&carddav, None).await.unwrap();
    dav.add_collection("/card/family/", Kind::AddressBook, "Family", None);
    assert!(matches!(read_all(&carddav, first.next_sync_token.as_deref()).await, Err(BackendError::StateLost)));
}

#[tokio::test]
async fn a_new_contact_goes_into_the_first_book_and_an_edit_keeps_its_other_lines() {
    let (dav, carddav) = adapter();
    let made = carddav
        .create_contact(&ContactFields { name: Some("Dora Lima".into()), emails: Some(vec!["dora@example.com".into()]), ..ContactFields::default() })
        .await
        .unwrap();
    assert!(made.resource.starts_with("/card/default/"), "{}", made.resource);
    dav.put_resource(&made.resource, &dav.body(&made.resource).unwrap().replace("END:VCARD", "X-KEEP:yes\r\nEND:VCARD"));
    let changed = carddav.update_contact(&made.resource, &ContactFields { organization: Some("Lima & Co".into()), ..ContactFields::default() }).await.unwrap();
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
    let e = read.people.iter().find(|p| p.name.as_deref() == Some("E")).unwrap();
    let f = read.people.iter().find(|p| p.name.as_deref() == Some("F")).unwrap();
    assert_eq!(f.photo_url, None);
    let url = e.photo_url.clone().unwrap();
    assert!(url.starts_with("carddav-photo:"));
    assert!(carddav.contact_photo(&url).await.unwrap().starts_with(&[0xff, 0xd8]));
}
