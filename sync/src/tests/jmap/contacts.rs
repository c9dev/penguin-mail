//! Contacts over FakeJmap.

use mailrs_gmail::ContactFields;
use mailrs_jmap::CONTACTS;

use super::over;
use crate::fake::FakeJmap;
use crate::{AnyContacts, ContactsService};

async fn contacts() -> (super::Harness, AnyContacts) {
    let h = over(FakeJmap::new().with_capability(CONTACTS)).await;
    let contacts = h
        .sync
        .services()
        .contacts
        .clone()
        .expect("the session offers contacts");
    (h, contacts)
}

#[tokio::test]
async fn a_first_read_brings_every_card_and_then_changes_alone() {
    let (h, contacts) = contacts().await;
    h.fake.add_card("Ada Lovelace", "ada@example.com");
    let page = contacts.connections(None, None).await.unwrap();
    assert_eq!(page.people[0].name.as_deref(), Some("Ada Lovelace"));
    assert_eq!(page.people[0].emails, ["ada@example.com"]);
    let token = page
        .next_sync_token
        .expect("a sync token after the last page");
    let grace = h.fake.add_card("Grace Hopper", "grace@example.com");
    let changed = contacts.connections(None, Some(&token)).await.unwrap();
    assert_eq!(
        changed
            .people
            .iter()
            .map(|p| p.resource.clone())
            .collect::<Vec<_>>(),
        [grace]
    );
}

#[tokio::test]
async fn a_card_added_during_a_long_read_comes_in_the_next_changes() {
    let (h, contacts) = contacts().await;
    for i in 0..201 {
        h.fake
            .add_card(&format!("Person {i}"), &format!("p{i}@example.com"));
    }
    let first = contacts.connections(None, None).await.unwrap();
    assert_eq!(first.people.len(), 200);
    let late = h.fake.add_card("Late", "late@example.com");
    let rest = contacts
        .connections(first.next_page_token.as_deref(), None)
        .await
        .unwrap();
    let token = rest
        .next_sync_token
        .expect("the read ends on the second page");
    let changed = contacts.connections(None, Some(&token)).await.unwrap();
    assert!(
        changed.people.iter().any(|p| p.resource == late),
        "the sync token must predate the card added mid-read"
    );
}

#[tokio::test]
async fn a_new_contact_goes_into_the_default_book() {
    let (h, contacts) = contacts().await;
    let fields = ContactFields {
        name: Some("Ada".into()),
        emails: Some(vec!["ada@example.com".into()]),
        phones: Some(vec!["+44 20".into()]),
        organization: Some("Engines".into()),
        ..Default::default()
    };
    let made = contacts.create_contact(&fields).await.unwrap();
    assert_eq!(made.organization.as_deref(), Some("Engines"));
    assert_eq!(made.phone.as_deref(), Some("+44 20"));
    let card = h.fake.with(|s| s.cards[&made.resource].clone());
    assert_eq!(card["addressBookIds"]["AB1"], true);
    assert_eq!(card["@type"], "Card");
}

#[tokio::test]
async fn an_edit_changes_only_the_fields_it_names() {
    let (h, contacts) = contacts().await;
    let id = h.fake.add_card("Ada", "ada@example.com");
    let fields = ContactFields {
        organization: Some("Engines".into()),
        ..ContactFields::default()
    };
    let changed = contacts.update_contact(&id, &fields).await.unwrap();
    assert_eq!(changed.name.as_deref(), Some("Ada"));
    assert_eq!(changed.emails, ["ada@example.com"]);
    assert_eq!(changed.organization.as_deref(), Some("Engines"));
}

#[tokio::test]
async fn a_forgotten_state_asks_for_a_whole_read() {
    let (h, contacts) = contacts().await;
    let token = contacts
        .connections(None, None)
        .await
        .unwrap()
        .next_sync_token
        .unwrap();
    h.fake.add_card("Ada", "ada@example.com");
    h.fake.forget_changes();
    assert!(matches!(
        contacts.connections(None, Some(&token)).await,
        Err(crate::BackendError::StateLost)
    ));
}

/// A card made elsewhere, with more than an edit can see: a split name,
/// tagged addresses, two phones and an organization with a department.
fn rich_card(h: &super::Harness) -> String {
    h.fake.with(|s| {
        s.cards.insert(
            "K9".into(),
            serde_json::json!({
                "id": "K9", "@type": "Card", "addressBookIds": { "AB1": true },
                "name": { "components": [{ "kind": "given", "value": "Ada" }, { "kind": "surname", "value": "Lovelace" }] },
                "emails": {
                    "work": { "address": "ada@work.example", "contexts": { "work": true }, "pref": 1 },
                    "home": { "address": "ada@home.example", "contexts": { "private": true } }
                },
                "phones": { "p1": { "number": "+351 210 000 000" }, "p2": { "number": "+351 910 000 000", "features": { "mobile": true } } },
                "organizations": { "o1": { "name": "Analytical Engines", "units": [{ "name": "Research" }] } }
            }),
        );
    });
    "K9".into()
}

#[tokio::test]
async fn adding_an_email_keeps_everything_else_on_the_card() {
    let (h, contacts) = contacts().await;
    let id = rich_card(&h);
    let fields = ContactFields {
        emails: Some(vec![
            "ada@work.example".into(),
            "ada@home.example".into(),
            "ada@new.example".into(),
        ]),
        ..ContactFields::default()
    };
    contacts.update_contact(&id, &fields).await.unwrap();
    let card = h.fake.with(|s| s.cards["K9"].clone());
    assert_eq!(card["name"]["components"][1]["value"], "Lovelace", "{card}");
    assert_eq!(card["emails"]["work"]["contexts"]["work"], true, "{card}");
    assert_eq!(
        card["emails"]["home"]["contexts"]["private"], true,
        "{card}"
    );
    let addresses: Vec<&str> = card["emails"]
        .as_object()
        .unwrap()
        .values()
        .filter_map(|e| e["address"].as_str())
        .collect();
    assert!(addresses.contains(&"ada@new.example"), "{card}");
    assert_eq!(card["phones"]["p2"]["number"], "+351 910 000 000", "{card}");
    assert_eq!(
        card["organizations"]["o1"]["units"][0]["name"], "Research",
        "{card}"
    );
}

#[tokio::test]
async fn an_edit_removes_and_renames_only_what_it_was_shown() {
    let (h, contacts) = contacts().await;
    let id = rich_card(&h);
    let fields = ContactFields {
        emails: Some(vec!["ada@work.example".into()]),
        phones: Some(vec!["+351 220 000 000".into()]),
        organization: Some("Difference Engines".into()),
        ..ContactFields::default()
    };
    contacts.update_contact(&id, &fields).await.unwrap();
    let card = h.fake.with(|s| s.cards["K9"].clone());
    assert!(card["emails"].get("home").is_none(), "{card}");
    assert_eq!(card["emails"]["work"]["pref"], 1, "{card}");
    // The edit saw only the first phone; the mobile it never saw stays.
    assert!(card["phones"].get("p1").is_none(), "{card}");
    assert_eq!(card["phones"]["p2"]["features"]["mobile"], true, "{card}");
    let numbers: Vec<&str> = card["phones"]
        .as_object()
        .unwrap()
        .values()
        .filter_map(|p| p["number"].as_str())
        .collect();
    assert!(numbers.contains(&"+351 220 000 000"), "{card}");
    assert_eq!(
        card["organizations"]["o1"]["name"], "Difference Engines",
        "{card}"
    );
    assert_eq!(
        card["organizations"]["o1"]["units"][0]["name"], "Research",
        "{card}"
    );
}

#[tokio::test]
async fn an_unchanged_name_keeps_its_parts() {
    let (h, contacts) = contacts().await;
    let id = rich_card(&h);
    let fields = ContactFields {
        name: Some("Ada Lovelace".into()),
        ..ContactFields::default()
    };
    contacts.update_contact(&id, &fields).await.unwrap();
    let card = h.fake.with(|s| s.cards["K9"].clone());
    assert_eq!(card["name"]["components"][0]["value"], "Ada", "{card}");
}

#[tokio::test]
async fn every_contact_field_goes_to_the_server_and_comes_back() {
    use mailrs_domain::contact::{Changes, Field};
    let (_h, contacts) = contacts().await;
    let made = contacts
        .create_contact(&ContactFields {
            name: Some("Priya Raman".into()),
            emails: Some(vec!["priya@fernwood.example".into()]),
            details: Changes {
                birthday: Some("1990-04-12".into()),
                notes: Some("Met in March".into()),
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
                addresses: Some(vec![[
                    String::new(),
                    String::new(),
                    "1 Main Street".into(),
                    "Lisbon".into(),
                    String::new(),
                    "1000-001".into(),
                    "Portugal".into(),
                ]]),
            },
            photo: Some(vec![1, 2, 3]),
            ..ContactFields::default()
        })
        .await
        .unwrap();
    let page = contacts.connections(None, None).await.unwrap();
    let read = page
        .people
        .iter()
        .find(|p| p.resource == made.resource)
        .expect("the card is listed");
    assert_eq!(read.details.birthday, "1990-04-12");
    assert_eq!(read.details.groups, ["Friends"]);
    assert_eq!(read.details.dates[0].value, "2015-06-20");
    assert_eq!(read.details.custom[0].label, "Badge");
    assert_eq!(read.details.addresses[0][3], "Lisbon");
    let photo = contacts
        .contact_photo(read.photo_url.as_deref().unwrap())
        .await
        .unwrap();
    assert_eq!(photo, [1, 2, 3]);
    let edited = contacts
        .update_contact(
            &made.resource,
            &ContactFields {
                details: Changes {
                    notes: Some(String::new()),
                    groups: Some(vec![]),
                    ..Changes::default()
                },
                ..ContactFields::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(edited.details.notes, "");
    assert!(edited.details.groups.is_empty());
    assert_eq!(edited.details.birthday, "1990-04-12", "the rest stays");
    contacts.delete_contact(&made.resource, None).await.unwrap();
    let after = contacts.connections(None, None).await.unwrap();
    assert!(after.people.iter().all(|p| p.resource != made.resource));
}
