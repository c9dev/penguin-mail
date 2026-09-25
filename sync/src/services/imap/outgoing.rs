//! Sending over SMTP and keeping drafts in the Drafts mailbox. A message
//! goes from the address in its From header, the identity the person
//! picked, to every address in To, Cc and Bcc; the Bcc header itself
//! leaves only in the Sent copy. A draft is a message in Drafts flagged
//! `\Draft`. Saving it again appends the new copy and then erases the
//! old, and the draft's id is the store's id for its message.

use mailrs_domain::translate::gettext;
use mailrs_domain::{Location, Role};
use mailrs_imap::UidSet;
use mailrs_mime::address::parse_address_list;

use super::syntax::string;
use super::{Imap, ImapApi, Submit};
use crate::BackendError;
use crate::api::{DraftRef, SavedDraft};
use crate::services::MailBackend;

const DRAFT: &str = "\\Draft";
const SEEN: &str = "\\Seen";
const DELETED: &str = "\\Deleted";

/// Where the header block of `raw` ends, after its blank line.
fn header_end(raw: &[u8]) -> usize {
    raw.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .or_else(|| raw.windows(2).position(|w| w == b"\n\n").map(|p| p + 2))
        .unwrap_or(raw.len())
}

/// `raw` without its Bcc header and the lines folded under it: the blind
/// copies' addresses must not reach the other recipients. The envelope
/// still carries them.
pub(super) fn without_bcc(raw: &[u8]) -> Vec<u8> {
    let (head, body) = raw.split_at(header_end(raw));
    let mut out = Vec::with_capacity(raw.len());
    let mut skipping = false;
    for line in head.split_inclusive(|b| *b == b'\n') {
        let folded = line.first().is_some_and(|b| *b == b' ' || *b == b'\t');
        if !folded {
            skipping = line.len() >= 4 && line[..4].eq_ignore_ascii_case(b"bcc:");
        }
        if !skipping {
            out.extend_from_slice(line);
        }
    }
    out.extend_from_slice(body);
    out
}

/// The Message-ID of `raw` without its angle brackets.
fn message_id_of(raw: &[u8]) -> Option<String> {
    mailrs_mime::parts(raw).and_then(|parts| {
        parts
            .header("Message-ID")
            .map(|id| id.trim().trim_matches(['<', '>']).to_string())
    })
}

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// Hands `raw` to the SMTP server and answers its Message-ID, which
    /// is all the server says about it.
    pub(super) async fn submit(&self, raw: &[u8]) -> Result<String, BackendError> {
        let parts = mailrs_mime::parts(raw).unwrap_or_default();
        let from = parts
            .header("From")
            .map(parse_address_list)
            .unwrap_or_default()
            .into_iter()
            .next()
            .map(|a| a.email)
            .unwrap_or_else(|| self.settings.address.clone());
        let mut to: Vec<String> = parts
            .headers
            .iter()
            .filter(|(name, _)| ["To", "Cc", "Bcc"].iter().any(|h| name.eq_ignore_ascii_case(h)))
            .flat_map(|(_, value)| parse_address_list(value))
            .map(|a| a.email)
            .collect();
        to.sort();
        to.dedup();
        if to.is_empty() {
            return Err(BackendError::Refused(gettext(
                "The message names nobody to send it to.",
            )));
        }
        self.smtp.submit(&from, &to, &without_bcc(raw)).await?;
        Ok(parts
            .header("Message-ID")
            .map(|id| id.trim().to_string())
            .unwrap_or_default())
    }

    /// Files `raw` in `mailbox` with `flags` and answers the copy's name:
    /// from APPENDUID, or without UIDPLUS the newest message there carrying
    /// its Message-ID.
    pub(super) async fn file(
        &self,
        raw: &[u8],
        mailbox: &str,
        flags: &[String],
    ) -> Result<String, BackendError> {
        if let Some(appended) = self.api.append(mailbox, flags, raw).await? {
            // Dovecot's first APPEND into a listed mailbox it has not made
            // yet answers a UIDVALIDITY one above what SELECT reports after,
            // and SELECT's is the one every later call checks. On a
            // mismatch the Message-ID search below names the copy.
            let uidvalidity = self.select(mailbox, None).await?.uidvalidity;
            if uidvalidity == appended.uidvalidity {
                return Ok(Location {
                    mailbox: mailbox.to_string(),
                    uidvalidity,
                    uid: appended.uid,
                }
                .to_string());
            }
        }
        let message_id = message_id_of(raw).ok_or(BackendError::NotFound)?;
        self.newest_with(mailbox, &message_id)
            .await?
            .ok_or(BackendError::NotFound)
    }

    /// The name of the newest message in `mailbox` whose Message-ID is
    /// `message_id`, given without angle brackets.
    async fn newest_with(&self, mailbox: &str, message_id: &str) -> Result<Option<String>, BackendError> {
        let uidvalidity = self.select(mailbox, None).await?.uidvalidity;
        let keys = format!("HEADER Message-ID {} UNDELETED", string(message_id));
        Ok(self
            .api
            .search(mailbox, &keys)
            .await?
            .into_iter()
            .max()
            .map(|uid| {
                Location {
                    mailbox: mailbox.to_string(),
                    uidvalidity,
                    uid,
                }
                .to_string()
            }))
    }

    /// Erases the message `name` locates: `\Deleted`, and `UID EXPUNGE`
    /// where the server has UIDPLUS. Without it the server expunges when
    /// it will, and the window never lists a message marked deleted.
    pub(super) async fn erase(&self, name: &str) -> Result<(), BackendError> {
        let at = self.current(name).await?;
        let uids = UidSet::from_uids([at.uid]);
        self.api
            .store(&at.mailbox, &uids, true, &[DELETED.to_string()])
            .await?;
        if self.capabilities_now().await?.uidplus {
            self.api.expunge(&at.mailbox, &uids).await?;
        }
        Ok(())
    }

    async fn drafts_mailbox(&self) -> Result<String, BackendError> {
        self.ensure_listed().await?;
        self.mailbox_for(Role::Drafts).ok_or(BackendError::Unsupported)
    }

    /// Saves `raw` as a draft, replacing the one `draft_id` names.
    pub(super) async fn save(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SavedDraft, BackendError> {
        let drafts = self.drafts_mailbox().await?;
        let saved = self
            .file(raw, &drafts, &[DRAFT.to_string(), SEEN.to_string()])
            .await?;
        // The new copy is on the server, so an old one left behind is a
        // second draft, never a lost one.
        if let Some(old) = draft_id
            && let Err(err) = self.erase(old).await
            && !matches!(err, BackendError::NotFound)
        {
            tracing::warn!(%err, "saved the draft, but could not remove the copy it replaces");
        }
        Ok(SavedDraft {
            draft_id: saved.clone(),
            message_id: saved.clone(),
            thread_id: thread_id.map_or(saved, str::to_string),
        })
    }

    /// Sends the draft as the server holds it, then erases it.
    pub(super) async fn send_saved(&self, draft_id: &str) -> Result<String, BackendError> {
        let raw = self
            .raw_messages(&[draft_id.to_string()])
            .await?
            .into_iter()
            .next()
            .ok_or(BackendError::NotFound)?
            .bytes;
        let sent = self.submit(&raw).await?;
        if let Err(err) = self.erase(draft_id).await {
            tracing::warn!(%err, "sent the draft, but could not remove it from Drafts");
        }
        Ok(sent)
    }

    /// Every draft in the Drafts mailbox, each named by its location.
    pub(super) async fn drafts(&self) -> Result<Vec<DraftRef>, BackendError> {
        let drafts = self.drafts_mailbox().await?;
        let uidvalidity = self.select(&drafts, None).await?.uidvalidity;
        Ok(self
            .api
            .search(&drafts, "UNDELETED")
            .await?
            .into_iter()
            .map(|uid| {
                let id = Location {
                    mailbox: drafts.clone(),
                    uidvalidity,
                    uid,
                }
                .to_string();
                DraftRef {
                    draft_id: id.clone(),
                    message_id: id,
                }
            })
            .collect())
    }

    /// The message in Sent carrying `message_id`, when there is one.
    pub(super) async fn sent_with(&self, message_id: &str) -> Result<Option<String>, BackendError> {
        self.ensure_listed().await?;
        let Some(sent) = self.mailbox_for(Role::Sent) else {
            return Ok(None);
        };
        self.newest_with(&sent, message_id.trim_matches(['<', '>'])).await
    }
}

#[cfg(test)]
mod tests {
    use super::without_bcc;

    #[test]
    fn the_bcc_header_and_its_folded_lines_stay_behind() {
        let raw = b"From: me@example.com\r\nTo: ann@example.com\r\nBcc: cy@example.com,\r\n dee@example.com\r\nSubject: Hi\r\n\r\nBcc: this line is the body\r\n";
        assert_eq!(
            without_bcc(raw),
            b"From: me@example.com\r\nTo: ann@example.com\r\nSubject: Hi\r\n\r\nBcc: this line is the body\r\n"
        );
    }
}
