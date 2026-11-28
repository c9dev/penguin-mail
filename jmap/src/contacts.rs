//! Contacts over JMAP (RFC 9610): address books, and cards in JSContact
//! (RFC 9553), read whole as JSON so a field this client does not know
//! survives an edit. Contacts can live in an account of their own.

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::mail::{ChangesResponse, Got, SetResponse};
use crate::session::CONTACTS;
use crate::{JmapClient, JmapError, Request};

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressBook {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CardsPage {
    pub state: String,
    pub cards: Vec<Value>,
    pub position: u64,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CardChanges {
    pub changes: ChangesResponse,
    pub cards: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CardWrite {
    Create(Map<String, Value>),
    Update {
        id: String,
        patch: Map<String, Value>,
    },
    Destroy {
        id: String,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CardQuery {
    #[serde(default)]
    position: u64,
    #[serde(default)]
    total: Option<u64>,
}

impl JmapClient {
    /// The contacts account, and `wanted` cut to what one `/get` may take
    /// (the session's `maxObjectsInGet`, at most 500, at least 1).
    async fn contacts_account(&self, wanted: usize) -> Result<(String, usize), JmapError> {
        let session = self.session().await?;
        let account = session
            .account_for(CONTACTS)
            .ok_or_else(|| JmapError::UnknownCapability(CONTACTS.into()))?;
        Ok((
            account.to_string(),
            wanted.clamp(1, session.limits().max_objects_in_get.clamp(1, 500)),
        ))
    }

    pub async fn address_books(&self) -> Result<Vec<AddressBook>, JmapError> {
        let (account, _) = self.contacts_account(1).await?;
        let mut request = Request::new();
        let id = request.call(
            CONTACTS,
            "AddressBook/get",
            json!({ "accountId": account, "ids": null }),
        );
        Ok(self
            .send(&request)
            .await?
            .take::<Got<AddressBook>>(&id, "AddressBook/get")?
            .list)
    }

    /// One page of every card, and the ContactCard state the page read.
    pub async fn cards_page(&self, position: u64, limit: usize) -> Result<CardsPage, JmapError> {
        let (account, limit) = self.contacts_account(limit).await?;
        let mut request = Request::new();
        let query = request.call(
            CONTACTS,
            "ContactCard/query",
            json!({
                "accountId": account, "position": position, "limit": limit, "calculateTotal": true
            }),
        );
        let get = request.call(
            CONTACTS,
            "ContactCard/get",
            json!({
                "accountId": account, "#ids": query.result_of("ContactCard/query", "/ids")
            }),
        );
        let mut answer = self.send(&request).await?;
        let listed: CardQuery = answer.take(&query, "ContactCard/query")?;
        let got: Got<Value> = answer.take(&get, "ContactCard/get")?;
        Ok(CardsPage {
            state: got.state,
            cards: got.list,
            position: listed.position,
            total: listed.total,
        })
    }

    /// The cards changed since `since`, at most `max`, fetched in the same
    /// request.
    pub async fn card_changes(&self, since: &str, max: usize) -> Result<CardChanges, JmapError> {
        let (account, max) = self.contacts_account(max).await?;
        let mut request = Request::new();
        let changes = request.call(
            CONTACTS,
            "ContactCard/changes",
            json!({ "accountId": account, "sinceState": since, "maxChanges": max }),
        );
        let created = request.call(
            CONTACTS,
            "ContactCard/get",
            json!({
                "accountId": account, "#ids": changes.result_of("ContactCard/changes", "/created")
            }),
        );
        let updated = request.call(
            CONTACTS,
            "ContactCard/get",
            json!({
                "accountId": account, "#ids": changes.result_of("ContactCard/changes", "/updated")
            }),
        );
        let mut answer = self.send(&request).await?;
        let changes: ChangesResponse = answer.take(&changes, "ContactCard/changes")?;
        let mut cards = answer.take::<Got<Value>>(&created, "ContactCard/get")?.list;
        cards.extend(answer.take::<Got<Value>>(&updated, "ContactCard/get")?.list);
        Ok(CardChanges { changes, cards })
    }

    /// The card with `id` as the server holds it, or `None` when it has
    /// none.
    pub async fn card(&self, id: &str) -> Result<Option<Value>, JmapError> {
        let (account, _) = self.contacts_account(1).await?;
        let mut request = Request::new();
        let get = request.call(
            CONTACTS,
            "ContactCard/get",
            json!({ "accountId": account, "ids": [id] }),
        );
        Ok(self
            .send(&request)
            .await?
            .take::<Got<Value>>(&get, "ContactCard/get")?
            .list
            .into_iter()
            .next())
    }

    /// Makes or changes a card, and answers it as the server now holds it.
    pub async fn set_card(&self, write: &CardWrite) -> Result<Value, JmapError> {
        let (account, _) = self.contacts_account(1).await?;
        if let CardWrite::Destroy { id } = write {
            let mut request = Request::new();
            let set = request.call(
                CONTACTS,
                "ContactCard/set",
                json!({ "accountId": account, "destroy": [id] }),
            );
            let mut answer = self.send(&request).await?;
            let written: SetResponse = answer.take(&set, "ContactCard/set")?;
            if let Some(refused) = written.refusal(id) {
                return Err(refused.error());
            }
            return Ok(Value::Null);
        }
        let (args, key, read) = match write {
            CardWrite::Create(card) => (
                json!({ "accountId": account, "create": { "k": card } }),
                "k".to_string(),
                "#k".to_string(),
            ),
            CardWrite::Update { id, patch } => (
                json!({ "accountId": account, "update": { id.clone(): patch } }),
                id.clone(),
                id.clone(),
            ),
            CardWrite::Destroy { .. } => unreachable!("a destroy returned above"),
        };
        let mut request = Request::new();
        let set = request.call(CONTACTS, "ContactCard/set", args);
        let get = request.call(
            CONTACTS,
            "ContactCard/get",
            json!({ "accountId": account, "ids": [read] }),
        );
        let mut answer = self.send(&request).await?;
        let written: SetResponse = answer.take(&set, "ContactCard/set")?;
        if let Some(refused) = written
            .not_created
            .as_ref()
            .and_then(|n| n.get(&key))
            .or_else(|| written.refusal(&key))
        {
            return Err(refused.error());
        }
        if matches!(write, CardWrite::Update { .. })
            && !written
                .updated
                .as_ref()
                .is_some_and(|u| u.contains_key(&key))
        {
            return Err(JmapError::Decode("the card was not updated".into()));
        }
        let got: Got<Value> = answer.take(&get, "ContactCard/get")?;
        got.list.into_iter().next().ok_or(JmapError::NotFound)
    }
}
