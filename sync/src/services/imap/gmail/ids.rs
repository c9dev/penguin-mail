//! Where a Gmail message is. The store names it by its hex X-GM-MSGID,
//! which survives a move; IMAP names it by mailbox and UID, which does
//! not. This keeps what the adapter has met and finds the rest by search.

use std::collections::{HashMap, VecDeque};

use mailrs_domain::Location;
use mailrs_imap::{FlagsOf, UidSet, gm_msgid};

use super::Imap;
use crate::BackendError;
use crate::services::imap::{ImapApi, Submit};

/// The most places [`Places`] remembers, the oldest dropped first.
const PLACES_KEPT: usize = 4096;

/// Where the messages this session met were last seen, by hex id.
#[derive(Default)]
pub(in crate::services::imap) struct Places {
    at: HashMap<String, Location>,
    order: VecDeque<String>,
}

impl Places {
    pub fn get(&self, id: &str) -> Option<Location> {
        self.at.get(id).cloned()
    }

    pub fn put(&mut self, id: String, at: Location) {
        if self.at.insert(id.clone(), at).is_none() {
            self.order.push_back(id);
            if self.order.len() > PLACES_KEPT
                && let Some(oldest) = self.order.pop_front()
            {
                self.at.remove(&oldest);
            }
        }
    }

    pub fn forget(&mut self, id: &str) {
        if self.at.remove(id).is_some() {
            self.order.retain(|kept| kept != id);
        }
    }
}

/// The most ids one search names.
const IDS_PER_SEARCH: usize = 100;

/// The most UIDs one flags fetch names.
const UIDS_PER_FETCH: usize = 5_000;

/// The number a hex id stands for, `None` for text that is not one.
pub(in crate::services::imap) fn msgid_of(id: &str) -> Option<u64> {
    (!id.is_empty() && id.len() <= 16 && !id.starts_with('-'))
        .then(|| u64::from_str_radix(id, 16).ok())
        .flatten()
}

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// Each of `uids` in `mailbox` (under `uidvalidity`) with the ids and
    /// labels Gmail gives it. The places are remembered. A message marked
    /// `\Deleted` is left out.
    pub(in crate::services::imap) async fn gm_pair(
        &self,
        mailbox: &str,
        uidvalidity: u32,
        uids: &[u32],
    ) -> Result<Vec<FlagsOf>, BackendError> {
        let mut paired = Vec::with_capacity(uids.len());
        for chunk in uids.chunks(UIDS_PER_FETCH) {
            let set = UidSet::from_uids(chunk.iter().copied());
            for flags in self.api.gmail_flags(mailbox, &set, None).await? {
                let deleted = flags
                    .flags
                    .iter()
                    .any(|f| f.eq_ignore_ascii_case("\\Deleted"));
                if let Some(meta) = flags.gmail.as_ref().filter(|_| !deleted) {
                    self.known().places.put(
                        meta.id(),
                        Location {
                            mailbox: mailbox.to_string(),
                            uidvalidity,
                            uid: flags.uid,
                        },
                    );
                    paired.push(flags);
                }
            }
        }
        Ok(paired)
    }

    /// Where each of the hex `ids` is now, found by one search a batch in
    /// All Mail, then Trash, then Spam. An id nobody holds is left out.
    pub(in crate::services::imap) async fn gm_find(
        &self,
        ids: &[String],
    ) -> Result<HashMap<String, Location>, BackendError> {
        let boxes = self.gm_synced().await?;
        let mut left: Vec<String> = ids.to_vec();
        let mut found = HashMap::new();
        for mailbox in [&boxes.all, &boxes.trash, &boxes.spam] {
            if left.is_empty() {
                break;
            }
            let selected = self.select(mailbox, None).await?;
            for batch in left.clone().chunks(IDS_PER_SEARCH) {
                let numbers: Vec<u64> = batch.iter().filter_map(|id| msgid_of(id)).collect();
                if numbers.is_empty() {
                    continue;
                }
                let keys = format!("{} UNDELETED", gm_msgid(&numbers));
                let uids = self.api.search(mailbox, &keys).await?;
                for flags in self.gm_pair(mailbox, selected.uidvalidity, &uids).await? {
                    if let Some(meta) = &flags.gmail {
                        let id = meta.id();
                        if let Some(at) = self.known().places.get(&id) {
                            found.insert(id, at);
                        }
                    }
                }
            }
            left.retain(|id| !found.contains_key(id));
        }
        Ok(found)
    }

    /// The place of `name`, a location as the plain dialect names mail or a
    /// hex id as the Gmail API does. `NotFound` for a message nobody holds.
    pub(in crate::services::imap) async fn gm_place(
        &self,
        name: &str,
    ) -> Result<Location, BackendError> {
        if let Some(at) = Location::parse(name) {
            return Ok(at);
        }
        if let Some(at) = self.known().places.get(name) {
            return Ok(at);
        }
        self.gm_find(&[name.to_string()])
            .await?
            .remove(name)
            .ok_or(BackendError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(uid: u32) -> Location {
        Location {
            mailbox: "[Gmail]/All Mail".into(),
            uidvalidity: 1,
            uid,
        }
    }

    #[test]
    fn places_drop_the_oldest_past_the_cap() {
        let mut places = Places::default();
        for n in 0..=PLACES_KEPT {
            places.put(format!("{n:x}"), at(n as u32 + 1));
        }
        assert_eq!(places.get("0"), None);
        assert_eq!(places.get("1"), Some(at(2)));
        assert_eq!(places.at.len(), PLACES_KEPT);
    }

    #[test]
    fn a_forgotten_place_leaves_the_queue_too() {
        let mut places = Places::default();
        places.put("a".into(), at(1));
        places.forget("a");
        places.put("a".into(), at(2));
        assert_eq!(places.order.len(), 1);
        assert_eq!(places.get("a"), Some(at(2)));
    }

    #[test]
    fn a_hex_id_reads_to_its_number_and_other_text_to_none() {
        assert_eq!(msgid_of("11bdf8bba45a4d91"), Some(0x11bd_f8bb_a45a_4d91));
        assert_eq!(msgid_of("INBOX/1/2"), None);
        assert_eq!(msgid_of(""), None);
        assert_eq!(msgid_of("fffffffffffffffff"), None);
    }
}
