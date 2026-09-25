//! The sync window over IMAP: which messages each synced mailbox holds
//! within it, listed by UID, and their metadata from one FETCH per mailbox
//! and batch. A message's id is its location the first time the adapter
//! lists it; the engine keeps it under that id from then on.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::RangeInclusive;

use chrono::NaiveDate;
use mailrs_domain::{Location, Memberships, MessageMeta, Role};
use mailrs_imap::{Fetched, Selected, UidSet};
use mailrs_store::threading::Links;
use serde::{Deserialize, Serialize};

use super::keywords::{is_deleted, keywords_of};
use super::state::Kept;
use super::syntax::imap_date;
use super::{BATCH_LIMIT, Imap, ImapApi, Submit};
use crate::BackendError;
use crate::services::{Backfill, Found, LIST_PAGE_SIZE, MailBackend, RemoteRef, Want};

/// Where a backfill stands: the mailbox it lists and the UID it has
/// listed down to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Page {
    mailbox: String,
    below: Option<u32>,
}

/// The most UIDs one SEARCH or flags FETCH over a mailbox names. The
/// client drops the connection past 100,000 answers to one command and
/// past 4 MiB of SEARCH answer, and a range of 50,000 stays well inside
/// both whatever the mailbox holds.
pub(super) const WINDOW: u32 = 50_000;

/// UID ranges of at most [`WINDOW`] UIDs that cover `from` to `to`,
/// lowest first; none when `to` is below `from`.
pub(super) fn windows(from: u32, to: u32) -> impl Iterator<Item = RangeInclusive<u32>> {
    let from = from.max(1);
    let count = match to >= from {
        true => (to - from) / WINDOW + 1,
        false => 0,
    };
    (0..count).map(move |i| {
        let start = from + i * WINDOW;
        start..=start.saturating_add(WINDOW - 1).min(to)
    })
}

/// The UIDs a backfill page searches first below its cursor: four pages'
/// worth, so a mailbox without gaps fills a page with one search and each
/// UID is answered about four times over the whole backfill.
const FIRST_SPAN: u32 = 4 * LIST_PAGE_SIZE;

/// The SEARCH keys for the window in `mailbox` on `today`: all of the
/// Inbox, mail since `days` before today elsewhere, and never a message
/// marked deleted, which another client or a move on a server without
/// MOVE left behind.
pub(super) fn window_keys(mailbox: &str, days: i64, today: NaiveDate) -> String {
    if mailbox.eq_ignore_ascii_case("INBOX") {
        return "UNDELETED".to_string();
    }
    let start = today
        .checked_sub_days(chrono::Days::new(days.max(0).unsigned_abs()))
        .unwrap_or(NaiveDate::MIN);
    format!("SINCE {} UNDELETED", imap_date(start))
}

/// A message as the adapter lists it: named by its location, and a thread
/// of its own until local threading places it.
fn named(mailbox: &str, uidvalidity: u32, uid: u32) -> RemoteRef {
    let id = Location {
        mailbox: mailbox.to_string(),
        uidvalidity,
        uid,
    }
    .to_string();
    RemoteRef {
        thread_id: id.clone(),
        id,
    }
}

/// A fetched message as the store keeps it, with the links its headers
/// name. The server sends no preview text, and the adapter does not know
/// the store's account id, which the engine fills in.
fn meta_of(at: &Location, fetched: &Fetched, role: Option<Role>) -> (MessageMeta, Links) {
    let id = at.to_string();
    let meta = MessageMeta {
        account_id: 0,
        thread_id: id.clone(),
        id,
        rfc822_msgid: fetched.message_id.clone(),
        from: fetched.from.clone(),
        to: fetched.to.clone(),
        cc: fetched.cc.clone(),
        subject: fetched.subject.clone(),
        // A server that sends no INTERNALDATE leaves the message at the
        // epoch, at the bottom of every list, rather than out of it.
        date: fetched.internal_date.unwrap_or_default(),
        snippet: String::new(),
        // A server that answers no size sends the message by its
        // structure, as a size of 0 does.
        size: fetched
            .size
            .map_or(0, |size| i64::try_from(size).unwrap_or(i64::MAX)),
        has_attachments: fetched.mixed,
        held: Memberships {
            mailboxes: vec![at.mailbox.clone()],
            keywords: keywords_of(&fetched.flags),
            categories: Vec::new(),
        },
        roles: role.into_iter().collect(),
        list_unsubscribe: fetched.list_unsubscribe.clone(),
        one_click: fetched
            .list_unsubscribe_post
            .as_deref()
            .is_some_and(|v| v.contains("One-Click")),
    };
    let links = Links {
        in_reply_to: fetched.in_reply_to.clone(),
        references: fetched.references.clone(),
    };
    (meta, links)
}

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// Lists the mailboxes once, so the roles are known before the first
    /// sync asks for them.
    pub(super) async fn ensure_listed(&self) -> Result<(), BackendError> {
        let listed = !self.known().folders.is_empty();
        if !listed {
            self.list_mailboxes().await?;
        }
        Ok(())
    }

    /// The mailboxes the account keeps in step: the Inbox, and Sent,
    /// Drafts and Archive where the server has them, then every mailbox a
    /// person opened.
    pub(super) async fn synced(&self) -> Result<Vec<String>, BackendError> {
        self.ensure_listed().await?;
        let mut synced = Vec::new();
        for role in [Role::Inbox, Role::Sent, Role::Drafts, Role::Archive] {
            if let Some(id) = self.mailbox_for(role)
                && !synced.contains(&id)
            {
                synced.push(id);
            }
        }
        let followed: Vec<String> = self.known().followed.iter().cloned().collect();
        for id in followed {
            if !synced.contains(&id) {
                synced.push(id);
            }
        }
        Ok(synced)
    }

    /// The role of the mailbox `id`, as the last listing gave it.
    pub(super) fn role_of(&self, id: &str) -> Option<Role> {
        self.known()
            .folders
            .iter()
            .find(|f| f.id == id)
            .and_then(|f| f.role)
    }

    /// The highest UID `mailbox` holds, 0 when it holds none: one below
    /// the UIDNEXT `selected` reports, or the server's answer for `UID *`
    /// when it left UIDNEXT out, as RFC 3501 allows.
    pub(super) async fn top_uid(
        &self,
        mailbox: &str,
        selected: &Selected,
    ) -> Result<u32, BackendError> {
        match selected.uidnext {
            Some(next) => Ok(next.saturating_sub(1)),
            None => Ok(self
                .api
                .search(mailbox, "UID *")
                .await?
                .into_iter()
                .max()
                .unwrap_or(0)),
        }
    }

    /// The UIDs in `from` to `to` that match `keys`, searched a
    /// [`WINDOW`] at a time. Each window's answer goes to `take` sorted,
    /// lowest first, before the next is asked for, so the caller holds
    /// one window's answer at a time.
    pub(super) async fn search_windows(
        &self,
        mailbox: &str,
        (from, to): (u32, u32),
        keys: &str,
        mut take: impl FnMut(Vec<u32>) + Send,
    ) -> Result<(), BackendError> {
        for window in windows(from, to) {
            let (start, end) = (*window.start(), *window.end());
            let keys = match keys.is_empty() {
                true => format!("UID {start}:{end}"),
                false => format!("UID {start}:{end} {keys}"),
            };
            let mut uids = self.api.search(mailbox, &keys).await?;
            uids.retain(|uid| window.contains(uid));
            uids.sort_unstable();
            uids.dedup();
            take(uids);
        }
        Ok(())
    }

    /// The window's UIDs in `mailbox`, newest first, with the UIDVALIDITY
    /// they belong to. Notes where this leaves the mailbox, so a mailbox a
    /// person follows right after this listing starts its feed from here
    /// rather than from whatever the server holds at the next look.
    pub(super) async fn window_uids(
        &self,
        mailbox: &str,
        days: i64,
    ) -> Result<(u32, Vec<u32>), BackendError> {
        let selected = self.select(mailbox, None).await?;
        let top = self.top_uid(mailbox, &selected).await?;
        self.known()
            .recent
            .insert(mailbox.to_string(), Kept::of(&selected, top));
        let keys = window_keys(mailbox, days, chrono::Local::now().date_naive());
        let mut uids = Vec::new();
        self.search_windows(mailbox, (1, top), &keys, |found| uids.extend(found))
            .await?;
        uids.reverse();
        Ok((selected.uidvalidity, uids))
    }

    /// One page of the window: each synced mailbox in turn, newest UID
    /// first, `LIST_PAGE_SIZE` at a time. The cursor names the mailbox and
    /// the UID the last page reached, and each page searches only below
    /// it, so mail that arrives or goes between pages moves nothing else
    /// and a long mailbox costs about one listing in all. A cursor naming
    /// a mailbox the account no longer syncs is a lost place.
    pub(super) async fn backfill_page(
        &self,
        days: i64,
        cursor: Option<&str>,
    ) -> Result<Backfill, BackendError> {
        let synced = self.synced().await?;
        let mut at = match cursor {
            None => Page {
                mailbox: synced.first().cloned().unwrap_or_else(|| "INBOX".into()),
                below: None,
            },
            Some(text) => serde_json::from_str(text).map_err(|_| BackendError::StateLost)?,
        };
        let mut index = synced
            .iter()
            .position(|m| *m == at.mailbox)
            .ok_or(BackendError::StateLost)?;
        let today = chrono::Local::now().date_naive();
        loop {
            let selected = self.select(&at.mailbox, None).await?;
            let top = match at.below {
                Some(below) => below.saturating_sub(1),
                None => self.top_uid(&at.mailbox, &selected).await?,
            };
            let keys = window_keys(&at.mailbox, days, today);
            let page = self.newest_below(&at.mailbox, &keys, top).await?;
            let lowest = page.last().copied();
            let full = page.len() == LIST_PAGE_SIZE as usize && lowest.is_some_and(|uid| uid > 1);
            let next = match (full, synced.get(index + 1)) {
                (true, _) => Some(Page {
                    mailbox: at.mailbox.clone(),
                    below: lowest,
                }),
                (false, Some(mailbox)) => Some(Page {
                    mailbox: mailbox.clone(),
                    below: None,
                }),
                (false, None) => None,
            };
            if page.is_empty()
                && let Some(following) = next
            {
                index += 1;
                at = following;
                continue;
            }
            return Ok(Backfill {
                refs: page
                    .iter()
                    .map(|uid| named(&at.mailbox, selected.uidvalidity, *uid))
                    .collect(),
                next: next.and_then(|p| serde_json::to_string(&p).ok()),
            });
        }
    }

    /// Up to `LIST_PAGE_SIZE` UIDs at or below `top` that match `keys`,
    /// newest first. The search starts with a span of [`FIRST_SPAN`] UIDs
    /// under `top` and doubles it, up to a [`WINDOW`], while the page is
    /// short, so a mailbox without gaps fills a page with one search and
    /// one with few matches reaches its bottom in a few.
    async fn newest_below(
        &self,
        mailbox: &str,
        keys: &str,
        top: u32,
    ) -> Result<Vec<u32>, BackendError> {
        let want = LIST_PAGE_SIZE as usize;
        let mut page = Vec::with_capacity(want);
        let (mut high, mut span) = (top, FIRST_SPAN);
        while high >= 1 && page.len() < want {
            let low = high.saturating_sub(span - 1).max(1);
            let mut found = Vec::new();
            self.search_windows(mailbox, (low, high), keys, |uids| found.extend(uids))
                .await?;
            page.extend(found.iter().rev().take(want - page.len()));
            if low == 1 {
                break;
            }
            high = low - 1;
            span = span.saturating_mul(2).min(WINDOW);
        }
        Ok(page)
    }

    /// Every message in the window, or in the window of `mailbox`.
    pub(super) async fn window_refs(
        &self,
        days: i64,
        mailbox: Option<&str>,
    ) -> Result<Vec<RemoteRef>, BackendError> {
        let mailboxes = match mailbox {
            Some(mailbox) => vec![mailbox.to_string()],
            None => self.synced().await?,
        };
        let mut refs = Vec::new();
        for mailbox in mailboxes {
            let (uidvalidity, uids) = self.window_uids(&mailbox, days).await?;
            refs.extend(
                uids.into_iter()
                    .map(|uid| named(&mailbox, uidvalidity, uid)),
            );
        }
        Ok(refs)
    }

    /// Every message in the Inbox.
    pub(super) async fn inbox_refs(&self) -> Result<Vec<RemoteRef>, BackendError> {
        let inbox = self
            .mailbox_for(Role::Inbox)
            .unwrap_or_else(|| "INBOX".into());
        self.window_refs(self.settings.window_days, Some(&inbox))
            .await
    }

    /// Metadata for `wants`, one FETCH per mailbox and batch. A want that
    /// names no location, a location from before the mailbox's
    /// UIDVALIDITY changed, a mailbox the server no longer has, and a
    /// message it no longer holds or has marked deleted all come back gone.
    pub(super) async fn fetch_metas(&self, wants: Vec<Want>) -> Result<Found, BackendError> {
        let mut found = Found::default();
        let mut by_mailbox: BTreeMap<String, Vec<Location>> = BTreeMap::new();
        let mut seen = HashSet::new();
        for want in wants {
            if !seen.insert(want.id.clone()) {
                continue;
            }
            match Location::parse(&want.id) {
                Some(at) => by_mailbox.entry(at.mailbox.clone()).or_default().push(at),
                None => found.gone.push(want.id),
            }
        }
        for (mailbox, wanted) in by_mailbox {
            let selected = match self.select(&mailbox, None).await {
                Ok(selected) => selected,
                // A server refuses to select a mailbox it no longer has,
                // which `ImapError::NoMailbox` reports as `NotFound`.
                Err(BackendError::NotFound) => {
                    found.gone.extend(wanted.iter().map(ToString::to_string));
                    continue;
                }
                Err(err) => return Err(err),
            };
            let role = self.role_of(&mailbox);
            let (current, stale): (Vec<Location>, Vec<Location>) = wanted
                .into_iter()
                .partition(|at| at.uidvalidity == selected.uidvalidity);
            found.gone.extend(stale.iter().map(ToString::to_string));
            for chunk in current.chunks(BATCH_LIMIT) {
                let uids = UidSet::from_uids(chunk.iter().map(|at| at.uid));
                let fetched = self.api.headers(&mailbox, &uids).await?;
                let by_uid: HashMap<u32, Fetched> =
                    fetched.into_iter().map(|f| (f.uid, f)).collect();
                for at in chunk {
                    match by_uid.get(&at.uid).filter(|f| !is_deleted(&f.flags)) {
                        Some(fetched) => {
                            let (meta, links) = meta_of(at, fetched, role);
                            found.links.insert(meta.id.clone(), links);
                            found.located.insert(meta.id.clone(), at.clone());
                            found.metas.push(meta);
                        }
                        None => found.gone.push(at.to_string()),
                    }
                }
            }
        }
        Ok(found)
    }

    /// Each thread by the location of its one message. The server keeps no
    /// threads, so the only thread it can name is one this adapter named
    /// for a lone message, as a search hit is; the engine reads the threads
    /// it made from the store.
    pub(super) async fn fetch_lone(&self, threads: Vec<String>) -> Result<Found, BackendError> {
        let found = self
            .fetch_metas(threads.into_iter().map(Want::message).collect())
            .await?;
        let whole = found.metas.iter().map(|m| vec![m.clone()]).collect();
        Ok(Found {
            metas: found.metas,
            gone: Vec::new(),
            whole,
            gone_threads: found.gone,
            links: found.links,
            located: found.located,
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::{window_keys, windows};

    /// Each window's first and last UID.
    fn bounds(from: u32, to: u32) -> Vec<(u32, u32)> {
        windows(from, to).map(|w| (*w.start(), *w.end())).collect()
    }

    #[test]
    fn windows_cover_the_span_in_ranges_of_fifty_thousand() {
        assert_eq!(
            bounds(1, 120_000),
            [(1, 50_000), (50_001, 100_000), (100_001, 120_000)]
        );
        assert_eq!(bounds(7, 7), [(7, 7)]);
        assert_eq!(bounds(0, 3), [(1, 3)]);
        assert!(bounds(5, 4).is_empty());
        assert!(bounds(1, 0).is_empty());
        assert_eq!(bounds(u32::MAX - 1, u32::MAX), [(u32::MAX - 1, u32::MAX)]);
    }

    #[test]
    fn the_window_is_the_whole_inbox_and_recent_mail_elsewhere() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 24).unwrap();
        assert_eq!(window_keys("INBOX", 30, today), "UNDELETED");
        assert_eq!(
            window_keys("Archive", 30, today),
            "SINCE 25-Aug-2026 UNDELETED"
        );
    }
}
