//! Gmail's window, backfill, label pages, search and metadata over IMAP.
//! Every listing answers hex ids; the places they stand at are kept in
//! [`super::ids::Places`].

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;
use mailrs_domain::{Location, MessageMeta};
use mailrs_imap::{Fetched, UidSet, gm_labels, gm_raw, gm_thrid};
use serde::{Deserialize, Serialize};

use super::labels::{self, GmailPlace, TABS};
use super::{Imap, ImapApi, Submit};
use crate::BackendError;
use crate::services::imap::syntax::imap_date;
use crate::services::imap::window::meta_of;
use crate::services::imap::{BATCH_LIMIT, keywords};
use crate::services::{Backfill, Found, LIST_PAGE_SIZE, RemoteRef, SearchQuery, Want};

/// Where a backfill stands: the index of the mailbox in
/// [`GmailMailboxes::in_order`] and the UID it has listed down to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Cursor {
    at: usize,
    below: Option<u32>,
}

/// The keys for All Mail's window: every message of the Inbox and the mail
/// of the last `days` days, never one marked deleted.
fn all_mail_keys(days: i64, today: NaiveDate) -> String {
    let Some(days) = crate::window_reach(days) else {
        return "UNDELETED".to_string();
    };
    let start = today
        .checked_sub_days(chrono::Days::new(days.unsigned_abs()))
        .unwrap_or(NaiveDate::MIN);
    format!(
        "UNDELETED OR SINCE {} {}",
        imap_date(start),
        gm_labels("\\Inbox").expect("a system label is a valid label"),
    )
}

/// The keys for the window of Trash or Spam.
fn side_keys(days: i64, today: NaiveDate) -> String {
    match crate::window_reach(days) {
        None => "UNDELETED".to_string(),
        Some(days) => {
            let start = today
                .checked_sub_days(chrono::Days::new(days.unsigned_abs()))
                .unwrap_or(NaiveDate::MIN);
            format!("SINCE {} UNDELETED", imap_date(start))
        }
    }
}

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// Every UID of `mailbox` that matches `keys`, with the UIDVALIDITY.
    async fn gm_matching(
        &self,
        mailbox: &str,
        keys: &str,
    ) -> Result<(u32, Vec<u32>), BackendError> {
        let selected = self.select(mailbox, None).await?;
        let top = self.top_uid(mailbox, &selected).await?;
        let mut uids = Vec::new();
        self.search_windows(mailbox, (1, top), keys, |found| uids.extend(found))
            .await?;
        Ok((selected.uidvalidity, uids))
    }

    /// `uids` of `mailbox` as the refs a listing hands over, newest first.
    async fn gm_refs(
        &self,
        mailbox: &str,
        uidvalidity: u32,
        uids: &[u32],
    ) -> Result<Vec<RemoteRef>, BackendError> {
        let mut paired = self.gm_pair(mailbox, uidvalidity, uids).await?;
        paired.sort_unstable_by_key(|f| std::cmp::Reverse(f.uid));
        Ok(paired
            .into_iter()
            .filter_map(|flags| flags.gmail)
            .map(|meta| RemoteRef {
                id: meta.id(),
                thread_id: meta.thread_id(),
            })
            .collect())
    }

    /// The window, or the part of it that carries the store's label
    /// `label`.
    pub(in crate::services::imap) async fn gm_window(
        &self,
        days: i64,
        label: Option<&str>,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        let boxes = self.gm_synced().await?;
        let today = chrono::Local::now().date_naive();
        let side = side_keys(days, today);
        let wanted: Vec<(String, String)> = match label {
            None => vec![
                (boxes.all.clone(), all_mail_keys(days, today)),
                (boxes.trash.clone(), side.clone()),
                (boxes.spam.clone(), side),
            ],
            Some("TRASH") => vec![(boxes.trash.clone(), side)],
            Some("SPAM") => vec![(boxes.spam.clone(), side)],
            Some(id) => match labels::search_key(id) {
                Some(key) => {
                    let since = match id {
                        "INBOX" => String::new(),
                        _ => all_mail_keys(days, today),
                    };
                    vec![(
                        boxes.all.clone(),
                        format!("{since} {key}").trim_start().to_string(),
                    )]
                }
                None => Vec::new(),
            },
        };
        let mut refs = Vec::new();
        for (mailbox, keys) in wanted {
            let (uidvalidity, uids) = self.gm_matching(&mailbox, &keys).await?;
            refs.extend(self.gm_refs(&mailbox, uidvalidity, &uids).await?);
        }
        Ok(refs)
    }

    /// One page of the window: All Mail, then Trash, then Spam, newest UID
    /// first.
    pub(in crate::services::imap) async fn gm_backfill(
        &self,
        days: i64,
        cursor: Option<&str>,
    ) -> Result<Backfill, BackendError> {
        let boxes = self.gm_synced().await?;
        let order = boxes.in_order();
        let mut at = match cursor {
            None => Cursor { at: 0, below: None },
            Some(text) => serde_json::from_str(text).map_err(|_| BackendError::StateLost)?,
        };
        if at.at >= order.len() {
            return Err(BackendError::StateLost);
        }
        let today = chrono::Local::now().date_naive();
        loop {
            let mailbox = order[at.at].clone();
            let selected = self.select(&mailbox, None).await?;
            let top = match at.below {
                Some(below) => below.saturating_sub(1),
                None => self.top_uid(&mailbox, &selected).await?,
            };
            let keys = match at.at {
                0 => all_mail_keys(days, today),
                _ => side_keys(days, today),
            };
            let page = self.newest_below(&mailbox, &keys, top).await?;
            let lowest = page.last().copied();
            let full = page.len() == LIST_PAGE_SIZE as usize && lowest.is_some_and(|uid| uid > 1);
            let next = match (full, at.at + 1 < order.len()) {
                (true, _) => Some(Cursor {
                    at: at.at,
                    below: lowest,
                }),
                (false, true) => Some(Cursor {
                    at: at.at + 1,
                    below: None,
                }),
                (false, false) => None,
            };
            if page.is_empty()
                && let Some(following) = next
            {
                at = following;
                continue;
            }
            return Ok(Backfill {
                refs: self.gm_refs(&mailbox, selected.uidvalidity, &page).await?,
                next: next.and_then(|c| serde_json::to_string(&c).ok()),
            });
        }
    }

    /// A page of the mail carrying the store's label `id`, newest first.
    /// The cursor is the All Mail location of the last hit.
    pub(in crate::services::imap) async fn gm_label_page(
        &self,
        id: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Backfill, BackendError> {
        let boxes = self.gm_synced().await?;
        let (mailbox, keys) = match id {
            "TRASH" => (boxes.trash.clone(), "UNDELETED".to_string()),
            "SPAM" => (boxes.spam.clone(), "UNDELETED".to_string()),
            other => match labels::search_key(other) {
                Some(key) => (boxes.all.clone(), format!("UNDELETED {key}")),
                None => {
                    return Ok(Backfill {
                        refs: Vec::new(),
                        next: None,
                    });
                }
            },
        };
        let selected = self.select(&mailbox, None).await?;
        let top = match cursor {
            Some(cursor) => {
                let at = Location::parse(cursor).ok_or(BackendError::StateLost)?;
                if at.mailbox != mailbox || at.uidvalidity != selected.uidvalidity {
                    return Err(BackendError::StateLost);
                }
                at.uid.saturating_sub(1)
            }
            None => self.top_uid(&mailbox, &selected).await?,
        };
        let limit = limit.max(1);
        let uids = self
            .newest_matching(&mailbox, &keys, top, limit.saturating_add(1))
            .await?;
        let more = uids.len() > limit;
        let page: Vec<u32> = uids.into_iter().take(limit).collect();
        let refs = self.gm_refs(&mailbox, selected.uidvalidity, &page).await?;
        let next = match (more, page.last()) {
            (true, Some(&uid)) => Some(
                Location {
                    mailbox,
                    uidvalidity: selected.uidvalidity,
                    uid,
                }
                .to_string(),
            ),
            _ => None,
        };
        Ok(Backfill { refs, next })
    }

    /// A search in Gmail's syntax, run by Gmail. A tree prints as the text
    /// the folders and smart mailboxes sent before they became trees.
    pub(in crate::services::imap) async fn gm_search(
        &self,
        query: &SearchQuery,
        limit: usize,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        let text = &match query {
            SearchQuery::Native(text) => text.clone(),
            SearchQuery::Tree(tree) => mailrs_gmail::query::print(tree),
        };
        let boxes = self.gm_synced().await?;
        let mailbox = if names_term(text, "in:trash") {
            &boxes.trash
        } else if names_term(text, "in:spam") {
            &boxes.spam
        } else {
            &boxes.all
        };
        let keys = format!("{} UNDELETED", gm_raw(text)?);
        let selected = self.select(mailbox, None).await?;
        let top = self.top_uid(mailbox, &selected).await?;
        let uids = self.newest_matching(mailbox, &keys, top, limit).await?;
        self.gm_refs(mailbox, selected.uidvalidity, &uids).await
    }

    /// The message of Sent with Message-ID `message_id`, by hex id.
    pub(in crate::services::imap) async fn gm_find_sent(
        &self,
        message_id: &str,
    ) -> Result<Option<String>, BackendError> {
        let boxes = self.gm_synced().await?;
        let wanted = message_id.trim_matches(['<', '>']);
        if wanted.contains(['"', '\r', '\n', '\\']) {
            return Ok(None);
        }
        let sent = gm_labels("\\Sent").expect("a system label is a valid label");
        let keys = format!("HEADER Message-ID \"{wanted}\" {sent} UNDELETED");
        let (uidvalidity, uids) = self.gm_matching(&boxes.all, &keys).await?;
        Ok(self
            .gm_refs(&boxes.all, uidvalidity, &uids)
            .await?
            .into_iter()
            .next()
            .map(|r| r.id))
    }

    /// Which of `uids` of All Mail the inbox tabs hold, and the personal
    /// tab for the rest of those in the Inbox.
    async fn gm_tabs(
        &self,
        mailbox: &str,
        inbox: &[u32],
    ) -> Result<HashMap<u32, &'static str>, BackendError> {
        let mut tabs = HashMap::new();
        if inbox.is_empty() {
            return Ok(tabs);
        }
        for chunk in inbox.chunks(5_000) {
            let set = UidSet::from_uids(chunk.iter().copied());
            for (name, tab) in TABS {
                let keys = format!(
                    "UID {set} {}",
                    gm_raw(&format!("category:{name} in:inbox"))?
                );
                for uid in self.api.search(mailbox, &keys).await? {
                    tabs.insert(uid, tab);
                }
            }
            for uid in chunk {
                tabs.entry(*uid).or_insert("CATEGORY_PERSONAL");
            }
        }
        Ok(tabs)
    }

    /// Metadata for `wants` by hex id. A message nobody holds comes back
    /// gone. A location under a UIDVALIDITY the mailbox no longer has
    /// comes back neither found nor gone: the message may still be there
    /// under a new UID, and the relisting that follows the change finds
    /// it.
    pub(in crate::services::imap) async fn gm_fetch(
        &self,
        wants: Vec<Want>,
    ) -> Result<Found, BackendError> {
        let boxes = self.gm_synced().await?;
        let mut found = Found::default();
        let mut left: Vec<String> = Vec::new();
        let mut renumbered: Vec<String> = Vec::new();
        for want in wants {
            // The engine names a message by where its ref says it sits, as
            // a location, or by its hex id before it has a ref.
            let named =
                super::ids::msgid_of(&want.id).is_some() || Location::parse(&want.id).is_some();
            match named {
                true if !left.contains(&want.id) => left.push(want.id),
                true => {}
                false => found.gone.push(want.id),
            }
        }
        for pass in 0..2 {
            if left.is_empty() {
                break;
            }
            let located: HashMap<String, Location> = match pass {
                0 => {
                    let known = self.known();
                    left.iter()
                        .filter_map(|id| {
                            Location::parse(id)
                                .or_else(|| known.places.get(id))
                                .map(|at| (id.clone(), at))
                        })
                        .collect()
                }
                _ => {
                    let hex: Vec<String> = left
                        .iter()
                        .filter(|id| Location::parse(id).is_none())
                        .cloned()
                        .collect();
                    self.gm_find(&hex).await?
                }
            };
            let mut by_mailbox: BTreeMap<String, Vec<(String, Location)>> = BTreeMap::new();
            for (id, at) in located {
                by_mailbox
                    .entry(at.mailbox.clone())
                    .or_default()
                    .push((id, at));
            }
            for (mailbox, group) in by_mailbox {
                let selected = match self.select(&mailbox, None).await {
                    Ok(selected) => selected,
                    Err(BackendError::NotFound) => continue,
                    Err(err) => return Err(err),
                };
                let (current, stale): (Vec<_>, Vec<_>) = group
                    .into_iter()
                    .partition(|(_, at)| at.uidvalidity == selected.uidvalidity);
                for (id, _) in stale {
                    self.known().places.forget(&id);
                    if Location::parse(&id).is_some() {
                        renumbered.push(id);
                    }
                }
                let place = boxes.place_of(&mailbox);
                let mut metas: Vec<(String, Location, Fetched)> = Vec::new();
                for chunk in current.chunks(BATCH_LIMIT) {
                    let uids = UidSet::from_uids(chunk.iter().map(|(_, at)| at.uid));
                    let fetched = self.api.gmail_headers(&mailbox, &uids).await?;
                    let mut by_uid: HashMap<u32, Fetched> =
                        fetched.into_iter().map(|f| (f.uid, f)).collect();
                    for (id, at) in chunk {
                        let Some(fetched) = by_uid.remove(&at.uid) else {
                            self.known().places.forget(id);
                            continue;
                        };
                        let right = fetched
                            .gmail
                            .as_ref()
                            .is_some_and(|g| Location::parse(id).is_some() || g.id() == *id);
                        if !right || keywords::is_deleted(&fetched.flags) {
                            self.known().places.forget(id);
                            continue;
                        }
                        metas.push((id.clone(), at.clone(), fetched));
                    }
                }
                let inbox: Vec<u32> = match place {
                    GmailPlace::AllMail => metas
                        .iter()
                        .filter(|(_, _, f)| {
                            f.gmail
                                .as_ref()
                                .is_some_and(|g| g.labels.iter().any(|l| l == "\\Inbox"))
                        })
                        .map(|(_, at, _)| at.uid)
                        .collect(),
                    _ => Vec::new(),
                };
                let tabs = self.gm_tabs(&mailbox, &inbox).await?;
                for (id, at, fetched) in metas {
                    let Some(gmail) = fetched.gmail.clone() else {
                        continue;
                    };
                    let (mut meta, _) = meta_of(&at, &fetched, None);
                    let all = labels::store_labels(
                        &gmail,
                        &fetched.flags,
                        place,
                        tabs.get(&at.uid).copied(),
                    );
                    meta.id = gmail.id();
                    meta.thread_id = gmail.thread_id();
                    meta.held = mailrs_gmail::labels::memberships(&all);
                    left.retain(|l| *l != id);
                    found.located.insert(meta.id.clone(), at);
                    found.metas.push(meta);
                }
            }
        }
        left.retain(|id| !renumbered.contains(id));
        found.gone.extend(left);
        Ok(found)
    }

    /// Every message of each thread, the thread by its hex id.
    pub(in crate::services::imap) async fn gm_fetch_whole(
        &self,
        threads: Vec<String>,
    ) -> Result<Found, BackendError> {
        let boxes = self.gm_synced().await?;
        let mut wants = Vec::new();
        for thread in &threads {
            let Some(number) = super::ids::msgid_of(thread) else {
                continue;
            };
            for mailbox in boxes.in_order() {
                let (uidvalidity, uids) = self
                    .gm_matching(&mailbox, &format!("{} UNDELETED", gm_thrid(number)))
                    .await?;
                for flags in self.gm_pair(&mailbox, uidvalidity, &uids).await? {
                    if let Some(meta) = flags.gmail {
                        wants.push(Want::message(meta.id()));
                    }
                }
            }
        }
        let found = self.gm_fetch(wants).await?;
        let mut whole: BTreeMap<String, Vec<MessageMeta>> = BTreeMap::new();
        for meta in &found.metas {
            whole
                .entry(meta.thread_id.clone())
                .or_default()
                .push(meta.clone());
        }
        let gone_threads = threads
            .into_iter()
            .filter(|t| !whole.contains_key(t))
            .collect();
        Ok(Found {
            metas: found.metas,
            gone: found.gone,
            whole: whole.into_values().collect(),
            gone_threads,
            links: found.links,
            located: found.located,
        })
    }
}

/// Whether Gmail's search `text` holds `term` as a word of its own,
/// outside quotes and not negated with `-`, whatever the case.
fn names_term(text: &str, term: &str) -> bool {
    let mut quoted = false;
    let mut words = Vec::new();
    let mut word = String::new();
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                word.push(c);
            }
            c if c.is_whitespace() && !quoted => words.push(std::mem::take(&mut word)),
            c => word.push(c),
        }
    }
    words.push(word);
    words.iter().any(|w| {
        w.trim_start_matches('(')
            .trim_end_matches(')')
            .eq_ignore_ascii_case(term)
    })
}

#[cfg(test)]
mod tests {
    use super::names_term;

    #[test]
    fn a_search_names_trash_only_as_a_word_of_its_own() {
        assert!(names_term("from:ann in:trash", "in:trash"));
        assert!(names_term("(IN:TRASH)", "in:trash"));
        assert!(!names_term("from:ann -in:trash", "in:trash"));
        assert!(!names_term("subject:\"in:trash please\"", "in:trash"));
    }
}
