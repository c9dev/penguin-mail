//! Mail operations over IMAP. A folder server holds each message in one
//! mailbox, so filing is a move: `UID MOVE` where the server has MOVE
//! (RFC 6851); else `UID COPY`, `\Deleted` and `UID EXPUNGE` where it has
//! UIDPLUS, so only those messages go; else a copy and the mark, and the
//! server expunges when it will. Keywords are flags, set before a move so
//! they travel with the message. Without UIDPLUS no answer names where a
//! copy landed, and the adapter finds each by its Message-ID.

use std::collections::HashMap;

use mailrs_domain::{Location, Role};
use mailrs_imap::{Capabilities, CopyUid, UidSet};

use super::keywords::flag_of;
use super::syntax::string;
use super::{BATCH_LIMIT, Imap, ImapApi, Submit};
use crate::services::{MailBackend, Relocated, Unapplied};
use crate::{BackendError, MailOp};

const DELETED: &str = "\\Deleted";

/// The mailbox a server without one is given the first time a person
/// archives.
const ARCHIVE: &str = "Archive";

/// Where a write moves messages.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Destination {
    Role(Role),
    Mailbox(String),
}

/// What one write asks of each message.
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    add: Vec<String>,
    remove: Vec<String>,
    to: Option<Destination>,
    destroy: bool,
}

/// `ops` as flags to set and clear and a mailbox to move to. Undo reverses
/// a move as adding the old mailbox back and taking the new one away, so
/// adding a mailbox moves there, and taking one away alone asks nothing of
/// a server where a message sits in one mailbox. Categories are Gmail's.
fn plan(ops: &[MailOp]) -> Result<Plan, BackendError> {
    let mut plan = Plan::default();
    for op in ops {
        match op {
            MailOp::Destroy => plan.destroy = true,
            MailOp::SetKeyword { keyword, on: true } => plan.add.push(flag_of(keyword)),
            MailOp::SetKeyword { keyword, on: false } => plan.remove.push(flag_of(keyword)),
            MailOp::MoveToRole(role) => plan.to = Some(Destination::Role(*role)),
            MailOp::MoveToMailbox(id) | MailOp::AddToMailbox(id) => {
                plan.to = Some(Destination::Mailbox(id.clone()));
            }
            MailOp::RemoveFromMailbox(_) => {}
            MailOp::SetCategory { .. } => return Err(BackendError::Unsupported),
        }
    }
    Ok(plan)
}

/// `names` in runs that share a mailbox and its UIDVALIDITY, `limit` at
/// most each, in the order given, so a refusal can say how many from the
/// front went through. A name that is no location runs alone as `None`.
fn runs(names: &[String], limit: usize) -> Vec<Option<Vec<Location>>> {
    let mut runs: Vec<Option<Vec<Location>>> = Vec::new();
    for name in names {
        let Some(at) = Location::parse(name) else {
            runs.push(None);
            continue;
        };
        match runs.last_mut() {
            Some(Some(run))
                if run.len() < limit
                    && run.first().is_some_and(|first| {
                        first.mailbox == at.mailbox && first.uidvalidity == at.uidvalidity
                    }) =>
            {
                run.push(at)
            }
            _ => runs.push(Some(vec![at])),
        }
    }
    runs
}

/// Where a COPYUID says each message of `run` landed in `to`.
fn relocations(run: &[Location], copy: &CopyUid, to: &str) -> Vec<Relocated> {
    let landed: HashMap<u32, u32> = copy.pairs.iter().copied().collect();
    run.iter()
        .filter_map(|at| {
            let uid = *landed.get(&at.uid)?;
            Some(Relocated {
                from: at.to_string(),
                to: Location {
                    mailbox: to.to_string(),
                    uidvalidity: copy.uidvalidity,
                    uid,
                },
            })
        })
        .collect()
}

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// Applies `ops` to the messages `names` locates, a run at a time.
    pub(super) async fn write(
        &self,
        names: &[String],
        ops: &[MailOp],
    ) -> Result<Vec<Relocated>, Unapplied> {
        let refused = |taken: usize, error: BackendError, relocated: Vec<Relocated>| Unapplied {
            taken,
            error,
            relocated,
        };
        let plan = plan(ops).map_err(|error| refused(0, error, Vec::new()))?;
        let capabilities = self
            .capabilities_now()
            .await
            .map_err(|error| refused(0, error, Vec::new()))?;
        let to = match &plan.to {
            Some(destination) => Some(
                self.destination(destination)
                    .await
                    .map_err(|error| refused(0, error, Vec::new()))?,
            ),
            None => None,
        };
        let mut relocated = Vec::new();
        let mut taken = 0;
        for run in runs(names, BATCH_LIMIT) {
            let Some(run) = run else {
                return Err(refused(taken, BackendError::NotFound, relocated));
            };
            match self
                .write_run(&run, &plan, to.as_deref(), &capabilities)
                .await
            {
                Ok(moved) => relocated.extend(moved),
                Err(error) => return Err(refused(taken, error, relocated)),
            }
            taken += run.len();
        }
        Ok(relocated)
    }

    /// One run's write: its flags, then its move, or its erasure.
    async fn write_run(
        &self,
        run: &[Location],
        plan: &Plan,
        to: Option<&str>,
        capabilities: &Capabilities,
    ) -> Result<Vec<Relocated>, BackendError> {
        let Some(first) = run.first() else {
            return Ok(Vec::new());
        };
        let mailbox = first.mailbox.as_str();
        // After a UIDVALIDITY change the same UIDs name other messages.
        if self.select(mailbox, None).await?.uidvalidity != first.uidvalidity {
            return Err(BackendError::NotFound);
        }
        let uids = UidSet::from_uids(run.iter().map(|at| at.uid));
        if plan.destroy {
            self.api
                .store(mailbox, &uids, true, &[DELETED.to_string()])
                .await?;
            if capabilities.uidplus {
                self.api.expunge(mailbox, &uids).await?;
            }
            return Ok(Vec::new());
        }
        if !plan.add.is_empty() {
            self.api.store(mailbox, &uids, true, &plan.add).await?;
        }
        if !plan.remove.is_empty() {
            self.api.store(mailbox, &uids, false, &plan.remove).await?;
        }
        match to {
            Some(to) if to != mailbox => self.move_run(mailbox, run, &uids, to, capabilities).await,
            _ => Ok(Vec::new()),
        }
    }

    /// Moves a run from `from` to `to` the best way the server offers.
    async fn move_run(
        &self,
        from: &str,
        run: &[Location],
        uids: &UidSet,
        to: &str,
        capabilities: &Capabilities,
    ) -> Result<Vec<Relocated>, BackendError> {
        // Without UIDPLUS no answer names where the copies land. With it, a
        // COPYUID that is too large or mismatched still comes back `None`,
        // after the move already happened, so each message's Message-ID is
        // always read from the source first, before it moves.
        let message_ids = self.message_ids(from, uids).await?;
        let copied = match capabilities.moves {
            true => self.api.move_to(from, uids, to).await?,
            false => {
                let copied = self.api.copy_to(from, uids, to).await?;
                self.api
                    .store(from, uids, true, &[DELETED.to_string()])
                    .await?;
                if capabilities.uidplus {
                    self.api.expunge(from, uids).await?;
                }
                copied
            }
        };
        match copied {
            Some(copy) => Ok(relocations(run, &copy, to)),
            None => self.find_moved(run, &message_ids, to).await,
        }
    }

    /// The Message-ID of each message of `uids`, by UID.
    async fn message_ids(
        &self,
        mailbox: &str,
        uids: &UidSet,
    ) -> Result<HashMap<u32, String>, BackendError> {
        Ok(self
            .api
            .headers(mailbox, uids)
            .await?
            .into_iter()
            .filter_map(|fetched| Some((fetched.uid, fetched.message_id?)))
            .collect())
    }

    /// Where each moved message landed in `to`: the newest UID there that
    /// carries its Message-ID. A message without one, or one the search
    /// misses, keeps its old ref; the feed then drops the old copy and
    /// brings the new one as a message of its own.
    async fn find_moved(
        &self,
        run: &[Location],
        message_ids: &HashMap<u32, String>,
        to: &str,
    ) -> Result<Vec<Relocated>, BackendError> {
        let uidvalidity = self.select(to, None).await?.uidvalidity;
        let mut relocated = Vec::new();
        for at in run {
            let Some(message_id) = message_ids.get(&at.uid) else {
                continue;
            };
            let keys = format!(
                "HEADER Message-ID {}",
                string(message_id.trim_matches(['<', '>']))
            );
            if let Some(uid) = self.api.search(to, &keys).await?.into_iter().max() {
                relocated.push(Relocated {
                    from: at.to_string(),
                    to: Location {
                        mailbox: to.to_string(),
                        uidvalidity,
                        uid,
                    },
                });
            }
        }
        Ok(relocated)
    }

    /// The mailbox a move goes to. A parent that holds no mail takes none.
    /// Archive is made the first time a server without one is asked to
    /// archive, and the listing after it gives the new mailbox its role.
    async fn destination(&self, destination: &Destination) -> Result<String, BackendError> {
        self.ensure_listed().await?;
        let role = match destination {
            Destination::Mailbox(id) => {
                let parent_only = self
                    .known()
                    .folders
                    .iter()
                    .any(|f| f.id == *id && f.parent_only);
                return match parent_only {
                    true => Err(BackendError::Unsupported),
                    false => Ok(id.clone()),
                };
            }
            Destination::Role(role) => *role,
        };
        if let Some(id) = self.mailbox_for(role) {
            return Ok(id);
        }
        if role != Role::Archive {
            return Err(BackendError::Unsupported);
        }
        self.api.create(ARCHIVE).await?;
        self.list_mailboxes().await?;
        self.mailbox_for(Role::Archive)
            .ok_or(BackendError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use mailrs_domain::Role;

    use super::{Destination, Plan, plan, runs};
    use crate::{BackendError, MailOp};

    #[test]
    fn undoing_a_move_moves_back_and_a_removal_alone_asks_nothing() {
        let undo = [
            MailOp::AddToMailbox("INBOX".into()),
            MailOp::RemoveFromMailbox("Archive".into()),
        ];
        assert_eq!(
            plan(&undo).unwrap(),
            Plan {
                to: Some(Destination::Mailbox("INBOX".into())),
                ..Plan::default()
            }
        );
        let mute = [
            MailOp::SetKeyword {
                keyword: "$muted".into(),
                on: true,
            },
            MailOp::MoveToRole(Role::Archive),
        ];
        assert_eq!(
            plan(&mute).unwrap(),
            Plan {
                add: vec!["$muted".into()],
                to: Some(Destination::Role(Role::Archive)),
                ..Plan::default()
            }
        );
        let read = [MailOp::SetKeyword {
            keyword: "$seen".into(),
            on: true,
        }];
        assert_eq!(plan(&read).unwrap().add, ["\\Seen"]);
        let sort = [MailOp::SetCategory {
            category: "CATEGORY_SOCIAL".into(),
            on: true,
        }];
        assert!(matches!(plan(&sort), Err(BackendError::Unsupported)));
    }

    #[test]
    fn a_write_goes_in_order_one_mailbox_and_batch_at_a_time() {
        let names: Vec<String> = [
            "INBOX/1/1",
            "INBOX/1/2",
            "INBOX/1/3",
            "Archive/1/1",
            "INBOX/1/4",
            "18c2a4f0",
        ]
        .map(String::from)
        .to_vec();
        let shape: Vec<Option<Vec<u32>>> = runs(&names, 2)
            .iter()
            .map(|run| {
                run.as_ref()
                    .map(|run| run.iter().map(|at| at.uid).collect())
            })
            .collect();
        assert_eq!(
            shape,
            [
                Some(vec![1, 2]),
                Some(vec![3]),
                Some(vec![1]),
                Some(vec![4]),
                None
            ]
        );
    }
}
