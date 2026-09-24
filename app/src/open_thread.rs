//! The thread on screen, as plain data: its messages and their bodies,
//! what the reader opened and allowed, and what the engine and the model
//! made of it. The conversation view draws one; the thread run and the
//! engine run read and change it, and their tests build one without a
//! widget.

use std::collections::{HashMap, HashSet};

use mailrs_domain::{AccountId, Address, FlagColor, MessageBody, MessageMeta, Role, Target};

use crate::protection::run::{Claimed, Installed};
use crate::protection::{self, Engine, Mark};
use crate::translation::{Body, Language, Prose, Translation};

pub mod inline;
mod page;
pub mod queued;
pub mod run;

pub use inline::{InlineImage, Served};
#[cfg(test)]
pub use page::Document;
pub use page::{Article, Cleaned, Page, ToClean};
pub use queued::Unsent;

/// What a reply or a forward starts from, read off the open thread.
pub struct Answering {
    pub account_id: AccountId,
    pub target: MessageMeta,
    /// The words to quote.
    pub text: String,
    pub html: Option<String>,
    pub thread: Vec<MessageMeta>,
    pub attachments: Vec<mailrs_domain::Attachment>,
    /// Whether the message arrived encrypted, which makes the new message
    /// start with Encrypt on. The composer then asks before it sends the
    /// quote in the clear to somebody it cannot encrypt to.
    pub secret: bool,
}

/// Everything shown for one open thread.
pub struct OpenThread {
    pub account_id: AccountId,
    pub thread_id: String,
    pub subject: String,
    pub messages: Vec<MessageMeta>,
    /// Missing entries are still loading; `Err` holds why a body failed.
    pub bodies: HashMap<String, Result<MessageBody, String>>,
    pub expanded: HashSet<String>,
    pub images_allowed: bool,
    /// Set when the view shows one message of the thread, not all of it.
    pub only_message: Option<String>,
    pub me: Vec<String>,
    /// The pictures each message names by `cid:`, by content id, once they
    /// have arrived. A message missing here is still waiting for them.
    pub inline_images: HashMap<String, HashMap<String, InlineImage>>,
    /// How many times a message's pictures were replaced, which goes into
    /// the address the page asks for each by. See [`inline`].
    image_versions: HashMap<String, u32>,
    /// Pictures for the attachment rows: Gmail's attachment id to a small
    /// `data:` URI. Shared across the thread, since an id is unique.
    pub thumbnails: HashMap<String, String>,
    /// Files the engine cut out of a signed or encrypted message, by
    /// message id, in the order that message's attachment list gives them.
    /// For an encrypted message Gmail holds only the ciphertext, so these
    /// bytes are the only copy, and they live no longer than this window.
    pub opened_files: HashMap<String, Vec<Vec<u8>>>,
    /// The messages that arrived encrypted and were opened here. A reply
    /// to one starts encrypted.
    pub sealed: HashSet<String>,
    /// Contact photos by lower-case sender address, as `data:` URIs. A
    /// sender with none keeps the initials avatar.
    pub photos: HashMap<String, String>,
    /// Set once the user unsubscribed from this thread's list.
    pub unsubscribed: bool,
    /// What the engine made of each protected message in this thread, by
    /// message id, once it has run. It stays here so redrawing the thread
    /// never asks again, and so the card survives the body being replaced
    /// by the one that was inside the encryption.
    pub marks: HashMap<String, Mark>,
    /// The protected messages an engine run has claimed. Either engine may
    /// hold a pinentry in front of the person for as long as they take,
    /// and asking about one message twice would put up two of them.
    pub asked: HashSet<String>,
    /// The flag colour chosen here, when the thread is flagged.
    pub flag_color: Option<FlagColor>,
    /// Messages translated in this window, by message id. They go no
    /// further than this: a translation is text a model derived, and
    /// tomorrow's model would write it differently.
    pub translations: HashMap<String, Translation>,
    /// Set when the pane shows a queued message rather than a Gmail
    /// thread: what it says above the message, and which buttons.
    pub queued: Option<Unsent>,
    /// The cleaned HTML of each body the page draws, by message id.
    cleaned: HashMap<String, page::Cleaned>,
    /// What the page on screen holds, or `None` before the first draw.
    drawn: Option<page::Drawn>,
}

impl OpenThread {
    /// A thread as the store first shows it: `messages` oldest first and
    /// the bodies already fetched. Unread messages and the newest one
    /// start open; nothing has been allowed, claimed or translated yet.
    pub fn new(
        target: &Target,
        subject: String,
        messages: Vec<MessageMeta>,
        bodies: HashMap<String, MessageBody>,
        me: Vec<String>,
    ) -> OpenThread {
        let mut expanded: HashSet<String> = messages
            .iter()
            .filter(|m| m.is_unread())
            .map(|m| m.id.clone())
            .collect();
        if let Some(last) = messages.last() {
            expanded.insert(last.id.clone());
        }
        OpenThread {
            account_id: target.account_id,
            thread_id: target.thread_id.clone(),
            subject,
            messages,
            bodies: bodies
                .into_iter()
                .map(|(id, body)| (id, Ok(body)))
                .collect(),
            expanded,
            images_allowed: false,
            only_message: target.message_id.clone(),
            me,
            inline_images: HashMap::new(),
            image_versions: HashMap::new(),
            thumbnails: HashMap::new(),
            opened_files: HashMap::new(),
            sealed: HashSet::new(),
            photos: HashMap::new(),
            unsubscribed: false,
            marks: HashMap::new(),
            asked: HashSet::new(),
            flag_color: None,
            translations: HashMap::new(),
            queued: None,
            cleaned: HashMap::new(),
            drawn: None,
        }
    }

    pub fn is_draft(&self) -> bool {
        self.messages.last().is_some_and(|m| m.in_role(Role::Drafts))
    }

    pub fn starred(&self) -> bool {
        self.messages.iter().any(|m| m.is_flagged())
    }

    pub fn unread(&self) -> bool {
        self.messages.iter().any(|m| m.is_unread())
    }

    pub fn muted(&self) -> bool {
        self.messages.iter().any(|m| m.is_muted())
    }

    /// What a mail action on this conversation applies to.
    pub fn target(&self) -> Target {
        Target {
            account_id: self.account_id,
            thread_id: self.thread_id.clone(),
            message_id: self.only_message.clone(),
        }
    }

    /// The message a reply answers: the newest one that is not a draft.
    /// A queued message has not gone yet, so nobody can answer it.
    pub fn reply_target(&self) -> Option<&MessageMeta> {
        if self.queued.is_some() {
            return None;
        }
        self.messages.iter().rev().find(|m| !m.in_role(Role::Drafts))
    }

    /// What a reply to or a forward of one message starts from: `only`, or
    /// the message a reply answers when it names none. `None` when there
    /// is no such message.
    pub fn answering(&self, only: Option<&str>, forward: bool) -> Option<Answering> {
        let target = match only {
            Some(id) => self.messages.iter().find(|m| m.id == id)?.clone(),
            None => self.reply_target()?.clone(),
        };
        let body = self
            .bodies
            .get(&target.id)
            .and_then(|body| body.as_ref().ok());
        Some(Answering {
            account_id: self.account_id,
            text: match body {
                Some(body) => crate::compose::body_text(body),
                None => target.snippet.clone(),
            },
            // A forward keeps the original's HTML and its files, so what
            // goes out is the message that arrived.
            html: body.filter(|_| forward).and_then(|body| body.html.clone()),
            attachments: body
                .filter(|_| forward)
                .map(|body| body.attachments.clone())
                .unwrap_or_default(),
            secret: self.sealed.contains(&target.id),
            thread: self.messages.clone(),
            target,
        })
    }

    /// The newest message that carries an invitation, with the
    /// `text/calendar` part it arrived in.
    pub fn invitation(&self) -> Option<(&MessageMeta, &str)> {
        self.messages.iter().rev().find_map(|meta| {
            let body = self.bodies.get(&meta.id)?.as_ref().ok()?;
            Some((meta, body.calendar.as_deref()?))
        })
    }

    /// Every message that arrived signed or encrypted and has no engine
    /// run yet, newest first, with the engine call each needs. The newest
    /// is the one being read, so it goes first.
    pub fn protected(&self) -> Vec<(&MessageMeta, Engine)> {
        self.messages
            .iter()
            .rev()
            .filter(|meta| !self.asked.contains(&meta.id))
            .filter_map(|meta| {
                let body = self.bodies.get(&meta.id)?.as_ref().ok()?;
                Some((meta, protection::engine(body)?))
            })
            .collect()
    }

    /// Those messages, claimed for one engine run, with what the engine
    /// needs to read each. `installed` says which engines this computer
    /// has, so a message whose engine is missing is left unclaimed for the
    /// day it turns up. Once per message: a second call leaves out what
    /// the first took, because either engine may hold a pinentry in front
    /// of the person for as long as they take and asking twice would put
    /// up two of them.
    pub fn take_protected(&mut self, installed: Installed) -> Vec<Claimed> {
        let wanted: Vec<(String, Engine)> = self
            .protected()
            .into_iter()
            .filter(|(_, opening)| installed.runs(*opening))
            .map(|(meta, opening)| (meta.id.clone(), opening))
            .collect();
        let target = self.target();
        let mut claimed = Vec::new();
        for (message_id, opening) in wanted {
            let Some(Ok(body)) = self.bodies.get(&message_id) else {
                continue;
            };
            let body = body.clone();
            self.asked.insert(message_id.clone());
            claimed.push(Claimed {
                target: target.clone(),
                message_id,
                opening,
                body,
            });
        }
        claimed
    }

    /// What the protection card says: the mark of the newest message an
    /// engine answered about, which is the one being read.
    pub fn card(&self) -> Option<&Mark> {
        self.messages
            .iter()
            .rev()
            .find_map(|meta| self.marks.get(&meta.id))
    }

    /// Whether this is the thread one of `targets` names. A target for
    /// one message of the thread counts, since acting on it changes the
    /// thread on screen.
    pub fn among(&self, targets: &[Target]) -> bool {
        targets
            .iter()
            .any(|t| t.account_id == self.account_id && t.thread_id == self.thread_id)
    }

    /// The newest sender who is not the user: the person Block Sender,
    /// the VIP toggle and a category move act on.
    pub fn other_sender(&self) -> Option<&Address> {
        self.messages
            .iter()
            .rev()
            .filter_map(|m| m.from.as_ref())
            .find(|a| {
                !self
                    .me
                    .iter()
                    .any(|mine| mine.eq_ignore_ascii_case(&a.email))
            })
    }

    /// Who sent the newest message, the user included, in lower case.
    /// Remote images are allowed per sender of what is on screen.
    pub fn newest_sender(&self) -> Option<String> {
        self.messages
            .last()
            .and_then(|m| m.from.as_ref())
            .map(|a| a.email.to_lowercase())
            .filter(|a| !a.trim().is_empty())
    }

    /// Every message's sender, oldest first, with an empty address for a
    /// message that names none.
    pub fn senders(&self) -> Vec<String> {
        self.messages
            .iter()
            .map(|m| m.from.as_ref().map(|a| a.email.clone()).unwrap_or_default())
            .collect()
    }

    /// Whether a reply can quote the message it answers: that message's
    /// body has arrived, or failed for good.
    pub fn quotable(&self) -> bool {
        self.reply_target()
            .is_some_and(|m| self.bodies.contains_key(&m.id))
    }

    /// The newest message and its body, when that message offers a way
    /// off the list: a `List-Unsubscribe` header, or a link in the body
    /// that reads as leaving. The second is why the whole body comes
    /// back rather than the header alone.
    pub fn list_unsubscribe(&self) -> Option<(&MessageMeta, &MessageBody)> {
        let target = self.reply_target()?;
        let body = self.bodies.get(&target.id)?.as_ref().ok()?;
        crate::unsubscribe::choose_with_body(
            body.list_unsubscribe.as_deref(),
            body.one_click_unsubscribe,
            body.html.as_deref(),
        )
        .map(|_| (target, body))
    }

    /// Takes in the thread's messages as the store now has them, and
    /// answers with the ids whose bodies are still missing. A message that
    /// was not here before and arrived unread opens, since the reader has
    /// not seen it; one they closed themselves stays closed. A thread
    /// showing one message keeps only that one, and an empty answer from
    /// the store leaves the list alone.
    pub fn take_messages(&mut self, fresh: &[MessageMeta]) -> Vec<String> {
        let fresh: Vec<MessageMeta> = fresh
            .iter()
            .filter(|m| self.only_message.as_ref().is_none_or(|id| &m.id == id))
            .cloned()
            .collect();
        for meta in &fresh {
            if !self.messages.iter().any(|m| m.id == meta.id) && meta.is_unread() {
                self.expanded.insert(meta.id.clone());
            }
        }
        if !fresh.is_empty() {
            self.messages = fresh;
        }
        self.messages
            .iter()
            .filter(|m| !self.bodies.contains_key(&m.id))
            .map(|m| m.id.clone())
            .collect()
    }

    /// Replaces the messages and answers whether their ids differ from the
    /// ones that were here, in that order.
    pub fn replace_messages(&mut self, fresh: Vec<MessageMeta>) -> bool {
        let same = self
            .messages
            .iter()
            .map(|m| &m.id)
            .eq(fresh.iter().map(|m| &m.id));
        self.messages = fresh;
        !same
    }

    /// Bodies as Gmail sent them, with the HTML already cleaned away from
    /// the GTK thread. The pictures they name come later.
    pub fn take_bodies(
        &mut self,
        bodies: Vec<(String, Result<MessageBody, String>)>,
        cleaned: HashMap<String, Cleaned>,
    ) {
        self.bodies.extend(bodies);
        self.take_cleaned(cleaned);
    }

    /// What the engine made of the protected message: the mark for the
    /// card, and the body and the files cut from what it checked or opened.
    /// Answers whether it gave a body. The pictures Gmail fetched for the
    /// message as it arrived go, since one could come from a part the
    /// signature does not cover; the ones the new body shows come out of
    /// its own files.
    pub fn take_engine_answer(&mut self, message_id: String, read: protection::Read) -> bool {
        self.marks.insert(message_id.clone(), read.mark);
        let Some(body) = read.body else {
            return false;
        };
        if read.sealed {
            self.sealed.insert(message_id.clone());
        }
        self.bodies.insert(message_id.clone(), Ok(body));
        let pictures = queued::pictures(self, &message_id, &read.files);
        self.replace_images(&message_id, pictures);
        if !read.files.is_empty() {
            self.opened_files.insert(message_id, read.files);
        }
        true
    }

    /// What the translation card says about a message translated here:
    /// the language it came from, whether it was cut short, and whether
    /// the page shows the translation.
    pub fn translation_of(&self, message_id: &str) -> Option<(Option<Language>, bool, bool)> {
        self.translations
            .get(message_id)
            .map(|said| (said.from, said.cut, said.shown))
    }

    /// Turns a translated message over, and answers with what the card
    /// should now say. `None` when the message has no translation.
    pub fn turn_translation(&mut self, message_id: &str) -> Option<(Option<Language>, bool, bool)> {
        let said = self.translations.get_mut(message_id)?;
        said.shown = !said.shown;
        Some((said.from, said.cut, said.shown))
    }

    /// The words the sender of `message_id` wrote in the thread's other
    /// messages, without what they quoted. A short note borrows its
    /// language from these when its own words are too few to tell.
    pub fn same_writer(&self, message_id: &str) -> String {
        let writer = |meta: &MessageMeta| meta.from.as_ref().map(|a| a.email.to_lowercase());
        let Some(who) = self
            .messages
            .iter()
            .find(|meta| meta.id == message_id)
            .and_then(writer)
        else {
            return String::new();
        };
        let mut out = String::new();
        for meta in self.messages.iter().rev() {
            if meta.id == message_id || writer(meta).as_deref() != Some(who.as_str()) {
                continue;
            }
            let Some(Ok(body)) = self.bodies.get(&meta.id) else {
                continue;
            };
            // The raw HTML is enough to count words in: `Prose` skips the
            // stylesheet and the tags either way.
            let prose = match (&body.text, &body.html) {
                (Some(text), _) if !text.trim().is_empty() => Prose::read(Body::Text(text)),
                (_, Some(html)) => Prose::read(Body::Html(html)),
                _ => continue,
            };
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&prose.sample());
        }
        out
    }

    /// A message's body as it arrived, which is what a translation is
    /// built from, with the start of the address of each picture it names.
    pub fn arrived(&self, message_id: &str) -> Option<(MessageBody, String)> {
        let body = self.bodies.get(message_id)?.as_ref().ok()?.clone();
        Some((body, self.picture_prefix(message_id)))
    }

    /// The bodies with a picture attached that has no thumbnail yet. Every
    /// body counts, not only the ones just fetched: a message read before
    /// is already in the store, and its pictures are as worth showing.
    pub fn wanting_thumbnails(&self) -> Vec<(String, MessageBody)> {
        self.bodies
            .iter()
            .filter_map(|(id, body)| Some((id, body.as_ref().ok()?)))
            .filter(|(_, body)| {
                body.attachments.iter().any(|a| {
                    a.attachment_id
                        .as_deref()
                        .is_some_and(|id| !self.thumbnails.contains_key(id))
                        && a.mime_type.starts_with("image/")
                        && !crate::render::shown_in_body(a, body)
                })
            })
            .map(|(id, body)| (id.clone(), body.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::OpenThread;
    use mailrs_domain::{MessageMeta, system_label};
    use std::collections::{HashMap, HashSet};

    /// One message of a thread. `unread` puts Gmail's own label on it.
    fn message(id: &str, unread: bool) -> MessageMeta {
        MessageMeta {
            account_id: 1,
            id: id.to_string(),
            thread_id: "t1".to_string(),
            rfc822_msgid: None,
            from: None,
            to: Vec::new(),
            cc: Vec::new(),
            subject: "Lunch".to_string(),
            date: 0,
            snippet: String::new(),
            size: 0,
            has_attachments: false,
            label_ids: match unread {
                true => vec![system_label::UNREAD.to_string()],
                false => Vec::new(),
            },
            list_unsubscribe: None,
            one_click: false,
        }
    }

    /// A thread with these messages on screen and nothing else filled in.
    fn thread(messages: Vec<MessageMeta>) -> OpenThread {
        OpenThread {
            account_id: 1,
            thread_id: "t1".to_string(),
            subject: "Lunch".to_string(),
            messages,
            bodies: HashMap::new(),
            expanded: HashSet::new(),
            images_allowed: false,
            only_message: None,
            me: Vec::new(),
            inline_images: HashMap::new(),
            image_versions: HashMap::new(),
            thumbnails: HashMap::new(),
            opened_files: HashMap::new(),
            sealed: HashSet::new(),
            photos: HashMap::new(),
            unsubscribed: false,
            marks: HashMap::new(),
            asked: HashSet::new(),
            flag_color: None,
            translations: HashMap::new(),
            queued: None,
            cleaned: HashMap::new(),
            drawn: None,
        }
    }

    #[test]
    fn a_body_the_engine_opened_brings_its_own_pictures_and_no_others() {
        use crate::protection::{Mark, Read, Tone};
        use mailrs_domain::{Attachment, MessageBody};

        let mut open = thread(vec![message("m1", false)]);
        open.bodies
            .insert("m1".to_string(), Ok(MessageBody::default()));
        // What Gmail fetched for the message as it arrived, including a
        // picture from a part nobody signed.
        let stranger = crate::open_thread::InlineImage {
            mime: "image/png".to_string(),
            bytes: vec![0].into(),
        };
        open.take_images(HashMap::from([(
            "m1".to_string(),
            HashMap::from([("stranger".to_string(), stranger)]),
        )]));
        assert!(matches!(
            open.picture("m1", 0, "stranger"),
            crate::open_thread::Served::Ready(_)
        ));
        let signed = MessageBody {
            html: Some("<img src=\"cid:logo\">".to_string()),
            attachments: vec![Attachment {
                part_id: "0".to_string(),
                filename: "logo.png".to_string(),
                mime_type: "image/png".to_string(),
                size: 1,
                attachment_id: None,
                content_id: Some("logo".to_string()),
            }],
            ..MessageBody::default()
        };
        open.take_engine_answer(
            "m1".to_string(),
            Read {
                mark: Mark {
                    title: "Signed by Ann".to_string(),
                    detail: None,
                    tone: Tone::Good,
                },
                body: Some(signed),
                files: vec![vec![1]],
                sealed: false,
                revocation_unchecked: false,
            },
        );
        let pictures = &open.inline_images["m1"];
        assert_eq!(pictures.len(), 1, "{pictures:?}");
        assert_eq!(*pictures["logo"].bytes, [1]);
        assert_eq!(pictures["logo"].mime, "image/png");
        // The page's old address for the stranger reaches nothing now, and
        // the logo comes under a new one.
        use crate::open_thread::Served;
        assert_eq!(open.picture("m1", 0, "stranger"), Served::Gone);
        assert_eq!(open.picture("m1", 0, "logo"), Served::Gone);
        assert!(matches!(open.picture("m1", 1, "logo"), Served::Ready(_)));
        assert_eq!(open.picture_prefix("m1"), "mailrs-cid:1/m1/1/");
    }

    /// What an engine gives back for a message it opened out of its
    /// encryption, or checked in the clear.
    fn engine_read(text: &str, sealed: bool) -> crate::protection::Read {
        crate::protection::Read {
            mark: crate::protection::Mark {
                title: "Encrypted".to_string(),
                detail: None,
                tone: crate::protection::Tone::Unchecked,
            },
            body: Some(mailrs_domain::MessageBody {
                text: Some(text.to_string()),
                ..mailrs_domain::MessageBody::default()
            }),
            files: Vec::new(),
            sealed,
            revocation_unchecked: false,
        }
    }

    /// The oracle this pins: a stranger puts somebody else's ciphertext in
    /// a message, the window opens it without asking, and a reply quotes
    /// the plaintext back to the stranger in the clear.
    #[test]
    fn a_reply_to_a_message_that_arrived_encrypted_starts_encrypted() {
        let mut open = thread(vec![message("m1", false)]);
        open.take_engine_answer(
            "m1".to_string(),
            engine_read("The key is under the mat.", true),
        );

        let answering = open.answering(None, false).expect("something to answer");
        assert!(answering.secret);
        assert_eq!(answering.text, "The key is under the mat.");
        assert!(
            open.answering(Some("m1"), true)
                .is_some_and(|forward| forward.secret)
        );
    }

    #[test]
    fn a_reply_to_a_message_in_the_clear_starts_as_the_writer_left_it() {
        let mut open = thread(vec![message("m1", false)]);
        open.take_engine_answer("m1".to_string(), engine_read("Meet at six.", false));

        let answering = open.answering(None, false).expect("something to answer");
        assert!(!answering.secret);
    }

    #[test]
    fn a_message_that_arrives_unread_opens() {
        let mut open = thread(vec![message("m1", false)]);
        open.take_messages(&[message("m1", false), message("m2", true)]);
        assert!(open.expanded.contains("m2"));
    }

    #[test]
    fn a_message_that_arrives_read_stays_closed() {
        let mut open = thread(vec![message("m1", false)]);
        open.take_messages(&[message("m1", false), message("m2", false)]);
        assert!(open.expanded.is_empty());
    }

    #[test]
    fn a_message_the_reader_closed_does_not_open_again() {
        let mut open = thread(vec![message("m1", true)]);
        open.take_messages(&[message("m1", true)]);
        assert!(open.expanded.is_empty());
    }

    #[test]
    fn a_thread_showing_one_message_keeps_only_that_one() {
        let mut open = thread(vec![message("m2", false)]);
        open.only_message = Some("m2".to_string());
        open.take_messages(&[message("m1", true), message("m2", false)]);
        let ids: Vec<&str> = open.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["m2"]);
    }

    #[test]
    fn the_messages_with_no_body_yet_are_the_ones_asked_for() {
        let mut open = thread(Vec::new());
        open.bodies
            .insert("m1".to_string(), Err("gone".to_string()));
        let missing = open.take_messages(&[message("m1", false), message("m2", false)]);
        assert_eq!(missing, ["m2"]);
    }

    #[test]
    fn an_empty_answer_from_the_store_leaves_the_messages_alone() {
        let mut open = thread(vec![message("m1", false)]);
        open.take_messages(&[]);
        let ids: Vec<&str> = open.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["m1"]);
    }

    #[test]
    fn the_same_ids_in_the_same_order_are_not_a_change() {
        let mut open = thread(vec![message("m1", false), message("m2", false)]);
        assert!(!open.replace_messages(vec![message("m1", true), message("m2", false)]));
    }

    #[test]
    fn a_message_the_thread_did_not_have_is_a_change() {
        let mut open = thread(vec![message("m1", false)]);
        assert!(open.replace_messages(vec![message("m1", false), message("m2", false)]));
    }
}
