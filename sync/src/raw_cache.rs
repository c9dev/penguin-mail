//! The last small messages an account fetched whole. A raw message holds
//! every file inside it, so saving a file, drawing a picture and checking
//! a signature right after a small message opened read these bytes instead
//! of fetching the whole message again. A message at or over `RAW_LIMIT`
//! never enters: its files come one part at a time.

use std::collections::VecDeque;
use std::sync::Arc;

/// How many bytes of raw mail one account keeps in memory.
pub const RAW_CACHE_BYTES: usize = 16 << 20;

/// Raw messages by the server's id, least recently used first.
#[derive(Debug)]
pub struct RawCache {
    cap: usize,
    used: usize,
    entries: VecDeque<(String, Arc<Vec<u8>>)>,
}

impl RawCache {
    pub fn new(cap: usize) -> RawCache {
        RawCache {
            cap,
            used: 0,
            entries: VecDeque::new(),
        }
    }

    /// The message's bytes, which now count as the most recently used.
    pub fn get(&mut self, id: &str) -> Option<Arc<Vec<u8>>> {
        let at = self.entries.iter().position(|(key, _)| key == id)?;
        let entry = self.entries.remove(at)?;
        let bytes = Arc::clone(&entry.1);
        self.entries.push_back(entry);
        Some(bytes)
    }

    /// Keeps `bytes`, dropping the least recently used until they fit. A
    /// message bigger than the whole cache is not kept.
    pub fn put(&mut self, id: String, bytes: Arc<Vec<u8>>) {
        if bytes.len() > self.cap {
            return;
        }
        if let Some(at) = self.entries.iter().position(|(key, _)| *key == id)
            && let Some((_, old)) = self.entries.remove(at)
        {
            self.used -= old.len();
        }
        while self.used + bytes.len() > self.cap {
            let Some((_, old)) = self.entries.pop_front() else {
                break;
            };
            self.used -= old.len();
        }
        self.used += bytes.len();
        self.entries.push_back((id, bytes));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::RawCache;

    fn bytes(n: usize) -> Arc<Vec<u8>> {
        Arc::new(vec![0; n])
    }

    #[test]
    fn the_cache_keeps_to_its_cap_and_drops_the_oldest() {
        let mut cache = RawCache::new(10);
        cache.put("a".into(), bytes(4));
        cache.put("b".into(), bytes(4));
        assert!(cache.get("a").is_some());
        cache.put("c".into(), bytes(4));
        assert!(cache.get("b").is_none(), "b was the least recent");
        assert!(cache.get("a").is_some() && cache.get("c").is_some());
    }

    #[test]
    fn a_message_bigger_than_the_cache_is_not_kept() {
        let mut cache = RawCache::new(10);
        cache.put("a".into(), bytes(4));
        cache.put("big".into(), bytes(11));
        assert!(cache.get("big").is_none());
        assert!(cache.get("a").is_some());
    }
}
