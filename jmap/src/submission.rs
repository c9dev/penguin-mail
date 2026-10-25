//! Sending later on the server (RFC 8621 section 7, with RFC 4865's
//! FUTURERELEASE): the message waits in Drafts, the submission holds it
//! for a number of seconds, and while it waits the client can call it
//! back. A server says how long it holds mail in the submission
//! capability's `maxDelayedSend`. Cyrus and Fastmail hold mail without
//! listing FUTURERELEASE, so that number, not the extension list, is the
//! gate.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::error::refused_outright;
use crate::mail::{SetError, pointer};
use crate::session::{MAIL, SUBMISSION};
use crate::{JmapClient, JmapError, Request, Session};

/// The keyword a message carries while it waits on the server in Drafts,
/// so this app and any other client can tell it from a draft. No
/// registered keyword says this, and RFC 5788 keeps `$` names for those.
pub const HELD: &str = "penguin-mail-held";

/// The longest delay, in seconds, the server holds a submission for: 0
/// when it holds none.
pub fn max_delayed_send(session: &Session) -> u64 {
    session
        .account_capability(SUBMISSION)
        .and_then(|c| c.get("maxDelayedSend"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// A message to hold: its blob, who it goes from and to, the Drafts
/// mailbox it waits in, and how many seconds from now it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    pub blob_id: String,
    pub identity_id: String,
    pub mail_from: String,
    pub rcpt_to: Vec<String>,
    pub drafts: String,
    pub hold_for: u64,
}

/// A hold the server took: the submission that calls it back, and the
/// copy waiting in Drafts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldSubmission {
    pub id: String,
    pub email_id: String,
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionState {
    pub id: String,
    #[serde(default)]
    pub email_id: String,
    /// `pending`, `final` or `canceled`.
    #[serde(default)]
    pub undo_status: String,
    /// When the server sends or sent the message, RFC 3339 in UTC.
    #[serde(default)]
    pub send_at: Option<String>,
    /// Who the server sends it to, the blind copies among them.
    #[serde(default)]
    pub envelope: Option<Envelope>,
}

/// The addresses a submission goes to, as the server keeps them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    #[serde(default)]
    pub rcpt_to: Vec<EnvelopeAddress>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EnvelopeAddress {
    pub email: String,
}

impl SubmissionState {
    /// The addresses the server sends it to.
    pub fn recipients(&self) -> Vec<String> {
        self.envelope
            .iter()
            .flat_map(|e| &e.rcpt_to)
            .map(|a| a.email.clone())
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Canceled {
    /// The server will not send it.
    Canceled,
    /// It went before the cancel arrived (`cannotUnsend`).
    TooLate,
    /// The server no longer has the submission (`notFound`). A cancel
    /// whose answer was lost leaves this, and so does a server that sent
    /// the message and forgot it, so it says nothing about whether it went.
    Gone,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Created {
    #[serde(default)]
    created: Option<BTreeMap<String, Value>>,
    #[serde(default)]
    not_created: Option<BTreeMap<String, SetError>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Updated {
    #[serde(default)]
    updated: Option<BTreeMap<String, Value>>,
    #[serde(default)]
    not_updated: Option<BTreeMap<String, SetError>>,
}

#[derive(Deserialize)]
struct Listing {
    #[serde(default)]
    list: Vec<SubmissionState>,
}

/// What a `/set` created under `key`, or the error it gave instead.
fn one(answer: Created, key: &str) -> Result<Value, JmapError> {
    if let Some(refused) = answer.not_created.as_ref().and_then(|n| n.get(key)) {
        return Err(refused.error());
    }
    answer
        .created
        .and_then(|mut created| created.remove(key))
        .ok_or_else(|| JmapError::Decode(format!("the server created nothing for {key}")))
}

fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `id` a created object came back with.
fn id_of(value: &Value, what: &str) -> Result<String, JmapError> {
    match value.get("id").and_then(Value::as_str) {
        Some(id) if !id.is_empty() => Ok(id.to_string()),
        _ => Err(JmapError::Decode(format!("{what} came back without an id"))),
    }
}

/// A failed `send` of the hold: when the request may have reached the
/// server (a lost connection, a timeout, a 5xx, an unreadable answer), the
/// server may hold the message, so the caller must not treat it as refused.
fn unknown_if_sent(err: JmapError) -> JmapError {
    match err {
        JmapError::Network(_) | JmapError::Decode(_) => JmapError::HoldUnknown,
        JmapError::Problem { status, .. } if status >= 500 => JmapError::HoldUnknown,
        other => other,
    }
}

fn submission_account(session: &Session) -> Result<String, JmapError> {
    session
        .account_for(SUBMISSION)
        .map(str::to_string)
        .ok_or_else(|| JmapError::UnknownCapability(SUBMISSION.into()))
}

impl JmapClient {
    /// Imports the message into Drafts and asks the server to send it
    /// `hold.hold_for` seconds from now, in one request. A hold refused
    /// outright destroys the copy, so nothing waits in Drafts that will not
    /// go. When no clear answer comes back the outcome is
    /// [`JmapError::HoldUnknown`] and the copy stays: the server may hold
    /// the message, and the look for it reads that copy.
    pub async fn hold(&self, hold: &Hold) -> Result<HeldSubmission, JmapError> {
        let session = self.session().await?;
        let sender = submission_account(&session)?;
        let mail = self.mail_account().await?;
        let mut request = Request::new();
        let import = request.call(
            MAIL,
            "Email/import",
            json!({
                "accountId": mail,
                "emails": { "m": {
                    "blobId": hold.blob_id,
                    "mailboxIds": { hold.drafts.clone(): true },
                    "keywords": { "$draft": true, "$seen": true, HELD: true }
                } }
            }),
        );
        let rcpt: Vec<Value> = hold.rcpt_to.iter().map(|a| json!({ "email": a })).collect();
        let submitted = request.call(SUBMISSION, "EmailSubmission/set", json!({
            "accountId": sender,
            "create": { "s": {
                "identityId": hold.identity_id,
                "emailId": "#m",
                "envelope": {
                    "mailFrom": { "email": hold.mail_from, "parameters": { "HOLDFOR": hold.hold_for.to_string() } },
                    "rcptTo": rcpt
                }
            } }
        }));
        let mut answer = self.send(&request).await.map_err(unknown_if_sent)?;
        // An answer that is no clear refusal may follow a hold the server
        // made, and the look for it reads the copy in Drafts.
        let unclear = |e: JmapError| {
            if refused_outright(&e) {
                e
            } else {
                JmapError::HoldUnknown
            }
        };
        let made: Created = answer.take(&import, "Email/import").map_err(unclear)?;
        // A refused import leaves `#m` naming nothing, so nothing went.
        if let Some(refused) = made.not_created.as_ref().and_then(|n| n.get("m")) {
            return Err(refused.error());
        }
        let email = one(made, "m").map_err(unclear)?;
        let email_id = id_of(&email, "the imported message").map_err(unclear)?;
        let thread_id = string(&email, "threadId");
        let refused = match answer.take::<Created>(&submitted, "EmailSubmission/set") {
            Ok(set) => match set.not_created.as_ref().and_then(|n| n.get("s")) {
                Some(refused) => refused.error(),
                None => {
                    let made = one(set, "s").and_then(|created| id_of(&created, "the submission"));
                    return made
                        .map(|id| HeldSubmission {
                            id,
                            email_id,
                            thread_id,
                        })
                        .map_err(unclear);
                }
            },
            Err(error) if refused_outright(&error) => error,
            Err(_) => return Err(JmapError::HoldUnknown),
        };
        // Nothing will go out, so the copy must not wait in Drafts.
        if self
            .set_emails(&Map::new(), std::slice::from_ref(&email_id), None)
            .await
            .is_err()
        {
            tracing::warn!("could not remove the Drafts copy of a hold the server refused");
        }
        Err(refused)
    }

    /// The submissions `ids` name, as the server holds them now.
    pub async fn submissions(&self, ids: &[String]) -> Result<Vec<SubmissionState>, JmapError> {
        let session = self.session().await?;
        let account = submission_account(&session)?;
        let mut request = Request::new();
        let got = request.call(SUBMISSION, "EmailSubmission/get", json!({
            "accountId": account, "ids": ids, "properties": ["id", "emailId", "undoStatus", "sendAt", "envelope"]
        }));
        let mut answer = self.send(&request).await?;
        Ok(answer.take::<Listing>(&got, "EmailSubmission/get")?.list)
    }

    /// The submissions that send the message `email_id`: how a client
    /// finds a hold whose answer it never got.
    pub async fn submissions_for(&self, email_id: &str) -> Result<Vec<String>, JmapError> {
        #[derive(Deserialize)]
        struct Ids {
            #[serde(default)]
            ids: Vec<String>,
        }
        let session = self.session().await?;
        let account = submission_account(&session)?;
        let mut request = Request::new();
        let asked = request.call(
            SUBMISSION,
            "EmailSubmission/query",
            json!({
                "accountId": account, "filter": { "emailIds": [email_id] }
            }),
        );
        let mut answer = self.send(&request).await?;
        Ok(answer.take::<Ids>(&asked, "EmailSubmission/query")?.ids)
    }

    /// Whether the server answers `EmailSubmission/get`, without which a
    /// hold cannot be called back. Apache James answers `unknownMethod`.
    pub async fn answers_submission_get(&self) -> Result<bool, JmapError> {
        match self.submissions(&[]).await {
            Ok(_) => Ok(true),
            Err(JmapError::UnknownCapability(_)) => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// Calls the hold `id` back.
    pub async fn cancel_submission(&self, id: &str) -> Result<Canceled, JmapError> {
        let session = self.session().await?;
        let account = submission_account(&session)?;
        let mut request = Request::new();
        let set = request.call(
            SUBMISSION,
            "EmailSubmission/set",
            json!({
                "accountId": account, "update": { id: { "undoStatus": "canceled" } }
            }),
        );
        let mut answer = self.send(&request).await?;
        let said: Updated = answer.take(&set, "EmailSubmission/set")?;
        if let Some(refused) = said.not_updated.as_ref().and_then(|n| n.get(id)) {
            return match refused.kind.as_str() {
                "cannotUnsend" => Ok(Canceled::TooLate),
                "notFound" => Ok(Canceled::Gone),
                _ => Err(refused.error()),
            };
        }
        match said.updated.as_ref().is_some_and(|u| u.contains_key(id)) {
            true => Ok(Canceled::Canceled),
            // James answers neither; it never calls a hold back.
            false => Err(JmapError::Decode(
                "the server said nothing about the cancel".into(),
            )),
        }
    }

    /// Moves a hold the server sent from Drafts to Sent, and takes off
    /// `$draft` and [`HELD`].
    pub async fn file_held(
        &self,
        email_id: &str,
        drafts: &str,
        sent: &str,
    ) -> Result<(), JmapError> {
        let mut patch = Map::new();
        patch.insert(format!("mailboxIds/{}", pointer(drafts)), Value::Null);
        patch.insert(format!("mailboxIds/{}", pointer(sent)), json!(true));
        patch.insert("keywords/$draft".into(), Value::Null);
        patch.insert(format!("keywords/{HELD}"), Value::Null);
        self.patch_email(email_id, patch).await
    }

    /// Takes [`HELD`] off a hold called back in time, which leaves an
    /// ordinary draft.
    pub async fn unhold(&self, email_id: &str) -> Result<(), JmapError> {
        let mut patch = Map::new();
        patch.insert(format!("keywords/{HELD}"), Value::Null);
        self.patch_email(email_id, patch).await
    }

    async fn patch_email(
        &self,
        email_id: &str,
        patch: Map<String, Value>,
    ) -> Result<(), JmapError> {
        let mut update = Map::new();
        update.insert(email_id.to_string(), Value::Object(patch));
        let answer = self.set_emails(&update, &[], None).await?;
        match answer.refusal(email_id) {
            Some(refused) => Err(refused.error()),
            None => Ok(()),
        }
    }
}
