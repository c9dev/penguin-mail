//! The one splice that turns an old list into a new one, so a list model
//! can update in place and keep its selection and scroll position.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Splice {
    pub position: u32,
    pub removed: u32,
    /// The replacement items, as a range of the new list.
    pub added: Range<usize>,
}

/// `None` when the lists are equal. Otherwise keeps the longest common
/// prefix and suffix and replaces what lies between.
pub fn splice<T: PartialEq>(old: &[T], new: &[T]) -> Option<Splice> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    if prefix == old.len() && prefix == new.len() {
        return None;
    }
    let room = old.len().min(new.len()) - prefix;
    let suffix = old.iter().rev().zip(new.iter().rev()).take(room).take_while(|(a, b)| a == b).count();
    Some(Splice {
        position: prefix as u32,
        removed: (old.len() - prefix - suffix) as u32,
        added: prefix..new.len() - suffix,
    })
}

#[cfg(test)]
mod tests {
    use super::{Splice, splice};

    #[test]
    fn equal_lists_need_nothing() {
        assert_eq!(splice(&[1, 2, 3], &[1, 2, 3]), None);
        assert_eq!(splice::<i32>(&[], &[]), None);
    }

    #[test]
    fn new_mail_at_the_top_is_one_insert() {
        assert_eq!(splice(&[2, 3], &[1, 2, 3]), Some(Splice { position: 0, removed: 0, added: 0..1 }));
    }

    #[test]
    fn a_removed_row_is_one_removal() {
        assert_eq!(splice(&[1, 2, 3], &[1, 3]), Some(Splice { position: 1, removed: 1, added: 1..1 }));
    }

    #[test]
    fn a_changed_row_is_replaced_alone() {
        assert_eq!(splice(&[1, 2, 3], &[1, 9, 3]), Some(Splice { position: 1, removed: 1, added: 1..2 }));
    }

    #[test]
    fn repeated_values_do_not_overlap_prefix_and_suffix() {
        assert_eq!(splice(&[1, 1], &[1]), Some(Splice { position: 1, removed: 1, added: 1..1 }));
        assert_eq!(splice(&[1], &[1, 1]), Some(Splice { position: 1, removed: 0, added: 1..2 }));
    }

    #[test]
    fn unrelated_lists_are_replaced_whole() {
        assert_eq!(splice(&[1, 2], &[3, 4, 5]), Some(Splice { position: 0, removed: 2, added: 0..3 }));
    }
}
