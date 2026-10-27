//! Gmail over IMAP (X-GM-EXT-1), the [`GmailImap`] adapter. Gmail keeps
//! every message once and shows it in a folder for each label, so it syncs
//! only All Mail,
//! Trash and Spam. A message is named by its X-GM-MSGID in lower-case hex,
//! which is the Gmail API's message id and survives a move; the mailbox and
//! UID it sits at now live in the store's remote ref. Its labels come from
//! X-GM-LABELS and are written as the Gmail API labels are.

mod drafts;
mod feed;
mod ids;
pub(super) mod labels;
mod listing;
mod writes;

pub(super) use ids::Places;

use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::time::Duration;

use mailrs_domain::{Location, MailSet, MailboxKind, RemoteMailbox, Role};
use mailrs_gmail::LabelColor;
use mailrs_mime::Parts;

use super::{BATCH_LIMIT, Imap, ImapApi, Submit};
use crate::api::{DraftRef, SavedDraft};
use crate::services::{
    Backfill, Changes, Found, IdentityService, KeywordsPage, MailBackend, MailCapabilities,
    RawMessage, Relocated, RemoteRef, SearchQuery, SendAsAddress, Storage, SyncState, Unapplied,
    Want,
};
use crate::{BackendError, MailOp};
use labels::GmailPlace;

/// Gmail's mail over its IMAP and SMTP servers: the adapter for a Gmail
/// account on the IMAP route. It runs on the same connection, listing and
/// session memory as the plain IMAP adapter, and reads that mail the way
/// the Gmail API does, through All Mail, labels and X-GM-MSGID.
pub struct GmailImap<I, S>(Imap<I, S>);

// By hand, as for `Imap`: the clients are shared rather than copied.
impl<I, S> Clone for GmailImap<I, S> {
    fn clone(&self) -> Self {
        GmailImap(self.0.clone())
    }
}

impl<I, S> GmailImap<I, S> {
    /// The Gmail adapter over `imap`'s connection and session memory.
    pub fn over(imap: Imap<I, S>) -> Self {
        GmailImap(imap)
    }

    /// Makes every look at the feed cover every synced mailbox, as
    /// [`Imap::look_at_every_mailbox`].
    #[cfg(any(test, feature = "fake"))]
    pub fn look_at_every_mailbox(&self) {
        self.0.look_at_every_mailbox();
    }
}

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// Where `name`, a location or a hex id as the Gmail API names mail,
    /// points, while its mailbox keeps the UIDVALIDITY the place names.
    pub(super) async fn gm_current(&self, name: &str) -> Result<Location, BackendError> {
        let at = self.gm_place(name).await?;
        self.checked(at).await
    }

    /// The messages as they arrived, in order, by their Gmail names.
    pub(super) async fn gm_raw_messages(
        &self,
        names: &[String],
    ) -> Result<Vec<RawMessage>, BackendError> {
        let mut raws = Vec::with_capacity(names.len());
        for name in names {
            let at = self.gm_current(name).await?;
            raws.push(self.raw_at(name, &at).await?);
        }
        Ok(raws)
    }
}

impl<I: ImapApi, S: Submit> MailBackend for GmailImap<I, S> {
    async fn storage(&self) -> Result<Option<Storage>, BackendError> {
        self.0.space().await
    }

    fn capabilities(&self) -> MailCapabilities {
        let all = self
            .0
            .known()
            .folders
            .iter()
            .find(|f| f.role == Some(Role::All))
            .map(|f| f.id.clone())
            .unwrap_or_default();
        MailCapabilities {
            labels: true,
            server_threads: true,
            files_sent_mail: true,
            categories: true,
            delete_forever: true,
            batch_limit: BATCH_LIMIT,
            keywords: self.0.stored_keywords(&all),
            native_search: true,
            paged_mailboxes: true,
            renames: true,
            restates: false,
            tags: false,
            focus: false,
            local_mailboxes: false,
            // Gmail's own mute filter is not reachable over IMAP, so the
            // engine files each muted reply as it arrives.
            files_muted_replies: false,
        }
    }

    fn provider_name(&self) -> &str {
        self.0.provider_name()
    }

    fn person_waiting(&self) -> bool {
        self.0.person_waiting()
    }

    async fn stand_by(&self, wait: Duration) {
        self.0.stand_by(wait).await;
    }

    fn made_by_person(&self, id: &str) -> bool {
        !labels::is_system(id)
    }

    async fn unread_counts(&self) -> Result<HashMap<MailSet, i64>, BackendError> {
        self.0.gm_unread().await
    }

    async fn mailboxes(&self) -> Result<Vec<RemoteMailbox>, BackendError> {
        self.0.gm_mailboxes().await
    }

    async fn changes(&self, since: Option<&SyncState>) -> Result<Changes, BackendError> {
        self.0.gm_changes(since).await
    }

    async fn create_mailbox(&self, name: &str) -> Result<RemoteMailbox, BackendError> {
        self.0.gm_create_label(name).await
    }

    async fn rename_mailbox(&self, id: &str, name: &str) -> Result<RemoteMailbox, BackendError> {
        self.0.gm_rename_label(id, name).await
    }

    async fn delete_mailbox(&self, id: &str) -> Result<(), BackendError> {
        self.0.gm_delete_label(id).await
    }

    /// Gmail's IMAP keeps no colour for a label.
    async fn set_mailbox_color(
        &self,
        _id: &str,
        _color: &LabelColor,
    ) -> Result<RemoteMailbox, BackendError> {
        Err(BackendError::Unsupported)
    }

    async fn mailbox_threads(&self, id: &str) -> Result<u64, BackendError> {
        self.0.gm_label_count(id).await
    }

    /// Every label's mail is in All Mail, which is always synced.
    fn follow(&self, _mailbox: &str) -> bool {
        false
    }

    fn reset_follow(&self, _mailbox: &str) {}

    fn set_window_open(&self, open: bool) {
        self.0.set_window_open(open);
    }

    fn check_now(&self) {
        self.0.check_now();
    }

    fn poll_interval(&self) -> Option<Duration> {
        self.0.poll_interval()
    }

    /// Watches All Mail, where every new message lands whatever its labels.
    async fn watch(&self) {
        let all = async { self.0.gm_synced().await.map(|boxes| boxes.all) };
        self.0.watch_inbox(all).await
    }

    async fn keywords_in(
        &self,
        mailbox: &str,
        uidvalidity: u32,
        uids: RangeInclusive<u32>,
    ) -> Result<KeywordsPage, BackendError> {
        self.0.keywords_in(mailbox, uidvalidity, uids).await
    }

    async fn uidvalidity(&self, mailbox: &str) -> Result<Option<u32>, BackendError> {
        self.0.uidvalidity(mailbox).await
    }

    async fn keywords_stored(
        &self,
        mailbox: &str,
    ) -> Result<&'static [&'static str], BackendError> {
        self.0.keywords_stored(mailbox).await
    }

    async fn backfill(&self, days: i64, cursor: Option<&str>) -> Result<Backfill, BackendError> {
        self.0.gm_backfill(days, cursor).await
    }

    async fn window_ids(
        &self,
        days: i64,
        mailbox: Option<&str>,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        self.0.gm_window(days, mailbox).await
    }

    async fn inbox_ids(&self) -> Result<Vec<RemoteRef>, BackendError> {
        let days = self.0.settings.window_days;
        self.0.gm_window(days, Some("INBOX")).await
    }

    async fn search(
        &self,
        query: &SearchQuery,
        limit: usize,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        self.0.gm_search(query, limit).await
    }

    async fn mailbox_page(
        &self,
        mailbox: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Backfill, BackendError> {
        self.0.gm_label_page(mailbox, cursor, limit).await
    }

    async fn find_sent(&self, message_id: &str) -> Result<Option<String>, BackendError> {
        self.0.gm_find_sent(message_id).await
    }

    async fn fetch(&self, wants: Vec<Want>) -> Result<Found, BackendError> {
        self.0.gm_fetch(wants).await
    }

    async fn fetch_whole(&self, threads: Vec<String>) -> Result<Found, BackendError> {
        self.0.gm_fetch_whole(threads).await
    }

    async fn fetch_raw(&self, ids: &[String]) -> Result<Vec<RawMessage>, BackendError> {
        self.0.gm_raw_messages(ids).await
    }

    async fn append(&self, raw: &[u8], mailbox: &str) -> Result<String, BackendError> {
        self.0.append(raw, mailbox).await
    }

    async fn fetch_structure(&self, id: &str) -> Result<Parts, BackendError> {
        let at = self.0.gm_current(id).await?;
        self.0.structure_at(id, &at).await
    }

    async fn fetch_part(&self, id: &str, path: &str) -> Result<Vec<u8>, BackendError> {
        let at = self.0.gm_current(id).await?;
        self.0.part_at(&at, path).await
    }

    fn mailbox_for(&self, role: Role) -> Option<String> {
        mailrs_gmail::labels::label_of_role(role).map(str::to_string)
    }

    fn set_of(&self, id: &str) -> MailSet {
        mailrs_gmail::labels::set_of(id)
    }

    async fn apply(
        &self,
        messages: &[String],
        ops: &[MailOp],
    ) -> Result<Vec<Relocated>, Unapplied> {
        self.0.gm_write(messages, ops).await
    }

    async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<String, BackendError> {
        self.0.send(raw, thread_id).await
    }

    async fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SavedDraft, BackendError> {
        self.0.gm_save_draft(draft_id, raw, thread_id).await
    }

    async fn send_draft(&self, draft_id: &str) -> Result<String, BackendError> {
        self.0.gm_send_draft(draft_id).await
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), BackendError> {
        self.0.gm_delete_draft(draft_id).await
    }

    async fn list_drafts(&self) -> Result<Vec<DraftRef>, BackendError> {
        self.0.gm_list_drafts().await
    }
}

impl<I: ImapApi, S: Submit> IdentityService for GmailImap<I, S> {
    /// The account's own address, as on any IMAP server. Send-as addresses
    /// live in Preferences: Gmail's own list needs the Gmail API.
    async fn identities(&self) -> Result<Vec<SendAsAddress>, BackendError> {
        self.0.identities().await
    }
}

/// The three mailboxes a Gmail account syncs, by the server's names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GmailMailboxes {
    pub all: String,
    pub trash: String,
    pub spam: String,
}

impl GmailMailboxes {
    /// The order a listing reads them in.
    pub fn in_order(&self) -> Vec<String> {
        vec![self.all.clone(), self.trash.clone(), self.spam.clone()]
    }

    /// The order a look reads them in: a message that left All Mail for
    /// Trash is met in Trash first.
    pub fn look_order(&self) -> Vec<String> {
        vec![self.trash.clone(), self.spam.clone(), self.all.clone()]
    }

    /// Which of the three `mailbox` is. Anything else counts as All Mail.
    pub fn place_of(&self, mailbox: &str) -> GmailPlace {
        match mailbox {
            m if m == self.trash => GmailPlace::Trash,
            m if m == self.spam => GmailPlace::Spam,
            _ => GmailPlace::AllMail,
        }
    }
}

fn label_row(name: &str) -> RemoteMailbox {
    RemoteMailbox {
        id: name.to_string(),
        name: name.to_string(),
        kind: MailboxKind::Label,
        role: None,
        color: None,
        hidden: false,
    }
}

/// The system labels the store lists besides a person's own.
const SYSTEM_LISTED: [&str; 6] = ["INBOX", "SENT", "DRAFT", "TRASH", "SPAM", "IMPORTANT"];

/// The inbox tabs the store lists.
const TABS_LISTED: [&str; 5] = [
    "CATEGORY_PERSONAL",
    "CATEGORY_SOCIAL",
    "CATEGORY_PROMOTIONS",
    "CATEGORY_UPDATES",
    "CATEGORY_FORUMS",
];

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// All Mail, Trash and Spam as the server names them, found by their
    /// special use since the names follow the account's language. All Mail
    /// is missing when the person left it out of IMAP in Gmail's settings.
    pub(super) async fn gm_synced(&self) -> Result<GmailMailboxes, BackendError> {
        self.ensure_listed().await?;
        let known = self.known();
        let named = |role: Role| {
            known
                .folders
                .iter()
                .find(|f| f.role == Some(role) && !f.parent_only)
                .map(|f| f.id.clone())
        };
        match (named(Role::All), named(Role::Trash), named(Role::Junk)) {
            (Some(all), Some(trash), Some(spam)) => Ok(GmailMailboxes { all, trash, spam }),
            (None, _, _) => Err(BackendError::Refused(
                "Gmail leaves All Mail out of IMAP".into(),
            )),
            _ => Err(BackendError::Refused(
                "Gmail lists no Trash or Spam folder".into(),
            )),
        }
    }

    /// The label folders Gmail lists for the person's own labels, as store
    /// ids with the server's name for each: everything outside the
    /// `[Gmail]` parent that holds All Mail, and the Inbox.
    async fn gm_label_folders(&self) -> Result<Vec<(String, mailrs_imap::Listed)>, BackendError> {
        let boxes = self.gm_synced().await?;
        let listed = self.api.list().await?;
        let delimiter = listed.iter().find_map(|l| l.delimiter).unwrap_or('/');
        let parent = boxes
            .all
            .rsplit_once(delimiter)
            .map(|(parent, _)| parent.to_string());
        Ok(listed
            .into_iter()
            .filter(|l| !l.name.eq_ignore_ascii_case("INBOX"))
            .filter(|l| match &parent {
                Some(parent) => {
                    l.name != *parent && !l.name.starts_with(&format!("{parent}{delimiter}"))
                }
                None => true,
            })
            .map(|l| (l.display.replace(delimiter, "/"), l))
            .collect())
    }

    /// The store's mailboxes for a Gmail account: Gmail's own labels with
    /// fixed ids, the inbox tabs, and a label for each of the person's
    /// folders named by its full name.
    pub(super) async fn gm_mailboxes(&self) -> Result<Vec<RemoteMailbox>, BackendError> {
        // Remembers the roles, for the calls that answer at once.
        self.list_mailboxes().await?;
        let mut boxes: Vec<RemoteMailbox> = SYSTEM_LISTED
            .iter()
            .chain(TABS_LISTED.iter())
            .map(|id| RemoteMailbox {
                id: (*id).into(),
                name: (*id).into(),
                kind: MailboxKind::System,
                role: mailrs_gmail::labels::role_of(id),
                color: None,
                hidden: false,
            })
            .collect();
        for (name, listed) in self.gm_label_folders().await? {
            boxes.push(RemoteMailbox {
                id: name.clone(),
                name,
                kind: match listed.no_select {
                    true => MailboxKind::Group,
                    false => MailboxKind::Label,
                },
                role: None,
                color: None,
                hidden: false,
            });
        }
        Ok(boxes)
    }

    /// Makes a label by its full name. Slashes nest it under another.
    pub(in crate::services::imap) async fn gm_create_label(
        &self,
        name: &str,
    ) -> Result<RemoteMailbox, BackendError> {
        self.api.create(&mailrs_imap::utf7::encode(name)).await?;
        self.list_mailboxes().await?;
        Ok(label_row(name))
    }

    /// Renames a label. Gmail renames the labels nested under it too, and
    /// the next listing brings them under their new names.
    pub(in crate::services::imap) async fn gm_rename_label(
        &self,
        id: &str,
        name: &str,
    ) -> Result<RemoteMailbox, BackendError> {
        self.api
            .rename(
                &mailrs_imap::utf7::encode(id),
                &mailrs_imap::utf7::encode(name),
            )
            .await?;
        self.list_mailboxes().await?;
        Ok(label_row(name))
    }

    pub(in crate::services::imap) async fn gm_delete_label(
        &self,
        id: &str,
    ) -> Result<(), BackendError> {
        self.api.delete(&mailrs_imap::utf7::encode(id)).await?;
        self.list_mailboxes().await?;
        Ok(())
    }

    /// How many messages carry the label `id`.
    pub(in crate::services::imap) async fn gm_label_count(
        &self,
        id: &str,
    ) -> Result<u64, BackendError> {
        let Some(key) = labels::search_key(id) else {
            return Ok(0);
        };
        let boxes = self.gm_synced().await?;
        let selected = self.select(&boxes.all, None).await?;
        let top = self.top_uid(&boxes.all, &selected).await?;
        let mut count = 0u64;
        self.search_windows(&boxes.all, (1, top), &format!("UNDELETED {key}"), |found| {
            count += found.len() as u64
        })
        .await?;
        Ok(count)
    }

    /// Unread counts from STATUS of the Inbox, Spam and each label's own
    /// folder, which Gmail counts across All Mail.
    pub(super) async fn gm_unread(
        &self,
    ) -> Result<std::collections::HashMap<MailSet, i64>, BackendError> {
        use futures::{StreamExt, stream};
        use std::time::Duration;
        use tokio::time::timeout;

        let limit = Duration::from_secs(10);
        let boxes = self.gm_synced().await?;
        let mut folders: Vec<(String, String)> = vec![
            ("INBOX".into(), "INBOX".into()),
            (boxes.spam.clone(), "SPAM".into()),
        ];
        for (name, listed) in self.gm_label_folders().await? {
            if !listed.no_select {
                folders.push((listed.name, name));
            }
        }
        let mut counts = std::collections::HashMap::new();
        let mut replies = stream::iter(folders)
            .map(|(folder, id)| async move {
                let count = timeout(limit, self.api.unread(&folder)).await;
                (id, count)
            })
            .buffer_unordered(2);
        while let Some((id, count)) = replies.next().await {
            match count {
                Ok(Ok(count)) => {
                    let set = mailrs_gmail::labels::set_of(&id);
                    if let MailSet::Role(_) = set {
                        counts.insert(MailSet::Mailbox(id), i64::from(count));
                    }
                    counts.insert(set, i64::from(count));
                }
                Ok(Err(
                    mailrs_imap::ImapError::NoMailbox(_) | mailrs_imap::ImapError::Refused(_),
                )) => {
                    continue;
                }
                Ok(Err(_)) | Err(_) => break,
            }
        }
        Ok(counts)
    }
}

#[cfg(test)]
mod tests {
    use super::GmailMailboxes;
    use super::labels::GmailPlace;

    #[test]
    fn a_mailbox_that_is_neither_trash_nor_spam_counts_as_all_mail() {
        let boxes = GmailMailboxes {
            all: "[Gmail]/Todos".into(),
            trash: "[Gmail]/Lixo".into(),
            spam: "[Gmail]/Spam".into(),
        };
        assert_eq!(boxes.place_of("[Gmail]/Lixo"), GmailPlace::Trash);
        assert_eq!(boxes.place_of("[Gmail]/Spam"), GmailPlace::Spam);
        assert_eq!(boxes.place_of("[Gmail]/Todos"), GmailPlace::AllMail);
        assert_eq!(boxes.look_order()[2], "[Gmail]/Todos");
    }
}
