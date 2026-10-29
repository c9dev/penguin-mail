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
/// Bodies and files share one budget, regardless of message count.
const KEPT_BYTES: usize = 8 << 20;

/// The answers, by message id, each with the keyring it was read against.
#[derive(Default)]
pub struct Verdicts {
    kept: HashMap<String, (SystemTime, Read)>,
    used: usize,
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
        if let Some((_, previous)) = self.kept.remove(&message_id) {
            self.used -= bytes(&message_id, &previous);
        }
        let size = bytes(&message_id, read);
        if size > KEPT_BYTES {
            return;
        }
        if self.kept.len() >= KEPT || self.used + size > KEPT_BYTES {
            self.kept.clear();
            self.used = 0;
        }
        self.used += size;
        self.kept.insert(message_id, (keyring, read.clone()));
    }
}

/// What cloning an answer retains, including the body and decoded files.
/// The entry count bounds the map's bookkeeping beside these bytes.
fn bytes(id: &str, read: &Read) -> usize {
    let text = |value: &Option<String>| value.as_ref().map_or(0, String::len);
    let mut size = std::mem::size_of::<(String, SystemTime, Read)>()
        + id.len()
        + read.mark.title.len()
        + text(&read.mark.detail)
        + read.files.len() * std::mem::size_of::<Vec<u8>>()
        + read.files.iter().map(Vec::len).sum::<usize>();
    if let Some(body) = &read.body {
        size += text(&body.html)
            + text(&body.text)
            + text(&body.calendar)
            + text(&body.list_unsubscribe)
            + text(&body.receipt_to)
            + text(&body.provenance.mailed_by)
            + text(&body.provenance.signed_by)
            + body.attachments.len() * std::mem::size_of::<mailrs_domain::Attachment>();
        for file in &body.attachments {
            size += file.part_id.len()
                + file.filename.len()
                + file.mime_type.len()
                + text(&file.attachment_id)
                + text(&file.content_id);
        }
        if let Some(report) = &body.read_report {
            size += report.by.len() + text(&report.original);
        }
    }
    size
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
    #[test]
    fn signed_bodies_and_files_share_one_memory_budget() {
        let mut verdicts = Verdicts::default();
        let mut answer = read(false);
        answer.body.as_mut().unwrap().html = Some("x".repeat(2 << 20));
        answer.files = vec![vec![1; 2 << 20]];
        for n in 0..4 {
            verdicts.keep(format!("m{n}"), Some(SystemTime::UNIX_EPOCH), &answer);
            let payload: usize = verdicts
                .kept
                .values()
                .map(|(_, answer)| {
                    answer.body.as_ref().unwrap().html.as_ref().unwrap().len()
                        + answer.files.iter().map(Vec::len).sum::<usize>()
                })
                .sum();
            assert!(payload <= 8 << 20, "cached {payload} bytes of signed mail");
        }
        assert!(verdicts.get("m3", Some(SystemTime::UNIX_EPOCH)).is_some());
    }

    #[test]
    fn replacing_an_answer_releases_its_previous_budget() {
        let mut verdicts = Verdicts::default();
        let mut answer = read(false);
        answer.files = vec![vec![1; 3 << 20]];
        let keyring = Some(SystemTime::UNIX_EPOCH);
        verdicts.keep("m1".into(), keyring, &answer);
        verdicts.keep("m2".into(), keyring, &answer);
        answer.files[0].push(2);
        verdicts.keep("m1".into(), keyring, &answer);
        assert!(verdicts.get("m2", keyring).is_some());
        assert_eq!(
            verdicts.get("m1", keyring).unwrap().files[0].len(),
            (3 << 20) + 1
        );
    }

    #[test]
    fn a_large_signed_message_can_be_read_without_being_cached() {
        let mut verdicts = Verdicts::default();
        let mut answer = read(false);
        answer.files = vec![vec![1; 9 << 20]];
        verdicts.keep("large".into(), Some(SystemTime::UNIX_EPOCH), &answer);
        assert!(
            verdicts
                .get("large", Some(SystemTime::UNIX_EPOCH))
                .is_none()
        );
        assert_eq!(
            answer.files[0].len(),
            9 << 20,
            "the open message keeps its file"
        );
    }
}
