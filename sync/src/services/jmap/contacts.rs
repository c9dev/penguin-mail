//! The account's address book over JMAP for Contacts (RFC 9610): cards in
//! JSContact, read a page at a time and then by their changes. A new card
//! goes into the default address book; an edit reads the card and sends
//! only the fields and entries it changes, so the rest of a card the
//! server holds stays as it was.

use std::collections::HashSet;

use mailrs_gmail::{ConnectionsPage, ContactFields, Person};
use mailrs_jmap::CardWrite;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{Jmap, JmapApi, backend};
use crate::BackendError;
use crate::services::ContactsService;

const CARDS_PER_PAGE: usize = 200;

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum PageToken {
    Reading { position: u64, state: String },
    Changing { since: String },
}

fn values<'a>(card: &'a Value, field: &str) -> impl Iterator<Item = &'a Value> + 'a {
    card.get(field)
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|m| m.values())
}

pub(crate) fn person_of(card: &Value) -> Person {
    let name = card["name"]["full"]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            let parts: Vec<&str> = card["name"]["components"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c["value"].as_str())
                .collect();
            (!parts.is_empty()).then(|| parts.join(" "))
        });
    let mut details = details_of(card);
    details.phones = values(card, "phones")
        .filter_map(|p| p["number"].as_str().map(str::to_string))
        .collect();
    Person {
        photo_url: values(card, "media")
            .find(|m| m["kind"] == "photo")
            .and_then(|m| m["uri"].as_str().map(str::to_string)),
        details,
        resource: card["id"].as_str().unwrap_or_default().to_string(),
        name,
        emails: values(card, "emails")
            .filter_map(|e| e["address"].as_str().map(str::to_string))
            .collect(),
        organization: values(card, "organizations")
            .find_map(|o| o["name"].as_str().map(str::to_string)),
        phone: values(card, "phones").find_map(|p| p["number"].as_str().map(str::to_string)),
    }
}

/// The label of the link that holds what JSContact has no home for: other
/// dates with their labels, and custom fields, as a JSON data URI. Seen
/// live: Cyrus and Stalwart both refuse a vendor property, so the extras
/// ride in a standard `links` entry.
const EXTRAS: &str = "mail.penguin:fields";

#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct Extras {
    dates: Vec<mailrs_domain::contact::Field>,
    custom: Vec<mailrs_domain::contact::Field>,
}

fn extras_of(card: &Value) -> Extras {
    use base64::Engine;
    values(card, "links")
        .filter(|l| l["label"] == EXTRAS)
        .find_map(|l| {
            let data = l["uri"].as_str()?.split_once(";base64,")?.1;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .ok()?;
            serde_json::from_slice(&bytes).ok()
        })
        .unwrap_or_default()
}

/// A contact date as a JSContact `PartialDate`.
fn date_json(value: &str) -> Option<Value> {
    let (year, month, day) = mailrs_domain::contact::date_parts(value)?;
    let mut date = json!({ "@type": "PartialDate", "month": month, "day": day });
    if let Some(year) = year {
        date["year"] = json!(year);
    }
    Some(date)
}

/// A `PartialDate` or `Timestamp` as `YYYY-MM-DD`, or `--MM-DD`.
fn date_text(date: &Value) -> Option<String> {
    if let Some(utc) = date["utc"].as_str() {
        return utc.get(..10).map(str::to_string);
    }
    let (month, day) = (date["month"].as_u64()?, date["day"].as_u64()?);
    Some(match date["year"].as_u64() {
        Some(year) => format!("{year:04}-{month:02}-{day:02}"),
        None => format!("--{month:02}-{day:02}"),
    })
}

/// The ADR parts a JSContact address component kind stands for.
const ADDRESS_KINDS: [&str; 7] = [
    "postOfficeBox",
    "apartment",
    "name",
    "locality",
    "region",
    "postcode",
    "country",
];

fn details_of(card: &Value) -> mailrs_domain::contact::Details {
    use mailrs_domain::contact::Details;
    let mut d = Details {
        uid: card["uid"].as_str().unwrap_or_default().to_string(),
        ..Default::default()
    };
    for a in values(card, "anniversaries") {
        if a["kind"] == "birth"
            && d.birthday.is_empty()
            && let Some(text) = date_text(&a["date"])
        {
            d.birthday = text;
        }
    }
    d.notes = values(card, "notes")
        .filter_map(|n| n["note"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    d.addresses = values(card, "addresses")
        .map(|a| {
            std::array::from_fn(|i| {
                a["components"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|c| c["kind"] == ADDRESS_KINDS[i])
                    .and_then(|c| c["value"].as_str())
                    .unwrap_or_default()
                    .to_string()
            })
        })
        .collect();
    d.urls = values(card, "links")
        .filter(|l| l["label"] != EXTRAS)
        .filter_map(|l| l["uri"].as_str().map(str::to_string))
        .collect();
    let mut groups: Vec<String> = card
        .get("keywords")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|m| m.iter().filter(|(_, on)| on.as_bool() == Some(true)))
        .map(|(k, _)| k.clone())
        .collect();
    groups.sort();
    d.groups = groups;
    let extras = extras_of(card);
    d.dates = extras.dates;
    d.custom = extras.custom;
    d
}

/// The JSContact properties that carry the detail fields `fields` names,
/// each as the whole map it replaces. Only a field the person changed is
/// named, so the rest of a card stays as the server holds it.
fn details_patch(fields: &ContactFields, held: Option<&Value>) -> Map<String, Value> {
    let c = &fields.details;
    let mut out = Map::new();
    let keyed = |prefix: &str, items: Vec<Value>| -> Value {
        Value::Object(
            items
                .into_iter()
                .enumerate()
                .map(|(i, v)| (format!("{prefix}{}", i + 1), v))
                .collect(),
        )
    };
    if let Some(birthday) = &c.birthday {
        let value = match date_json(birthday) {
            Some(date) => {
                json!({ "a1": { "@type": "Anniversary", "kind": "birth", "date": date } })
            }
            None => Value::Null,
        };
        out.insert("anniversaries".into(), value);
    }
    if let Some(notes) = &c.notes {
        let value = match notes.is_empty() {
            true => Value::Null,
            false => json!({ "n1": { "@type": "Note", "note": notes } }),
        };
        out.insert("notes".into(), value);
    }
    if let Some(addresses) = &c.addresses {
        let items = addresses
            .iter()
            .map(|a| {
                let components: Vec<Value> = a
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| !v.is_empty())
                    .map(|(i, v)| json!({ "@type": "AddressComponent", "kind": ADDRESS_KINDS[i], "value": v }))
                    .collect();
                json!({ "@type": "Address", "components": components, "isOrdered": false })
            })
            .collect();
        out.insert("addresses".into(), keyed("a", items));
    }
    if c.urls.is_some() || c.dates.is_some() || c.custom.is_some() {
        // Websites and the extras share the `links` map, so a change to
        // either writes both, the other as the card holds it.
        let before = held.map(details_of).unwrap_or_default();
        let urls = c.urls.clone().unwrap_or(before.urls);
        let extras = Extras {
            dates: c.dates.clone().unwrap_or(before.dates),
            custom: c.custom.clone().unwrap_or(before.custom),
        };
        let mut items: Vec<Value> = urls
            .iter()
            .map(|u| json!({ "@type": "Link", "kind": "generic", "uri": u }))
            .collect();
        if !extras.dates.is_empty() || !extras.custom.is_empty() {
            use base64::Engine;
            let uri = format!(
                "data:application/json;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(json!(extras).to_string())
            );
            items.push(json!({ "@type": "Link", "kind": "other", "label": EXTRAS, "uri": uri }));
        }
        out.insert(
            "links".into(),
            if items.is_empty() {
                Value::Null
            } else {
                keyed("l", items)
            },
        );
    }
    if let Some(groups) = &c.groups {
        let map: Map<String, Value> = groups.iter().map(|g| (g.clone(), json!(true))).collect();
        out.insert("keywords".into(), Value::Object(map));
    }
    if let Some(photo) = &fields.photo {
        let value = match photo.is_empty() {
            true => Value::Null,
            false => {
                use base64::Engine;
                let uri = format!(
                    "data:image/png;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(photo)
                );
                json!({ "m1": { "@type": "Media", "kind": "photo", "uri": uri } })
            }
        };
        out.insert("media".into(), value);
    }
    out
}

/// The JSContact fields `fields` sets on a new card, each map keyed `e1`,
/// `p1`, `o1`.
fn fields_of(fields: &ContactFields) -> Map<String, Value> {
    let keyed = |prefix: &str, items: &[String], key: &str| -> Value {
        let map: Map<String, Value> = items
            .iter()
            .enumerate()
            .map(|(i, v)| (format!("{prefix}{}", i + 1), json!({ key: v })))
            .collect();
        Value::Object(map)
    };
    let mut card = Map::new();
    if let Some(name) = &fields.name {
        card.insert("name".into(), json!({ "full": name }));
    }
    if let Some(emails) = &fields.emails {
        card.insert("emails".into(), keyed("e", emails, "address"));
    }
    if let Some(phones) = &fields.phones {
        card.insert("phones".into(), keyed("p", phones, "number"));
    }
    if let Some(organization) = &fields.organization {
        card.insert(
            "organizations".into(),
            json!({ "o1": { "name": organization } }),
        );
    }
    for (key, value) in details_patch(fields, None) {
        if !value.is_null() {
            card.insert(key, value);
        }
    }
    card
}

/// The entries of one map on `card`, `emails` say, as key and the value
/// under `key`, in the order `person_of` reads them.
fn entries<'a>(card: &'a Value, field: &str, key: &str) -> Vec<(&'a str, &'a str)> {
    card.get(field)
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|m| m.iter())
        .filter_map(|(k, v)| v[key].as_str().map(|value| (k.as_str(), value)))
        .collect()
}

/// A key `prefix` + number that `card`'s `field` map does not hold yet.
fn free_key(card: &Value, field: &str, prefix: &str, taken: &[String]) -> String {
    let used = |k: &str| card[field].get(k).is_some() || taken.iter().any(|t| t == k);
    (1..)
        .map(|n| format!("{prefix}{n}"))
        .find(|k| !used(k))
        .unwrap_or_default()
}

/// The patch that brings one map of `card` in line with `wanted`, a list
/// of values under `key`. `seen` holds the keys of the entries the person
/// was shown: one of those missing from `wanted` goes, and every other
/// entry stays with its tags. A value not on the card yet is added.
fn patch_entries(
    patch: &mut Map<String, Value>,
    card: &Value,
    (field, key, prefix): (&str, &str, &str),
    wanted: &[String],
    seen: &[&str],
) {
    let held = entries(card, field, key);
    let same = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
    if card.get(field).and_then(Value::as_object).is_none() {
        if !wanted.is_empty() {
            let map: Map<String, Value> = wanted
                .iter()
                .enumerate()
                .map(|(i, v)| (format!("{prefix}{}", i + 1), json!({ key: v })))
                .collect();
            patch.insert(field.into(), Value::Object(map));
        }
        return;
    }
    for (k, value) in &held {
        if seen.contains(k) && !wanted.iter().any(|w| same(w, value)) {
            patch.insert(format!("{field}/{k}"), Value::Null);
        }
    }
    let mut taken: Vec<String> = Vec::new();
    let mut added: Vec<&String> = Vec::new();
    for value in wanted {
        if held.iter().any(|(_, v)| same(v, value)) || added.iter().any(|v| same(v, value)) {
            continue;
        }
        let k = free_key(card, field, prefix, &taken);
        patch.insert(format!("{field}/{k}"), json!({ key: value }));
        taken.push(k);
        added.push(value);
    }
}

/// What turns `card` into the card `fields` asks for, touching only the
/// fields and entries the person changed. The editor shows every address
/// but only the first phone and organization, so the phones and
/// organizations it never showed stay, and a renamed organization keeps
/// its departments.
fn patch_of(card: &Value, fields: &ContactFields) -> Map<String, Value> {
    let shown = person_of(card);
    let mut patch = Map::new();
    if let Some(name) = &fields.name
        && Some(name.trim()) != shown.name.as_deref().map(str::trim)
    {
        let value = match name.trim() {
            "" => Value::Null,
            name => json!({ "full": name }),
        };
        patch.insert("name".into(), value);
    }
    if let Some(emails) = &fields.emails {
        let seen: Vec<&str> = entries(card, "emails", "address")
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        patch_entries(&mut patch, card, ("emails", "address", "e"), emails, &seen);
    }
    if let Some(phones) = &fields.phones {
        let seen: Vec<&str> = entries(card, "phones", "number")
            .into_iter()
            .take(1)
            .map(|(k, _)| k)
            .collect();
        patch_entries(&mut patch, card, ("phones", "number", "p"), phones, &seen);
    }
    if let Some(organization) = &fields.organization {
        let wanted = organization.trim();
        match entries(card, "organizations", "name").first() {
            Some((k, _)) if wanted.is_empty() => {
                patch.insert(format!("organizations/{k}"), Value::Null);
            }
            Some((k, held)) if *held != wanted => {
                patch.insert(format!("organizations/{k}/name"), json!(wanted));
            }
            Some(_) => {}
            None if wanted.is_empty() => {}
            None if card
                .get("organizations")
                .and_then(Value::as_object)
                .is_some() =>
            {
                let k = free_key(card, "organizations", "o", &[]);
                patch.insert(format!("organizations/{k}"), json!({ "name": wanted }));
            }
            None => {
                patch.insert("organizations".into(), json!({ "o1": { "name": wanted } }));
            }
        }
    }
    for (key, value) in details_patch(fields, Some(card)) {
        let held = &card[key.as_str()];
        if *held != value && !(held.is_null() && value.is_null()) {
            patch.insert(key, value);
        }
    }
    patch
}

pub(crate) fn card_of(fields: &ContactFields, book: Option<&str>) -> Map<String, Value> {
    let mut card = fields_of(fields);
    card.insert("@type".into(), json!("Card"));
    card.insert("version".into(), json!("1.0"));
    if let Some(book) = book {
        card.insert("addressBookIds".into(), json!({ book: true }));
    }
    card
}

impl<J: JmapApi> Jmap<J> {
    /// One page of a full read. `first` is the state the read began at: a
    /// card added while the pages go by may shift the positions, so the
    /// read ends on that first state and the changes since then bring
    /// anything the paging skipped. A card seen twice is listed once.
    async fn read_page(
        &self,
        position: u64,
        first: Option<String>,
    ) -> Result<ConnectionsPage, BackendError> {
        let page = self
            .api()
            .cards_page(position, CARDS_PER_PAGE)
            .await
            .map_err(backend)?;
        let read = page.cards.len() as u64;
        let mut seen = HashSet::new();
        let people = page
            .cards
            .iter()
            .map(person_of)
            .filter(|p| seen.insert(p.resource.clone()))
            .collect();
        let state = first.unwrap_or(page.state);
        let more =
            read == CARDS_PER_PAGE as u64 && page.total.is_none_or(|total| position + read < total);
        let (next_page_token, next_sync_token) = match more {
            true => (
                Some(
                    json!(PageToken::Reading {
                        position: position + read,
                        state
                    })
                    .to_string(),
                ),
                None,
            ),
            false => (None, Some(state)),
        };
        Ok(ConnectionsPage {
            people,
            deleted: Vec::new(),
            next_page_token,
            next_sync_token,
        })
    }

    async fn changes_page(&self, since: &str) -> Result<ConnectionsPage, BackendError> {
        let page = self
            .api()
            .card_changes(since, CARDS_PER_PAGE)
            .await
            .map_err(backend)?;
        let people = page.cards.iter().map(person_of).collect();
        let state = page.changes.new_state;
        let (next_page_token, next_sync_token) = match page.changes.has_more_changes {
            true => (
                Some(json!(PageToken::Changing { since: state }).to_string()),
                None,
            ),
            false => (None, Some(state)),
        };
        Ok(ConnectionsPage {
            people,
            deleted: page.changes.destroyed,
            next_page_token,
            next_sync_token,
        })
    }
}

impl<J: JmapApi> ContactsService for Jmap<J> {
    async fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<ConnectionsPage, BackendError> {
        match (page_token, sync_token) {
            (Some(token), _) => match serde_json::from_str::<PageToken>(token)
                .map_err(|_| BackendError::StateLost)?
            {
                PageToken::Reading { position, state } => {
                    self.read_page(position, Some(state)).await
                }
                PageToken::Changing { since } => self.changes_page(&since).await,
            },
            (None, Some(since)) => self.changes_page(since).await,
            (None, None) => self.read_page(0, None).await,
        }
    }

    /// A card's photo travels inside it as a data URI.
    async fn contact_photo(&self, url: &str) -> Result<Vec<u8>, BackendError> {
        use base64::Engine;
        let encoded = url
            .strip_prefix("data:")
            .and_then(|rest| rest.split_once(";base64,"))
            .map(|(_, data)| data)
            .ok_or(BackendError::NotFound)?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| BackendError::NotFound)
    }

    fn edits_details(&self) -> bool {
        true
    }

    async fn delete_contact(
        &self,
        resource: &str,
        _revision: Option<&str>,
    ) -> Result<(), BackendError> {
        self.api()
            .set_card(&CardWrite::Destroy {
                id: resource.to_string(),
            })
            .await
            .map_err(backend)?;
        Ok(())
    }

    async fn create_contact(&self, fields: &ContactFields) -> Result<Person, BackendError> {
        let books = self.api().address_books().await.map_err(backend)?;
        let book = books
            .iter()
            .find(|b| b.is_default)
            .or(books.first())
            .map(|b| b.id.clone());
        let card = self
            .api()
            .set_card(&CardWrite::Create(card_of(fields, book.as_deref())))
            .await
            .map_err(backend)?;
        Ok(person_of(&card))
    }

    /// Reads the card first, so the edit patches only what changed.
    async fn update_contact(
        &self,
        resource: &str,
        fields: &ContactFields,
    ) -> Result<Person, BackendError> {
        let card = self
            .api()
            .card(resource)
            .await
            .map_err(backend)?
            .ok_or(BackendError::NotFound)?;
        let patch = patch_of(&card, fields);
        if patch.is_empty() {
            return Ok(person_of(&card));
        }
        let write = CardWrite::Update {
            id: resource.to_string(),
            patch,
        };
        let card = self.api().set_card(&write).await.map_err(backend)?;
        Ok(person_of(&card))
    }
}

#[cfg(test)]
mod tests {
    use mailrs_domain::contact::{Changes, Field};

    use super::*;

    fn every_field() -> ContactFields {
        ContactFields {
            name: Some("Priya Raman".into()),
            emails: Some(vec!["priya@fernwood.example".into()]),
            phones: Some(vec!["+351 910 000 000".into()]),
            organization: Some("Fernwood".into()),
            photo: Some(vec![0x89, b'P', b'N', b'G']),
            details: Changes {
                birthday: Some("1990-04-12".into()),
                notes: Some("Met in March".into()),
                addresses: Some(vec![[
                    "Box 7".into(),
                    "Floor 2".into(),
                    "1 Main Street".into(),
                    "Lisbon".into(),
                    "LX".into(),
                    "1000-001".into(),
                    "Portugal".into(),
                ]]),
                urls: Some(vec!["https://example.org".into()]),
                groups: Some(vec!["Friends".into(), "Work".into()]),
                dates: Some(vec![Field {
                    label: "Anniversary".into(),
                    value: "2015-06-20".into(),
                }]),
                custom: Some(vec![Field {
                    label: "Badge".into(),
                    value: "42".into(),
                }]),
            },
            ..ContactFields::default()
        }
    }

    #[test]
    fn a_card_holds_and_gives_back_every_contact_field() {
        let mut card = card_of(&every_field(), Some("b1"));
        card.insert("id".into(), json!("c1"));
        let person = person_of(&Value::Object(card));
        let d = &person.details;
        assert_eq!(person.name.as_deref(), Some("Priya Raman"));
        assert_eq!(d.birthday, "1990-04-12");
        assert_eq!(d.notes, "Met in March");
        assert_eq!(d.addresses[0][2], "1 Main Street");
        assert_eq!(d.addresses[0][0], "Box 7");
        assert_eq!(d.urls, ["https://example.org"]);
        assert_eq!(d.groups, ["Friends", "Work"]);
        assert_eq!(d.dates[0].label, "Anniversary");
        assert_eq!(d.custom[0].value, "42");
        assert!(
            person
                .photo_url
                .as_deref()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
    }

    #[test]
    fn an_edit_patches_only_the_fields_that_changed() {
        let mut card = card_of(&every_field(), Some("b1"));
        card.insert("id".into(), json!("c1"));
        let card = Value::Object(card);
        let same = patch_of(&card, &every_field());
        assert!(
            same.keys().all(|k| k == "media"),
            "an unchanged card sends nothing but its photo: {same:?}"
        );
        let edit = ContactFields {
            details: Changes {
                notes: Some("Changed".into()),
                groups: Some(vec!["Friends".into()]),
                ..Changes::default()
            },
            ..ContactFields::default()
        };
        let patch = patch_of(&card, &edit);
        assert_eq!(patch["notes"]["n1"]["note"], "Changed");
        assert_eq!(patch["keywords"], json!({ "Friends": true }));
        assert_eq!(patch.len(), 2);
    }
}
