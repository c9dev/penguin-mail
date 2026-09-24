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
        let mut all: Vec<u32> = uids.into_iter().filter(|&u| u > 0).collect();
        all.sort_unstable();
        all.dedup();
        let mut set = UidSet::new();
        for uid in all {
            set.push(uid..=uid);
        }
        set
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

    /// Adds `from` to `to`, both included.
    pub fn insert(&mut self, from: u32, to: u32) {
        let (from, to) = (from.min(to).max(1), from.max(to));
        if to == 0 {
            return;
        }
        let mut ranges = std::mem::take(&mut self.ranges);
        ranges.push(from..=to);
        ranges.sort_unstable_by_key(|r| *r.start());
        for range in ranges {
            self.push(range);
        }
    }

    /// Appends `range`, which starts at or after the last range's start,
    /// merging it into the last range when they touch.
    fn push(&mut self, range: RangeInclusive<u32>) {
        if let Some(last) = self.ranges.last_mut()
            && *range.start() <= last.end().saturating_add(1)
        {
            let end = (*last.end()).max(*range.end());
            *last = *last.start()..=end;
            return;
        }
        self.ranges.push(range);
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn contains(&self, uid: u32) -> bool {
        self.ranges.iter().any(|r| r.contains(&uid))
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
}
