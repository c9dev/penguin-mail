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
    Person {
        details: mailrs_domain::contact::Details {
            phones: values(card, "phones")
                .filter_map(|p| p["number"].as_str().map(str::to_string))
                .collect(),
            ..Default::default()
        },
        resource: card["id"].as_str().unwrap_or_default().to_string(),
        name,
        emails: values(card, "emails")
            .filter_map(|e| e["address"].as_str().map(str::to_string))
            .collect(),
        photo_url: None,
        organization: values(card, "organizations")
            .find_map(|o| o["name"].as_str().map(str::to_string)),
        phone: values(card, "phones").find_map(|p| p["number"].as_str().map(str::to_string)),
    }
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

    /// Cards carry no photo URL this adapter hands out.
    async fn contact_photo(&self, _: &str) -> Result<Vec<u8>, BackendError> {
        Err(BackendError::NotFound)
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
