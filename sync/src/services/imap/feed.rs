//! The IMAP change feed. Each look selects the synced mailboxes and asks
//! what changed since the state the last look wrote, the cheapest way the
//! server allows. QRESYNC answers changed flags and expunged UIDs in the
//! SELECT itself (RFC 7162). CONDSTORE alone answers changed flags for a
//! FETCH with CHANGEDSINCE, and a UID SEARCH says which messages remain.
//! A server with neither sends the window's flags as they stand now; the
//! engine, which can read the store, compares them with what is stored
//! rather than this adapter keeping a copy of its last look. New mail is
//! every UID from the UIDNEXT seen last.

use mailrs_domain::Location;
use mailrs_imap::{Capabilities, FlagsOf, ImapError, Selected, Since, UidSet};

use super::keywords::flag_changes;
use super::state::{ImapState, Kept};
use super::{Imap, ImapApi, SYSTEM_KEYWORDS, Submit};
use crate::BackendError;
use crate::services::{Changes, RemoteChange, SyncState};

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// With no state, where every synced mailbox stands now. With one,
    /// every change since it in the synced mailboxes, and the state after.
    pub(super) async fn feed(&self, since: Option<&SyncState>) -> Result<Changes, BackendError> {
        let Some(since) = since else {
            return self.feed_start().await;
        };
        let mut state = ImapState::read(since)?;
        let capabilities = self.capabilities_now().await?;
        let mut changes = Vec::new();
        for mailbox in self.synced().await? {
            let kept = match state.mailboxes.get(&mailbox) {
                Some(kept) => {
                    self.mailbox_changes(&mailbox, *kept, &capabilities, &mut changes)
                        .await?
                }
                // A mailbox the account starts keeping in step now: its new
                // mail counts from here.
                None => Kept::of(&self.select(&mailbox, None).await?),
            };
            state.mailboxes.insert(mailbox, kept);
        }
        Ok(Changes {
            changes,
            state: state.written(),
        })
    }

    /// Where every synced mailbox stands now, with no changes: the start
    /// of the feed.
    pub(super) async fn feed_start(&self) -> Result<Changes, BackendError> {
        let mut state = ImapState::default();
        for mailbox in self.synced().await? {
            let selected = self.select(&mailbox, None).await?;
            state.mailboxes.insert(mailbox, Kept::of(&selected));
        }
        Ok(Changes {
            changes: Vec::new(),
            state: state.written(),
        })
    }

    /// What changed in `mailbox` since `kept`, added to `changes`, and
    /// where the mailbox stands after.
    async fn mailbox_changes(
        &self,
        mailbox: &str,
        kept: Kept,
        capabilities: &Capabilities,
        changes: &mut Vec<RemoteChange>,
    ) -> Result<Kept, BackendError> {
        // QRESYNC's SELECT parameter takes a MODSEQ of 1 or more.
        let since = match (capabilities.qresync, kept.modseq) {
            (true, Some(modseq)) if modseq > 0 => Some(Since {
                uidvalidity: kept.uidvalidity,
                modseq,
                // The server reports every UID it expunged since `modseq`;
                // one the store never held drops out as a stale name.
                known: None,
            }),
            _ => None,
        };
        let selected = self.select_for_feed(mailbox, since).await?;
        if selected.uidvalidity != kept.uidvalidity {
            // Every UID the store holds for the mailbox is void.
            return Err(BackendError::StateLost);
        }
        let name = |uid: u32| {
            Location {
                mailbox: mailbox.to_string(),
                uidvalidity: selected.uidvalidity,
                uid,
            }
            .to_string()
        };
        let is_new = |uid: u32| uid >= kept.uidnext;
        // The server answers QRESYNC's report only when it actually
        // applied QRESYNC to this SELECT; a plain SELECT, and a QRESYNC
        // SELECT that fell back to one, both report `qresync: false` with
        // empty `vanished` and `changed`, which say nothing and must not
        // move the stored MODSEQ as if they did.
        let quick = selected.qresync;
        let flags: Vec<FlagsOf> = match (quick, capabilities.condstore, kept.modseq) {
            (true, _, _) => {
                changes.extend(
                    selected
                        .vanished
                        .iter()
                        .filter(|uid| !is_new(*uid))
                        .map(|uid| RemoteChange::Deleted { id: name(uid) }),
                );
                selected.changed.clone()
            }
            (false, true, Some(modseq)) => {
                changes.push(self.holds(mailbox, &name).await?);
                self.api
                    .flags(mailbox, &UidSet::from_uid(1), Some(modseq))
                    .await?
            }
            _ => {
                changes.push(self.holds(mailbox, &name).await?);
                let (_, window) = self.window_uids(mailbox, self.settings.window_days).await?;
                match window.is_empty() {
                    true => Vec::new(),
                    false => {
                        self.api
                            .flags(mailbox, &UidSet::from_uids(window.iter().copied()), None)
                            .await?
                    }
                }
            }
        };
        let stored = self.known().keywords.unwrap_or(SYSTEM_KEYWORDS);
        for changed in flags.iter().filter(|f| !is_new(f.uid)) {
            changes.extend(flag_changes(&name(changed.uid), &changed.flags, stored));
        }
        // A server that leaves UIDNEXT out gets asked every time.
        if selected.uidnext.is_none_or(|next| next > kept.uidnext) {
            let keys = format!("UID {}:* UNDELETED", kept.uidnext);
            let mut fresh = self.api.search(mailbox, &keys).await?;
            // `n:*` always names the last message, even one below `n`.
            fresh.retain(|uid| is_new(*uid));
            fresh.sort_unstable();
            changes.extend(fresh.into_iter().map(|uid| {
                let id = name(uid);
                RemoteChange::Added {
                    thread_id: id.clone(),
                    id,
                }
            }));
        }
        Ok(Kept::of(&selected))
    }

    /// Selects `mailbox`, asking for QRESYNC's report when `since` gives
    /// it. A QRESYNC SELECT past its byte budget answers `Protocol` and
    /// drops the connection; the fallback is a plain SELECT, once, so
    /// this look compares by CONDSTORE or in full instead, the same as a
    /// server that never offered QRESYNC.
    async fn select_for_feed(
        &self,
        mailbox: &str,
        since: Option<Since>,
    ) -> Result<Selected, BackendError> {
        if since.is_some() {
            match self.api.select(mailbox, since).await {
                Ok(selected) => {
                    self.note_keywords(mailbox, &selected);
                    return Ok(selected);
                }
                Err(ImapError::Protocol(_)) => {}
                Err(err) => return Err(err.into()),
            }
        }
        self.select(mailbox, None).await
    }

    /// Every message `mailbox` holds now, as one change. A server without
    /// QRESYNC names no UID it expunged, so this is how an expunge made
    /// elsewhere reaches the store.
    async fn holds(
        &self,
        mailbox: &str,
        name: &impl Fn(u32) -> String,
    ) -> Result<RemoteChange, BackendError> {
        let every = self.api.search(mailbox, "ALL").await?;
        Ok(RemoteChange::Holds {
            mailbox: mailbox.to_string(),
            ids: every.into_iter().map(name).collect(),
        })
    }
}
