//! The account's address book over Graph.
//!
//! Graph keeps contacts in folders and runs its delta per folder. The sync
//! token is JSON, `{"folders": {"<folder id>": "<delta link>"}}`. A page
//! token is `{"folders": [ids still to read], "at": "<folder>", "link":
//! "<next link>", "done": {"<folder id>": "<delta link>"}}`, so one call
//! reads one Graph page of one folder and the caller walks on.

use std::collections::BTreeMap;

use mailrs_gmail::{ConnectionsPage, ContactFields, Person};
use mailrs_graph::{GraphContact, PhysicalAddress};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{GraphApi, Microsoft, Service};
use crate::BackendError;
use crate::services::ContactsService;

/// The photo size past which the adapter stops reading.
const PHOTO_LIMIT: usize = 4 * 1024 * 1024;

/// The name `Person::photo_url` carries; only this adapter reads it.
const PHOTO_PREFIX: &str = "graph:contact/";

#[derive(Serialize, Deserialize, Default)]
struct SyncState {
    folders: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct PageState {
    folders: Vec<String>,
    at: String,
    link: Option<String>,
    done: BTreeMap<String, String>,
}

fn lost<T>(parsed: serde_json::Result<T>) -> Result<T, BackendError> {
    parsed.map_err(|_| BackendError::StateLost)
}

/// Graph keeps three postal addresses, by the place they stand for.
fn address_of(a: &PhysicalAddress) -> mailrs_domain::contact::Address {
    let part = |v: &Option<String>| v.clone().unwrap_or_default();
    [
        String::new(),
        String::new(),
        part(&a.street),
        part(&a.city),
        part(&a.state),
        part(&a.postal_code),
        part(&a.country_or_region),
    ]
}

fn physical(a: &mailrs_domain::contact::Address) -> Value {
    let part = |s: &String| (!s.is_empty()).then(|| s.clone());
    // Graph has one street line, so a PO box and an extended address join it.
    let street: Vec<&str> = [&a[0], &a[1], &a[2]]
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(String::as_str)
        .collect();
    json!({
        "street": (!street.is_empty()).then(|| street.join("\n")),
        "city": part(&a[3]),
        "state": part(&a[4]),
        "postalCode": part(&a[5]),
        "countryOrRegion": part(&a[6]),
    })
}

fn details_of(c: &GraphContact) -> mailrs_domain::contact::Details {
    mailrs_domain::contact::Details {
        phones: c
            .mobile_phone
            .iter()
            .chain(&c.business_phones)
            .chain(&c.home_phones)
            .cloned()
            .collect(),
        // Graph answers a birthday as a date and time at midnight UTC.
        birthday: c
            .birthday
            .as_deref()
            .and_then(|b| b.get(..10))
            .filter(|b| !b.starts_with("0001"))
            .unwrap_or_default()
            .to_string(),
        notes: c.personal_notes.clone().unwrap_or_default(),
        urls: c.business_home_page.iter().cloned().collect(),
        groups: c.categories.clone(),
        addresses: [&c.home_address, &c.business_address, &c.other_address]
            .into_iter()
            .flatten()
            .map(address_of)
            .collect(),
        ..Default::default()
    }
}

fn person_of(c: &GraphContact) -> Person {
    let name = c.display_name.clone().or_else(|| {
        let joined = [c.given_name.as_deref(), c.surname.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        (!joined.is_empty()).then_some(joined)
    });
    let phone = c
        .mobile_phone
        .clone()
        .or_else(|| c.business_phones.first().cloned())
        .or_else(|| c.home_phones.first().cloned());
    Person {
        details: details_of(c),
        resource: c.id.clone(),
        name,
        emails: c
            .email_addresses
            .iter()
            .filter_map(|e| e.address.clone())
            .collect(),
        photo_url: Some(format!("{PHOTO_PREFIX}{}", c.id)),
        organization: c.company_name.clone(),
        phone,
    }
}

/// Graph's body for the fields set; a field left `None` stays out, so a
/// change touches only what it names. A number in `home` stays a home
/// number; the first other one is the mobile and the rest are business.
fn body_of(fields: &ContactFields, home: &[String]) -> Value {
    let mut body = serde_json::Map::new();
    if let Some(name) = &fields.name {
        body.insert("displayName".into(), json!(name));
    }
    if let Some(emails) = &fields.emails {
        let addresses: Vec<Value> = emails.iter().map(|a| json!({ "address": a })).collect();
        body.insert("emailAddresses".into(), json!(addresses));
    }
    if let Some(phones) = &fields.phones {
        let (home, other): (Vec<&String>, Vec<&String>) =
            phones.iter().partition(|p| home.contains(p));
        body.insert("mobilePhone".into(), json!(other.first()));
        body.insert("homePhones".into(), json!(home));
        body.insert(
            "businessPhones".into(),
            json!(other.iter().skip(1).collect::<Vec<_>>()),
        );
    }
    if let Some(company) = &fields.organization {
        body.insert("companyName".into(), json!(company));
    }
    let d = &fields.details;
    if let Some(birthday) = &d.birthday {
        body.insert(
            "birthday".into(),
            match mailrs_domain::contact::date_parts(birthday) {
                // Graph wants a year; a birthday without one keeps 1604, as Apple writes it.
                Some((year, month, day)) => {
                    json!(format!("{:04}-{month:02}-{day:02}T00:00:00Z", year.unwrap_or(1604)))
                }
                None => Value::Null,
            },
        );
    }
    if let Some(notes) = &d.notes {
        body.insert("personalNotes".into(), json!(notes));
    }
    if let Some(urls) = &d.urls {
        body.insert("businessHomePage".into(), json!(urls.first()));
    }
    if let Some(groups) = &d.groups {
        body.insert("categories".into(), json!(groups));
    }
    if let Some(addresses) = &d.addresses {
        for (at, key) in ["homeAddress", "businessAddress", "otherAddress"]
            .into_iter()
            .enumerate()
        {
            body.insert(key.into(), addresses.get(at).map_or(Value::Null, physical));
        }
    }
    Value::Object(body)
}

impl<G: GraphApi> Microsoft<G> {
    /// The folders to read: the default one, which Graph leaves out of
    /// the list, then the others.
    async fn contact_folder_ids(&self) -> Result<Vec<String>, BackendError> {
        let err = |e| self.service_error(Service::Contacts, e);
        let mut ids: Vec<String> = self
            .graph
            .default_contact_folder()
            .await
            .map_err(err)?
            .into_iter()
            .collect();
        let others = self.graph.contact_folders().await.map_err(err)?;
        for folder in others {
            if !ids.contains(&folder.id) {
                ids.push(folder.id);
            }
        }
        Ok(ids)
    }
}

impl<G: GraphApi> ContactsService for Microsoft<G> {
    /// Graph holds a birthday, notes, one website, categories and three
    /// postal addresses. Other dates, custom fields, further websites and
    /// addresses, and a photo have no home there.
    fn edits_details(&self) -> bool {
        true
    }

    async fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<ConnectionsPage, BackendError> {
        let known = match sync_token {
            Some(text) => lost(serde_json::from_str::<SyncState>(text))?.folders,
            None => BTreeMap::new(),
        };
        let mut state = match page_token {
            Some(text) => lost(serde_json::from_str::<PageState>(text))?,
            None => {
                let mut folders = self.contact_folder_ids().await?;
                // Graph names the default folder only while it holds a
                // contact, so a folder the token knew stays in the round.
                // One that is gone answers NotFound below.
                for id in known.keys() {
                    if !folders.contains(id) {
                        folders.push(id.clone());
                    }
                }
                if folders.is_empty() {
                    let sync = json!(SyncState::default()).to_string();
                    return Ok(ConnectionsPage {
                        next_sync_token: Some(sync),
                        ..ConnectionsPage::default()
                    });
                }
                let at = folders.remove(0);
                PageState {
                    folders,
                    at,
                    link: None,
                    done: BTreeMap::new(),
                }
            }
        };
        let link = state.link.clone().or_else(|| known.get(&state.at).cloned());
        let page = self
            .graph
            .contact_delta(&state.at, link.as_deref())
            .await
            .map_err(|e| match e {
                // A folder that went away takes its delta link with it.
                mailrs_graph::GraphError::NotFound if link.is_some() => BackendError::StateLost,
                e => self.service_error(Service::Contacts, e),
            })?;

        let (mut people, mut deleted) = (Vec::new(), Vec::new());
        for contact in &page.value {
            match contact.removed {
                Some(_) => deleted.push(contact.id.clone()),
                None => people.push(person_of(contact)),
            }
        }
        let (mut next_page_token, mut next_sync_token) = (None, None);
        if let Some(next) = page.next_link {
            state.link = Some(next);
        } else {
            let delta = page.delta_link.ok_or(BackendError::StateLost)?;
            state.done.insert(state.at.clone(), delta);
            state.link = None;
            if state.folders.is_empty() {
                next_sync_token = Some(
                    json!(SyncState {
                        folders: std::mem::take(&mut state.done)
                    })
                    .to_string(),
                );
            } else {
                state.at = state.folders.remove(0);
            }
        }
        if next_sync_token.is_none() {
            next_page_token = Some(json!(state).to_string());
        }
        Ok(ConnectionsPage {
            people,
            deleted,
            next_page_token,
            next_sync_token,
        })
    }

    async fn contact_photo(&self, url: &str) -> Result<Vec<u8>, BackendError> {
        let id = url
            .strip_prefix(PHOTO_PREFIX)
            .ok_or(BackendError::NotFound)?;
        let photo = self
            .graph
            .contact_photo(id, PHOTO_LIMIT)
            .await
            .map_err(|e| self.service_error(Service::Contacts, e))?;
        Ok(photo.unwrap_or_default())
    }

    async fn create_contact(&self, fields: &ContactFields) -> Result<Person, BackendError> {
        let made = self
            .graph
            .create_contact(&body_of(fields, &[]))
            .await
            .map_err(|e| self.service_error(Service::Contacts, e))?;
        Ok(person_of(&made))
    }

    async fn update_contact(
        &self,
        resource: &str,
        fields: &ContactFields,
    ) -> Result<Person, BackendError> {
        let err = |e| self.service_error(Service::Contacts, e);
        // Outlook.com rebuilds a display name a change leaves out, from the
        // name parts or else the company, so a new company would become the
        // contact's name. Graph's contact reference says to send the name
        // with every update.
        let current = match fields.name.is_none() || fields.phones.is_some() {
            true => Some(self.graph.contact_name(resource).await.map_err(err)?),
            false => None,
        };
        let home = current.as_ref().map_or(&[][..], |c| &c.home_phones[..]);
        let mut body = body_of(fields, home);
        if fields.name.is_none()
            && let Some(name) = current.and_then(|c| c.display_name)
        {
            body["displayName"] = json!(name);
        }
        let changed = self
            .graph
            .update_contact(resource, &body)
            .await
            .map_err(|e| self.service_error(Service::Contacts, e))?;
        Ok(person_of(&changed))
    }
}

#[cfg(test)]
mod tests {
    use mailrs_domain::contact::Changes;

    use super::*;

    #[test]
    fn graph_holds_birthday_notes_website_categories_and_three_addresses() {
        let fields = ContactFields {
            details: Changes {
                birthday: Some("1990-04-12".into()),
                notes: Some("Met in March".into()),
                urls: Some(vec!["https://example.org".into(), "https://two.example".into()]),
                groups: Some(vec!["Friends".into()]),
                addresses: Some(vec![
                    [
                        String::new(),
                        String::new(),
                        "1 Main Street".into(),
                        "Lisbon".into(),
                        "LX".into(),
                        "1000-001".into(),
                        "Portugal".into(),
                    ],
                    Default::default(),
                ]),
                ..Changes::default()
            },
            ..ContactFields::default()
        };
        let body = body_of(&fields, &[]);
        assert_eq!(body["birthday"], "1990-04-12T00:00:00Z");
        assert_eq!(body["businessHomePage"], "https://example.org");
        assert_eq!(body["homeAddress"]["city"], "Lisbon");
        assert_eq!(body["otherAddress"], Value::Null);
        let read: GraphContact = serde_json::from_value(json!({
            "id": "c1",
            "birthday": "1990-04-12T00:00:00Z",
            "personalNotes": "Met in March",
            "businessHomePage": "https://example.org",
            "categories": ["Friends"],
            "homeAddress": body["homeAddress"],
        }))
        .unwrap();
        let d = person_of(&read).details;
        assert_eq!(d.birthday, "1990-04-12");
        assert_eq!(d.notes, "Met in March");
        assert_eq!(d.groups, ["Friends"]);
        assert_eq!(d.addresses[0][3], "Lisbon");
        assert_eq!(d.addresses[0][6], "Portugal");
    }
}
