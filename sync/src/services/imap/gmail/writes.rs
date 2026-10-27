//! Changes to Gmail mail over IMAP. A label is added or removed in All
//! Mail with X-GM-LABELS; Starred and Unread are flags; Trash and Spam are
//! moves between the three mailboxes. A write never touches a label's own
//! folder, where a change would only copy or hide the message.

use std::collections::HashMap;

use mailrs_domain::Location;
use mailrs_imap::UidSet;

use super::labels::{GmailPlace, imap_label};
use super::{GmailMailboxes, Imap, ImapApi, Submit};
use crate::services::google::writes::labels_for;
use crate::services::imap::BATCH_LIMIT;
use crate::services::{Relocated, Unapplied};
use crate::{BackendError, MailOp};

const DELETED: &str = "\\Deleted";

/// The mailbox a run of messages sits in, which of the three it is, and
/// the account's three mailboxes the run can move between.
struct RunAt<'a> {
    boxes: &'a GmailMailboxes,
    mailbox: &'a str,
    kind: GmailPlace,
}

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

/// Whether a message moved back to All Mail from Trash or Spam goes to
/// the Inbox: when the ops add the Inbox, or name no other label and do
/// not take the Inbox away.
fn inbox_on_restore(add: &[String], remove: &[String]) -> bool {
    let other_label = add
        .iter()
        .filter(|l| {
            !matches!(
                l.as_str(),
                "TRASH" | "SPAM" | "INBOX" | "IMPORTANT" | "MUTE"
            )
        })
        .any(|l| imap_label(l).is_some());
    add.iter().any(|l| l == "INBOX") || (!other_label && !remove.iter().any(|l| l == "INBOX"))
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
        let boxes = self.gm_synced().await?;
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
            let at = RunAt {
                boxes: &boxes,
                mailbox: &mailbox,
                kind: boxes.place_of(&mailbox),
            };
            match destroy {
                true => self.gm_destroy(&at, &run, &uids).await?,
                false => {
                    self.gm_run(&at, &run, &uids, (&add, &remove), relocated)
                        .await?
                }
            }
            *taken += run.len();
        }
        Ok(())
    }

    /// One run's labels, flags and moves. Messages a move could not find
    /// afterwards make the run fail with `NotFound`, after the ones that
    /// landed are recorded.
    async fn gm_run(
        &self,
        RunAt {
            boxes,
            mailbox,
            kind: at,
        }: &RunAt<'_>,
        run: &[(String, Location)],
        uids: &UidSet,
        (add, remove): (&[String], &[String]),
        relocated: &mut Vec<Relocated>,
    ) -> Result<(), BackendError> {
        let named = |list: &[String], id: &str| list.iter().any(|l| l == id);
        let moving = |id: &str| !matches!(id, "TRASH" | "SPAM");
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
        let at = *at;
        let restores =
            at != GmailPlace::AllMail && (named(remove, "TRASH") || named(remove, "SPAM"));
        // Labels and flags first, where the message is now. Gmail keeps a
        // trashed message's labels, so they change in Trash or Spam too;
        // a restore's labels wait for the move back to All Mail, and the
        // Inbox label in Trash or Spam waits for a restore.
        if at == GmailPlace::AllMail || !restores {
            let here = |list: &[String]| -> Vec<String> {
                list.iter()
                    .filter(|l| at == GmailPlace::AllMail || l.as_str() != "\\Inbox")
                    .cloned()
                    .collect()
            };
            let (on, off) = (here(&adds), here(&removes));
            if !on.is_empty() {
                self.api.store_labels(mailbox, uids, true, &on).await?;
            }
            if !off.is_empty() {
                self.api.store_labels(mailbox, uids, false, &off).await?;
            }
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
        let target = if named(add, "TRASH") && at != GmailPlace::Trash {
            Some(boxes.trash.as_str())
        } else if named(add, "SPAM") && at != GmailPlace::Spam {
            Some(boxes.spam.as_str())
        } else if restores {
            Some(boxes.all.as_str())
        } else {
            None
        };
        if let Some(to) = target {
            let landed = self.gm_move(mailbox, run, uids, to).await?;
            // Recorded before anything else can fail: the messages have
            // left `mailbox` whatever happens to their labels.
            relocated.extend(landed.iter().map(|(from, to)| Relocated {
                from: from.clone(),
                to: to.clone(),
            }));
            if to == boxes.all {
                // Back in All Mail, the message takes the labels the ops
                // name. A restore that names no label of its own, and
                // does not archive, puts it back in the Inbox.
                let back = UidSet::from_uids(landed.iter().map(|(_, at)| at.uid));
                let mut adds = adds;
                if inbox_on_restore(add, remove) && !named(&adds, "\\Inbox") {
                    adds.push("\\Inbox".to_string());
                }
                self.api.store_labels(to, &back, true, &adds).await?;
                self.api.store_labels(to, &back, false, &removes).await?;
            }
            if landed.len() < run.len() {
                return Err(BackendError::NotFound);
            }
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
    /// A message the move into Trash could not find afterwards stays, and
    /// the run fails with `NotFound` once the rest are erased.
    async fn gm_destroy(
        &self,
        at: &RunAt<'_>,
        run: &[(String, Location)],
        uids: &UidSet,
    ) -> Result<(), BackendError> {
        let (in_trash, trash_uids, all_landed) = match at.kind {
            GmailPlace::AllMail => {
                let landed = self.gm_move(at.mailbox, run, uids, &at.boxes.trash).await?;
                (
                    at.boxes.trash.clone(),
                    UidSet::from_uids(landed.iter().map(|(_, at)| at.uid)),
                    landed.len() == run.len(),
                )
            }
            _ => (at.mailbox.to_string(), uids.clone(), true),
        };
        self.api
            .store(&in_trash, &trash_uids, true, &[DELETED.to_string()])
            .await?;
        self.api.expunge(&in_trash, &trash_uids).await?;
        for (name, _) in run {
            self.known().places.forget(name);
        }
        match all_landed {
            true => Ok(()),
            false => Err(BackendError::NotFound),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::inbox_on_restore;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_plain_restore_goes_back_to_the_inbox() {
        assert!(inbox_on_restore(&[], &ids(&["TRASH"])));
        assert!(inbox_on_restore(&ids(&["INBOX"]), &ids(&["SPAM"])));
    }

    #[test]
    fn a_move_to_a_label_out_of_trash_keeps_it_out_of_the_inbox() {
        assert!(!inbox_on_restore(&ids(&["Work"]), &ids(&["TRASH"])));
    }

    #[test]
    fn not_spam_then_archive_stays_out_of_the_inbox() {
        assert!(!inbox_on_restore(&[], &ids(&["SPAM", "INBOX"])));
    }
}
