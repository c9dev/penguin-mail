//! The thread on screen, as plain data: its messages and their bodies,
//! what the reader opened and allowed, and what the engine and the model
//! made of it. The conversation view draws one; the thread run and the
//! engine run read and change it, and their tests build one without a
//! widget.

use std::collections::{HashMap, HashSet};

use mailrs_domain::{
    AccountId, Address, FlagColor, MessageBody, MessageMeta, Target, system_label,
};

use crate::protection::run::{Claimed, Installed};
use crate::protection::{self, Engine, Mark};
use crate::translation::{Body, Language, Prose, Translation};

pub mod queued;
pub mod run;

pub use queued::Unsent;

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
    /// Inline images per message: `Content-ID` to `data:` URI.
    pub inline_images: HashMap<String, HashMap<String, String>>,
    /// Pictures for the attachment rows: Gmail's attachment id to a small
    /// `data:` URI. Shared across the thread, since an id is unique.
    pub thumbnails: HashMap<String, String>,
    /// Files that came out of an encrypted message, by message id, in the
    /// order that message's attachment list gives them. Gmail holds the
    /// ciphertext, so these bytes are the only copy and they live no
    /// longer than this window.
    pub opened_files: HashMap<String, Vec<Vec<u8>>>,
    /// Contact photos by lower-case sender address, as `data:` URIs. A
    /// sender with none keeps the initials avatar.
    pub photos: HashMap<String, String>,
    /// Set once the user unsubscribed from this thread's list.
    pub unsubscribed: bool,
    /// What the engine made of the protected message in this thread, once
    /// it has run. It stays here so redrawing the thread never asks again,
    /// and so the card survives the body being replaced by the one that
    /// was inside the encryption.
    pub pgp: Option<Mark>,
    /// Set as soon as an engine is asked about this thread. Either one may
    /// hold a pinentry in front of the person for as long as they take,
    /// and asking twice would put up two of them.
    pub pgp_asked: bool,
    /// The flag colour chosen here, when the thread is flagged.
    pub flag_color: Option<FlagColor>,
    /// Messages translated in this window, by message id. They go no
    /// further than this: a translation is text a model derived, and
    /// tomorrow's model would write it differently.
    pub translations: HashMap<String, Translation>,
    /// Set when the pane shows a queued message rather than a Gmail
    /// thread: what it says above the message, and which buttons.
    pub queued: Option<Unsent>,
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
            thumbnails: HashMap::new(),
            opened_files: HashMap::new(),
            photos: HashMap::new(),
            unsubscribed: false,
            pgp: None,
            pgp_asked: false,
            flag_color: None,
            translations: HashMap::new(),
            queued: None,
        }
    }

    pub fn is_draft(&self) -> bool {
        self.messages
            .last()
            .is_some_and(|m| m.has_label(system_label::DRAFT))
    }

    pub fn starred(&self) -> bool {
        self.messages
            .iter()
            .any(|m| m.has_label(system_label::STARRED))
    }

    pub fn unread(&self) -> bool {
        self.messages.iter().any(|m| m.is_unread())
    }

    pub fn muted(&self) -> bool {
        self.messages
            .iter()
            .any(|m| m.has_label(system_label::MUTE))
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
        self.messages
            .iter()
            .rev()
            .find(|m| !m.has_label(system_label::DRAFT))
    }

    /// The newest message that carries an invitation, with the
    /// `text/calendar` part it arrived in.
    pub fn invitation(&self) -> Option<(&MessageMeta, &str)> {
        self.messages.iter().rev().find_map(|meta| {
            let body = self.bodies.get(&meta.id)?.as_ref().ok()?;
            Some((meta, body.calendar.as_deref()?))
        })
    }

    /// The newest message that arrived signed or encrypted, with the
    /// engine call it needs. A thread holds one such message far more
    /// often than two, and the newest is the one being read.
    pub fn protected(&self) -> Option<(&MessageMeta, Engine)> {
        self.messages.iter().rev().find_map(|meta| {
            let body = self.bodies.get(&meta.id)?.as_ref().ok()?;
            Some((meta, protection::engine(body)?))
        })
    }

    /// That message, claimed for one engine run, with what the engine
    /// needs to read it. `installed` says which engines this computer has,
    /// so a message whose engine is missing is left unclaimed for the day
    /// it turns up. Once per thread: a second call gives nothing back,
    /// because either engine may hold a pinentry in front of the person
    /// for as long as they take and asking twice would put up two of them.
    pub fn take_protected(&mut self, installed: Installed) -> Option<Claimed> {
        if self.pgp_asked {
            return None;
        }
        let (message_id, opening) = {
            let (meta, opening) = self.protected()?;
            (meta.id.clone(), opening)
        };
        if !installed.runs(opening) {
            return None;
        }
        let body = self.bodies.get(&message_id)?.as_ref().ok()?.clone();
        self.pgp_asked = true;
        Some(Claimed {
            target: self.target(),
            message_id,
            opening,
            body,
        })
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

    /// The `List-Unsubscribe` header of the newest message, when it has one.
    pub fn list_unsubscribe(&self) -> Option<(&MessageMeta, &MessageBody)> {
        let target = self.reply_target()?;
        let body = self.bodies.get(&target.id)?.as_ref().ok()?;
        body.list_unsubscribe.is_some().then_some((target, body))
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

    /// Bodies and the inline images that go in them, as Gmail sent them.
    pub fn take_bodies(
        &mut self,
        bodies: Vec<(String, Result<MessageBody, String>)>,
        images: HashMap<String, HashMap<String, String>>,
    ) {
        self.bodies.extend(bodies);
        self.inline_images.extend(images);
    }

    /// What the engine made of the protected message: the mark for the
    /// card, and, when it opened one, the body and the files that were
    /// inside. Answers whether it opened a body.
    pub fn take_engine_answer(&mut self, message_id: String, read: protection::Read) -> bool {
        self.pgp = Some(read.mark);
        let Some(body) = read.body else {
            return false;
        };
        if !read.files.is_empty() {
            self.opened_files.insert(message_id.clone(), read.files);
        }
        self.bodies.insert(message_id, Ok(body));
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

    /// A message's body as it arrived, with its inline images, which is
    /// what a translation is built from.
    pub fn arrived(&self, message_id: &str) -> Option<(MessageBody, HashMap<String, String>)> {
        let body = self.bodies.get(message_id)?.as_ref().ok()?.clone();
        let images = self
            .inline_images
            .get(message_id)
            .cloned()
            .unwrap_or_default();
        Some((body, images))
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

    pub fn has_remote_images(&self) -> bool {
        self.bodies
            .values()
            .filter_map(|b| b.as_ref().ok())
            .filter_map(|b| b.html.as_deref())
            .any(|html| {
                let lower = html.to_ascii_lowercase();
                lower.contains("src=\"http")
                    || lower.contains("src='http")
                    || lower.contains("url(http")
                    || lower.contains("url('http")
                    || lower.contains("url(\"http")
            })
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
            thumbnails: HashMap::new(),
            opened_files: HashMap::new(),
            photos: HashMap::new(),
            unsubscribed: false,
            pgp: None,
            pgp_asked: false,
            flag_color: None,
            translations: HashMap::new(),
            queued: None,
        }
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
