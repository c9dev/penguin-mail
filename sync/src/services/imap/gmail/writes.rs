//! Changes to Gmail mail over IMAP. A label is added or removed in All
//! Mail with X-GM-LABELS; Starred and Unread are flags; Trash and Spam are
//! moves between the three mailboxes. A write never touches a label's own
//! folder, where a change would only copy or hide the message.

use std::collections::HashMap;

use mailrs_domain::Location;
use mailrs_imap::UidSet;

use super::labels::{Where, imap_label};
use super::{Imap, ImapApi, Submit, Trio};
use crate::services::google::writes::labels_for;
use crate::services::imap::BATCH_LIMIT;
use crate::services::{Relocated, Unapplied};
use crate::{BackendError, MailOp};

const DELETED: &str = "\\Deleted";

/// `names` placed, in runs that share a mailbox and its UIDVALIDITY, at
/// most `limit` each, in the order given.
fn runs(placed: Vec<(String, Location)>, limit: usize) -> Vec<Vec<(String, Location)>> {
    let mut runs: Vec<Vec<(String, Location)>> = Vec::new();
    for (name, at) in placed {
        match runs.last_mut() {
            Some(run)
                if run.len() < limit
                    && run.first().is_some_and(|(_, first)| {
                        first.mailbox == at.mailbox && first.uidvalidity == at.uidvalidity
                    }) =>
            {
                run.push((name, at))
            }
            _ => runs.push(vec![(name, at)]),
        }
    }
    runs
}

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// Applies `ops` to the messages `names`, a run at a time.
    pub(in crate::services::imap) async fn gm_write(
        &self,
        names: &[String],
        ops: &[MailOp],
    ) -> Result<Vec<Relocated>, Unapplied> {
        let mut taken = 0;
        let mut relocated = Vec::new();
        match self
            .gm_write_runs(names, ops, &mut taken, &mut relocated)
            .await
        {
            Ok(()) => Ok(relocated),
            Err(error) => Err(Unapplied {
                taken,
                error,
                relocated,
            }),
        }
    }

    async fn gm_write_runs(
        &self,
        names: &[String],
        ops: &[MailOp],
        taken: &mut usize,
        relocated: &mut Vec<Relocated>,
    ) -> Result<(), BackendError> {
        if ops
            .iter()
            .any(|op| matches!(op, MailOp::SetCategory { .. }))
        {
            return Err(BackendError::Unsupported);
        }
        let destroy = ops == [MailOp::Destroy];
        let (add, remove) = match destroy {
            true => (Vec::new(), Vec::new()),
            false => labels_for(ops)?,
        };
        let trio = self.gm_trio().await?;
        let mut placed = Vec::with_capacity(names.len());
        for name in names {
            placed.push((name.clone(), self.gm_place(name).await?));
        }
        for run in runs(placed, BATCH_LIMIT) {
            let Some((_, first)) = run.first() else {
                continue;
            };
            let mailbox = first.mailbox.clone();
            // After a UIDVALIDITY change the same UIDs name other messages.
            if self.select(&mailbox, None).await?.uidvalidity != first.uidvalidity {
                return Err(BackendError::NotFound);
            }
            let uids = UidSet::from_uids(run.iter().map(|(_, at)| at.uid));
            let at = trio.place_of(&mailbox);
            match destroy {
                true => self.gm_destroy(&trio, &mailbox, at, &run, &uids).await?,
                false => {
                    self.gm_run(&trio, &mailbox, at, &run, &uids, (&add, &remove), relocated)
                        .await?
                }
            }
            *taken += run.len();
        }
        Ok(())
    }

    /// One run's labels, flags and moves.
    #[allow(clippy::too_many_arguments)]
    async fn gm_run(
        &self,
        trio: &Trio,
        mailbox: &str,
        at: Where,
        run: &[(String, Location)],
        uids: &UidSet,
        (add, remove): (&[String], &[String]),
        relocated: &mut Vec<Relocated>,
    ) -> Result<(), BackendError> {
        let named = |list: &[String], id: &str| list.iter().any(|l| l == id);
        let moving = |id: &str| !matches!(id, "TRASH" | "SPAM");
        // Labels and flags first, where the message is now.
        if at == Where::AllMail {
            let adds: Vec<String> = add
                .iter()
                .filter(|l| moving(l))
                .filter_map(|l| imap_label(l))
                .collect();
            let removes: Vec<String> = remove
                .iter()
                .filter(|l| moving(l))
                .filter_map(|l| imap_label(l))
                .collect();
            self.api.store_labels(mailbox, uids, true, &adds).await?;
            self.api
                .store_labels(mailbox, uids, false, &removes)
                .await?;
        }
        let flags = |on: &[String], off: &[String]| -> (Vec<String>, Vec<String>) {
            let mut set = Vec::new();
            let mut clear = Vec::new();
            if named(on, "STARRED") {
                set.push("\\Flagged".to_string());
            }
            if named(off, "STARRED") {
                clear.push("\\Flagged".to_string());
            }
            // Unread is the lack of `\Seen`.
            if named(on, "UNREAD") {
                clear.push("\\Seen".to_string());
            }
            if named(off, "UNREAD") {
                set.push("\\Seen".to_string());
            }
            (set, clear)
        };
        let (set, clear) = flags(add, remove);
        if !set.is_empty() {
            self.api.store(mailbox, uids, true, &set).await?;
        }
        if !clear.is_empty() {
            self.api.store(mailbox, uids, false, &clear).await?;
        }
        let target = if named(add, "TRASH") && at != Where::Trash {
            Some(trio.trash.as_str())
        } else if named(add, "SPAM") && at != Where::Spam {
            Some(trio.spam.as_str())
        } else if at != Where::AllMail && (named(remove, "TRASH") || named(remove, "SPAM")) {
            Some(trio.all.as_str())
        } else {
            None
        };
        if let Some(to) = target {
            let landed = self.gm_move(mailbox, run, uids, to).await?;
            if to == trio.all {
                // Restoring from Trash or Spam puts the message back in
                // the Inbox.
                let back = UidSet::from_uids(landed.iter().map(|(_, at)| at.uid));
                self.api
                    .store_labels(to, &back, true, &["\\Inbox".to_string()])
                    .await?;
            }
            relocated.extend(landed.into_iter().map(|(from, to)| Relocated { from, to }));
        }
        Ok(())
    }

    /// Moves a run from `from` to `to` and answers where each message
    /// landed, by the name it was handed in under.
    async fn gm_move(
        &self,
        from: &str,
        run: &[(String, Location)],
        uids: &UidSet,
        to: &str,
    ) -> Result<Vec<(String, Location)>, BackendError> {
        let copied = self.api.move_to(from, uids, to).await?;
        let by_uid: HashMap<u32, &String> = run.iter().map(|(name, at)| (at.uid, name)).collect();
        if let Some(copy) = copied {
            let landed: Vec<(String, Location)> = copy
                .pairs
                .iter()
                .filter_map(|(old, new)| {
                    let name = (*by_uid.get(old)?).clone();
                    Some((
                        name,
                        Location {
                            mailbox: to.to_string(),
                            uidvalidity: copy.uidvalidity,
                            uid: *new,
                        },
                    ))
                })
                .collect();
            if landed.len() == run.len() {
                self.remember(&landed);
                return Ok(landed);
            }
        }
        // No usable COPYUID: the ids find the copies.
        for (name, _) in run {
            self.known().places.forget(name);
        }
        let names: Vec<String> = run.iter().map(|(name, _)| name.clone()).collect();
        let mut found = self.gm_find(&names).await?;
        Ok(names
            .into_iter()
            .filter_map(|name| found.remove(&name).map(|at| (name, at)))
            .collect())
    }

    fn remember(&self, landed: &[(String, Location)]) {
        let mut known = self.known();
        for (name, at) in landed {
            known.places.put(name.clone(), at.clone());
        }
    }

    /// Erases a run for good: out of All Mail into Trash first, then
    /// `\Deleted` and a UID EXPUNGE there. Nothing is expunged in All Mail,
    /// where Gmail would only archive the message.
    async fn gm_destroy(
        &self,
        trio: &Trio,
        mailbox: &str,
        at: Where,
        run: &[(String, Location)],
        uids: &UidSet,
    ) -> Result<(), BackendError> {
        let (in_trash, trash_uids) = match at {
            Where::AllMail => {
                let landed = self.gm_move(mailbox, run, uids, &trio.trash).await?;
                (
                    trio.trash.clone(),
                    UidSet::from_uids(landed.iter().map(|(_, at)| at.uid)),
                )
            }
            _ => (mailbox.to_string(), uids.clone()),
        };
        self.api
            .store(&in_trash, &trash_uids, true, &[DELETED.to_string()])
            .await?;
        self.api.expunge(&in_trash, &trash_uids).await?;
        for (name, _) in run {
            self.known().places.forget(name);
        }
        Ok(())
    }
}
