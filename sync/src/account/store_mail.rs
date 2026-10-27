//! The account's mail service in the store's ids. On a folder server a
//! message that moves takes a new name, its remote ref, while the store
//! keeps the id it first met the message by. [`StoreMail`] takes store
//! ids, hands the server each message's name as it stands now, and reads
//! the server's answers back into store ids, one store read per call. It
//! also holds the lock that keeps a look out while a write moves mail.
//!
//! A label server's names are the store's ids, as are those of a server
//! that keeps a message's id when it moves (Graph's immutable ids, JMAP),
//! so for them nothing here reads the store and nothing waits.
//!
//! [`StoreMail::untranslated`] reaches the service itself, for the rare
//! caller that needs the server's names; every other call goes through
//! here.

use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::{
    AccountId, EpochMillis, MailSet, Membership, Memberships, MessageMeta, RemoteMailbox, Role,
};
use mailrs_gmail::LabelColor;
use mailrs_mime::Parts;
use mailrs_store::remote_refs::{self, Resolved};
use mailrs_store::{Db, messages};
use tokio::sync::{Mutex, MutexGuard, OwnedMutexGuard};

use crate::api::{DraftRef, SavedDraft};
use crate::{
    AnyMail, BackendError, Backfill, Claimed, Found, Held, KeywordsPage, Look, MailBackend,
    MailCapabilities, MailOp, RawMessage, ReceiptOut, Relocated, RemoteChange, RemoteRef,
    SearchQuery, Settled, Storage, SyncError, SyncState, Unapplied, Unheld, Want,
};

/// One account's mail service, spoken to in store ids.
pub(crate) struct StoreMail {
    mail: AnyMail,
    db: Db,
    account_id: AccountId,
    /// Whether the server gives a message a new name when it moves; read
    /// once, since a service's capabilities never change.
    renames: bool,
    /// Taken by a write from its change to the store to the moment the
    /// server has it and its moved messages' refs are recorded, and by
    /// every sync pass that writes what the server says. A pass in between
    /// would put the mail back where the write took it from, and on a
    /// folder server read a moved message's old place as expunged and
    /// delete it.
    moving: Arc<Mutex<()>>,
}

/// The move lock, held: what a write and a sync pass do while holding it.
pub(crate) struct Moves<'a> {
    mail: &'a StoreMail,
    _held: MutexGuard<'a, ()>,
}

/// A tentative first load. Its lock and rollback live through a queued
/// store write even if the caller stops waiting for that write.
pub(crate) struct Following {
    mail: AnyMail,
    mailbox: Option<String>,
    _moving: OwnedMutexGuard<()>,
}

impl Following {
    pub(crate) async fn store<F, R>(self, db: &Db, write: F) -> mailrs_store::Result<R>
    where
        F: FnOnce(&rusqlite::Connection) -> mailrs_store::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let (value, mut following) = db.write(move |c| Ok((write(c)?, self))).await?;
        following.mailbox = None;
        Ok(value)
    }
}

impl Drop for Following {
    fn drop(&mut self) {
        if let Some(mailbox) = &self.mailbox {
            self.mail.reset_follow(mailbox);
        }
    }
}

/// A message by its store id, with the name the server knows it by, for
/// a write to send. Only [`Moves::apply`] reads the name.
#[derive(Debug, Clone)]
pub(crate) struct Named {
    id: String,
    name: String,
}

impl Named {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
}

/// What the server's names stand for in the store, read in one batch.
pub(crate) struct Resolution(HashMap<String, Resolved>);

impl Resolution {
    /// The store id `name` stands for, or `None` for a stale name.
    pub(crate) fn stored(&self, name: &str) -> Option<String> {
        stored_id(name, &self.0)
    }
}

impl StoreMail {
    pub(crate) fn new(mail: AnyMail, db: Db, account_id: AccountId) -> Self {
        let renames = mail.capabilities().renames;
        StoreMail {
            mail,
            db,
            account_id,
            renames,
            moving: Arc::new(Mutex::new(())),
        }
    }

    /// Whether a message on this server takes a new name when it moves,
    /// as on IMAP, whose names are mailbox and UID.
    pub(crate) fn renames(&self) -> bool {
        self.renames
    }

    /// The service itself, which hands names to the server as it is
    /// given them. Only for a caller that needs the server's names, such
    /// as one that reads a message's mailbox from its IMAP location.
    pub(crate) fn untranslated(&self) -> &AnyMail {
        &self.mail
    }

    /// Waits for any write or sync pass still under way and keeps others
    /// out until the answer drops. A write changes the store before the
    /// server, so a pass that read the server in between would put the
    /// mail back where it was, on any server. On IMAP a move also renames
    /// the message, which a pass must not read halfway.
    pub(crate) async fn hold_moves(&self) -> Moves<'_> {
        Moves {
            mail: self,
            _held: self.moving.lock().await,
        }
    }

    /// The server's name for each of `ids`, in the same order: what a
    /// caller that reads a message's place from its name needs. An id
    /// without a ref goes as it is.
    pub(crate) async fn server_names(&self, ids: &[String]) -> Result<Vec<String>, SyncError> {
        if !self.renames || ids.is_empty() {
            return Ok(ids.to_vec());
        }
        let (account_id, wanted) = (self.account_id, ids.to_vec());
        let known = self
            .db
            .read(move |c| remote_refs::remotes_of(c, account_id, &wanted))
            .await?;
        Ok(ids
            .iter()
            .map(|id| known.get(id).cloned().unwrap_or_else(|| id.clone()))
            .collect())
    }

    async fn server_name(&self, id: &str) -> Result<String, SyncError> {
        Ok(self
            .server_names(&[id.to_string()])
            .await?
            .pop()
            .unwrap_or_else(|| id.to_string()))
    }

    /// What each of `names` from the server stands for in the store.
    pub(crate) async fn resolve(&self, names: Vec<String>) -> Result<Resolution, SyncError> {
        if !self.renames || names.is_empty() {
            return Ok(Resolution(HashMap::new()));
        }
        let account_id = self.account_id;
        Ok(Resolution(
            self.db
                .read(move |c| remote_refs::resolve(c, account_id, &names))
                .await?,
        ))
    }

    /// The store id of the server's one `name`, which names what a call
    /// just made: a stale name, or a store that cannot say, leaves it as
    /// it is. It never fails, since the call it follows has happened and
    /// a caller that saw an error would make it again, sending a message
    /// twice.
    async fn stored_or_same(&self, name: String) -> String {
        if !self.renames {
            return name;
        }
        match self.resolve(vec![name.clone()]).await {
            Ok(resolved) => resolved.stored(&name).unwrap_or(name),
            Err(err) => {
                tracing::warn!(account = self.account_id, error = %err, "could not read which stored message the server named");
                name
            }
        }
    }

    /// `refs` from a listing under store ids. A name that is some stored
    /// message's stale id drops out: that message is listed under its new
    /// name.
    async fn refs_as_stored(&self, refs: Vec<RemoteRef>) -> Result<Vec<RemoteRef>, SyncError> {
        if !self.renames {
            return Ok(refs);
        }
        let resolved = self
            .resolve(refs.iter().map(|m| m.id.clone()).collect())
            .await?;
        Ok(refs
            .into_iter()
            .filter_map(|m| {
                Some(RemoteRef {
                    id: resolved.stored(&m.id)?,
                    thread_id: m.thread_id,
                })
            })
            .collect())
    }

    /// Metadata for `wants`, at the least cost the server allows. A want
    /// may also name a message the store has not met by the server's name
    /// for it, from a listing: a name without a ref goes as it is.
    pub(crate) async fn fetch(&self, wants: Vec<Want>) -> Result<Found, SyncError> {
        let wants = match self.renames {
            true => {
                let ids: Vec<String> = wants.iter().map(|w| w.id.clone()).collect();
                let names = self.server_names(&ids).await?;
                wants
                    .into_iter()
                    .zip(names)
                    .map(|(want, id)| Want {
                        id,
                        thread_id: want.thread_id,
                    })
                    .collect()
            }
            false => wants,
        };
        let found = self.mail.fetch(wants).await?;
        self.found_as_stored(found).await
    }

    /// Every message of each thread.
    pub(crate) async fn fetch_whole(&self, threads: Vec<String>) -> Result<Found, SyncError> {
        let found = self.mail.fetch_whole(threads).await?;
        self.found_as_stored(found).await
    }

    /// `found` with the server's names read back as store ids.
    async fn found_as_stored(&self, found: Found) -> Result<Found, SyncError> {
        if !self.renames {
            return Ok(found);
        }
        let resolved = self.resolve(names_in(&found)).await?;
        Ok(renamed(found, &resolved.0))
    }

    /// The messages as they arrived, in order, each under its store id.
    pub(crate) async fn fetch_raw(&self, ids: &[String]) -> Result<Vec<RawMessage>, SyncError> {
        let names = self.server_names(ids).await?;
        let mut raws = self.mail.fetch_raw(&names).await?;
        if self.renames {
            let by_name: HashMap<&str, &str> = names
                .iter()
                .map(String::as_str)
                .zip(ids.iter().map(String::as_str))
                .collect();
            for raw in &mut raws {
                if let Some(id) = by_name.get(raw.id.as_str()) {
                    raw.id = id.to_string();
                }
            }
        }
        Ok(raws)
    }

    pub(crate) async fn fetch_structure(&self, id: &str) -> Result<Parts, SyncError> {
        Ok(self
            .mail
            .fetch_structure(&self.server_name(id).await?)
            .await?)
    }

    pub(crate) async fn fetch_part(&self, id: &str, path: &str) -> Result<Vec<u8>, SyncError> {
        Ok(self
            .mail
            .fetch_part(&self.server_name(id).await?, path)
            .await?)
    }

    /// One page of the sync window, under store ids.
    pub(crate) async fn backfill(
        &self,
        days: i64,
        cursor: Option<&str>,
    ) -> Result<Backfill, SyncError> {
        let page = self.mail.backfill(days, cursor).await?;
        Ok(Backfill {
            refs: self.refs_as_stored(page.refs).await?,
            next: page.next,
        })
    }

    /// The sync window's messages, or those of it in `mailbox`, under store ids.
    pub(crate) async fn window_ids(
        &self,
        days: i64,
        mailbox: Option<&str>,
    ) -> Result<Vec<RemoteRef>, SyncError> {
        let listed = self.mail.window_ids(days, mailbox).await?;
        self.refs_as_stored(listed).await
    }

    /// Every message in the inbox, under store ids.
    pub(crate) async fn inbox_ids(&self) -> Result<Vec<RemoteRef>, SyncError> {
        let listed = self.mail.inbox_ids().await?;
        self.refs_as_stored(listed).await
    }

    /// What `query` matches on the server, under store ids.
    pub(crate) async fn search(
        &self,
        query: &SearchQuery,
        limit: usize,
    ) -> Result<Vec<RemoteRef>, SyncError> {
        let found = self.mail.search(query, limit).await?;
        self.refs_as_stored(found).await
    }

    /// One page of an explicit server mailbox, under store ids. The
    /// cursor stays in server names even when a moved message's id does not.
    pub(crate) async fn mailbox_page(
        &self,
        mailbox: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Backfill, SyncError> {
        let page = self.mail.mailbox_page(mailbox, cursor, limit).await?;
        Ok(Backfill {
            refs: self.refs_as_stored(page.refs).await?,
            next: page.next,
        })
    }

    /// One page of the unread mail in an explicit server mailbox, under
    /// store ids, as [`Self::mailbox_page`] pages all of it.
    pub(crate) async fn unread_page(
        &self,
        mailbox: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Backfill, SyncError> {
        let page = self.mail.unread_page(mailbox, cursor, limit).await?;
        Ok(Backfill {
            refs: self.refs_as_stored(page.refs).await?,
            next: page.next,
        })
    }

    /// The sent message carrying the `Message-ID` header `message_id`.
    pub(crate) async fn find_sent(&self, message_id: &str) -> Result<Option<String>, SyncError> {
        match self.mail.find_sent(message_id).await? {
            Some(name) => Ok(Some(self.stored_or_same(name).await)),
            None => Ok(None),
        }
    }

    /// Sends raw RFC 822 bytes, and answers the sent message's id.
    pub(crate) async fn send(
        &self,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<String, SyncError> {
        let sent = self.mail.send(raw, thread_id).await?;
        Ok(self.stored_or_same(sent).await)
    }

    /// Files a copy of `raw` in `mailbox`, and answers the copy's id.
    pub(crate) async fn append(&self, raw: &[u8], mailbox: &str) -> Result<String, SyncError> {
        let filed = self.mail.append(raw, mailbox).await?;
        Ok(self.stored_or_same(filed).await)
    }

    /// Creates a draft, or replaces the draft `draft_id`.
    pub(crate) async fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SavedDraft, SyncError> {
        let old = match draft_id {
            Some(id) => Some(self.server_name(id).await?),
            None => None,
        };
        let saved = self.mail.save_draft(old.as_deref(), raw, thread_id).await?;
        Ok(SavedDraft {
            draft_id: self.stored_or_same(saved.draft_id).await,
            message_id: self.stored_or_same(saved.message_id).await,
            thread_id: saved.thread_id,
        })
    }

    /// Sends a draft as the server holds it, and answers the sent message's id.
    pub(crate) async fn send_draft(&self, draft_id: &str) -> Result<String, SyncError> {
        let sent = self
            .mail
            .send_draft(&self.server_name(draft_id).await?)
            .await?;
        Ok(self.stored_or_same(sent).await)
    }

    pub(crate) async fn delete_draft(&self, draft_id: &str) -> Result<(), SyncError> {
        Ok(self
            .mail
            .delete_draft(&self.server_name(draft_id).await?)
            .await?)
    }

    /// Every draft in the account, under store ids. A draft whose name is
    /// stale drops out.
    pub(crate) async fn list_drafts(&self) -> Result<Vec<DraftRef>, SyncError> {
        let drafts = self.mail.list_drafts().await?;
        if !self.renames {
            return Ok(drafts);
        }
        let names = drafts
            .iter()
            .flat_map(|d| [d.draft_id.clone(), d.message_id.clone()])
            .collect();
        let resolved = self.resolve(names).await?;
        Ok(drafts
            .into_iter()
            .filter_map(|d| {
                Some(DraftRef {
                    draft_id: resolved.stored(&d.draft_id)?,
                    message_id: resolved.stored(&d.message_id)?,
                })
            })
            .collect())
    }

    /// The hold whose copy in Drafts is the stored message `copy`.
    pub(crate) async fn hold_of_copy(&self, copy: &str) -> Result<Option<Claimed>, SyncError> {
        Ok(self
            .mail
            .hold_of_copy(&self.server_name(copy).await?)
            .await?)
    }

    /// Sends `receipt` the server's own way; its message is a store id.
    pub(crate) async fn send_receipt(&self, receipt: &ReceiptOut) -> Result<(), SyncError> {
        let mut receipt = receipt.clone();
        receipt.message_id = self.server_name(&receipt.message_id).await?;
        Ok(self.mail.send_receipt(&receipt).await?)
    }

    /// Where the server's feed of changes stands now.
    pub(crate) async fn state_now(&self) -> Result<SyncState, SyncError> {
        Ok(self.mail.changes(None).await?.state)
    }

    // The calls below name no message, or name one by an id the server
    // never changes, and go to the service as they are.

    pub(crate) fn capabilities(&self) -> MailCapabilities {
        self.mail.capabilities()
    }

    pub(crate) fn provider_name(&self) -> &str {
        self.mail.provider_name()
    }

    pub(crate) fn person_waiting(&self) -> bool {
        self.mail.person_waiting()
    }

    pub(crate) async fn stand_by(&self, wait: Duration) {
        self.mail.stand_by(wait).await;
    }

    pub(crate) fn made_by_person(&self, id: &str) -> bool {
        self.mail.made_by_person(id)
    }

    pub(crate) fn mailbox_for(&self, role: Role) -> Option<String> {
        self.mail.mailbox_for(role)
    }

    pub(crate) fn set_of(&self, id: &str) -> MailSet {
        self.mail.set_of(id)
    }

    pub(crate) async fn follow(&self, mailbox: &str) -> Option<Following> {
        let moving = Arc::clone(&self.moving).lock_owned().await;
        self.mail.follow(mailbox).then(|| Following {
            mail: self.mail.clone(),
            mailbox: Some(mailbox.to_string()),
            _moving: moving,
        })
    }

    pub(crate) async fn look_for_mail(&self) -> Look {
        self.mail.look_for_mail().await
    }

    pub(crate) async fn mailboxes(&self) -> Result<Vec<RemoteMailbox>, BackendError> {
        self.mail.mailboxes().await
    }

    pub(crate) async fn create_mailbox(&self, name: &str) -> Result<RemoteMailbox, BackendError> {
        self.mail.create_mailbox(name).await
    }

    pub(crate) async fn rename_mailbox(
        &self,
        id: &str,
        name: &str,
    ) -> Result<RemoteMailbox, BackendError> {
        self.mail.rename_mailbox(id, name).await
    }

    pub(crate) async fn delete_mailbox(&self, id: &str) -> Result<(), BackendError> {
        self.mail.delete_mailbox(id).await
    }

    pub(crate) async fn set_mailbox_color(
        &self,
        id: &str,
        color: &LabelColor,
    ) -> Result<RemoteMailbox, BackendError> {
        self.mail.set_mailbox_color(id, color).await
    }

    pub(crate) async fn mailbox_threads(&self, id: &str) -> Result<u64, BackendError> {
        self.mail.mailbox_threads(id).await
    }

    pub(crate) async fn uidvalidity(&self, mailbox: &str) -> Result<Option<u32>, BackendError> {
        self.mail.uidvalidity(mailbox).await
    }

    pub(crate) async fn keywords_stored(
        &self,
        mailbox: &str,
    ) -> Result<&'static [&'static str], BackendError> {
        self.mail.keywords_stored(mailbox).await
    }

    pub(crate) async fn keywords_in(
        &self,
        mailbox: &str,
        uidvalidity: u32,
        uids: RangeInclusive<u32>,
    ) -> Result<KeywordsPage, BackendError> {
        self.mail.keywords_in(mailbox, uidvalidity, uids).await
    }

    pub(crate) async fn storage(&self) -> Result<Option<Storage>, BackendError> {
        self.mail.storage().await
    }

    // Send Later holds live only on a server whose ids never change.

    pub(crate) async fn schedule(&self, raw: &[u8], at: EpochMillis) -> Result<Held, BackendError> {
        self.mail.schedule(raw, at).await
    }

    pub(crate) async fn find_hold(&self, message_id: &str) -> Result<Option<Held>, BackendError> {
        self.mail.find_hold(message_id).await
    }

    pub(crate) async fn unschedule(
        &self,
        handle: &str,
        message_id: &str,
        at: EpochMillis,
    ) -> Result<Unheld, BackendError> {
        self.mail.unschedule(handle, message_id, at).await
    }

    pub(crate) async fn settle(
        &self,
        handle: &str,
        message_id: &str,
        at: EpochMillis,
    ) -> Result<Settled, BackendError> {
        self.mail.settle(handle, message_id, at).await
    }
}

impl Moves<'_> {
    /// `ids` with the names the server knows them by now, for
    /// [`Moves::apply`]. Read under the lock, so no write moves them
    /// between this and the write.
    pub(crate) async fn named(&self, ids: &[String]) -> Result<Vec<Named>, SyncError> {
        let names = self.mail.server_names(ids).await?;
        Ok(ids
            .iter()
            .cloned()
            .zip(names)
            .map(|(id, name)| Named { id, name })
            .collect())
    }

    pub(crate) async fn destroy_checked(
        &self,
        messages: &[Named],
        mailbox: &str,
    ) -> Vec<Result<(), BackendError>> {
        let names: Vec<String> = messages.iter().map(|m| m.name.clone()).collect();
        self.mail.mail.destroy_checked(&names, mailbox).await
    }

    /// Applies `ops` to `messages`, in order, as
    /// [`MailBackend::apply`] does, and answers where the server moved
    /// each one under its store id.
    pub(crate) async fn apply(
        &self,
        messages: &[Named],
        ops: &[MailOp],
    ) -> Result<Vec<Relocated>, Unapplied> {
        let names: Vec<String> = messages.iter().map(|m| m.name.clone()).collect();
        let by_name: HashMap<&str, &str> = messages
            .iter()
            .map(|m| (m.name.as_str(), m.id.as_str()))
            .collect();
        let as_stored = |moved: Vec<Relocated>| -> Vec<Relocated> {
            moved
                .into_iter()
                .filter_map(|r| {
                    Some(Relocated {
                        from: by_name.get(r.from.as_str())?.to_string(),
                        to: r.to,
                    })
                })
                .collect()
        };
        match self.mail.mail.apply(&names, ops).await {
            Ok(moved) => Ok(as_stored(moved)),
            Err(Unapplied {
                taken,
                error,
                relocated,
            }) => Err(Unapplied {
                taken,
                error,
                relocated: as_stored(relocated),
            }),
        }
    }

    /// Every change since `since` under store ids, as
    /// [`MailBackend::changes`] answers them. A change that names a stale
    /// name drops out: it tells of the place a message left, and the feed
    /// reports its new place on its own. So does a keyword change the
    /// store already reflects.
    pub(crate) async fn changes(&self, since: &SyncState) -> Result<crate::Changes, SyncError> {
        let mut found = self.mail.mail.changes(Some(since)).await?;
        if self.mail.renames {
            let names = found
                .changes
                .iter()
                .filter_map(named_in)
                .map(str::to_string)
                .collect();
            let resolved = self.mail.resolve(names).await?;
            let changes = std::mem::take(&mut found.changes)
                .into_iter()
                .filter_map(|change| change_as_stored(change, &resolved.0))
                .collect();
            found.changes = self.drop_changes_already_held(changes).await?;
        }
        Ok(found)
    }

    /// Drops the memberships a `Gained` or `Lost` change names that the
    /// store already reflects. A flag report carries a message's whole
    /// flag set, and a change this app made itself comes back in the next
    /// one, so without this a look would write what the store holds and
    /// redraw threads that did not change. Reads each named message's
    /// keywords once from the store.
    async fn drop_changes_already_held(
        &self,
        changes: Vec<RemoteChange>,
    ) -> Result<Vec<RemoteChange>, SyncError> {
        let ids: Vec<String> = changes
            .iter()
            .filter_map(|change| match change {
                RemoteChange::Gained { id, .. }
                | RemoteChange::Refresh { id, .. }
                | RemoteChange::Lost { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        if ids.is_empty() {
            return Ok(changes);
        }
        let account_id = self.mail.account_id;
        let held = self
            .mail
            .db
            .read(move |c| messages::memberships_of(c, account_id, &ids))
            .await?;
        Ok(changes
            .into_iter()
            .filter_map(|change| still_to_apply(change, &held))
            .collect())
    }
}

/// The store id the server's `name` stands for: the stored message a ref
/// gives that name, the name itself for a message the store has not met,
/// or `None` for a stale name, which the message it named has left.
fn stored_id(name: &str, resolved: &HashMap<String, Resolved>) -> Option<String> {
    match resolved.get(name) {
        Some(Resolved::Stored(id)) => Some(id.clone()),
        Some(Resolved::Stale) => None,
        None => Some(name.to_string()),
    }
}

/// The message `change` names, when it names one.
fn named_in(change: &RemoteChange) -> Option<&str> {
    match change {
        RemoteChange::Added { id, .. }
        | RemoteChange::Deleted { id }
        | RemoteChange::Gained { id, .. }
        | RemoteChange::Refresh { id, .. }
        | RemoteChange::Located { id, .. }
        | RemoteChange::Lost { id, .. } => Some(id),
        RemoteChange::Vanished { .. }
        | RemoteChange::Holds { .. }
        | RemoteChange::StateLost { .. }
        | RemoteChange::CompareKeywords { .. } => None,
    }
}

/// `change` under the store's id for the message it names, or `None` when
/// that name is stale.
fn change_as_stored(
    change: RemoteChange,
    resolved: &HashMap<String, Resolved>,
) -> Option<RemoteChange> {
    Some(match change {
        RemoteChange::Added { id, thread_id } => RemoteChange::Added {
            id: stored_id(&id, resolved)?,
            thread_id,
        },
        RemoteChange::Deleted { id } => RemoteChange::Deleted {
            id: stored_id(&id, resolved)?,
        },
        RemoteChange::Gained {
            id,
            thread_id,
            memberships,
        } => RemoteChange::Gained {
            id: stored_id(&id, resolved)?,
            thread_id,
            memberships,
        },
        RemoteChange::Refresh { id, memberships } => RemoteChange::Refresh {
            id: stored_id(&id, resolved)?,
            memberships,
        },
        RemoteChange::Lost { id, memberships } => RemoteChange::Lost {
            id: stored_id(&id, resolved)?,
            memberships,
        },
        // The id is the store's own, whatever refs it holds.
        located @ RemoteChange::Located { .. } => located,
        // These name UIDs or a whole mailbox, which the engine reads
        // against the refs of the mailbox as they stand, so a message that
        // moved away is not there.
        whole @ (RemoteChange::Vanished { .. }
        | RemoteChange::Holds { .. }
        | RemoteChange::StateLost { .. }
        | RemoteChange::CompareKeywords { .. }) => whole,
    })
}

/// `change` with whatever `held` already gives each message dropped from
/// its memberships: a `Gained` keeps only what the message does not carry
/// yet, a `Lost` only what it still carries. `None` once nothing is left
/// to write. A message `held` does not name is not yet stored, so a
/// `Gained` for it passes through unfiltered and a `Lost` for it drops,
/// since neither writes anything a missing message can hold.
fn still_to_apply(
    change: RemoteChange,
    held: &HashMap<String, Memberships>,
) -> Option<RemoteChange> {
    match change {
        RemoteChange::Gained {
            id,
            thread_id,
            memberships,
        } => {
            let current = held.get(&id);
            let memberships: Vec<Membership> = memberships
                .into_iter()
                .filter(|m| !current.is_some_and(|h| h.has(m)))
                .collect();
            (!memberships.is_empty()).then_some(RemoteChange::Gained {
                id,
                thread_id,
                memberships,
            })
        }
        RemoteChange::Refresh { id, memberships } => {
            let current = held.get(&id)?;
            let memberships: Vec<_> = memberships
                .into_iter()
                .filter(|m| !current.has(m))
                .collect();
            (!memberships.is_empty()).then_some(RemoteChange::Refresh { id, memberships })
        }
        RemoteChange::Lost { id, memberships } => {
            let current = held.get(&id);
            let memberships: Vec<Membership> = memberships
                .into_iter()
                .filter(|m| current.is_some_and(|h| h.has(m)))
                .collect();
            (!memberships.is_empty()).then_some(RemoteChange::Lost { id, memberships })
        }
        other => Some(other),
    }
}

/// Every message name `found` uses, once each.
fn names_in(found: &Found) -> Vec<String> {
    let mut names: Vec<String> = found
        .metas
        .iter()
        .map(|m| m.id.clone())
        .chain(found.gone.iter().cloned())
        .chain(found.whole.iter().flatten().map(|m| m.id.clone()))
        .chain(found.links.keys().cloned())
        .chain(found.located.keys().cloned())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// `found` under store ids. Whatever a stale name stood for drops out: a
/// message that moved is stored under its first id and reported under
/// its new name.
fn renamed(found: Found, resolved: &HashMap<String, Resolved>) -> Found {
    let stored_meta = |mut meta: MessageMeta| {
        meta.id = stored_id(&meta.id, resolved)?;
        Some(meta)
    };
    Found {
        metas: found.metas.into_iter().filter_map(&stored_meta).collect(),
        gone: found
            .gone
            .iter()
            .filter_map(|name| stored_id(name, resolved))
            .collect(),
        whole: found
            .whole
            .into_iter()
            .map(|thread| thread.into_iter().filter_map(&stored_meta).collect())
            .collect(),
        gone_threads: found.gone_threads,
        links: found
            .links
            .into_iter()
            .filter_map(|(name, links)| Some((stored_id(&name, resolved)?, links)))
            .collect(),
        located: found
            .located
            .into_iter()
            .filter_map(|(name, at)| Some((stored_id(&name, resolved)?, at)))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn a_queued_first_load_keeps_polling_out_after_cancellation() {
        use crate::tests::imap_harness;
        use std::time::Duration;

        let h = imap_harness().await;
        h.imap.add_mailbox("Work", None);
        h.bootstrap().await;
        let following = h.sync.mail.follow("Work").await.unwrap();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let db = h.db.clone();
        let writer = tokio::spawn(async move {
            db.write(move |_| {
                let _ = entered.send(());
                let _ = held.recv();
                Ok(())
            })
            .await
            .unwrap();
        });
        ready.await.unwrap();
        let mut storing = Box::pin(following.store(&h.db, |_| Ok(())));
        // Db::write queues its closure before its first pending await.
        assert!(futures::poll!(storing.as_mut()).is_pending());
        drop(storing);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), h.sync.incremental())
                .await
                .is_err()
        );
        drop(release);
        writer.await.unwrap();
        h.db.write(|_| Ok(())).await.unwrap();
        assert!(!h.is_followed("Work"));
        h.sync.follow_mailbox("Work").await.unwrap();
        assert!(h.is_followed("Work"));
    }

    use std::collections::HashMap;

    use mailrs_domain::{Location, Membership, Memberships};
    use mailrs_imap::UidSet;
    use mailrs_store::remote_refs::Resolved;

    use super::{change_as_stored, renamed, still_to_apply};
    use crate::fake::meta;
    use crate::{Found, RemoteChange};

    fn at(mailbox: &str, uidvalidity: u32, uid: u32) -> Location {
        Location {
            mailbox: mailbox.into(),
            uidvalidity,
            uid,
        }
    }

    #[test]
    fn a_moved_message_comes_back_under_its_first_id_and_its_old_name_says_nothing() {
        let found = Found {
            metas: vec![
                meta("Archive/3/10", "Archive/3/10", 0, &[]),
                meta("INBOX/7/43", "INBOX/7/43", 0, &[]),
            ],
            gone: vec!["INBOX/7/42".into(), "INBOX/7/44".into()],
            located: HashMap::from([
                ("Archive/3/10".to_string(), at("Archive", 3, 10)),
                ("INBOX/7/43".to_string(), at("INBOX", 7, 43)),
            ]),
            ..Found::default()
        };
        let resolved = HashMap::from([
            (
                "Archive/3/10".to_string(),
                Resolved::Stored("INBOX/7/42".into()),
            ),
            ("INBOX/7/42".to_string(), Resolved::Stale),
        ]);

        let found = renamed(found, &resolved);

        let ids: Vec<&str> = found.metas.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["INBOX/7/42", "INBOX/7/43"]);
        assert_eq!(found.gone, ["INBOX/7/44"]);
        assert_eq!(found.located.get("INBOX/7/42"), Some(&at("Archive", 3, 10)));
        assert!(!found.located.contains_key("Archive/3/10"));
    }

    #[test]
    fn a_change_about_a_place_a_message_left_says_nothing() {
        let resolved = HashMap::from([
            (
                "Archive/3/10".to_string(),
                Resolved::Stored("INBOX/7/42".into()),
            ),
            ("INBOX/7/42".to_string(), Resolved::Stale),
        ]);
        let vanished = RemoteChange::Deleted {
            id: "INBOX/7/42".into(),
        };
        assert_eq!(change_as_stored(vanished, &resolved), None);
        let arrived = RemoteChange::Added {
            id: "Archive/3/10".into(),
            thread_id: "Archive/3/10".into(),
        };
        assert_eq!(
            change_as_stored(arrived, &resolved),
            Some(RemoteChange::Added {
                id: "INBOX/7/42".into(),
                thread_id: "Archive/3/10".into(),
            })
        );
        let holds = RemoteChange::Holds {
            mailbox: "Archive".into(),
            uidvalidity: 3,
            uids: UidSet::from_uids([10]),
        };
        assert_eq!(change_as_stored(holds.clone(), &resolved), Some(holds));
    }

    #[test]
    fn a_gained_or_lost_change_drops_what_the_store_already_reflects() {
        let held = HashMap::from([(
            "INBOX/1/4".to_string(),
            Memberships {
                keywords: vec!["$seen".into()],
                ..Memberships::default()
            },
        )]);
        let gained = RemoteChange::Gained {
            id: "INBOX/1/4".into(),
            thread_id: "INBOX/1/4".into(),
            memberships: vec![
                Membership::Keyword("$seen".into()),
                Membership::Keyword("$flagged".into()),
            ],
        };
        assert_eq!(
            still_to_apply(gained, &held),
            Some(RemoteChange::Gained {
                id: "INBOX/1/4".into(),
                thread_id: "INBOX/1/4".into(),
                memberships: vec![Membership::Keyword("$flagged".into())],
            })
        );
        let lost_of_what_it_lacks = RemoteChange::Lost {
            id: "INBOX/1/4".into(),
            memberships: vec![Membership::Keyword("$answered".into())],
        };
        assert_eq!(still_to_apply(lost_of_what_it_lacks, &held), None);
        let lost_of_a_message_not_yet_stored = RemoteChange::Lost {
            id: "INBOX/1/9".into(),
            memberships: vec![Membership::Keyword("$seen".into())],
        };
        assert_eq!(
            still_to_apply(lost_of_a_message_not_yet_stored, &held),
            None
        );
    }
}
