//! The account's address book over Graph.
//!
//! Graph keeps contacts in folders and runs its delta per folder. The sync
//! token is JSON, `{"folders": {"<folder id>": "<delta link>"}}`. A page
//! token is `{"folders": [ids still to read], "at": "<folder>", "link":
//! "<next link>", "done": {"<folder id>": "<delta link>"}}`, so one call
//! reads one Graph page of one folder and the caller walks on.

use std::collections::BTreeMap;

use mailrs_gmail::{ConnectionsPage, ContactFields, Person};
use mailrs_graph::GraphContact;
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

fn person_of(c: &GraphContact) -> Person {
    let name = c.display_name.clone().or_else(|| {
        let joined = [c.given_name.as_deref(), c.surname.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" ");
        (!joined.is_empty()).then_some(joined)
    });
    let phone = c
        .mobile_phone
        .clone()
        .or_else(|| c.business_phones.first().cloned())
        .or_else(|| c.home_phones.first().cloned());
    Person {
        resource: c.id.clone(),
        name,
        emails: c.email_addresses.iter().filter_map(|e| e.address.clone()).collect(),
        photo_url: Some(format!("{PHOTO_PREFIX}{}", c.id)),
        organization: c.company_name.clone(),
        phone,
    }
}

/// Graph's body for the fields set; a field left `None` stays out, so a
/// change touches only what it names.
fn body_of(fields: &ContactFields) -> Value {
    let mut body = serde_json::Map::new();
    if let Some(name) = &fields.name {
        body.insert("displayName".into(), json!(name));
    }
    if let Some(emails) = &fields.emails {
        let addresses: Vec<Value> = emails.iter().map(|a| json!({ "address": a })).collect();
        body.insert("emailAddresses".into(), json!(addresses));
    }
    if let Some(phones) = &fields.phones {
        body.insert("mobilePhone".into(), json!(phones.first()));
        body.insert("businessPhones".into(), json!(phones.iter().skip(1).collect::<Vec<_>>()));
    }
    if let Some(company) = &fields.organization {
        body.insert("companyName".into(), json!(company));
    }
    Value::Object(body)
}

impl<G: GraphApi> Microsoft<G> {
    /// The folders to read: the default one, which Graph leaves out of
    /// the list, then the others.
    async fn contact_folder_ids(&self) -> Result<Vec<String>, BackendError> {
        let err = |e| self.service_error(Service::Contacts, e);
        let mut ids: Vec<String> = self.graph.default_contact_folder().await.map_err(err)?.into_iter().collect();
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
                    return Ok(ConnectionsPage { next_sync_token: Some(sync), ..ConnectionsPage::default() });
                }
                let at = folders.remove(0);
                PageState { folders, at, link: None, done: BTreeMap::new() }
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
                next_sync_token = Some(json!(SyncState { folders: std::mem::take(&mut state.done) }).to_string());
            } else {
                state.at = state.folders.remove(0);
            }
        }
        if next_sync_token.is_none() {
            next_page_token = Some(json!(state).to_string());
        }
        Ok(ConnectionsPage { people, deleted, next_page_token, next_sync_token })
    }

    async fn contact_photo(&self, url: &str) -> Result<Vec<u8>, BackendError> {
        let id = url.strip_prefix(PHOTO_PREFIX).ok_or(BackendError::NotFound)?;
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
            .create_contact(&body_of(fields))
            .await
            .map_err(|e| self.service_error(Service::Contacts, e))?;
        Ok(person_of(&made))
    }

    async fn update_contact(&self, resource: &str, fields: &ContactFields) -> Result<Person, BackendError> {
        let changed = self
            .graph
            .update_contact(resource, &body_of(fields))
            .await
            .map_err(|e| self.service_error(Service::Contacts, e))?;
        Ok(person_of(&changed))
    }
}
