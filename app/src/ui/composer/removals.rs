//! The styles of text the composer's buffer let go of, kept for Undo and
//! Redo to put back.
//!
//! GTK's undo history records the characters a change inserted or deleted
//! and none of their tags, so an Undo that brings deleted words back brings
//! them back plain. The editor keeps each stretch of deleted text here with
//! the tags it carried, and when Undo or Redo inserts the same characters
//! at the same place, it applies those tags again. GTK joins a run of
//! Backspace or Delete presses into one step, so a deletion next to the one
//! before it is also kept joined to it.

use std::collections::VecDeque;

/// How many stretches to keep. Older ones fall away, and an Undo that
/// reaches that far back styles the text as typing would.
const KEPT: usize = 128;

/// One stretch of deleted text: the offset it started at, its characters,
/// and the tags over each run of them, as a length in characters and the
/// tags that run carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal<T> {
    pub at: i32,
    pub text: String,
    pub runs: Vec<(i32, Vec<T>)>,
}

impl<T: Clone> Removal<T> {
    fn length(&self) -> i32 {
        self.text.chars().count() as i32
    }

    /// `self` followed by `after`, starting where `self` starts.
    fn then(&self, after: &Removal<T>) -> Removal<T> {
        Removal {
            at: self.at,
            text: format!("{}{}", self.text, after.text),
            runs: self.runs.iter().chain(&after.runs).cloned().collect(),
        }
    }
}

#[derive(Debug)]
pub struct Removals<T> {
    kept: VecDeque<Removal<T>>,
}

impl<T> Default for Removals<T> {
    fn default() -> Self {
        Removals {
            kept: VecDeque::new(),
        }
    }
}

impl<T: Clone> Removals<T> {
    /// Keeps `removal`, and the newest stretch joined to it when the two
    /// touch: a Backspace ends where the stretch before it starts, and a
    /// Delete starts where it starts.
    pub fn removed(&mut self, removal: Removal<T>) {
        let joined = self.kept.back().and_then(|newest| {
            if removal.at + removal.length() == newest.at {
                Some(removal.then(newest))
            } else if removal.at == newest.at {
                Some(newest.then(&removal))
            } else {
                None
            }
        });
        self.keep(removal);
        if let Some(joined) = joined {
            self.keep(joined);
        }
    }

    fn keep(&mut self, removal: Removal<T>) {
        if self.kept.len() == KEPT {
            self.kept.pop_front();
        }
        self.kept.push_back(removal);
    }

    /// The newest stretch that held `text` at `at`.
    pub fn find(&self, at: i32, text: &str) -> Option<&Removal<T>> {
        self.kept
            .iter()
            .rev()
            .find(|removal| removal.at == at && removal.text == text)
    }

    /// Forgets everything, for a body replaced in a way Undo cannot reach.
    pub fn clear(&mut self) {
        self.kept.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn removal(at: i32, text: &str, runs: &[(i32, &'static str)]) -> Removal<&'static str> {
        Removal {
            at,
            text: text.to_string(),
            runs: runs.iter().map(|(n, tag)| (*n, vec![*tag])).collect(),
        }
    }

    #[test]
    fn deleted_words_are_found_where_they_were() {
        let mut kept = Removals::default();
        kept.removed(removal(6, "words", &[(5, "bold")]));
        assert_eq!(
            kept.find(6, "words"),
            Some(&removal(6, "words", &[(5, "bold")]))
        );
    }

    #[test]
    fn the_same_words_elsewhere_are_not_found() {
        let mut kept = Removals::default();
        kept.removed(removal(6, "words", &[(5, "bold")]));
        assert_eq!(kept.find(7, "words"), None);
        assert_eq!(kept.find(6, "word"), None);
    }

    #[test]
    fn the_newest_deletion_of_the_same_words_wins() {
        let mut kept = Removals::default();
        kept.removed(removal(0, "hi", &[(2, "italic")]));
        kept.removed(removal(9, "x", &[(1, "plain")]));
        kept.removed(removal(0, "hi", &[(2, "bold")]));
        assert_eq!(
            kept.find(0, "hi").map(|r| r.runs.clone()),
            Some(vec![(2, vec!["bold"])])
        );
    }

    #[test]
    fn backspaces_in_a_row_are_found_as_one_stretch() {
        let mut kept = Removals::default();
        kept.removed(removal(8, "c", &[(1, "bold")]));
        kept.removed(removal(7, "b", &[(1, "plain")]));
        kept.removed(removal(6, "a", &[(1, "italic")]));
        let found = kept.find(6, "abc").expect("joined");
        assert_eq!(
            found.runs,
            vec![(1, vec!["italic"]), (1, vec!["plain"]), (1, vec!["bold"])]
        );
        // Each press on its own is still there, for a GTK that kept them
        // as separate steps.
        assert!(kept.find(7, "b").is_some());
    }

    #[test]
    fn deletes_in_a_row_are_found_as_one_stretch() {
        let mut kept = Removals::default();
        kept.removed(removal(3, "a", &[(1, "bold")]));
        kept.removed(removal(3, "b", &[(1, "plain")]));
        let found = kept.find(3, "ab").expect("joined");
        assert_eq!(found.runs, vec![(1, vec!["bold"]), (1, vec!["plain"])]);
    }

    #[test]
    fn deletions_apart_are_not_joined() {
        let mut kept = Removals::default();
        kept.removed(removal(0, "a", &[]));
        kept.removed(removal(5, "b", &[]));
        assert_eq!(kept.find(0, "ab"), None);
        assert_eq!(kept.find(5, "ab"), None);
    }

    #[test]
    fn old_stretches_fall_away() {
        let mut kept = Removals::default();
        kept.removed(removal(0, "first", &[]));
        for at in 0..KEPT as i32 {
            kept.removed(removal(100 + at * 10, "x", &[]));
        }
        assert_eq!(kept.find(0, "first"), None);
        assert!(kept.find(100 + (KEPT as i32 - 1) * 10, "x").is_some());
    }

    #[test]
    fn clearing_forgets_everything() {
        let mut kept = Removals::default();
        kept.removed(removal(0, "a", &[]));
        kept.clear();
        assert_eq!(kept.find(0, "a"), None);
    }
}
