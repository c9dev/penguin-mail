//! A POP3 account's mail service. A POP3 server holds one list of messages
//! and nothing else, so this adapter answers every mailbox and message
//! question from the store, which for this account is the mail and not a
//! cache of it: six role mailboxes and the folders a person makes, all
//! kept here. It holds the POP3 client for the downloader and opens no
//! session itself. Sending goes over SMTP, as an IMAP account's does, and
//! the copy, drafts and every write land in the store.

use std::collections::BTreeSet;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use mailrs_domain::mailbox::keyword;
use mailrs_domain::translate::gettext;
use mailrs_domain::{
    AccountId, EpochMillis, MailSet, MailboxKind, Memberships, MessageMeta, RemoteMailbox,
    RemoveSetting, Role,
};
use mailrs_gmail::LabelColor;
use mailrs_mime::Parts;
use mailrs_pop3::{Pop3Api, Pop3Error};
use mailrs_store::messages::{self, Change};
use mailrs_store::threading::Links;
use mailrs_store::threads::{self, ThreadFilter};
use mailrs_store::{Db, StoreError, accounts, local_messages, mailboxes};
use rusqlite::Connection;

use super::imap::submit_over;
use super::{
    Backfill, Changes, Found, IdentityService, KeywordsPage, MailBackend, MailCapabilities,
    RawMessage, Relocated, RemoteRef, SearchQuery, SendAsAddress, Submit, SyncState, Unapplied,
    Want,
};
use crate::api::{DraftRef, SavedDraft};
use crate::{BackendError, MailOp, now_millis};

/// The mailboxes a POP3 account has from the start, by id, name and role
/// (spec section 2).
const ROLES: [(&str, &str, Role); 6] = [
    ("inbox", "Inbox", Role::Inbox),
    ("sent", "Sent", Role::Sent),
    ("drafts", "Drafts", Role::Drafts),
    ("trash", "Trash", Role::Trash),
    ("junk", "Junk", Role::Junk),
    ("archive", "Archive", Role::Archive),
];

/// The gap between checks while the window is open, and with only the
/// tray (spec section 3).
const POLL_OPEN: Duration = Duration::from_secs(60);
const POLL_TRAY: Duration = Duration::from_secs(5 * 60);

/// The most messages one change names; the store takes any number.
const BATCH_LIMIT: usize = 1000;

/// A new store id for a message made here: a sent copy or a draft.
fn made_here_id() -> String {
    format!("local/{:016x}", rand::random::<u64>())
}

/// What a POP3 account's settings say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pop3Settings {
    /// The address the account signs in with and sends as.
    pub address: String,
    /// Who runs the server, as people know them.
    pub provider_name: String,
}

/// The message `raw` as the store keeps it, filed in `mailbox` with
/// `keywords`. `received` is when it reached this computer, which stands
/// for the date a server would have given it; with `trust_date`, the
/// message's own Date header stands instead when it is no later.
pub(crate) fn local_meta(
    account_id: AccountId,
    id: &str,
    raw: &[u8],
    mailbox: &str,
    keywords: &[&str],
    received: EpochMillis,
    trust_date: bool,
) -> (MessageMeta, Links) {
    let summary = mailrs_mime::summary(raw);
    let date = match (trust_date, summary.date) {
        (true, Some(date)) if date <= received => date,
        _ => received,
    };
    let meta = MessageMeta {
        account_id,
        id: id.to_string(),
        thread_id: id.to_string(),
        rfc822_msgid: summary.message_id,
        from: summary.from,
        to: summary.to,
        cc: summary.cc,
        subject: summary.subject,
        date,
        snippet: summary.snippet,
        size: i64::try_from(raw.len()).unwrap_or(i64::MAX),
        has_attachments: summary.has_files,
        held: Memberships {
            mailboxes: vec![mailbox.to_string()],
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            categories: Vec::new(),
        },
        roles: Vec::new(),
        list_unsubscribe: summary.list_unsubscribe,
        one_click: summary
            .list_unsubscribe_post
            .as_deref()
            .is_some_and(|v| v.contains("One-Click")),
    };
    let links = Links {
        in_reply_to: summary.in_reply_to,
        references: summary.references,
    };
    (meta, links)
}

/// Keeps `raw` and its message row in one write, threaded by its links.
/// Answers the threads it touched.
pub(crate) fn keep_local(
    c: &Connection,
    account_id: AccountId,
    meta: MessageMeta,
    links: Links,
    raw: &[u8],
) -> mailrs_store::Result<BTreeSet<String>> {
    local_messages::put(c, account_id, &meta.id, raw)?;
    let generation = accounts::sync_cursor(c, account_id)?.sync_gen;
    let change = Change::UpsertLocal {
        meta: Box::new(meta),
        generation,
        links,
    };
    Ok(messages::apply(c, account_id, &[change])?.threads)
}

/// Asks the next check to remove the downloaded messages among `ids` from
/// the server, when the account removes mail there (spec section 5). A
/// message made here has no server copy.
fn ask_removal(c: &Connection, account_id: AccountId, ids: &[String]) -> mailrs_store::Result<()> {
    if accounts::pop3_remove(c, account_id)? == RemoveSetting::Never {
        return Ok(());
    }
    mailrs_store::pop3::want_removed_of(c, account_id, ids)
}

impl From<Pop3Error> for BackendError {
    fn from(err: Pop3Error) -> Self {
        match err {
            Pop3Error::Network(detail) => BackendError::Offline(detail),
            Pop3Error::Auth { .. } => BackendError::NeedsReauth,
            Pop3Error::InUse(_) => BackendError::RateLimited(None),
            other @ (Pop3Error::Tls { .. }
            | Pop3Error::Refused(_)
            | Pop3Error::Protocol(_)
            | Pop3Error::Unsupported(_)
            | Pop3Error::TooLarge) => BackendError::Refused(other.to_string()),
        }
    }
}

fn stored(err: StoreError) -> BackendError {
    BackendError::Refused(err.to_string())
}

/// A POP3 account's mail and identity, over SMTP client `S` and POP3
/// client `P`.
pub struct Pop3<S, P> {
    db: Db,
    account_id: AccountId,
    smtp: Arc<S>,
    client: Arc<P>,
    settings: Arc<Pop3Settings>,
    tray_only: Arc<AtomicBool>,
}

impl<S, P> Clone for Pop3<S, P> {
    fn clone(&self) -> Self {
        Pop3 {
            db: self.db.clone(),
            account_id: self.account_id,
            smtp: Arc::clone(&self.smtp),
            client: Arc::clone(&self.client),
            settings: Arc::clone(&self.settings),
            tray_only: Arc::clone(&self.tray_only),
        }
    }
}

impl<S, P> Pop3<S, P> {
    pub fn new(
        db: Db,
        account_id: AccountId,
        smtp: Arc<S>,
        client: Arc<P>,
        settings: Pop3Settings,
    ) -> Self {
        Pop3 {
            db,
            account_id,
            smtp,
            client,
            settings: Arc::new(settings),
            tray_only: Arc::default(),
        }
    }

    /// The POP3 client, for the downloader.
    pub fn client(&self) -> &Arc<P> {
        &self.client
    }

    async fn read<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection, AccountId) -> mailrs_store::Result<T> + Send + 'static,
    ) -> Result<T, BackendError> {
        let account_id = self.account_id;
        self.db.read(move |c| f(c, account_id)).await.map_err(stored)
    }

    async fn write<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection, AccountId) -> mailrs_store::Result<T> + Send + 'static,
    ) -> Result<T, BackendError> {
        let account_id = self.account_id;
        self.db.write(move |c| f(c, account_id)).await.map_err(stored)
    }

    async fn raw(&self, id: &str) -> Result<Vec<u8>, BackendError> {
        let id = id.to_string();
        self.read(move |c, a| local_messages::get(c, a, &id))
            .await?
            .ok_or(BackendError::NotFound)
    }

    fn role_of(id: &str) -> Option<Role> {
        ROLES
            .iter()
            .find(|(role_id, _, _)| *role_id == id)
            .map(|(_, _, role)| *role)
    }

    /// The mailbox `id` as the store lists it.
    async fn listed(&self, id: &str) -> Result<RemoteMailbox, BackendError> {
        let id = id.to_string();
        self.read(move |c, a| Ok(mailboxes::listed(c, a)?.into_iter().find(|m| m.id == id)))
            .await?
            .ok_or(BackendError::NotFound)
    }
}

impl<S: Submit, P: Pop3Api> MailBackend for Pop3<S, P> {
    fn capabilities(&self) -> MailCapabilities {
        MailCapabilities {
            labels: false,
            server_threads: false,
            files_sent_mail: false,
            categories: false,
            delete_forever: true,
            batch_limit: BATCH_LIMIT,
            // Every keyword stays on this computer, marked local.
            keywords: &[],
            native_search: false,
            renames: false,
            restates: false,
            tags: false,
            focus: false,
            local_mailboxes: true,
            files_muted_replies: false,
        }
    }

    fn provider_name(&self) -> &str {
        &self.settings.provider_name
    }

    fn person_waiting(&self) -> bool {
        false
    }

    async fn stand_by(&self, wait: Duration) {
        tokio::time::sleep(wait).await;
    }

    fn made_by_person(&self, id: &str) -> bool {
        Self::role_of(id).is_none()
    }

    /// The six role mailboxes, then the folders a person made.
    async fn mailboxes(&self) -> Result<Vec<RemoteMailbox>, BackendError> {
        let mut all: Vec<RemoteMailbox> = ROLES
            .iter()
            .map(|(id, name, role)| RemoteMailbox {
                id: id.to_string(),
                name: name.to_string(),
                kind: MailboxKind::System,
                role: Some(*role),
                color: None,
                hidden: false,
            })
            .collect();
        let made = self.read(mailboxes::listed).await?;
        all.extend(
            made.into_iter()
                .filter(|m| Self::role_of(&m.id).is_none() && m.role.is_none()),
        );
        Ok(all)
    }

    /// Nothing changes on a server the account keeps no mailboxes on; the
    /// engine never asks.
    async fn changes(&self, _since: Option<&SyncState>) -> Result<Changes, BackendError> {
        Ok(Changes {
            changes: Vec::new(),
            state: SyncState::new("local"),
        })
    }

    async fn create_mailbox(&self, name: &str) -> Result<RemoteMailbox, BackendError> {
        Ok(RemoteMailbox {
            id: format!("folder/{:016x}", rand::random::<u64>()),
            name: name.to_string(),
            kind: MailboxKind::Folder,
            role: None,
            color: None,
            hidden: false,
        })
    }

    /// The caller stores the answer, and renames the folders nested under
    /// this one itself, since the capabilities say no rename moves them.
    async fn rename_mailbox(&self, id: &str, name: &str) -> Result<RemoteMailbox, BackendError> {
        if Self::role_of(id).is_some() {
            return Err(BackendError::Refused(gettext(
                "Penguin Mail keeps the names of Inbox, Sent, Drafts, Trash, Junk and Archive.",
            )));
        }
        let old = self.listed(id).await?;
        Ok(RemoteMailbox {
            name: name.to_string(),
            ..old
        })
    }

    /// Deletes the mail in the folder with it, as the confirmation says of
    /// any folder; the caller then drops the folder. Mail left in no
    /// mailbox would show nowhere and still fill the disk.
    async fn delete_mailbox(&self, id: &str) -> Result<(), BackendError> {
        if Self::role_of(id).is_some() {
            return Err(BackendError::Refused(gettext(
                "Penguin Mail keeps Inbox, Sent, Drafts, Trash, Junk and Archive.",
            )));
        }
        let set = MailSet::Mailbox(id.to_string());
        self.write(move |c, a| {
            let ids: Vec<String> = messages::held_by(c, a, &set)?.into_iter().collect();
            ask_removal(c, a, &ids)?;
            let deletions: Vec<Change> = ids
                .into_iter()
                .map(|message_id| Change::Delete { message_id })
                .collect();
            messages::apply(c, a, &deletions)?;
            Ok(())
        })
        .await
    }

    /// The colour lives with the mailbox in the store, where the caller
    /// keeps what this answers.
    async fn set_mailbox_color(
        &self,
        id: &str,
        color: &LabelColor,
    ) -> Result<RemoteMailbox, BackendError> {
        let mailbox = self.listed(id).await?;
        Ok(RemoteMailbox {
            color: Some(color.background_color.clone()),
            ..mailbox
        })
    }

    /// The conversations the store holds there, which is all of them.
    async fn mailbox_threads(&self, id: &str) -> Result<u64, BackendError> {
        let set = self.set_of(id);
        let count = self
            .read(move |c, a| threads::count_threads(c, &ThreadFilter::account(a, set)))
            .await?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    fn follow(&self, _mailbox: &str) -> bool {
        false
    }

    fn set_window_open(&self, open: bool) {
        self.tray_only.store(!open, Ordering::Relaxed);
    }

    /// How often the engine runs a check (spec section 3).
    fn poll_interval(&self) -> Option<Duration> {
        Some(match self.tray_only.load(Ordering::Relaxed) {
            true => POLL_TRAY,
            false => POLL_OPEN,
        })
    }

    /// POP3 cannot say when mail arrives.
    async fn watch(&self) {
        std::future::pending::<()>().await
    }

    async fn keywords_in(
        &self,
        _mailbox: &str,
        _uidvalidity: u32,
        _uids: RangeInclusive<u32>,
    ) -> Result<KeywordsPage, BackendError> {
        Ok(KeywordsPage::default())
    }

    async fn uidvalidity(&self, _mailbox: &str) -> Result<Option<u32>, BackendError> {
        Ok(None)
    }

    async fn keywords_stored(&self, _mailbox: &str) -> Result<&'static [&'static str], BackendError> {
        Ok(&[])
    }

    /// The store holds everything already, so there is no window to page.
    async fn backfill(&self, _days: i64, _cursor: Option<&str>) -> Result<Backfill, BackendError> {
        Ok(Backfill {
            refs: Vec::new(),
            next: None,
        })
    }

    async fn window_ids(
        &self,
        _days: i64,
        _mailbox: Option<&str>,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        Ok(Vec::new())
    }

    async fn inbox_ids(&self) -> Result<Vec<RemoteRef>, BackendError> {
        Ok(Vec::new())
    }

    /// The caller searches the store itself, which holds every message.
    async fn search(&self, _query: &SearchQuery, _limit: usize) -> Result<Vec<RemoteRef>, BackendError> {
        Ok(Vec::new())
    }

    /// The store keeps the header with its angle brackets, whichever form
    /// the caller hands in.
    async fn find_sent(&self, message_id: &str) -> Result<Option<String>, BackendError> {
        let message_id = format!("<{}>", message_id.trim().trim_matches(['<', '>']));
        self.read(move |c, a| mailrs_store::pop3::sent_with(c, a, &message_id))
            .await
    }

    /// The stored rows. `append` and `save_draft` write a message's row in
    /// the same write as its bytes, so a raw copy never stands without one;
    /// an id the store lacks is gone.
    async fn fetch(&self, wants: Vec<Want>) -> Result<Found, BackendError> {
        let ids: Vec<String> = wants.into_iter().map(|w| w.id).collect();
        let asked = ids.clone();
        let metas = self.read(move |c, a| messages::by_ids(c, a, &asked)).await?;
        let gone = ids
            .into_iter()
            .filter(|id| metas.iter().all(|m| &m.id != id))
            .collect();
        Ok(Found {
            metas,
            gone,
            ..Found::default()
        })
    }

    async fn fetch_whole(&self, threads: Vec<String>) -> Result<Found, BackendError> {
        let held = self
            .read(move |c, a| {
                threads
                    .into_iter()
                    .map(|t| Ok((messages::thread_messages(c, a, &t)?, t)))
                    .collect::<mailrs_store::Result<Vec<_>>>()
            })
            .await?;
        let mut found = Found::default();
        for (metas, thread) in held {
            match metas.is_empty() {
                true => found.gone_threads.push(thread),
                false => found.whole.push(metas),
            }
        }
        Ok(found)
    }

    async fn fetch_raw(&self, ids: &[String]) -> Result<Vec<RawMessage>, BackendError> {
        let mut raws = Vec::with_capacity(ids.len());
        for id in ids {
            raws.push(RawMessage {
                id: id.clone(),
                bytes: self.raw(id).await?,
            });
        }
        Ok(raws)
    }

    /// Files a read copy of what the account sent, here.
    async fn append(&self, raw: &[u8], mailbox: &str) -> Result<String, BackendError> {
        let id = made_here_id();
        let (meta, links) = local_meta(
            self.account_id,
            &id,
            raw,
            mailbox,
            &[keyword::SEEN],
            now_millis(),
            true,
        );
        let raw = raw.to_vec();
        self.write(move |c, a| keep_local(c, a, meta, links, &raw))
            .await?;
        Ok(id)
    }

    async fn fetch_structure(&self, id: &str) -> Result<Parts, BackendError> {
        mailrs_mime::parts(&self.raw(id).await?).ok_or(BackendError::NotFound)
    }

    async fn fetch_part(&self, id: &str, path: &str) -> Result<Vec<u8>, BackendError> {
        mailrs_mime::part(&self.raw(id).await?, path).ok_or(BackendError::NotFound)
    }

    fn mailbox_for(&self, role: Role) -> Option<String> {
        ROLES
            .iter()
            .find(|(_, _, r)| *r == role)
            .map(|(id, _, _)| id.to_string())
    }

    fn set_of(&self, id: &str) -> MailSet {
        match Self::role_of(id) {
            Some(role) => MailSet::Role(role),
            None => MailSet::Mailbox(id.to_string()),
        }
    }

    /// The engine sends this account nothing but Delete Forever, after the
    /// store has every other change. Delete Forever asks the next check to
    /// remove a downloaded message from the server when the account
    /// removes mail there (spec section 5); the caller then deletes the
    /// rows, and the raw copy goes with them.
    async fn apply(&self, messages: &[String], ops: &[MailOp]) -> Result<Vec<Relocated>, Unapplied> {
        if !matches!(ops, [MailOp::Destroy]) {
            return Ok(Vec::new());
        }
        let ids = messages.to_vec();
        self.write(move |c, a| ask_removal(c, a, &ids))
            .await
            .map_err(|error| Unapplied {
                taken: 0,
                error,
                relocated: Vec::new(),
            })?;
        Ok(Vec::new())
    }

    async fn send(&self, raw: &[u8], _thread_id: Option<&str>) -> Result<String, BackendError> {
        submit_over(&*self.smtp, &self.settings.address, raw).await
    }

    /// A draft is a message in Drafts, read and marked `$draft`. Saving
    /// again keeps the new copy and drops the old in one write.
    async fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        _thread_id: Option<&str>,
    ) -> Result<SavedDraft, BackendError> {
        let id = made_here_id();
        let (meta, links) = local_meta(
            self.account_id,
            &id,
            raw,
            "drafts",
            &[keyword::DRAFT, keyword::SEEN],
            now_millis(),
            true,
        );
        let (old, raw, new) = (draft_id.map(str::to_string), raw.to_vec(), id.clone());
        let thread_id = self
            .write(move |c, a| {
                keep_local(c, a, meta, links, &raw)?;
                if let Some(old) = old {
                    messages::apply(c, a, &[Change::Delete { message_id: old }])?;
                }
                Ok(messages::thread_id_of(c, a, &new)?.unwrap_or_else(|| new.clone()))
            })
            .await?;
        Ok(SavedDraft {
            draft_id: id.clone(),
            message_id: id,
            thread_id,
        })
    }

    async fn send_draft(&self, draft_id: &str) -> Result<String, BackendError> {
        let raw = self.raw(draft_id).await?;
        let sent = submit_over(&*self.smtp, &self.settings.address, &raw).await?;
        self.delete_draft(draft_id).await?;
        Ok(sent)
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), BackendError> {
        let id = draft_id.to_string();
        let held = self
            .write(move |c, a| {
                let held = messages::thread_id_of(c, a, &id)?.is_some();
                messages::apply(c, a, &[Change::Delete { message_id: id }])?;
                Ok(held)
            })
            .await?;
        match held {
            true => Ok(()),
            false => Err(BackendError::NotFound),
        }
    }

    async fn list_drafts(&self) -> Result<Vec<DraftRef>, BackendError> {
        let ids = self
            .read(|c, a| messages::held_by(c, a, &MailSet::Role(Role::Drafts)))
            .await?;
        Ok(ids
            .into_iter()
            .map(|id| DraftRef {
                draft_id: id.clone(),
                message_id: id,
            })
            .collect())
    }
}

impl<S: Submit, P: Pop3Api> IdentityService for Pop3<S, P> {
    /// The account's own address; POP3 keeps no list of others.
    async fn identities(&self) -> Result<Vec<SendAsAddress>, BackendError> {
        Ok(vec![SendAsAddress {
            email: self.settings.address.clone(),
            name: None,
            signature: String::new(),
            default: true,
        }])
    }
}
