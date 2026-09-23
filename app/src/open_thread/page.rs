//! The page an open thread draws, and the words a translation reads off it.
//!
//! Three decisions live here rather than in the view: which body each
//! message shows (its translation, the body that arrived, a line saying it
//! is loading, or why it failed), the cleaned HTML kept for each body, and
//! which message counts as the one being read. The conversation view and
//! the thread run's fake both draw through [`OpenThread::page`], so a test
//! reads the HTML the window would load.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use mailrs_domain::MessageBody;

use super::{InlineImages, OpenThread};
use crate::render::{BodyState, Conversation, MessageView, Sanitized, Theme, render};
use crate::sanitize::sanitize_html;
use crate::translation::{Body, Prose};

/// A message body after cleaning, with a mark of the HTML and the inline
/// images it was made from, and what two scans of the cleaned HTML found.
/// A different mark means the body needs cleaning again. The scans each
/// read the whole body in lower case, so they run once, with the
/// cleaning, rather than on every draw.
#[derive(Debug, Clone)]
pub struct Cleaned {
    mark: u64,
    html: String,
    /// Whether it loads anything from the web, which is what the Load
    /// Images banner offers.
    remote: bool,
    /// Whether it chooses its own colours, and so keeps its white page.
    paints: bool,
}

impl Cleaned {
    /// Cleans `html` with the pictures it names. Slow on a long message.
    pub fn new(html: &str, images: &InlineImages) -> Cleaned {
        clean(html, images)
    }
}

/// Cleans the HTML of one body with the pictures it names.
fn clean(source: &str, images: &InlineImages) -> Cleaned {
    let html = sanitize_html(source, images);
    let lower = html.to_ascii_lowercase();
    Cleaned {
        mark: body_mark(source, images),
        remote: loads_remote(&lower),
        paints: paints_itself(&lower),
        html,
    }
}

/// Whether lower-case HTML loads a picture or a background from the web.
fn loads_remote(lower: &str) -> bool {
    ["src=\"http", "src='http", "url(http", "url('http", "url(\"http"]
        .iter()
        .any(|mark| lower.contains(mark))
}

/// Whether lower-case HTML chooses its own colours. Mail that does is
/// written for a white page: a newsletter's white boxes and dark text only
/// read against it. Mail that does not, which is most of what a person
/// writes, takes the window's own colours instead of sitting in a white
/// slab in a dark window.
fn paints_itself(lower: &str) -> bool {
    ["bgcolor=", "background", "color:", "color=", "<table"]
        .iter()
        .any(|mark| lower.contains(mark))
}

/// HTML bodies waiting to be cleaned, each with the pictures it names. A
/// long newsletter takes milliseconds, and a thread of forty of them took
/// the GTK thread 50 ms in a release build, so the thread run's ports
/// gather them here, clean them on a worker thread, and hand the result to
/// [`OpenThread::take_cleaned`].
#[derive(Debug, Default)]
pub struct ToClean(Vec<(String, String, InlineImages)>);

impl ToClean {
    /// The bodies among these with HTML to draw.
    pub fn of<'a>(
        bodies: impl IntoIterator<Item = (&'a String, &'a MessageBody)>,
        images: &HashMap<String, InlineImages>,
    ) -> ToClean {
        ToClean(
            bodies
                .into_iter()
                .filter_map(|(id, body)| {
                    let html = html_of(body)?.to_string();
                    Some((id.clone(), html, images.get(id).cloned().unwrap_or_default()))
                })
                .collect(),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Cleans them all. Slow, so not on the GTK thread.
    pub fn clean(self) -> HashMap<String, Cleaned> {
        self.0
            .into_iter()
            .map(|(id, html, images)| {
                let cleaned = clean(&html, &images);
                (id, cleaned)
            })
            .collect()
    }
}

impl OpenThread {
    /// Keeps cleaned copies made somewhere else, each only while it was
    /// made from the body and the pictures the thread holds now. Anything
    /// left without one is cleaned when the page is next drawn.
    pub fn take_cleaned(&mut self, cleaned: HashMap<String, Cleaned>) {
        for (id, copy) in cleaned {
            if self.mark_of(&id) == Some(copy.mark) {
                self.cleaned.insert(id, copy);
            }
        }
    }

    fn images_of(&self, id: &str) -> InlineImages {
        self.inline_images.get(id).cloned().unwrap_or_default()
    }

    /// The mark a cleaned copy of this message's body would carry now.
    fn mark_of(&self, id: &str) -> Option<u64> {
        let body = self.bodies.get(id)?.as_ref().ok()?;
        Some(body_mark(html_of(body)?, &self.images_of(id)))
    }

    /// The whole document for the thread as it stands, in `theme`.
    /// Cleaning a long body costs milliseconds, so each cleaned copy is
    /// kept until its body or its pictures change.
    pub fn page(&mut self, theme: &Theme) -> String {
        self.clean_bodies();
        let views: Vec<MessageView> = self
            .messages
            .iter()
            .map(|meta| {
                // A message showing its translation draws the translated
                // body and the translated HTML. What arrived stays where
                // it was, for the way back.
                let showing = self
                    .translations
                    .get(&meta.id)
                    .filter(|translation| translation.shown);
                MessageView {
                    meta,
                    body: match (showing, self.bodies.get(&meta.id)) {
                        (Some(translation), _) => BodyState::Loaded(&translation.body),
                        (None, None) => BodyState::Loading,
                        (None, Some(Ok(body))) => BodyState::Loaded(body),
                        (None, Some(Err(reason))) => BodyState::Failed(reason),
                    },
                    expanded: self.expanded.contains(&meta.id),
                    thumbnails: &self.thumbnails,
                    sanitized: match showing {
                        Some(translation) => translation.clean.as_ref(),
                        None => self.cleaned.get(&meta.id),
                    }
                    .map(|cleaned| Sanitized {
                        html: &cleaned.html,
                        paints: cleaned.paints,
                    }),
                }
            })
            .collect();
        render(
            &Conversation {
                subject: &self.subject,
                messages: views,
                me: &self.me,
                photos: &self.photos,
                allow_remote: self.images_allowed,
            },
            theme,
        )
    }

    /// The message a translation applies to, with the prose the page
    /// draws for it: the newest open message whose body has arrived. The
    /// HTML is the cleaned copy, so the words come out of the markup the
    /// reader is looking at.
    pub fn prose(&self) -> Option<(String, Prose)> {
        let meta = self.messages.iter().rev().find(|meta| {
            self.expanded.contains(&meta.id) && self.bodies.get(&meta.id).is_some_and(Result::is_ok)
        })?;
        let body = self.bodies.get(&meta.id)?.as_ref().ok()?;
        let prose = match self.cleaned.get(&meta.id) {
            Some(cleaned) => Prose::read(Body::Html(&cleaned.html)),
            None => Prose::read(Body::Text(body.text.as_deref().unwrap_or(""))),
        };
        Some((meta.id.clone(), prose))
    }

    /// Whether a body the page drew loads anything from the web.
    pub fn has_remote_images(&self) -> bool {
        self.cleaned.values().any(|cleaned| cleaned.remote)
    }

    /// Cleans every HTML body whose cleaned copy is missing or was made
    /// from something else, and forgets the copies of bodies that left.
    fn clean_bodies(&mut self) {
        let bodies = &self.bodies;
        self.cleaned.retain(|id, _| bodies.contains_key(id));
        let stale: Vec<String> = self
            .messages
            .iter()
            .filter(|meta| {
                let now = self.mark_of(&meta.id);
                now.is_some() && self.cleaned.get(&meta.id).map(|seen| seen.mark) != now
            })
            .map(|meta| meta.id.clone())
            .collect();
        let bodies = stale.iter().filter_map(|id| {
            let body = self.bodies.get(id)?.as_ref().ok()?;
            Some((id, body))
        });
        let cleaned = ToClean::of(bodies, &self.inline_images).clean();
        self.cleaned.extend(cleaned);
    }
}

/// The HTML part of a body, when it has one worth drawing.
fn html_of(body: &MessageBody) -> Option<&str> {
    body.html.as_deref().filter(|h| !h.trim().is_empty())
}

/// One number standing for the HTML and the inline images a cleaned body
/// was made from, so the cleaned copy is thrown away as soon as either
/// changes. It reads the whole body rather than its length, because two
/// bodies of the same length are still two bodies: opening an encrypted
/// message puts a different body under the same message id, and the
/// reader would otherwise go on looking at the cleaned ciphertext.
fn body_mark(html: &str, images: &HashMap<String, String>) -> u64 {
    let mut whole = DefaultHasher::new();
    html.hash(&mut whole);
    // A HashMap hands its entries back in whatever order it likes, so each
    // one is hashed on its own and the results mixed with xor, which
    // answers the same whichever order they come in.
    let mixed = images.iter().fold(0, |mixed, (cid, uri)| {
        let mut each = DefaultHasher::new();
        cid.hash(&mut each);
        uri.hash(&mut each);
        mixed ^ each.finish()
    });
    mixed.hash(&mut whole);
    whole.finish()
}

#[cfg(test)]
mod tests {
    use super::{Cleaned, body_mark, clean};
    use crate::open_thread::OpenThread;
    use crate::render::Theme;
    use mailrs_domain::{MessageBody, MessageMeta, Target};
    use std::collections::HashMap;

    fn theme() -> Theme {
        Theme {
            dark: false,
            accent: "#3584e4".to_string(),
        }
    }

    /// A thread of one open message whose body is this HTML.
    fn thread(html: &str) -> OpenThread {
        let meta = MessageMeta {
            account_id: 1,
            id: "m1".to_string(),
            thread_id: "t1".to_string(),
            rfc822_msgid: None,
            from: None,
            to: Vec::new(),
            cc: Vec::new(),
            subject: "Kites".to_string(),
            date: 0,
            snippet: String::new(),
            size: 0,
            has_attachments: false,
            label_ids: Vec::new(),
            list_unsubscribe: None,
            one_click: false,
        };
        let body = MessageBody {
            html: Some(html.to_string()),
            ..MessageBody::default()
        };
        OpenThread::new(
            &Target::thread(1, "t1"),
            "Kites".to_string(),
            vec![meta],
            HashMap::from([("m1".to_string(), body)]),
            Vec::new(),
        )
    }

    /// A cleaned copy made elsewhere from this body is drawn as it is:
    /// the page does not clean the body a second time.
    #[test]
    fn a_body_cleaned_elsewhere_is_not_cleaned_again() {
        let mut open = thread("<p>Kites</p>");
        let mut made = clean("<p>Kites</p>", &Default::default());
        made.html = "<p>Cleaned elsewhere</p>".to_string();
        open.take_cleaned(HashMap::from([("m1".to_string(), made)]));
        assert!(open.page(&theme()).contains("<p>Cleaned elsewhere</p>"));
    }

    /// Opening an encrypted message puts a new body under the same id; a
    /// copy cleaned from the old one must not stand in for it.
    #[test]
    fn a_copy_cleaned_from_another_body_is_turned_away() {
        let mut open = thread("<p>Kites</p>");
        let stale = Cleaned {
            html: "<p>Ciphertext</p>".to_string(),
            ..clean("<p>Ciphertext</p>", &Default::default())
        };
        open.take_cleaned(HashMap::from([("m1".to_string(), stale)]));
        let page = open.page(&theme());
        assert!(page.contains("<p>Kites</p>") && !page.contains("Ciphertext"));
    }

    /// What the scans find is kept with the cleaned copy: a cleaned copy
    /// that says so draws on its own white page, whatever its words.
    #[test]
    fn the_page_trusts_what_cleaning_found() {
        let mut open = thread("<p>Kites</p>");
        let mut made = clean("<p>Kites</p>", &Default::default());
        assert!(!made.paints && !made.remote);
        made.paints = true;
        open.take_cleaned(HashMap::from([("m1".to_string(), made)]));
        let page = open.page(&theme());
        assert!(page.contains("body html\""), "{page}");
    }

    #[test]
    fn a_newsletter_paints_itself_and_a_note_does_not() {
        let note = clean("<div dir=\"ltr\">Monday works.</div>", &Default::default());
        let sale = clean(
            "<table bgcolor=\"#ffffff\"><tr><td>Sale</td></tr></table>",
            &Default::default(),
        );
        assert!(!note.paints && sale.paints);
    }

    /// A remote picture a sender left inside a comment never reaches the
    /// page, so it is no reason to offer Load Images.
    #[test]
    fn remote_images_are_looked_for_in_what_the_page_draws() {
        let hidden = clean(
            "<p>Hi</p><!-- <img src=\"https://tracker.example/p.gif\"> -->",
            &Default::default(),
        );
        assert!(!hidden.remote);
        for shown in [
            "<img src='https://news.example/a.png'>",
            "<div style=\"background:url(https://news.example/b.png)\">x</div>",
        ] {
            assert!(clean(shown, &Default::default()).remote, "{shown}");
        }
        let mut open = thread("<p>Hi</p><!-- <img src=\"https://tracker.example/p.gif\"> -->");
        open.page(&theme());
        assert!(!open.has_remote_images());
    }

    fn images(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(cid, uri)| (cid.to_string(), uri.to_string()))
            .collect()
    }

    #[test]
    fn a_body_that_did_not_change_keeps_its_cleaned_copy() {
        let pictures = images(&[("cid1", "data:image/png;base64,AAAA")]);
        assert_eq!(
            body_mark("<p>Hello</p>", &pictures),
            body_mark("<p>Hello</p>", &pictures)
        );
    }

    #[test]
    fn two_bodies_of_the_same_length_are_two_bodies() {
        let pictures = images(&[("cid1", "data:image/png;base64,AAAA")]);
        assert_ne!(
            body_mark("<p>Hello</p>", &pictures),
            body_mark("<p>Howdy</p>", &pictures)
        );
    }

    #[test]
    fn an_image_that_changed_is_a_new_body() {
        assert_ne!(
            body_mark(
                "<p>Hello</p>",
                &images(&[("cid1", "data:image/png;base64,AAAA")])
            ),
            body_mark(
                "<p>Hello</p>",
                &images(&[("cid1", "data:image/png;base64,BBBB")])
            )
        );
        assert_ne!(
            body_mark(
                "<p>Hello</p>",
                &images(&[("cid1", "data:image/png;base64,AAAA")])
            ),
            body_mark(
                "<p>Hello</p>",
                &images(&[("cid2", "data:image/png;base64,AAAA")])
            )
        );
        assert_ne!(
            body_mark("<p>Hello</p>", &images(&[])),
            body_mark(
                "<p>Hello</p>",
                &images(&[("cid1", "data:image/png;base64,AAAA")])
            )
        );
    }

    #[test]
    fn the_order_the_images_arrived_in_says_nothing() {
        let one = images(&[("cid1", "first"), ("cid2", "second")]);
        let other = images(&[("cid2", "second"), ("cid1", "first")]);
        assert_eq!(
            body_mark("<p>Hello</p>", &one),
            body_mark("<p>Hello</p>", &other)
        );
    }
}
