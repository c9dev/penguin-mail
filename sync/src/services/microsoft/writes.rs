//! Moving, marking, tagging and erasing mail, and making and changing
//! folders. A message sits in one folder, so filing is a move. Graph keeps
//! the id across the move, so nothing is relocated. Categories are one
//! list per message that Graph replaces whole, so a tag change reads each
//! message's list first. Marks go before the move, in their own `$batch`,
//! since Graph runs the entries of one batch in any order.

use std::collections::BTreeSet;

use mailrs_domain::mailbox::keyword::{FLAGGED, SEEN};
use mailrs_domain::{MailboxKind, RemoteMailbox, Role, category};
use mailrs_gmail::LabelColor;
use mailrs_graph::{GraphError, MessagePatch, Write};

use super::folders::PRESETS;
use super::{GraphApi, Microsoft, backend, tag_name};
use crate::services::{Relocated, Unapplied};
use crate::{BackendError, MailOp};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Destination {
    Role(Role),
    Folder(String),
}

/// What one write asks of each message.
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    read: Option<bool>,
    flagged: Option<bool>,
    other: Option<bool>,
    tags_on: Vec<String>,
    tags_off: Vec<String>,
    to: Option<Destination>,
    destroy: bool,
}

/// `ops` as a patch, a tag change and a move. Undo reverses a move as
/// adding the old folder back and taking the new one away, so adding a
/// folder moves there and taking one away alone asks nothing.
fn plan(ops: &[MailOp]) -> Result<Plan, BackendError> {
    let mut plan = Plan::default();
    for op in ops {
        match op {
            MailOp::Destroy => plan.destroy = true,
            MailOp::SetKeyword { keyword, on } if keyword == SEEN => plan.read = Some(*on),
            MailOp::SetKeyword { keyword, on } if keyword == FLAGGED => plan.flagged = Some(*on),
            // Any other keyword stays on this computer (`split_keywords`).
            MailOp::SetKeyword { .. } => {}
            MailOp::SetCategory { category: c, on } if c == category::OTHER => plan.other = Some(*on),
            MailOp::SetCategory { .. } => return Err(BackendError::Unsupported),
            MailOp::AddToMailbox(id) => match tag_name(id) {
                Some(name) => plan.tags_on.push(name.to_string()),
                None => plan.to = Some(Destination::Folder(id.clone())),
            },
            MailOp::RemoveFromMailbox(id) => {
                if let Some(name) = tag_name(id) {
                    plan.tags_off.push(name.to_string());
                }
            }
            MailOp::MoveToRole(role) => plan.to = Some(Destination::Role(*role)),
            MailOp::MoveToMailbox(id) => plan.to = Some(Destination::Folder(id.clone())),
        }
    }
    Ok(plan)
}

/// The count of messages from the front whose every write went through,
/// and the first error, when one failed.
fn taken(outcomes: &[Result<(), GraphError>]) -> Option<(usize, GraphError)> {
    outcomes
        .iter()
        .position(Result::is_err)
        .map(|i| (i, outcomes[i].clone().unwrap_err()))
}

impl<G: GraphApi> Microsoft<G> {
    pub(super) async fn write(&self, messages: &[String], ops: &[MailOp]) -> Result<Vec<Relocated>, Unapplied> {
        let refuse = |taken: usize, error: BackendError| Unapplied { taken, error, relocated: Vec::new() };
        let plan = plan(ops).map_err(|e| refuse(0, e))?;
        if plan.destroy {
            let writes: Vec<Write> = messages.iter().map(|id| Write::PermanentDelete { id: id.clone() }).collect();
            return self.run(&writes).await.map(|()| Vec::new());
        }
        let categories = match plan.tags_on.is_empty() && plan.tags_off.is_empty() {
            true => vec![None; messages.len()],
            false => self.categories_of(messages, &plan).await.map_err(|e| refuse(0, e))?,
        };
        let patches: Vec<Write> = messages
            .iter()
            .zip(categories)
            .map(|(id, categories)| Write::Patch {
                id: id.clone(),
                patch: MessagePatch { is_read: plan.read, flagged: plan.flagged, other: plan.other, categories },
            })
            .filter(|w| !matches!(w, Write::Patch { patch, .. } if *patch == MessagePatch::default()))
            .collect();
        if !patches.is_empty() {
            self.run(&patches).await?;
        }
        if let Some(to) = &plan.to {
            let folder = match to {
                Destination::Folder(id) => id.clone(),
                Destination::Role(role) => {
                    self.known().roles.get(role).cloned().ok_or_else(|| refuse(0, BackendError::Unsupported))?
                }
            };
            let moves: Vec<Write> = messages.iter().map(|id| Write::Move { id: id.clone(), folder: folder.clone() }).collect();
            self.run(&moves).await?;
        }
        Ok(Vec::new())
    }

    /// Each message's category list after the plan's tag change, read from
    /// Graph so a category another client added is kept.
    async fn categories_of(&self, messages: &[String], plan: &Plan) -> Result<Vec<Option<Vec<String>>>, BackendError> {
        let answers = self.graph().messages(messages).await.map_err(backend)?;
        answers
            .into_iter()
            .map(|answer| {
                let held = answer.map_err(backend)?;
                let mut set: BTreeSet<String> = held.categories.unwrap_or_default().into_iter().collect();
                set.extend(plan.tags_on.iter().cloned());
                for off in &plan.tags_off {
                    set.remove(off);
                }
                Ok(Some(set.into_iter().collect()))
            })
            .collect()
    }

    async fn run(&self, writes: &[Write]) -> Result<(), Unapplied> {
        let outcomes = self
            .graph()
            .apply(writes)
            .await
            .map_err(|e| Unapplied { taken: 0, error: backend(e), relocated: Vec::new() })?;
        match taken(&outcomes) {
            None => Ok(()),
            Some((taken, error)) => Err(Unapplied { taken, error: backend(error), relocated: Vec::new() }),
        }
    }

    /// Makes a folder a person names, `Parent/Child` nesting under an
    /// existing or a new parent.
    pub(super) async fn make_folder(&self, name: &str) -> Result<RemoteMailbox, BackendError> {
        let mut parent: Option<String> = None;
        let mut path = String::new();
        for segment in name.split('/').filter(|s| !s.is_empty()) {
            path = if path.is_empty() { segment.to_string() } else { format!("{path}/{segment}") };
            let existing = self.known().names.iter().find(|(_, p)| **p == path).map(|(id, _)| id.clone());
            let id = match existing {
                Some(id) => id,
                None => {
                    let made = self.graph().create_folder(parent.as_deref(), segment).await.map_err(backend)?;
                    self.known().names.insert(made.id.clone(), path.clone());
                    made.id
                }
            };
            parent = Some(id);
        }
        let id = parent.ok_or_else(|| BackendError::Refused("a folder needs a name".into()))?;
        Ok(RemoteMailbox { id, name: path, kind: MailboxKind::Folder, role: None, color: None, hidden: false })
    }

    /// Renames a folder where it is. Graph cannot rename a category, and a
    /// new parent would be a move this adapter does not make.
    pub(super) async fn rename_folder(&self, id: &str, name: &str) -> Result<RemoteMailbox, BackendError> {
        if tag_name(id).is_some() {
            return Err(BackendError::Unsupported);
        }
        let old = self.known().names.get(id).cloned().ok_or(BackendError::NotFound)?;
        let parent = |path: &str| path.rsplit_once('/').map(|(p, _)| p.to_string());
        if parent(&old) != parent(name) {
            return Err(BackendError::Unsupported);
        }
        let leaf = name.rsplit('/').next().unwrap_or(name);
        self.graph().rename_folder(id, leaf).await.map_err(backend)?;
        self.known().names.insert(id.to_string(), name.to_string());
        Ok(RemoteMailbox { id: id.to_string(), name: name.to_string(), kind: MailboxKind::Folder, role: None, color: None, hidden: false })
    }

    pub(super) async fn remove_folder(&self, id: &str) -> Result<(), BackendError> {
        if tag_name(id).is_some() {
            return Err(BackendError::Unsupported);
        }
        self.graph().delete_folder(id).await.map_err(backend)?;
        self.known().names.remove(id);
        Ok(())
    }

    /// A tag takes the Outlook preset nearest `color`'s background; a folder
    /// has no colour on Graph.
    pub(super) async fn color_tag(&self, id: &str, color: &LabelColor) -> Result<RemoteMailbox, BackendError> {
        let name = tag_name(id).ok_or(BackendError::Unsupported)?;
        let graph_id = self.known().category_ids.get(name).cloned().ok_or(BackendError::NotFound)?;
        let preset = nearest_preset(&color.background_color);
        self.graph().set_category_color(&graph_id, preset).await.map_err(backend)?;
        Ok(RemoteMailbox {
            id: id.to_string(),
            name: name.to_string(),
            kind: MailboxKind::Tag,
            role: None,
            color: super::folders::preset_color(preset),
            hidden: false,
        })
    }
}

/// The Outlook preset whose colour is nearest `hex`, by the distance
/// between the two in RGB.
fn nearest_preset(hex: &str) -> &'static str {
    let rgb = |h: &str| -> (i32, i32, i32) {
        let h = h.trim_start_matches('#');
        let channel = |i: usize| i32::from_str_radix(h.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
        (channel(0), channel(2), channel(4))
    };
    let (r, g, b) = rgb(hex);
    PRESETS
        .iter()
        .min_by_key(|(_, preset)| {
            let (pr, pg, pb) = rgb(preset);
            (pr - r).pow(2) + (pg - g).pow(2) + (pb - b).pow(2)
        })
        .map_or("preset0", |(name, _)| name)
}
