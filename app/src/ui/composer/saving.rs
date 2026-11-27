//! The draft saves a composer has made, as the close dialog and the sender
//! see them. Save Draft while a save runs waits for that save and builds
//! on the draft it made, rather than making a second one. Discard deletes
//! every draft this composer made, whether the save was the autosave's or
//! the writer's, since the writer just said they did not want any of it.
//! A draft the composer reopened is not its own to delete.

use std::cell::{Cell, RefCell};

use futures::channel::oneshot;
use mailrs_domain::AccountId;

/// Drafts to delete from their accounts: an account and a draft id each.
pub type Gone = Vec<(AccountId, String)>;

/// The draft this composer holds in one account.
struct Kept {
    account: AccountId,
    id: String,
    /// Whether this composer made it, rather than reopened it.
    made: bool,
}

#[derive(Default)]
pub struct Saving {
    /// Who waits for the running save, or `None` while none runs.
    waiting: RefCell<Option<Vec<oneshot::Sender<Option<String>>>>>,
    /// The account the running save writes to.
    running: Cell<Option<AccountId>>,
    discarded: Cell<bool>,
    /// At most one draft per account: the message lives in one account at
    /// a time, and a save under another account moves it.
    kept: RefCell<Vec<Kept>>,
}

impl Saving {
    /// A save to `account` started.
    pub fn began(&self, account: AccountId) {
        *self.waiting.borrow_mut() = Some(Vec::new());
        self.running.set(Some(account));
    }

    /// The draft id the running save gets back, or `None` when it fails.
    /// `None` here when no save runs.
    pub fn wait(&self) -> Option<oneshot::Receiver<Option<String>>> {
        let mut waiting = self.waiting.borrow_mut();
        let (tell, heard) = oneshot::channel();
        waiting.as_mut()?.push(tell);
        Some(heard)
    }

    /// The draft this composer saved last in `account`, if any.
    pub fn latest(&self, account: AccountId) -> Option<String> {
        let kept = self.kept.borrow();
        kept.iter()
            .find(|k| k.account == account)
            .map(|k| k.id.clone())
    }

    /// The writer discarded the message. The drafts to delete now; the
    /// one a running save is writing is deleted when that save lands.
    pub fn discard(&self) -> Gone {
        self.discarded.set(true);
        let running = self.running.get();
        let mut gone = Gone::new();
        self.kept.borrow_mut().retain(|k| {
            let own = k.made && Some(k.account) != running;
            if own {
                gone.push((k.account, k.id.clone()));
            }
            !own
        });
        gone
    }

    /// The running save ended. `was` is the draft it started from, `saved`
    /// the one it got back, or `None` when it failed. Whoever waits hears
    /// `saved`. Returns the drafts to delete: the one just saved after
    /// Discard, and otherwise those the message left for this account.
    pub fn landed(&self, account: AccountId, was: Option<&str>, saved: Option<&str>) -> Gone {
        self.running.set(None);
        for tell in self.waiting.borrow_mut().take().unwrap_or_default() {
            let _ = tell.send(saved.map(str::to_string));
        }
        let Some(saved) = saved else {
            return Gone::new();
        };
        let mut kept = self.kept.borrow_mut();
        let made = match kept.iter_mut().find(|k| k.account == account) {
            Some(k) => {
                k.id = saved.to_string();
                k.made
            }
            None => {
                let made = was.is_none();
                kept.push(Kept {
                    account,
                    id: saved.to_string(),
                    made,
                });
                made
            }
        };
        if !self.discarded.get() {
            drop(kept);
            return self.left(account);
        }
        let mut gone = Gone::new();
        if made {
            kept.retain(|k| {
                let out = k.account == account;
                if out {
                    gone.push((k.account, k.id.clone()));
                }
                !out
            });
        }
        gone
    }

    /// The drafts this composer made in accounts other than `account`,
    /// which the message has left for it, taken off the list.
    pub fn left(&self, account: AccountId) -> Gone {
        let mut gone = Gone::new();
        self.kept.borrow_mut().retain(|k| {
            let out = k.account != account && k.made;
            if out {
                gone.push((k.account, k.id.clone()));
            }
            !out
        });
        gone
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: AccountId = 1;
    const B: AccountId = 2;

    fn gone(account: AccountId, id: &str) -> Gone {
        vec![(account, id.to_string())]
    }

    #[test]
    fn save_while_a_save_runs_hears_the_draft_it_made() {
        let saving = Saving::default();
        assert!(
            saving.wait().is_none(),
            "nothing runs, so nothing to wait for"
        );
        saving.began(A);
        let heard = saving.wait().expect("a save runs");
        assert!(saving.landed(A, None, Some("d1")).is_empty());
        assert_eq!(
            futures::executor::block_on(heard),
            Ok(Some("d1".to_string()))
        );
        assert!(saving.wait().is_none(), "the save is over");
        assert_eq!(saving.latest(A), Some("d1".to_string()));
    }

    #[test]
    fn a_failed_save_tells_its_waiters_so() {
        let saving = Saving::default();
        saving.began(A);
        let heard = saving.wait().expect("a save runs");
        assert!(saving.landed(A, None, None).is_empty());
        assert_eq!(futures::executor::block_on(heard), Ok(None));
        assert_eq!(saving.latest(A), None);
    }

    #[test]
    fn discard_during_a_save_deletes_the_draft_it_made() {
        let saving = Saving::default();
        saving.began(A);
        assert!(
            saving.discard().is_empty(),
            "the running save is not there yet"
        );
        assert_eq!(saving.landed(A, None, Some("d1")), gone(A, "d1"));
    }

    /// The autosave made d1, the writer typed on and closed with Discard.
    #[test]
    fn discard_deletes_an_autosaved_draft_that_no_save_is_running_on() {
        let saving = Saving::default();
        saving.began(A);
        assert!(saving.landed(A, None, Some("d1")).is_empty());
        assert_eq!(saving.discard(), gone(A, "d1"));
    }

    /// A later save replaced d1 with d2 (IMAP appends and erases): Discard
    /// deletes the one that is there now, and a second Discard has nothing.
    #[test]
    fn discard_deletes_the_latest_draft_of_a_chain_of_saves() {
        let saving = Saving::default();
        for (was, made) in [(None, "d1"), (Some("d1"), "d2")] {
            saving.began(A);
            assert!(saving.landed(A, was, Some(made)).is_empty());
        }
        assert_eq!(saving.discard(), gone(A, "d2"));
        assert!(saving.discard().is_empty());
    }

    #[test]
    fn discard_during_an_update_of_one_it_made_deletes_the_new_id() {
        let saving = Saving::default();
        saving.began(A);
        saving.landed(A, None, Some("d1"));
        saving.began(A);
        assert!(saving.discard().is_empty(), "d1 is being rewritten");
        assert_eq!(saving.landed(A, Some("d1"), Some("d2")), gone(A, "d2"));
    }

    /// A draft saved before stays: Discard drops the changes since, as it
    /// does with no save running.
    #[test]
    fn discard_keeps_the_draft_the_composer_reopened() {
        let saving = Saving::default();
        saving.began(A);
        saving.discard();
        assert!(saving.landed(A, Some("d0"), Some("d2")).is_empty());
        assert_eq!(saving.latest(A), Some("d2".to_string()));
        saving.began(A);
        assert!(
            saving.landed(A, None, None).is_empty(),
            "a failed save made nothing"
        );
    }

    #[test]
    fn a_save_in_another_account_moves_the_draft_it_made() {
        let saving = Saving::default();
        saving.began(A);
        saving.landed(A, None, Some("da"));
        saving.began(B);
        assert_eq!(saving.landed(B, None, Some("db")), gone(A, "da"));
        assert_eq!(saving.latest(A), None);
        assert_eq!(saving.latest(B), Some("db".to_string()));
    }

    #[test]
    fn a_reopened_draft_is_not_deleted_when_the_sender_changes() {
        let saving = Saving::default();
        saving.began(A);
        saving.landed(A, Some("d0"), Some("d0b"));
        saving.began(B);
        assert!(saving.landed(B, None, Some("db")).is_empty());
        assert_eq!(saving.latest(A), Some("d0b".to_string()));
    }

    /// Autosave in A, From switched to B, Send: the send deletes B's own
    /// draft itself, and the window deletes the one left in A.
    #[test]
    fn a_send_from_another_account_leaves_no_draft_in_the_first() {
        let saving = Saving::default();
        saving.began(A);
        saving.landed(A, None, Some("da"));
        assert_eq!(saving.left(B), gone(A, "da"));
        assert!(saving.left(B).is_empty(), "deleted once");
    }

    #[test]
    fn a_send_keeps_the_draft_in_its_own_account_for_the_send_to_delete() {
        let saving = Saving::default();
        saving.began(A);
        saving.landed(A, None, Some("da"));
        assert!(saving.left(A).is_empty());
        assert_eq!(saving.latest(A), Some("da".to_string()));
    }

    /// Switching back to A finds the draft saved there, not B's.
    #[test]
    fn each_account_keeps_its_own_latest_draft_id() {
        let saving = Saving::default();
        saving.began(A);
        saving.landed(A, Some("d0"), Some("d1"));
        assert_eq!(saving.latest(A), Some("d1".to_string()));
        assert_eq!(saving.latest(B), None);
    }
}
