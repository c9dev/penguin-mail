//! Bodies over IMAP: the whole message by `BODY.PEEK[]` for a small one;
//! for a large one, or one of unknown size, its BODYSTRUCTURE with the
//! text parts the body needs, each by `BODY.PEEK[<part>]`, and later one
//! file at a time by the same part path the raw reader gives. `PEEK`
//! leaves the message unread on the server.

use mailrs_domain::Location;
use mailrs_imap::ImapError;
use mailrs_mime::{Part, Parts};

use super::{Imap, ImapApi, Submit};
use crate::BackendError;
use crate::services::RawMessage;

/// How deep the walk for text parts goes. A sender can nest parts without
/// limit, and nothing deeper holds text the body shows.
const DEPTH: usize = 64;

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// Where `name` points, while the mailbox keeps the UIDVALIDITY it
    /// names. After a change the same UID can name another message, so the
    /// old location answers `NotFound` until the feed relists the mailbox.
    pub(super) async fn current(&self, name: &str) -> Result<Location, BackendError> {
        let at = Location::parse(name).ok_or(BackendError::NotFound)?;
        let selected = self.select(&at.mailbox, None).await?;
        match selected.uidvalidity == at.uidvalidity {
            true => Ok(at),
            false => Err(BackendError::NotFound),
        }
    }

    /// The messages as they arrived, in order.
    pub(super) async fn raw_messages(&self, names: &[String]) -> Result<Vec<RawMessage>, BackendError> {
        let mut raws = Vec::with_capacity(names.len());
        for name in names {
            let at = self.current(name).await?;
            let bytes = self
                .api
                .body(&at.mailbox, at.uid, "")
                .await?
                .ok_or(BackendError::NotFound)?;
            raws.push(RawMessage {
                id: name.clone(),
                bytes,
            });
        }
        Ok(raws)
    }

    /// The message's parts with its top-level headers and the bytes of the
    /// text parts its body needs. A text part that fails to arrive leaves
    /// the message readable without it and marks the parts incomplete, so
    /// the body read from them is not kept.
    pub(super) async fn structure_of(&self, name: &str) -> Result<Parts, BackendError> {
        let at = self.current(name).await?;
        let structure = self
            .api
            .structure(&at.mailbox, at.uid)
            .await?
            .ok_or(BackendError::NotFound)?;
        // The header block ends with its blank line, so it reads as a
        // message with no body.
        let header = self
            .api
            .body(&at.mailbox, at.uid, "HEADER")
            .await?
            .unwrap_or_default();
        let mut parts = structure.parts(&header);
        if self.is_unreadable(name) {
            parts.incomplete = true;
            return Ok(parts);
        }
        for path in text_paths(&parts) {
            match self.api.body(&at.mailbox, at.uid, &path).await {
                Ok(Some(bytes)) => match structure.decode(&path, &bytes) {
                    Some(data) => parts.set_data(&path, data),
                    None => parts.incomplete = true,
                },
                Ok(None) => parts.incomplete = true,
                // The guard refused the answer (nested too deep, a line
                // too long, a literal past the budget) and the connection
                // went with it. Asking again brings the same answer, so
                // the message shows without its text for this session.
                Err(err @ ImapError::Protocol(_)) => {
                    tracing::warn!(message = name, %err, "the text of a message is unreadable");
                    self.mark_unreadable(name);
                    parts.incomplete = true;
                    break;
                }
                Err(err) => {
                    tracing::warn!(message = name, %err, "could not fetch a text part");
                    parts.incomplete = true;
                }
            }
        }
        Ok(parts)
    }

    /// One part of the message by its part path, with its transfer
    /// encoding undone.
    pub(super) async fn part_of(&self, name: &str, path: &str) -> Result<Vec<u8>, BackendError> {
        let at = self.current(name).await?;
        let structure = self
            .api
            .structure(&at.mailbox, at.uid)
            .await?
            .ok_or(BackendError::NotFound)?;
        let bytes = self
            .api
            .body(&at.mailbox, at.uid, path)
            .await?
            .ok_or(BackendError::NotFound)?;
        structure.decode(path, &bytes).ok_or(BackendError::NotFound)
    }
}

/// The paths of the parts a body needs the bytes of: the plain and HTML
/// texts that are not files, and an invitation's calendar by the rule the
/// Gmail adapter follows, `text/calendar` where there is one and any part
/// `is_calendar` takes otherwise. Nothing inside a forwarded message: the
/// body lists that message as a file.
fn text_paths(parts: &Parts) -> Vec<String> {
    let mut leaves = Vec::new();
    leaves_of(&parts.root, 0, &mut leaves);
    let has_calendar = leaves.iter().any(|p| p.mime_type == "text/calendar");
    leaves
        .into_iter()
        .filter(|part| {
            let file = part.attachment || part.filename.is_some();
            let text = (part.mime_type == "text/plain" || part.mime_type == "text/html") && !file;
            let calendar = part.mime_type == "text/calendar"
                || (!has_calendar
                    && mailrs_mime::is_calendar(
                        &part.mime_type,
                        part.filename.as_deref().unwrap_or_default(),
                    ));
            text || calendar
        })
        .map(|part| part.path.clone())
        .collect()
}

/// The parts under `part` that hold bytes, down to `DEPTH`, leaving out a
/// forwarded message and everything in it.
fn leaves_of<'a>(part: &'a Part, depth: usize, out: &mut Vec<&'a Part>) {
    if depth > DEPTH || part.mime_type == "message/rfc822" {
        return;
    }
    if part.mime_type.starts_with("multipart/") {
        for child in &part.children {
            leaves_of(child, depth + 1, out);
        }
        return;
    }
    out.push(part);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mailrs_mime::{Part, Parts};

    use super::super::{Imap, ImapSettings, UNREADABLE_KEPT};
    use super::text_paths;
    use crate::fake::{FakeImap, FakeSmtp};

    #[test]
    fn the_unreadable_memo_keeps_the_latest_messages_only() {
        let imap = Imap::new(
            Arc::new(FakeImap::new()),
            Arc::new(FakeSmtp::default()),
            ImapSettings {
                address: "me@example.com".into(),
                provider_name: "Fastmail".into(),
                files_sent_mail: false,
                window_days: 30,
            },
        );
        for n in 0..=UNREADABLE_KEPT {
            imap.mark_unreadable(&format!("INBOX/1/{n}"));
        }
        imap.mark_unreadable("INBOX/1/1");
        assert!(!imap.is_unreadable("INBOX/1/0"), "the oldest is forgotten");
        assert!(imap.is_unreadable(&format!("INBOX/1/{UNREADABLE_KEPT}")));
        assert_eq!(imap.known().unreadable.len(), UNREADABLE_KEPT);
    }

    fn part(path: &str, mime: &str, filename: Option<&str>, children: Vec<Part>) -> Part {
        Part {
            path: path.into(),
            mime_type: mime.into(),
            filename: filename.map(str::to_string),
            attachment: filename.is_some(),
            children,
            ..Part::default()
        }
    }

    #[test]
    fn the_body_needs_its_texts_and_calendar_and_not_its_files_or_a_forwarded_message() {
        let parts = Parts {
            root: part(
                "",
                "multipart/mixed",
                None,
                vec![
                    part(
                        "1",
                        "multipart/alternative",
                        None,
                        vec![
                            part("1.1", "text/plain", None, vec![]),
                            part("1.2", "text/html", None, vec![]),
                            part("1.3", "text/calendar", Some("invite.ics"), vec![]),
                        ],
                    ),
                    part("2", "application/pdf", Some("plans.pdf"), vec![]),
                    part("3", "text/plain", Some("notes.txt"), vec![]),
                    part(
                        "4",
                        "message/rfc822",
                        None,
                        vec![part("4.1", "text/plain", None, vec![])],
                    ),
                ],
            ),
            ..Parts::default()
        };
        assert_eq!(text_paths(&parts), ["1.1", "1.2", "1.3"]);
    }
}
