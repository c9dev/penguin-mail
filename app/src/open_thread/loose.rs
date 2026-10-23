//! A message shown from its own bytes rather than from the store: an
//! `.eml` file, or a `message/rfc822` part of a stored message.
//!
//! Such a message is in no mailbox, carries no keywords and has no thread
//! of its own, so the thread it opens as is made here: one read message
//! under a thread id of the app's own scheme, `loose:<n>`, that no store
//! or server call accepts, the way `outbox:<n>` stands for a queued
//! message. Its files and the pictures its body names by `cid:` are cut
//! out of the bytes up front and held on the thread, since nothing could
//! fetch them later, and its attachment rows carry no server handle for
//! the same reason.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use mailrs_domain::{AccountId, Memberships, MessageBody, MessageMeta, Target};
use mailrs_mime::Summary;

use super::{OpenThread, queued};

/// Where a loose message came from, for the window's title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    File(PathBuf),
    /// A part of a stored message, whose subject this carries.
    Attachment {
        subject: String,
    },
}

/// A message shown from its own bytes rather than from the store.
#[derive(Debug, Clone)]
pub struct Loose {
    pub raw: Arc<Vec<u8>>,
    /// The account that acts as "me" and sends replies: the one the
    /// containing message lives in, or the default account for a file.
    pub account_id: AccountId,
    /// False when the app has no account at all, so a reply has nowhere
    /// to go out from.
    pub has_account: bool,
    pub origin: Origin,
}

/// A loose message's bytes read once: its headers, its body, and the
/// bytes of each file the body lists. Made away from the window, since a
/// message near the size ceiling takes most of a second to read.
#[derive(Debug, Clone)]
pub struct Parsed {
    pub summary: Summary,
    pub body: MessageBody,
    pub files: Vec<Vec<u8>>,
}

/// Reads `raw` once for everything the thread needs. The attachment rows
/// carry no server handle: nothing here asks one.
pub fn parse(raw: &[u8]) -> Parsed {
    let summary = mailrs_mime::headers(raw);
    let (mut body, files) = mailrs_mime::opened(raw);
    for attachment in &mut body.attachments {
        attachment.attachment_id = None;
    }
    Parsed {
        summary,
        body,
        files,
    }
}

const PREFIX: &str = "loose:";

/// Numbers the loose messages opened in this process, so two windows on
/// the same file are two threads.
static NEXT: AtomicU64 = AtomicU64::new(1);

/// The thread id of the `n`th loose message.
pub fn loose_row(n: u64) -> String {
    format!("{PREFIX}{n}")
}

/// The number a loose thread id carries, or `None` for any other id.
pub fn loose_id(thread_id: &str) -> Option<u64> {
    thread_id.strip_prefix(PREFIX)?.parse().ok()
}

impl OpenThread {
    /// One message from `loose.raw`, read as `parsed`, open, under a
    /// fresh `loose:<n>` thread id, with its files held here.
    pub fn loose(loose: &Loose, parsed: Parsed, me: Vec<String>) -> OpenThread {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let id = format!("loose-{n}");
        let target = Target {
            account_id: loose.account_id,
            thread_id: loose_row(n),
            message_id: Some(id.clone()),
        };
        let Parsed {
            summary,
            body,
            files,
        } = parsed;
        let meta = MessageMeta {
            account_id: loose.account_id,
            id: id.clone(),
            thread_id: target.thread_id.clone(),
            rfc822_msgid: summary.message_id,
            from: summary.from,
            to: summary.to,
            cc: summary.cc,
            subject: summary.subject.clone(),
            date: summary.date.unwrap_or_default(),
            snippet: summary.snippet,
            size: loose.raw.len() as i64,
            has_attachments: !body.attachments.is_empty(),
            held: Memberships::read(),
            roles: Vec::new(),
            list_unsubscribe: None,
            one_click: false,
        };
        let mut thread = OpenThread::new(
            &target,
            summary.subject,
            vec![meta],
            HashMap::from([(id.clone(), body)]),
            me,
        );
        let pictures = queued::pictures(&thread, &id, &files);
        thread.inline_images.insert(id.clone(), pictures);
        if !files.is_empty() {
            thread.opened_files.insert(id, files);
        }
        thread.loose = Some(loose.clone());
        thread
    }

    /// The raw bytes behind `message_id`, which only a loose message has.
    pub fn raw_of(&self, message_id: &str) -> Option<Arc<Vec<u8>>> {
        let loose = self.loose.as_ref()?;
        self.messages
            .iter()
            .any(|m| m.id == message_id)
            .then(|| Arc::clone(&loose.raw))
    }

    pub fn is_loose(&self) -> bool {
        self.loose.is_some()
    }
}

#[cfg(test)]
pub(crate) fn fixture_raw() -> Vec<u8> {
    let inner = concat!(
        "From: Carl <carl@example.com>\r\n",
        "Subject: Inner\r\n",
        "Content-Type: text/plain\r\n",
        "\r\n",
        "inner\r\n",
    );
    format!(
        concat!(
            "From: Ann <ann@example.com>\r\n",
            "To: Bob <bob@example.com>\r\n",
            "Subject: Kite plans\r\n",
            "Date: Tue, 10 Mar 2026 09:00:00 +0000\r\n",
            "Message-ID: <kites@example.com>\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=\"outer\"\r\n",
            "\r\n",
            "--outer\r\n",
            "Content-Type: multipart/related; boundary=\"related\"\r\n",
            "\r\n",
            "--related\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "\r\n",
            "<p>Hi <img src=\"cid:kite\"></p>\r\n",
            "--related\r\n",
            "Content-Type: image/png\r\n",
            "Content-ID: <kite>\r\n",
            "Content-Disposition: inline; filename=\"kite.png\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "iVBORw0KGgo=\r\n",
            "--related--\r\n",
            "--outer\r\n",
            "Content-Type: text/plain\r\n",
            "Content-Disposition: attachment; filename=\"notes.txt\"\r\n",
            "\r\n",
            "Bring string\r\n",
            "--outer\r\n",
            "Content-Type: message/rfc822\r\n",
            "Content-Disposition: attachment\r\n",
            "\r\n",
            "{inner}",
            "--outer--\r\n",
        ),
        inner = inner
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::open_thread::OpenThread;

    fn loose_from(raw: Vec<u8>) -> Loose {
        Loose {
            raw: Arc::new(raw),
            account_id: 1,
            has_account: true,
            origin: Origin::Attachment {
                subject: "Fwd".into(),
            },
        }
    }

    #[test]
    fn a_loose_message_opens_as_one_read_message_with_its_files_held() {
        let thread = OpenThread::loose(
            &loose_from(fixture_raw()),
            parse(&fixture_raw()),
            vec!["bob@example.com".into()],
        );
        assert_eq!(thread.messages.len(), 1);
        let m = &thread.messages[0];
        assert_eq!(m.subject, "Kite plans");
        assert_eq!(
            m.from.as_ref().map(|a| a.email.as_str()),
            Some("ann@example.com")
        );
        assert_eq!(m.rfc822_msgid.as_deref(), Some("<kites@example.com>"));
        assert!(!m.is_unread());
        assert!(m.has_attachments);
        assert_eq!(thread.subject, "Kite plans");
        assert_eq!(thread.only_message.as_deref(), Some(m.id.as_str()));
        assert!(loose_id(&thread.thread_id).is_some());
        assert_eq!(thread.account_id, 1);
        let body = thread.bodies[&m.id].as_ref().unwrap();
        assert!(
            body.html
                .as_deref()
                .unwrap_or_default()
                .contains("cid:kite")
        );
        let names: Vec<&str> = body
            .attachments
            .iter()
            .map(|a| a.filename.as_str())
            .collect();
        assert!(names.contains(&"notes.txt"), "{names:?}");
        assert!(names.iter().any(|n| n.ends_with(".eml")), "{names:?}");
        assert!(
            body.attachments.iter().all(|a| a.attachment_id.is_none()),
            "nothing reaches for a server"
        );
        let held = &thread.opened_files[&m.id];
        assert_eq!(held.len(), body.attachments.len());
        let notes = body
            .attachments
            .iter()
            .position(|a| a.filename == "notes.txt")
            .unwrap();
        assert_eq!(held[notes], b"Bring string".to_vec());
        assert!(thread.inline_images[&m.id].contains_key("kite"));
        assert_eq!(
            thread.raw_of(&m.id).map(|r| r.len()),
            Some(fixture_raw().len())
        );
        assert!(thread.is_loose());
    }

    #[test]
    fn each_loose_message_gets_its_own_thread_id() {
        let a = OpenThread::loose(&loose_from(fixture_raw()), parse(&fixture_raw()), vec![]);
        let b = OpenThread::loose(&loose_from(fixture_raw()), parse(&fixture_raw()), vec![]);
        assert_ne!(a.thread_id, b.thread_id);
        assert_eq!(loose_row(5), "loose:5");
        assert_eq!(loose_id("loose:5"), Some(5));
        assert_eq!(loose_id("outbox:5"), None);
        assert_eq!(loose_id("t1"), None);
    }

    #[test]
    fn bytes_that_are_no_message_still_open_with_nothing_in_them() {
        let raw = b"just some text\n".to_vec();
        let thread = OpenThread::loose(&loose_from(raw.clone()), parse(&raw), vec![]);
        assert_eq!(thread.messages.len(), 1);
        assert_eq!(thread.messages[0].subject, "");
        assert!(thread.raw_of(&thread.messages[0].id).is_some());
    }
}
