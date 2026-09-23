//! What the engine said about signed messages this run, so opening one
//! again draws its card without fetching the raw message or starting gpg.
//!
//! Only a message that arrived in the clear is kept. Its body is the part
//! the signature covers, which Gmail holds too; what came out of an
//! encrypted message never outlives the window it was opened in. The
//! answers live in memory and go with the process.

use std::collections::HashMap;
use std::time::SystemTime;

use super::Read;

/// How many answers to keep before starting again. A reader who opens more
/// signed mail than this in one run pays for gpg again, which is all a
/// miss costs.
const KEPT: usize = 512;

/// The answers, by message id, each with the keyring it was read against.
#[derive(Default)]
pub struct Verdicts {
    kept: HashMap<String, (SystemTime, Read)>,
}

impl Verdicts {
    /// The answer kept for `message_id`, while the keyring is the one it
    /// was read against. A key imported, revoked or vouched for since
    /// changes what the card should say, and the keyring's time changes
    /// with it.
    pub fn get(&self, message_id: &str, keyring: Option<SystemTime>) -> Option<Read> {
        let (read_against, read) = self.kept.get(message_id)?;
        (Some(*read_against) == keyring).then(|| read.clone())
    }

    /// Keeps `read` for `message_id`, unless the message arrived encrypted,
    /// the engine gave no answer about it (a failure can pass, and asking
    /// again is how to find out), nobody could check a certificate's
    /// revocation (the authority may answer next time), or there is no
    /// keyring time to check it against later.
    pub fn keep(&mut self, message_id: String, keyring: Option<SystemTime>, read: &Read) {
        let Some(keyring) = keyring else {
            return;
        };
        if read.sealed || read.body.is_none() || read.revocation_unchecked {
            return;
        }
        if self.kept.len() >= KEPT {
            self.kept.clear();
        }
        self.kept.insert(message_id, (keyring, read.clone()));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::protection::{Mark, Tone};

    fn read(sealed: bool) -> Read {
        Read {
            mark: Mark {
                title: "Signed by Ann".into(),
                detail: None,
                tone: Tone::Good,
            },
            body: Some(mailrs_domain::MessageBody::default()),
            files: Vec::new(),
            sealed,
            revocation_unchecked: false,
        }
    }

    #[test]
    fn a_signed_message_is_remembered_while_the_keyring_stays_put() {
        let mut verdicts = Verdicts::default();
        let then = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        verdicts.keep("m1".into(), Some(then), &read(false));

        assert!(verdicts.get("m1", Some(then)).is_some());
        let later = then + Duration::from_secs(1);
        assert!(
            verdicts.get("m1", Some(later)).is_none(),
            "a changed keyring asks again"
        );
        assert!(verdicts.get("m2", Some(then)).is_none());
    }

    #[test]
    fn what_came_out_of_an_encrypted_message_is_never_kept() {
        let mut verdicts = Verdicts::default();
        let then = SystemTime::UNIX_EPOCH;
        verdicts.keep("m1".into(), Some(then), &read(true));
        assert!(verdicts.get("m1", Some(then)).is_none());
    }

    #[test]
    fn an_engine_that_could_not_answer_is_asked_again() {
        let mut verdicts = Verdicts::default();
        let then = SystemTime::UNIX_EPOCH;
        let refused = Read {
            body: None,
            ..read(false)
        };
        verdicts.keep("m1".into(), Some(then), &refused);
        assert!(verdicts.get("m1", Some(then)).is_none());
    }

    #[test]
    fn a_revocation_nobody_could_check_is_not_kept() {
        let mut verdicts = Verdicts::default();
        let then = SystemTime::UNIX_EPOCH;
        let unchecked = Read {
            revocation_unchecked: true,
            ..read(false)
        };
        verdicts.keep("m1".into(), Some(then), &unchecked);
        assert!(verdicts.get("m1", Some(then)).is_none());
    }

    #[test]
    fn a_keyring_with_no_time_keeps_nothing() {
        let mut verdicts = Verdicts::default();
        verdicts.keep("m1".into(), None, &read(false));
        assert!(verdicts.get("m1", None).is_none());
    }
}
