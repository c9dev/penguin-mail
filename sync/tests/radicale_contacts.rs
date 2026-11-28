//! The CardDAV contacts adapter over the real client against Radicale: a
//! card with every field the editor has is created, read back, edited,
//! cleared field by field and deleted, and a birthday without a year
//! survives the trip.
//!
//! One test, because it points `SSL_CERT_FILE` at a root made for the
//! run, and the environment belongs to the whole process.

use std::sync::Arc;

use mailrs_dav::{DavClient, Login};
use mailrs_domain::contact::{Changes, Field};
use mailrs_gmail::ContactFields;
use mailrs_sync::ContactsService;
use mailrs_sync::services::CardDav;
use mailrs_testmail::{Certs, Radicale};

const USER: &str = "me@example.test";

#[test]
fn the_carddav_adapter_keeps_every_field_on_radicale() {
    if mailrs_testmail::docker().is_none() {
        return;
    }
    let Some(certs) = Certs::make() else { return };
    // SAFETY: the only test in this binary, before any thread starts.
    unsafe { mailrs_testmail::trust(&certs.root()) };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(mailrs_sync::WORKER_STACK)
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let password = mailrs_testmail::password();
        let Some(radicale) = Radicale::start(&certs, USER, &password).await else {
            return;
        };
        radicale.make_address_book(USER, &password, "people").await;
        let client = DavClient::new(
            &radicale.url(),
            mailrs_dav::Kind::AddressBook,
            Login::new(USER, &password),
        )
        .expect("a client");
        let contacts = CardDav::new(Arc::new(client));

        let fields = ContactFields {
            name: Some("Priya Raman".into()),
            emails: Some(vec!["priya@fernwood.example".into()]),
            phones: Some(vec!["+351 910 000 000".into()]),
            organization: Some("Fernwood".into()),
            photo: Some(vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10]),
            details: Changes {
                birthday: Some("1990-04-12".into()),
                notes: Some("Met in March".into()),
                addresses: Some(vec![[
                    String::new(),
                    String::new(),
                    "1 Main Street".into(),
                    "Lisbon".into(),
                    "LX".into(),
                    "1000-001".into(),
                    "Portugal".into(),
                ]]),
                urls: Some(vec!["https://example.org".into()]),
                groups: Some(vec!["Friends".into()]),
                dates: Some(vec![Field {
                    label: "Anniversary".into(),
                    value: "2015-06-20".into(),
                }]),
                custom: Some(vec![Field {
                    label: "Badge".into(),
                    value: "42".into(),
                }]),
            },
            ..Default::default()
        };
        let made = contacts.create_contact(&fields).await.expect("a card");
        let page = contacts.connections(None, None).await.expect("the cards");
        let read = page
            .people
            .iter()
            .find(|p| p.resource == made.resource)
            .expect("Radicale lists the card");
        assert_eq!(read.name.as_deref(), Some("Priya Raman"));
        assert_eq!(read.emails, ["priya@fernwood.example"]);
        assert_eq!(read.phone.as_deref(), Some("+351 910 000 000"));
        assert_eq!(read.organization.as_deref(), Some("Fernwood"));
        assert!(read.photo_url.is_some(), "the photo is kept: {read:?}");
        let d = &read.details;
        assert_eq!(d.birthday, "1990-04-12");
        assert_eq!(d.notes, "Met in March");
        assert_eq!(d.urls, ["https://example.org"]);
        assert_eq!(d.groups, ["Friends"]);
        assert_eq!(d.addresses.len(), 1, "{d:?}");
        assert_eq!(d.addresses[0][2], "1 Main Street");
        assert_eq!(d.addresses[0][3], "Lisbon");
        assert_eq!(d.addresses[0][6], "Portugal");
        assert_eq!(d.dates[0].label, "Anniversary");
        assert_eq!(d.dates[0].value, "2015-06-20");
        assert_eq!(d.custom[0].label, "Badge");
        assert_eq!(d.custom[0].value, "42");

        // An edit touches only what it names.
        let edited = contacts
            .update_contact(
                &made.resource,
                &ContactFields {
                    details: Changes {
                        notes: Some("Changed".into()),
                        birthday: Some("--04-12".into()),
                        groups: Some(vec!["Friends".into(), "Work".into()]),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
            .expect("an edit");
        assert_eq!(edited.details.notes, "Changed");
        assert_eq!(edited.details.birthday, "--04-12");
        assert_eq!(edited.details.groups, ["Friends", "Work"]);
        assert_eq!(edited.details.urls, ["https://example.org"]);
        assert_eq!(edited.organization.as_deref(), Some("Fernwood"));

        // Clearing a field removes it from the card.
        contacts
            .update_contact(
                &made.resource,
                &ContactFields {
                    organization: Some(String::new()),
                    details: Changes {
                        addresses: Some(Vec::new()),
                        urls: Some(Vec::new()),
                        dates: Some(Vec::new()),
                        custom: Some(Vec::new()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
            .expect("an edit that clears");
        let page = contacts.connections(None, None).await.expect("the cards");
        let cleared = page
            .people
            .iter()
            .find(|p| p.resource == made.resource)
            .expect("the card is still there");
        assert!(
            cleared
                .organization
                .as_deref()
                .unwrap_or_default()
                .is_empty(),
            "{cleared:?}"
        );
        let d = &cleared.details;
        assert!(d.addresses.is_empty(), "{d:?}");
        assert!(d.urls.is_empty(), "{d:?}");
        assert!(d.dates.is_empty(), "{d:?}");
        assert!(d.custom.is_empty(), "{d:?}");
        assert_eq!(d.birthday, "--04-12", "the birthday is kept");
        assert_eq!(d.notes, "Changed");

        contacts
            .delete_contact(&made.resource, None)
            .await
            .expect("a delete");
        let after = contacts.connections(None, None).await.expect("the cards");
        assert!(after.people.iter().all(|p| p.resource != made.resource));
    });
}
