//! A set of UIDs in the form IMAP commands take them: `1:3,5,9:*`.

use std::fmt;
use std::ops::RangeInclusive;

/// UIDs in one mailbox, kept as sorted ranges that neither touch nor
/// overlap, so the printed form stays as short as IMAP allows. A set that
/// runs to `u32::MAX` prints its end as `*`, IMAP's "the highest UID in
/// the mailbox".
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct UidSet {
    ranges: Vec<RangeInclusive<u32>>,
}

impl UidSet {
    pub fn new() -> Self {
        UidSet::default()
    }

    /// The set of `uids`, in any order, repeats allowed. UID 0 does not
    /// exist in IMAP and is left out.
    pub fn from_uids(uids: impl IntoIterator<Item = u32>) -> Self {
        UidSet::from_ranges(uids.into_iter().map(|uid| uid..=uid))
    }

    /// The set of `ranges`, in any order, overlapping or not, each read
    /// either way round. It sorts once and merges once, so a server's
    /// VANISHED of 100,000 ranges builds in about 0.1 s in a debug build.
    pub fn from_ranges(ranges: impl IntoIterator<Item = RangeInclusive<u32>>) -> Self {
        UidSet::from(ranges.into_iter().collect::<Vec<_>>())
    }

    /// `from` to `to`, both included.
    pub fn range(from: u32, to: u32) -> Self {
        let mut set = UidSet::new();
        set.insert(from, to);
        set
    }

    /// Every UID from `first` up, printed `first:*`. A server answers
    /// `first:*` with the highest message even when every UID is below
    /// `first`, so a caller filters what comes back through
    /// [`UidSet::contains`].
    pub fn from_uid(first: u32) -> Self {
        UidSet::range(first, u32::MAX)
    }

    /// Adds `from` to `to`, both included, merging it with the ranges it
    /// touches, found by binary search.
    pub fn insert(&mut self, from: u32, to: u32) {
        let Some(range) = normal(from, to) else {
            return;
        };
        let (from, to) = (*range.start(), *range.end());
        // The first range that ends at or after the UID before `from`, and
        // the first that starts after the UID after `to`: everything
        // between touches the new range.
        let first = self
            .ranges
            .partition_point(|r| r.end().saturating_add(1) < from);
        let last = self
            .ranges
            .partition_point(|r| *r.start() <= to.saturating_add(1));
        let merged = match self.ranges.get(first..last) {
            Some([head, .., tail]) | Some([head @ tail]) => {
                (*head.start()).min(from)..=(*tail.end()).max(to)
            }
            _ => from..=to,
        };
        self.ranges.splice(first..last, [merged]);
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn contains(&self, uid: u32) -> bool {
        let at = self.ranges.partition_point(|r| *r.end() < uid);
        self.ranges.get(at).is_some_and(|r| *r.start() <= uid)
    }

    /// How many UIDs the set names, counting an open end up to `u32::MAX`.
    pub fn len(&self) -> u64 {
        self.ranges
            .iter()
            .map(|r| u64::from(*r.end()) - u64::from(*r.start()) + 1)
            .sum()
    }

    /// The ranges, lowest first.
    pub fn ranges(&self) -> &[RangeInclusive<u32>] {
        &self.ranges
    }

    /// Every UID in the set, lowest first. Walks an open end to
    /// `u32::MAX`, so call it on sets built from UIDs a server or the
    /// store named, never on one from [`UidSet::from_uid`].
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.ranges.iter().flat_map(|r| r.clone())
    }
}

/// `from` to `to` the right way round, without UID 0, which IMAP does not
/// have; `None` when nothing is left.
fn normal(from: u32, to: u32) -> Option<RangeInclusive<u32>> {
    let (from, to) = (from.min(to).max(1), from.max(to));
    (to > 0).then_some(from..=to)
}

/// The set of an owned list of ranges, in any order, each read either way
/// round. The list is sorted and merged in place and its spare capacity
/// given back, so building a set of millions of ranges holds that one
/// list and nothing beside it.
impl From<Vec<RangeInclusive<u32>>> for UidSet {
    fn from(mut ranges: Vec<RangeInclusive<u32>>) -> Self {
        ranges.retain_mut(|range| match normal(*range.start(), *range.end()) {
            Some(normal) => {
                *range = normal;
                true
            }
            None => false,
        });
        ranges.sort_unstable_by_key(|range| *range.start());
        // Each range either extends the last one kept or becomes the next
        // kept one; `kept` never passes the range being read.
        let mut kept = 0;
        for i in 0..ranges.len() {
            let range = ranges[i].clone();
            if kept > 0 && *range.start() <= ranges[kept - 1].end().saturating_add(1) {
                let last = &mut ranges[kept - 1];
                let end = (*last.end()).max(*range.end());
                *last = *last.start()..=end;
            } else {
                ranges[kept] = range;
                kept += 1;
            }
        }
        ranges.truncate(kept);
        ranges.shrink_to_fit();
        UidSet { ranges }
    }
}

impl fmt::Display for UidSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, range) in self.ranges.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            let end = match *range.end() {
                u32::MAX => "*".to_string(),
                end => end.to_string(),
            };
            match range.start() == range.end() {
                true => f.write_str(&end)?,
                false => write!(f, "{}:{end}", range.start())?,
            }
        }
        Ok(())
    }
}

impl FromIterator<u32> for UidSet {
    fn from_iter<I: IntoIterator<Item = u32>>(iter: I) -> Self {
        UidSet::from_uids(iter)
    }
}

#[cfg(test)]
mod tests {
    use super::UidSet;

    #[test]
    fn neighboring_uids_print_as_one_range() {
        let set = UidSet::from_uids([5, 1, 2, 3, 9, 10, 11, 7]);
        assert_eq!(set.to_string(), "1:3,5,7,9:11");
    }

    #[test]
    fn repeats_and_zero_leave_no_trace() {
        let set = UidSet::from_uids([0, 4, 4, 4]);
        assert_eq!(set.to_string(), "4");
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn an_open_end_prints_as_a_star() {
        assert_eq!(UidSet::from_uid(4392).to_string(), "4392:*");
        assert!(UidSet::from_uid(4392).contains(u32::MAX));
        assert!(!UidSet::from_uid(4392).contains(4391));
    }

    #[test]
    fn overlapping_ranges_merge() {
        let mut set = UidSet::range(10, 20);
        set.insert(15, 30);
        set.insert(1, 9);
        set.insert(40, 40);
        assert_eq!(set.to_string(), "1:30,40");
        assert_eq!(set.len(), 31);
    }

    #[test]
    fn a_reversed_range_reads_the_same_as_the_right_way_round() {
        assert_eq!(UidSet::range(9, 3), UidSet::range(3, 9));
    }

    #[test]
    fn an_empty_set_prints_nothing() {
        assert!(UidSet::new().is_empty());
        assert_eq!(UidSet::new().to_string(), "");
        assert_eq!(UidSet::from_uids([]).len(), 0);
    }

    #[test]
    fn iter_walks_every_uid_in_order() {
        let set = UidSet::from_uids([7, 3, 4]);
        assert_eq!(set.iter().collect::<Vec<_>>(), vec![3, 4, 7]);
    }

    /// Inserting in any order gives the set a plain list of UIDs gives.
    #[test]
    fn inserts_in_any_order_match_the_uids_they_name() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = |n: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % u64::from(n)) as u32
        };
        for _ in 0..200 {
            let mut set = UidSet::new();
            let mut uids = std::collections::BTreeSet::new();
            for _ in 0..next(20) {
                let from = next(60);
                let to = from + next(6);
                set.insert(from, to);
                uids.extend((from..=to).filter(|&u| u > 0));
            }
            assert_eq!(set, UidSet::from_uids(uids.iter().copied()));
            for uid in 0..70 {
                assert_eq!(set.contains(uid), uids.contains(&uid), "{set} {uid}");
            }
        }
    }

    #[test]
    fn ranges_in_any_order_build_one_merged_set() {
        // A server may name a range either way round.
        let backwards = std::ops::RangeInclusive::new(30, 25);
        let set = UidSet::from_ranges([9..=12, 1..=3, 4..=4, 11..=20, 0..=0, backwards]);
        assert_eq!(set.to_string(), "1:4,9:20,25:30");
    }

    /// A set built from an owned list sorts and merges it in that list,
    /// so the peak is the list itself, and gives the spare capacity back.
    #[test]
    fn a_range_list_becomes_a_set_in_place_with_no_spare_capacity() {
        let backwards = std::ops::RangeInclusive::new(30, 25);
        let mut ranges = Vec::with_capacity(1_000);
        ranges.extend([9..=12, 1..=3, 4..=4, 11..=20, 0..=0, backwards]);
        let set = UidSet::from(ranges);
        assert_eq!(set.to_string(), "1:4,9:20,25:30");
        assert_eq!(set.ranges.capacity(), 3);
        let big = UidSet::from_ranges((0..100_000u32).map(|i| i * 3..=i * 3));
        assert_eq!(big.ranges.capacity(), big.ranges.len());
    }

    /// A set of 100,000 ranges builds and answers in under a second
    /// in a debug build; a sort per insert took minutes. Inserting in
    /// rising order, as a sync adds new mail, appends at the end.
    #[test]
    fn a_hundred_thousand_ranges_build_and_answer_fast() {
        let started = std::time::Instant::now();
        let set = UidSet::from_ranges((0..100_000u32).rev().map(|i| i * 3 + 1..=i * 3 + 1));
        let mut inserted = UidSet::new();
        for i in 0..100_000u32 {
            inserted.insert(i * 3 + 1, i * 3 + 1);
        }
        assert_eq!(set, inserted);
        assert_eq!(set.ranges().len(), 100_000);
        assert_eq!((0..300_000).filter(|&u| set.contains(u)).count(), 100_000);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }
}
