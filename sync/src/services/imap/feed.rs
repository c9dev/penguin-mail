//! The IMAP change feed. Each look selects the synced mailboxes and asks
//! what changed since the state the last look wrote, the cheapest way the
//! server allows. QRESYNC answers changed flags and expunged UIDs in the
//! SELECT itself (RFC 7162). CONDSTORE alone answers changed flags for a
//! FETCH with CHANGEDSINCE, and a UID SEARCH says which messages remain.
//! A server with neither sends the window's flags as they stand now; the
//! engine, which can read the store, compares them with what is stored
//! rather than this adapter keeping a copy of its last look. New mail is
//! every UID from the UIDNEXT seen last.
//!
//! Every search and flags fetch over a mailbox goes in UID ranges of at
//! most [`WINDOW`](super::window::WINDOW), so a mailbox of any size stays inside what the client
//! takes from one command, and a look holds one window's answer at a
//! time.

use std::ops::RangeInclusive;

use mailrs_domain::Location;
use mailrs_imap::{Capabilities, FlagsOf, ImapError, Selected, Since, UidSet};

use super::keywords::flag_changes;
use super::state::{ImapState, Kept};
use super::window::{window_keys, windows};
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
                None => self.kept_now(&mailbox).await?,
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
            let kept = self.kept_now(&mailbox).await?;
            state.mailboxes.insert(mailbox, kept);
        }
        Ok(Changes {
            changes: Vec::new(),
            state: state.written(),
        })
    }

    /// Where `mailbox` stands now.
    async fn kept_now(&self, mailbox: &str) -> Result<Kept, BackendError> {
        let selected = self.select(mailbox, None).await?;
        let top = self.top_uid(mailbox, &selected).await?;
        Ok(Kept::of(&selected, top))
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
                // The server reports every UID it expunged since `modseq`,
                // as a set the engine tests stored UIDs against.
                known: None,
            }),
            _ => None,
        };
        let selected = self.select_for_feed(mailbox, since).await?;
        let top = self.top_uid(mailbox, &selected).await?;
        if selected.uidvalidity != kept.uidvalidity {
            // Every UID the store holds for this mailbox is void, and no
            // other mailbox's. The engine lists this one again, and its
            // state starts over from here.
            changes.push(RemoteChange::StateLost {
                mailbox: mailbox.to_string(),
                uidvalidity: selected.uidvalidity,
            });
            return Ok(Kept::of(&selected, top));
        }
        // The messages met before this look; the ones above arrive as new
        // mail, flags and all.
        let met = (1, top.min(kept.uidnext.saturating_sub(1)));
        let look = Look {
            mailbox,
            uidvalidity: selected.uidvalidity,
            uidnext: kept.uidnext,
            stored: self.known().keywords.unwrap_or(SYSTEM_KEYWORDS),
        };
        // QRESYNC's report counts only when the server applied QRESYNC to
        // this SELECT. A plain SELECT, and a QRESYNC SELECT that fell back
        // to one, report `qresync: false`, and the look then compares by
        // CONDSTORE or in full. Either way the look covers every change up
        // to the HIGHESTMODSEQ this SELECT reports, so the state after
        // takes that MODSEQ.
        match (selected.qresync, capabilities.condstore, kept.modseq) {
            (true, _, _) => {
                // The server's set goes to the engine whole, cut to the
                // UIDs met before; the engine tests each stored UID
                // against it.
                let vanished = match met.1 >= met.0 {
                    true => selected.vanished.intersection(&UidSet::range(met.0, met.1)),
                    false => UidSet::new(),
                };
                if !vanished.is_empty() {
                    changes.push(RemoteChange::Vanished {
                        mailbox: mailbox.to_string(),
                        uidvalidity: selected.uidvalidity,
                        uids: vanished,
                    });
                }
                look.flags(&selected.changed, changes);
            }
            (false, true, Some(modseq)) => {
                changes.push(self.holds(mailbox, selected.uidvalidity, top).await?);
                match self.changed_since(&look, met, modseq, changes).await {
                    Ok(()) => {}
                    // The client dropped a CHANGEDSINCE answer past its
                    // budget, and the connection with it; this look reads
                    // the window's flags instead, once.
                    Err(ImapError::Protocol(_)) => self.window_flags(&look, met, changes).await?,
                    Err(err) => return Err(err.into()),
                }
            }
            _ => {
                changes.push(self.holds(mailbox, selected.uidvalidity, top).await?);
                self.window_flags(&look, met, changes).await?;
            }
        }
        let mut fresh = Vec::new();
        self.search_windows(mailbox, (kept.uidnext, top), "UNDELETED", |found| {
            fresh.extend(found)
        })
        .await?;
        changes.extend(fresh.into_iter().map(|uid| {
            let id = look.name(uid);
            RemoteChange::Added {
                thread_id: id.clone(),
                id,
            }
        }));
        let mut after = Kept::of(&selected, top);
        // A server that leaves UIDNEXT out: new mail counts from above the
        // highest UID met so far, even when the newest message has gone.
        if selected.uidnext.is_none() {
            after.uidnext = after.uidnext.max(kept.uidnext);
        }
        Ok(after)
    }

    /// The flags that changed since `modseq` among the messages in `met`,
    /// a window at a time, added to `changes`.
    async fn changed_since(
        &self,
        look: &Look<'_>,
        met: (u32, u32),
        modseq: u64,
        changes: &mut Vec<RemoteChange>,
    ) -> Result<(), ImapError> {
        for window in windows(met.0, met.1) {
            let uids = UidSet::range(*window.start(), *window.end());
            let flags = self.api.flags(look.mailbox, &uids, Some(modseq)).await?;
            look.flags(&flags, changes);
        }
        Ok(())
    }

    /// The flags of every message in the window among `met`, as they stand
    /// now, a window of UIDs at a time, added to `changes`.
    async fn window_flags(
        &self,
        look: &Look<'_>,
        met: (u32, u32),
        changes: &mut Vec<RemoteChange>,
    ) -> Result<(), BackendError> {
        let keys = window_keys(
            look.mailbox,
            self.settings.window_days,
            chrono::Local::now().date_naive(),
        );
        for window in windows(met.0, met.1) {
            let mut ranges = Vec::new();
            self.search_windows(
                look.mailbox,
                (*window.start(), *window.end()),
                &keys,
                |found| push_runs(&mut ranges, &found),
            )
            .await?;
            let uids = UidSet::from(ranges);
            if uids.is_empty() {
                continue;
            }
            let flags = self.api.flags(look.mailbox, &uids, None).await?;
            look.flags(&flags, changes);
        }
        Ok(())
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

    /// Every message `mailbox` holds now, up to `top`, as one change. A
    /// server without QRESYNC names no UID it expunged, so this is how an
    /// expunge made elsewhere reaches the store. The UIDs arrive a window
    /// at a time and go into the set as runs, so a mailbox with few gaps
    /// costs a few ranges.
    async fn holds(
        &self,
        mailbox: &str,
        uidvalidity: u32,
        top: u32,
    ) -> Result<RemoteChange, BackendError> {
        let mut ranges = Vec::new();
        self.search_windows(mailbox, (1, top), "", |found| {
            push_runs(&mut ranges, &found)
        })
        .await?;
        Ok(RemoteChange::Holds {
            mailbox: mailbox.to_string(),
            uidvalidity,
            uids: UidSet::from(ranges),
        })
    }
}

/// One mailbox's look: what names its messages and what their flags mean.
struct Look<'a> {
    mailbox: &'a str,
    uidvalidity: u32,
    /// The UIDNEXT the last look saw. A message at or above it arrives as
    /// new mail, which brings its flags along.
    uidnext: u32,
    /// The keywords the server stores.
    stored: &'static [&'static str],
}

impl Look<'_> {
    fn name(&self, uid: u32) -> String {
        Location {
            mailbox: self.mailbox.to_string(),
            uidvalidity: self.uidvalidity,
            uid,
        }
        .to_string()
    }

    /// The changes that bring each message met before to the flags in
    /// `flags`, added to `changes`.
    fn flags(&self, flags: &[FlagsOf], changes: &mut Vec<RemoteChange>) {
        for changed in flags.iter().filter(|f| f.uid < self.uidnext) {
            changes.extend(flag_changes(
                &self.name(changed.uid),
                &changed.flags,
                self.stored,
            ));
        }
    }
}

/// Adds `uids` to `ranges` a run of consecutive UIDs at a time, so a
/// mailbox with few gaps costs a few ranges rather than one per message.
/// [`UidSet::from`] sorts and merges what comes out of any order.
fn push_runs(ranges: &mut Vec<RangeInclusive<u32>>, uids: &[u32]) {
    for &uid in uids {
        match ranges.last_mut() {
            Some(last) if last.end().checked_add(1) == Some(uid) => *last = *last.start()..=uid,
            _ => ranges.push(uid..=uid),
        }
    }
}
