//! The change feed for Gmail over IMAP. Every look covers Trash, Spam and
//! All Mail, in that order, so a message that left All Mail for Trash is
//! met in Trash first. Gmail offers CONDSTORE and no QRESYNC: new UIDs are
//! new mail, a changed MODSEQ names a label change, and a search says what
//! each mailbox still holds. Each message that is new to a mailbox or
//! changed is reported at its place and as added, so the engine fetches
//! its labels again and the ref follows it between mailboxes.

use mailrs_domain::Location;
use mailrs_imap::UidSet;

use super::{Imap, ImapApi, Submit};
use crate::BackendError;
use crate::services::imap::keywords::is_deleted;
use crate::services::imap::state::{ImapState, Kept};
use crate::services::imap::window::{window_keys, windows};
use crate::services::{Changes, RemoteChange, SyncState};

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// With no state, where the three mailboxes stand now. With one, what
    /// changed in them since, and the state after.
    pub(in crate::services::imap) async fn gm_changes(
        &self,
        since: Option<&SyncState>,
    ) -> Result<Changes, BackendError> {
        let boxes = self.gm_synced().await?;
        let order = boxes.look_order();
        let Some(since) = since else {
            let mut state = ImapState::default();
            for mailbox in &order {
                let kept = self.kept_now(mailbox).await?;
                state.mailboxes.insert(mailbox.clone(), kept);
            }
            return Ok(Changes {
                changes: Vec::new(),
                state: state.written(),
            });
        };
        let mut state = ImapState::read(since)?;
        let mut changes = Vec::new();
        // The first look says where the window's messages sit, so the refs
        // of mail the store already holds are right without fetching a
        // header.
        if !state.located {
            for mailbox in &order {
                self.gm_locate_window(mailbox, &mut changes).await?;
            }
            state.located = true;
        }
        for mailbox in order {
            let kept = match state.mailboxes.get(&mailbox).cloned() {
                Some(kept) => {
                    self.gm_mailbox_changes(&mailbox, kept, &mut changes)
                        .await?
                }
                None => self.kept_now(&mailbox).await?,
            };
            state.mailboxes.insert(mailbox, kept);
        }
        Ok(Changes {
            changes,
            state: state.written(),
        })
    }

    /// A `Located` for each message of the window in `mailbox`.
    async fn gm_locate_window(
        &self,
        mailbox: &str,
        changes: &mut Vec<RemoteChange>,
    ) -> Result<(), BackendError> {
        let selected = self.select(mailbox, None).await?;
        let top = self.top_uid(mailbox, &selected).await?;
        let keys = window_keys(
            mailbox,
            self.settings.window_days,
            chrono::Local::now().date_naive(),
        );
        let mut uids = Vec::new();
        self.search_windows(mailbox, (1, top), &keys, |found| uids.extend(found))
            .await?;
        for flags in self.gm_pair(mailbox, selected.uidvalidity, &uids).await? {
            if let Some(meta) = flags.gmail {
                changes.push(RemoteChange::Located {
                    id: meta.id(),
                    at: Location {
                        mailbox: mailbox.to_string(),
                        uidvalidity: selected.uidvalidity,
                        uid: flags.uid,
                    },
                });
            }
        }
        Ok(())
    }

    /// Reports `uids` of `mailbox` as seen at their places and as added.
    async fn gm_announce(
        &self,
        mailbox: &str,
        uidvalidity: u32,
        uids: &[u32],
        changes: &mut Vec<RemoteChange>,
    ) -> Result<(), BackendError> {
        for flags in self.gm_pair(mailbox, uidvalidity, uids).await? {
            if let Some(meta) = flags.gmail {
                changes.push(RemoteChange::Located {
                    id: meta.id(),
                    at: Location {
                        mailbox: mailbox.to_string(),
                        uidvalidity,
                        uid: flags.uid,
                    },
                });
                changes.push(RemoteChange::Added {
                    id: meta.id(),
                    thread_id: meta.thread_id(),
                });
            }
        }
        Ok(())
    }

    async fn gm_mailbox_changes(
        &self,
        mailbox: &str,
        kept: Kept,
        changes: &mut Vec<RemoteChange>,
    ) -> Result<Kept, BackendError> {
        let selected = self.select(mailbox, None).await?;
        let top = self.top_uid(mailbox, &selected).await?;
        if selected.uidvalidity != kept.uidvalidity {
            // Every UID is void, and the ids are not: report where the
            // window's messages sit under the new number, and what it holds.
            let keys = window_keys(
                mailbox,
                self.settings.window_days,
                chrono::Local::now().date_naive(),
            );
            let mut uids = Vec::new();
            self.search_windows(mailbox, (1, top), &keys, |found| uids.extend(found))
                .await?;
            self.gm_announce(mailbox, selected.uidvalidity, &uids, changes)
                .await?;
            let held = self.held(mailbox, top).await?;
            changes.push(RemoteChange::Holds {
                mailbox: mailbox.to_string(),
                uidvalidity: selected.uidvalidity,
                uids: held,
            });
            return Ok(Kept::of(&selected, top));
        }
        // Labels and flags that changed among the messages met before.
        if let Some(modseq) = kept.modseq
            && kept.uidnext > 1
        {
            let met = top.min(kept.uidnext - 1);
            for window in windows(1, met) {
                let set = UidSet::range(*window.start(), *window.end());
                let mut changed = Vec::new();
                for flags in self.api.gmail_flags(mailbox, &set, Some(modseq)).await? {
                    if !is_deleted(&flags.flags) {
                        changed.push(flags.uid);
                    }
                }
                self.gm_announce(mailbox, selected.uidvalidity, &changed, changes)
                    .await?;
            }
        }
        let mut fresh = Vec::new();
        self.search_windows(mailbox, (kept.uidnext, top), "UNDELETED", |found| {
            fresh.extend(found)
        })
        .await?;
        self.gm_announce(mailbox, selected.uidvalidity, &fresh, changes)
            .await?;
        let held = self.held(mailbox, top).await?;
        changes.push(RemoteChange::Holds {
            mailbox: mailbox.to_string(),
            uidvalidity: selected.uidvalidity,
            uids: held,
        });
        let mut after = Kept::of(&selected, top);
        if selected.uidnext.is_none() {
            after.uidnext = after.uidnext.max(kept.uidnext);
        }
        Ok(after)
    }
}
