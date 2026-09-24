//! A queued message in the conversation pane. Nothing Gmail holds stands
//! for it, so the one message the pane shows is built here from what the
//! outbox kept: the draft the composer would reopen, or failing that the
//! bytes that go out.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Local};
use mailrs_domain::{MessageBody, MessageMeta, Target};
use mailrs_store::outbox::Queued;
use mailrs_sync::{outbox_row, waiting_line};

use super::{InlineImage, OpenThread};
use crate::compose::{self, Draft};
use crate::protection::opened_body;

/// What the pane says about the queued message it shows, above the
/// message itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsent {
    /// When it goes, or why it has not gone and when the next try is, in
    /// the words its row uses.
    pub line: String,
    /// It hit a problem, so it sits in the Outbox rather than in Send
    /// Later, and the pane offers Edit, Send Now and Delete.
    pub stuck: bool,
}

impl Unsent {
    pub fn of(queued: &Queued, now: DateTime<Local>) -> Unsent {
        Unsent {
            line: waiting_line(queued, now),
            stuck: queued.problem.is_some(),
        }
    }
}

impl OpenThread {
    /// The queued message as the pane shows it: one message, open, under
    /// the id its list row carries, with its files held here since Gmail
    /// has none of them.
    pub fn queued(queued: &Queued, unsent: Unsent, me: Vec<String>) -> OpenThread {
        let target = Target::thread(queued.account_id, outbox_row(queued.id));
        let id = format!("queued-{}", queued.id);
        let draft = serde_json::from_str::<Draft>(&queued.composer).ok();
        let (body, files) = body_of(queued, draft.as_ref());
        let meta = meta_of(queued, draft.as_ref(), &id, &target.thread_id, &body);
        let mut thread = OpenThread::new(
            &target,
            meta.subject.clone(),
            vec![meta],
            HashMap::from([(id.clone(), body)]),
            me,
        );
        thread.expanded = HashSet::from([id.clone()]);
        let pictures = pictures(&thread, &id, &files);
        thread.inline_images.insert(id.clone(), pictures);
        if !files.is_empty() {
            thread.opened_files.insert(id, files);
        }
        thread.queued = Some(unsent);
        thread
    }

    /// What the outbox now says about the queued message on screen.
    pub fn take_unsent(&mut self, unsent: Unsent) {
        self.queued = Some(unsent);
    }
}

/// The body the writer wrote, with its files. The draft comes first
/// because it holds their words before any signing or encryption; the
/// bytes that go out stand in for a draft this version cannot read.
fn body_of(queued: &Queued, draft: Option<&Draft>) -> (MessageBody, Vec<Vec<u8>>) {
    if let Some(part) = draft.and_then(|draft| compose::build_body_part(draft).ok()) {
        return opened_body(&part);
    }
    match &queued.raw {
        Some(raw) => opened_body(raw),
        None => (MessageBody::default(), Vec::new()),
    }
}

/// The header of the one message: who it is from and to as the draft
/// names them, or as the row does when there is no draft to read. Its
/// date is when the writer sent it, from the bytes built then; the line
/// above the message says when it goes.
fn meta_of(
    queued: &Queued,
    draft: Option<&Draft>,
    id: &str,
    thread_id: &str,
    body: &MessageBody,
) -> MessageMeta {
    let (from, to, cc) = match draft {
        Some(draft) => (Some(draft.from.clone()), draft.to.clone(), draft.cc.clone()),
        None => (
            None,
            compose::parse_recipients(&queued.recipients),
            Vec::new(),
        ),
    };
    MessageMeta {
        account_id: queued.account_id,
        id: id.to_string(),
        thread_id: thread_id.to_string(),
        rfc822_msgid: None,
        from,
        to,
        cc,
        subject: queued.subject.clone(),
        date: written_at(queued).unwrap_or_else(mailrs_sync::now_millis),
        snippet: String::new(),
        size: 0,
        has_attachments: !body.attachments.is_empty(),
        held: mailrs_domain::Memberships::read(),
        roles: vec![],
        list_unsubscribe: None,
        one_click: false,
    }
}

/// When the bytes that go out were built, from their `Date` header. A
/// message whose bytes Gmail holds has none here.
fn written_at(queued: &Queued) -> Option<mailrs_domain::EpochMillis> {
    let raw = queued.raw.as_deref()?;
    let parsed = mail_parser::MessageParser::default().parse_headers(raw)?;
    Some(parsed.date()?.to_timestamp() * 1000)
}

/// The pictures the text shows by `cid:`, from the files the message
/// carries.
pub(super) fn pictures(
    thread: &OpenThread,
    id: &str,
    files: &[Vec<u8>],
) -> HashMap<String, InlineImage> {
    let Some(Ok(body)) = thread.bodies.get(id) else {
        return HashMap::new();
    };
    body.attachments
        .iter()
        .zip(files)
        .filter(|(file, _)| file.mime_type.starts_with("image/"))
        .filter_map(|(file, bytes)| {
            let cid = file.content_id.as_deref()?.trim_matches(['<', '>']);
            let picture = InlineImage {
                mime: file.mime_type.clone(),
                bytes: bytes.as_slice().into(),
            };
            Some((cid.to_string(), picture))
        })
        .collect()
}

/// A draft from me to `to` saying `words`, for the tests here and the
/// thread run's.
#[cfg(test)]
pub(crate) fn draft_to(account_id: mailrs_domain::AccountId, to: &str, words: &str) -> Draft {
    let mut draft = Draft::new(
        account_id,
        mailrs_domain::Address {
            name: Some("Me".to_string()),
            email: "me@example.com".to_string(),
        },
    );
    draft.to = compose::parse_recipients(to);
    draft.subject = "Kite plans".to_string();
    draft.markdown = words.to_string();
    draft
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::OutgoingAttachment;
    use chrono::TimeZone;

    fn now() -> DateTime<Local> {
        Local.timestamp_millis_opt(0).single().expect("a time")
    }

    fn queued(draft: &Draft, problem: Option<&str>) -> Queued {
        Queued {
            id: 7,
            account_id: 1,
            subject: draft.subject.clone(),
            recipients: "Ann".to_string(),
            composer: serde_json::to_string(draft).expect("a draft writes"),
            problem: problem.map(str::to_string),
            attempts: 1,
            ..Queued::default()
        }
    }

    fn shown(queued: &Queued) -> OpenThread {
        OpenThread::queued(queued, Unsent::of(queued, now()), Vec::new())
    }

    #[test]
    fn the_pane_names_it_as_its_row_does() {
        let draft = draft_to(1, "ann@example.com", "See you at ten.");
        let open = shown(&queued(&draft, None));
        assert_eq!(open.target(), Target::thread(1, outbox_row(7)));
    }

    #[test]
    fn the_message_shows_what_the_writer_wrote_and_who_it_goes_to() {
        let draft = draft_to(1, "Ann <ann@example.com>", "See you at ten.");
        let open = shown(&queued(&draft, None));
        let meta = &open.messages[0];
        assert_eq!(meta.to[0].email, "ann@example.com");
        assert_eq!(
            meta.from.as_ref().map(|a| a.email.as_str()),
            Some("me@example.com")
        );
        let body = open.bodies[&meta.id].as_ref().expect("a body");
        assert!(
            body.text
                .as_deref()
                .unwrap_or("")
                .contains("See you at ten.")
        );
        assert!(open.expanded.contains(&meta.id));
    }

    #[test]
    fn a_stuck_message_says_why_and_offers_the_outbox_buttons() {
        let draft = draft_to(1, "ann@example.com", "Hi");
        let open = shown(&queued(&draft, Some("The network is down.")));
        let unsent = open.queued.expect("the pane says why");
        assert!(unsent.stuck);
        assert!(unsent.line.starts_with("The network is down. Trying again"));
    }

    #[test]
    fn a_send_later_message_says_when_it_goes() {
        let draft = draft_to(1, "ann@example.com", "Hi");
        let unsent = shown(&queued(&draft, None)).queued.expect("a line");
        assert!(!unsent.stuck);
        assert!(unsent.line.starts_with("Sends "));
    }

    #[test]
    fn its_files_are_held_here_and_a_picture_in_the_text_is_drawn() {
        let mut draft = draft_to(1, "ann@example.com", "");
        draft.markdown = "Look: ![kite](cid:kite1)".to_string();
        draft.attachments = vec![
            OutgoingAttachment {
                filename: "kite.png".to_string(),
                mime_type: "image/png".to_string(),
                data: vec![1, 2, 3],
                content_id: Some("kite1".to_string()),
            },
            OutgoingAttachment {
                filename: "plan.txt".to_string(),
                mime_type: "text/plain".to_string(),
                data: b"Fly at ten".to_vec(),
                content_id: None,
            },
        ];
        let open = shown(&queued(&draft, None));
        let id = &open.messages[0].id;
        assert_eq!(open.opened_files[id].len(), 2);
        assert_eq!(open.inline_images[id]["kite1"].mime, "image/png");
    }

    #[test]
    fn with_no_draft_to_read_the_bytes_that_go_out_are_shown() {
        let mut message = queued(&draft_to(1, "ann@example.com", ""), None);
        message.composer = "{}".to_string();
        message.raw = Some(
            b"From: me@example.com\r\nTo: ann@example.com\r\nSubject: Kite plans\r\n\
              Date: Wed, 9 Sep 2026 00:26:40 +0000\r\n\r\nBring string.\r\n"
                .to_vec(),
        );
        let open = shown(&message);
        let meta = &open.messages[0];
        let body = open.bodies[&meta.id].as_ref().expect("a body");
        assert!(body.text.as_deref().unwrap_or("").contains("Bring string."));
        assert_eq!(meta.date, 1_788_913_600_000);
        assert_eq!(meta.to[0].display(), "Ann");
    }

    #[test]
    fn nobody_replies_to_a_message_that_has_not_gone() {
        let draft = draft_to(1, "ann@example.com", "Hi");
        let open = shown(&queued(&draft, None));
        assert!(open.reply_target().is_none());
    }
}
