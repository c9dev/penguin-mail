//! The page an open thread draws, and the words a translation reads off it.
//!
//! Four decisions live here rather than in the view: which body each
//! message shows (its translation, the body that arrived, a line saying it
//! is loading, or why it failed), the cleaned HTML kept for each body,
//! which message counts as the one being read, and whether a change needs
//! the whole page loaded again or only some of its articles replaced. The
//! conversation view and the thread run's fake both draw through
//! [`OpenThread::page`], so a test reads the HTML the window would load.
//!
//! Loading the page again costs WebKit most of a second on a long thread
//! and puts the reader back at the top, so a change that leaves the head
//! and the list of messages alone replaces only the `<article>` elements
//! whose HTML changed. [`Drawn`] remembers what the page on screen holds,
//! one hash per article, to tell which.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use mailrs_domain::{MessageBody, MessageMeta};

use super::{InlineImages, OpenThread};
use crate::render::{self, BodyState, Head, MessageView, Sanitized, TAIL, Theme};
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

    /// What the page on screen needs for the thread as it stands, in
    /// `theme`: the whole document, when the head or the list of messages
    /// changed or nothing was drawn yet, and otherwise the articles whose
    /// HTML changed, which may be none. The answer assumes the caller puts
    /// it on screen. Cleaning a long body costs milliseconds, so each
    /// cleaned copy is kept until its body or its pictures change.
    pub fn page(&mut self, theme: &Theme) -> Page {
        self.clean_bodies();
        let head = render::head(
            &Head {
                subject: &self.subject,
                count: self.messages.len(),
                allow_remote: self.images_allowed,
            },
            theme,
        );
        let articles: Vec<Article> = self.messages.iter().map(|meta| self.article(meta)).collect();
        let now = Drawn {
            head: hash(&head),
            articles: articles
                .iter()
                .map(|article| (article.message_id.clone(), hash(&article.html)))
                .collect(),
        };
        let page = match self.drawn.take() {
            Some(before) if before.head == now.head && before.same_messages(&now) => Page::Patch(
                articles
                    .into_iter()
                    .zip(now.articles.iter().zip(&before.articles))
                    .filter(|(_, ((_, fresh), (_, drawn)))| fresh != drawn)
                    .map(|(article, _)| article)
                    .collect(),
            ),
            _ => Page::Whole(Document { head, articles }),
        };
        self.drawn = Some(now);
        page
    }

    /// Forgets what the page on screen holds, so the next [`Self::page`]
    /// is the whole document: WebKit's process went away, or a patch could
    /// not be applied.
    pub fn page_lost(&mut self) {
        self.drawn = None;
    }

    /// Opens or closes one message, which the view does in the page itself
    /// rather than drawing it again: that keeps the reader's place and the
    /// find highlight. The record of what is drawn follows, so the next
    /// page does not replace the article for it. Answers whether the
    /// message is now open.
    pub fn toggle(&mut self, message_id: &str) -> bool {
        let open = !self.expanded.contains(message_id);
        self.set_open(message_id, open);
        open
    }

    /// Opens every message and answers the ones that were closed.
    pub fn open_every_message(&mut self) -> Vec<String> {
        let closed: Vec<String> = self
            .messages
            .iter()
            .map(|m| m.id.clone())
            .filter(|id| !self.expanded.contains(id))
            .collect();
        for id in &closed {
            self.set_open(id, true);
        }
        closed
    }

    /// Closes these messages.
    pub fn close_messages(&mut self, ids: &[String]) {
        for id in ids {
            self.set_open(id, false);
        }
    }

    fn set_open(&mut self, message_id: &str, open: bool) {
        match open {
            true => self.expanded.insert(message_id.to_string()),
            false => self.expanded.remove(message_id),
        };
        let Some(meta) = self.messages.iter().find(|m| m.id == message_id) else {
            return;
        };
        let now = hash(&self.article(meta).html);
        if let Some(drawn) = self.drawn.as_mut()
            && let Some((_, seen)) = drawn.articles.iter_mut().find(|(id, _)| id == message_id)
        {
            *seen = now;
        }
    }

    /// One message's article as the page draws it now.
    fn article(&self, meta: &MessageMeta) -> Article {
        Article {
            message_id: meta.id.clone(),
            html: render::article(&self.view(meta), &self.me, &self.photos),
        }
    }

    /// What the page draws for one message. A message showing its
    /// translation draws the translated body and the translated HTML.
    /// What arrived stays where it was, for the way back.
    fn view<'a>(&'a self, meta: &'a MessageMeta) -> MessageView<'a> {
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

/// What the page on screen needs.
#[derive(Debug, Clone)]
pub enum Page {
    /// Load this document in place of whatever is there.
    Whole(Document),
    /// Put each of these in place of the article with the same message
    /// id, in this order. None means the page is up to date.
    Patch(Vec<Article>),
}

/// A whole page: the head, then one article per message, oldest first.
#[derive(Debug, Clone)]
pub struct Document {
    head: String,
    articles: Vec<Article>,
}

impl Document {
    /// The HTML to load, with `mark` put on the root element as it is,
    /// such as ` data-load="3"`, which tells one load from the next.
    pub fn html(&self, mark: &str) -> String {
        let size = self.articles.iter().map(|a| a.html.len()).sum::<usize>();
        let mut html = String::with_capacity(self.head.len() + size + mark.len() + TAIL.len());
        let root = "<!doctype html><html";
        match self.head.strip_prefix(root) {
            Some(rest) => {
                html.push_str(root);
                html.push_str(mark);
                html.push_str(rest);
            }
            None => html.push_str(&self.head),
        }
        for article in &self.articles {
            html.push_str(&article.html);
        }
        html.push_str(TAIL);
        html
    }

    /// Puts each of `patch` in place of the article with its message id,
    /// as the page on screen does with a patch.
    #[cfg(test)]
    pub fn patch(&mut self, patch: &[Article]) {
        for fresh in patch {
            if let Some(old) = self
                .articles
                .iter_mut()
                .find(|old| old.message_id == fresh.message_id)
            {
                *old = fresh.clone();
            }
        }
    }
}

/// One message's `<article id="m-...">` element, whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Article {
    pub message_id: String,
    pub html: String,
}

/// What the page on screen was last given: a hash of its head, and of each
/// message's article in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Drawn {
    head: u64,
    articles: Vec<(String, u64)>,
}

impl Drawn {
    /// Whether both list the same messages in the same order.
    fn same_messages(&self, other: &Drawn) -> bool {
        self.articles
            .iter()
            .map(|(id, _)| id)
            .eq(other.articles.iter().map(|(id, _)| id))
    }
}

fn hash(html: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    html.hash(&mut hasher);
    hasher.finish()
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
    use super::{Article, Cleaned, Page, body_mark, clean};
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

    /// The document of a page that has to be loaded whole.
    fn whole(page: Page) -> String {
        match page {
            Page::Whole(document) => document.html(""),
            Page::Patch(patch) => panic!("a patch where a whole page was due: {patch:?}"),
        }
    }

    /// The messages a patch replaces.
    fn patched(page: Page) -> Vec<String> {
        match page {
            Page::Patch(patch) => patch.into_iter().map(|a| a.message_id).collect(),
            Page::Whole(_) => panic!("a whole page where a patch was due"),
        }
    }

    #[test]
    fn the_first_page_is_whole_and_an_unchanged_one_patches_nothing() {
        let mut open = thread("<p>Kites</p>");
        assert!(whole(open.page(&theme())).contains("<p>Kites</p>"));
        assert!(patched(open.page(&theme())).is_empty());
    }

    #[test]
    fn a_new_theme_loads_the_whole_page() {
        let mut open = thread("<p>Kites</p>");
        open.page(&theme());
        let dark = Theme {
            dark: true,
            ..theme()
        };
        assert!(whole(open.page(&dark)).contains("color-scheme:dark"));
    }

    /// Letting remote pictures in changes the policy in the head, which no
    /// patch can reach.
    #[test]
    fn allowing_remote_images_loads_the_whole_page() {
        let mut open = thread("<p>Kites</p>");
        open.page(&theme());
        open.images_allowed = true;
        assert!(whole(open.page(&theme())).contains("img-src data: https: http:"));
    }

    #[test]
    fn a_changed_body_patches_its_article_alone() {
        let mut open = thread("<p>Kites</p>");
        let mut second = open.messages[0].clone();
        second.id = "m2".to_string();
        open.messages.push(second);
        open.page(&theme());
        open.bodies.insert(
            "m2".to_string(),
            Ok(MessageBody {
                text: Some("Tomorrow, then".to_string()),
                ..MessageBody::default()
            }),
        );
        let page = open.page(&theme());
        let Page::Patch(patch) = page else {
            panic!("a whole page for one body");
        };
        assert_eq!(patch.len(), 1);
        assert_eq!(patch[0].message_id, "m2");
        assert!(patch[0].html.starts_with("<article class=\"message"));
        assert!(patch[0].html.contains("id=\"m-m2\"") && patch[0].html.contains("Tomorrow, then"));
        assert!(patch[0].html.ends_with("</article>"));
    }

    /// The page opens and closes a message by itself, so the next draw
    /// has nothing to replace for it.
    #[test]
    fn a_message_opened_in_the_page_is_not_replaced_after() {
        let mut open = thread("<p>Kites</p>");
        open.page(&theme());
        assert!(!open.toggle("m1"));
        assert!(patched(open.page(&theme())).is_empty());
        assert_eq!(open.open_every_message(), ["m1"]);
        assert!(patched(open.page(&theme())).is_empty());
    }

    #[test]
    fn a_page_that_was_lost_is_drawn_whole() {
        let mut open = thread("<p>Kites</p>");
        open.page(&theme());
        open.page_lost();
        assert!(whole(open.page(&theme())).contains("<p>Kites</p>"));
    }

    #[test]
    fn a_document_takes_a_patch_where_the_page_does() {
        let mut open = thread("<p>Kites</p>");
        let Page::Whole(mut document) = open.page(&theme()) else {
            panic!("the first page is whole");
        };
        let fresh = Article {
            message_id: "m1".to_string(),
            html: "<article id=\"m-m1\">Kites, again</article>".to_string(),
        };
        document.patch(std::slice::from_ref(&fresh));
        let html = document.html(" data-load=\"2\"");
        assert!(html.starts_with("<!doctype html><html data-load=\"2\""), "{html}");
        assert!(html.contains("Kites, again") && !html.contains("<p>Kites</p>"));
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
        assert!(whole(open.page(&theme())).contains("<p>Cleaned elsewhere</p>"));
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
        let page = whole(open.page(&theme()));
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
        let page = whole(open.page(&theme()));
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
