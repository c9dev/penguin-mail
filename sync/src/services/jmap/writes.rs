//! Moves, marks and Delete Forever as `Email/set` patches. A patch names
//! only what it changes, so another client's change to the same message
//! stays. A message never sits in no mailbox: taking it out of its last
//! one files it in Archive. A write the server takes for some messages and
//! refuses for one says how many from the front went through. Checked
//! deletions report each message separately, including later successes.

use std::collections::BTreeSet;

use mailrs_domain::Role;
use mailrs_domain::translate::gettext;
use mailrs_jmap::{SetResponse, pointer};
use serde_json::{Map, Value, json};

use super::{Jmap, JmapApi, backend};
use crate::services::{Relocated, Unapplied};
use crate::{BackendError, MailOp};

/// The most messages one `Email/set` names, when the server's
/// `maxObjectsInSet` allows it.
const PER_SET: usize = 500;

/// One step of a write on a message's mailboxes.
enum Place {
    Add(String),
    Remove(String),
    Only(String),
}

/// One write's patch, the same for every message it names.
struct Patch {
    fields: Map<String, Value>,
    /// The mailbox steps in order, to work out where each message ends up.
    places: Vec<Place>,
}

impl Patch {
    /// Whether the write takes mailboxes away, so a message could end up in
    /// none: each message needs a look before it goes.
    fn removes(&self) -> bool {
        self.places.iter().any(|p| matches!(p, Place::Remove(_)))
    }

    /// The mailboxes a message in `now` is in after the write.
    fn end_in(&self, now: &BTreeSet<String>) -> BTreeSet<String> {
        let mut here = now.clone();
        for place in &self.places {
            match place {
                Place::Add(id) => drop(here.insert(id.clone())),
                Place::Remove(id) => drop(here.remove(id)),
                Place::Only(id) => here = BTreeSet::from([id.clone()]),
            }
        }
        here
    }
}

fn unapplied(taken: usize, error: BackendError) -> Unapplied {
    Unapplied {
        taken,
        error,
        relocated: Vec::new(),
    }
}

/// Puts `id` in or takes it out of the patch, inside a whole
/// `mailboxIds` when a move already set one: a patch may not name a path
/// and a path under it.
fn put_mailbox(fields: &mut Map<String, Value>, id: &str, on: bool) {
    if let Some(Value::Object(whole)) = fields.get_mut("mailboxIds") {
        match on {
            true => whole.insert(id.to_string(), json!(true)),
            false => whole.remove(id),
        };
        return;
    }
    fields.insert(
        format!("mailboxIds/{}", pointer(id)),
        if on { json!(true) } else { Value::Null },
    );
}

fn set_whole(fields: &mut Map<String, Value>, id: &str) {
    fields.retain(|key, _| !key.starts_with("mailboxIds"));
    let mut whole = Map::new();
    whole.insert(id.to_string(), json!(true));
    fields.insert("mailboxIds".into(), Value::Object(whole));
}

/// The first message of `chunk`, in order, the server refused, and why.
/// A message the server no longer has counts as done.
fn first_refusal(chunk: &[String], answer: &SetResponse) -> Option<(usize, BackendError)> {
    chunk.iter().enumerate().find_map(|(at, id)| {
        let refused = answer.refusal(id).filter(|e| e.kind != "notFound")?;
        Some((at, backend(refused.error())))
    })
}

impl<J: JmapApi> Jmap<J> {
    fn patch_of(&self, ops: &[MailOp]) -> Result<Patch, BackendError> {
        let known = self.known();
        let role_id = |role: &Role| {
            known.roles.get(role).cloned().ok_or_else(|| {
                BackendError::Refused(gettext("The server has no mailbox for this."))
            })
        };
        let mut fields = Map::new();
        let mut places = Vec::new();
        for op in ops {
            match op {
                MailOp::AddToMailbox(id) => {
                    put_mailbox(&mut fields, id, true);
                    places.push(Place::Add(id.clone()));
                }
                MailOp::RemoveFromMailbox(id) => {
                    put_mailbox(&mut fields, id, false);
                    places.push(Place::Remove(id.clone()));
                }
                MailOp::MoveToRole(role) => {
                    let target = role_id(role)?;
                    set_whole(&mut fields, &target);
                    places.push(Place::Only(target));
                }
                MailOp::MoveToMailbox(id) => {
                    set_whole(&mut fields, id);
                    places.push(Place::Only(id.clone()));
                }
                MailOp::SetKeyword { keyword, on } => {
                    fields.insert(
                        format!("keywords/{}", pointer(keyword)),
                        if *on { json!(true) } else { Value::Null },
                    );
                }
                MailOp::SetCategory { .. } => {
                    return Err(BackendError::Refused(gettext(
                        "This server does not sort mail into categories.",
                    )));
                }
                MailOp::Destroy => {}
            }
        }
        Ok(Patch { fields, places })
    }

    /// Each message's patch: the write's own and, for a message it would
    /// take out of every mailbox, Archive in their place.
    async fn updates_for(
        &self,
        chunk: &[String],
        patch: &Patch,
    ) -> Result<Map<String, Value>, BackendError> {
        let mut update = Map::new();
        if !patch.removes() {
            for id in chunk {
                update.insert(id.clone(), Value::Object(patch.fields.clone()));
            }
            return Ok(update);
        }
        let got = self
            .api()
            .emails(chunk, &["id", "mailboxIds"])
            .await
            .map_err(backend)?;
        let mut archive = self.known().roles.get(&Role::Archive).cloned();
        for email in got.list {
            let now: BTreeSet<String> = email
                .mailbox_ids
                .iter()
                .filter(|(_, on)| **on)
                .map(|(id, _)| id.clone())
                .collect();
            let mut fields = patch.fields.clone();
            if patch.end_in(&now).is_empty() {
                if archive.is_none() {
                    archive = self.ensure_archive().await?;
                }
                let archive = archive.as_deref().ok_or_else(|| {
                    BackendError::Refused(gettext("A message must stay in at least one mailbox, and this server has no Archive."))
                })?;
                put_mailbox(&mut fields, archive, true);
            }
            update.insert(email.id, Value::Object(fields));
        }
        Ok(update)
    }

    pub(super) async fn destroy_in(
        &self,
        messages: &[String],
        mailbox: &str,
    ) -> Vec<Result<(), BackendError>> {
        let limits = match self.api().session().await {
            Ok(session) => session.limits(),
            Err(error) => return vec![Err(backend(error)); messages.len()],
        };
        let limit = limits
            .max_objects_in_set
            .min(limits.max_objects_in_get)
            .clamp(1, PER_SET);
        let mut results = Vec::with_capacity(messages.len());
        for chunk in messages.chunks(limit) {
            let answer = async {
                let got = self
                    .api()
                    .emails(chunk, &["id", "mailboxIds"])
                    .await
                    .map_err(backend)?;
                if got.state.is_empty()
                    || got
                        .list
                        .iter()
                        .any(|m| m.mailbox_ids.get(mailbox) != Some(&true))
                {
                    return Err(BackendError::Changed);
                }
                self.api()
                    .set_emails(&Map::new(), chunk, Some(&got.state))
                    .await
                    .map_err(backend)
            }
            .await;
            let answer = match answer {
                Ok(answer) => answer,
                Err(error) => {
                    results.resize(messages.len(), Err(error));
                    break;
                }
            };
            let destroyed = answer.destroyed.unwrap_or_default();
            let refused = answer.not_destroyed.unwrap_or_default();
            let mut failure = None;
            for id in chunk {
                let result = match refused.get(id) {
                    Some(error) if error.kind == "notFound" => Ok(()),
                    Some(error) => Err(backend(error.error())),
                    None if destroyed.contains(id) => Ok(()),
                    None => Err(BackendError::StateLost),
                };
                if let Err(error) = &result {
                    failure.get_or_insert_with(|| error.clone());
                }
                results.push(result);
            }
            if let Some(error) = failure {
                results.resize(messages.len(), Err(error));
                break;
            }
        }
        results
    }

    pub(super) async fn write(
        &self,
        messages: &[String],
        ops: &[MailOp],
    ) -> Result<Vec<Relocated>, Unapplied> {
        self.listed().await.map_err(|e| unapplied(0, e))?;
        let destroy = ops.iter().any(|op| matches!(op, MailOp::Destroy));
        if ops
            .iter()
            .any(|op| matches!(op, MailOp::MoveToRole(Role::Archive)))
        {
            self.ensure_archive().await.map_err(|e| unapplied(0, e))?;
        }
        let patch = match destroy {
            true => None,
            false => Some(self.patch_of(ops).map_err(|e| unapplied(0, e))?),
        };
        let per_set = self
            .api()
            .session()
            .await
            .map_err(|e| unapplied(0, backend(e)))?
            .limits()
            .max_objects_in_set
            .clamp(1, PER_SET);
        let mut taken = 0;
        for chunk in messages.chunks(per_set) {
            let answer = match &patch {
                None => self.api().set_emails(&Map::new(), chunk, None).await,
                Some(patch) => {
                    let update = self
                        .updates_for(chunk, patch)
                        .await
                        .map_err(|e| unapplied(taken, e))?;
                    self.api().set_emails(&update, &[], None).await
                }
            }
            .map_err(|e| unapplied(taken, backend(e)))?;
            // The server applied every id it did not refuse, also those past
            // the first refusal. Only the ones before it count as taken, as
            // `Unapplied` counts from the front; the next sync restates the
            // rest, which the feed does for every changed message.
            if let Some((at, error)) = first_refusal(chunk, &answer) {
                return Err(unapplied(taken + at, error));
            }
            taken += chunk.len();
        }
        Ok(Vec::new())
    }
}
