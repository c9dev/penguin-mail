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
//! whose inputs changed. [`Drawn`] keeps each article with what it was
//! built from, so a change to one message builds one article again and
//! the rest cost a comparison.
//!
//! Drawing never cleans a body. Cleaning a long newsletter takes
//! milliseconds, so the thread run sends what [`OpenThread::to_clean`]
//! names through its effect port, away from the GTK thread, and an HTML
//! body says it is loading until its cleaned copy comes back.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::NaiveDate;
use mailrs_domain::{AccountId, MessageBody, MessageMeta};

use super::{OpenThread, inline};
use crate::quoted;
use crate::render::{self, BodyState, Face, Head, MessageView, Receipt, Sanitized, TAIL, Theme};
use crate::sanitize::sanitize_html;
use crate::translation::{Body, Prose};

/// A message body after cleaning, with the body and the version of its
/// pictures it was made from, and what three scans of the cleaned HTML
/// found. The scans each read the whole body, so they run once, with the
/// cleaning, rather than on every draw.
#[derive(Debug, Clone)]
pub struct Cleaned {
    /// The body this was cleaned from. A copy counts only while the thread
    /// holds that same body: opening an encrypted message puts a new one
    /// under the same id, and the reader must not go on looking at the
    /// cleaned ciphertext. A translation's words come from no body.
    of: Option<Arc<MessageBody>>,
    /// The version of the message's pictures its addresses point at.
    version: u32,
    /// Tells this copy from every other one, for the article drawn from it.
    stamp: u64,
    html: String,
    /// Whether it loads anything from the web, which is what the Load
    /// Images banner offers.
    remote: bool,
    /// Whether it chooses its own colours, and so keeps its white page.
    paints: bool,
    /// The quoted history at its end, which the page folds away.
    history: Option<Range<usize>>,
    /// Cleaning this body failed. The copy stands for that, so the same
    /// body is not cleaned again, and the page falls back to its text.
    failed: bool,
}

impl Cleaned {
    /// What stands in for a body that could not be cleaned.
    fn failure(of: Option<Arc<MessageBody>>, version: u32) -> Cleaned {
        Cleaned {
            of,
            version,
            stamp: STAMPS.fetch_add(1, Ordering::Relaxed),
            html: String::new(),
            remote: false,
            paints: false,
            history: None,
            failed: true,
        }
    }

    /// Whether cleaning failed, so there is no HTML to draw.
    pub fn is_failed(&self) -> bool {
        self.failed
    }
}

/// Where the next cleaned copy's stamp comes from. Copies are made on
/// worker threads.
static STAMPS: AtomicU64 = AtomicU64::new(1);

/// Cleans `source`, pointing each picture it names by `cid:` at an
/// address that starts with `pictures`. Slow on a long message.
fn clean(source: &str, pictures: &str, of: Option<Arc<MessageBody>>, version: u32) -> Cleaned {
    let html = sanitize_html(source, Some(pictures));
    let lower = html.to_ascii_lowercase();
    Cleaned {
        of,
        version,
        stamp: STAMPS.fetch_add(1, Ordering::Relaxed),
        remote: loads_remote(&lower),
        paints: paints_itself(&lower),
        history: quoted::history_in_html(&html),
        html,
        failed: false,
    }
}

/// Whether lower-case HTML loads a picture or a background from the web.
fn loads_remote(lower: &str) -> bool {
    [
        "src=\"http",
        "src='http",
        "url(http",
        "url('http",
        "url(\"http",
    ]
    .iter()
    .any(|mark| lower.contains(mark))
}

/// Whether lower-case HTML chooses its own colours. Mail that does is
/// written for a white page: a newsletter's white boxes and dark text only
/// read against it. Mail that does not, which is most of what a person
/// writes, takes the window's own colours instead of sitting in a white
/// slab in a dark window.
///
/// A quote's own grey and a border's colour don't count: a reply's
/// quoted history carries them in every mail client, and it is still a
/// note.
fn paints_itself(lower: &str) -> bool {
    let lower = without_quote_tags(lower);
    let text_colour = ["color:", "color="].iter().any(|mark| {
        lower
            .match_indices(mark)
            .any(|(at, _)| !lower[..at].ends_with('-'))
    });
    text_colour
        || ["bgcolor=", "background", "<table"]
            .iter()
            .any(|mark| lower.contains(mark))
}

/// `lower` with each `<blockquote ...>` opening tag cut down to its name,
/// so the style a quote carries is left out of [`paints_itself`].
fn without_quote_tags(lower: &str) -> String {
    let mut out = String::with_capacity(lower.len());
    let mut rest = lower;
    while let Some(at) = rest.find("<blockquote") {
        out.push_str(&rest[..at]);
        out.push_str("<blockquote>");
        rest = &rest[at..];
        rest = rest.find('>').map_or("", |end| &rest[end + 1..]);
    }
    out.push_str(rest);
    out
}

/// HTML waiting to be cleaned, each with the start of its picture
/// addresses. A long newsletter takes milliseconds, and a thread of forty
/// of them took the GTK thread 50 ms in a release build, so the thread run
/// hands these to its effect port, which cleans them on a worker thread,
/// and gives the result to [`OpenThread::take_cleaned`].
#[derive(Debug, Default)]
pub struct ToClean(Vec<Dirty>);

#[derive(Debug)]
struct Dirty {
    message_id: String,
    source: Source,
    version: u32,
    pictures: String,
}

impl Dirty {
    /// The body this is cleaned from, as a copy of it will record.
    fn of(&self) -> Option<Arc<MessageBody>> {
        match &self.source {
            Source::Body(body) => Some(Arc::clone(body)),
            Source::Words(_) => None,
        }
    }
}

#[derive(Debug)]
enum Source {
    /// A body as it arrived, shared with the thread rather than copied.
    Body(Arc<MessageBody>),
    /// HTML made here, such as a translation.
    Words(String),
}

impl ToClean {
    /// The bodies among these with HTML to draw, as the account's store or
    /// Gmail sent them, which is the first version of each.
    pub fn of<'a>(
        account_id: AccountId,
        bodies: impl IntoIterator<Item = (&'a String, &'a Arc<MessageBody>)>,
    ) -> ToClean {
        ToClean(
            bodies
                .into_iter()
                .filter(|(_, body)| html_of(body).is_some())
                .map(|(id, body)| Dirty {
                    message_id: id.clone(),
                    source: Source::Body(Arc::clone(body)),
                    version: 0,
                    pictures: inline::prefix(account_id, id, 0),
                })
                .collect(),
        )
    }

    /// HTML made for one message, such as the words a model translated,
    /// with the start of the addresses of the pictures it names.
    pub fn words(message_id: String, html: String, pictures: String) -> ToClean {
        ToClean(vec![Dirty {
            message_id,
            source: Source::Words(html),
            version: 0,
            pictures,
        }])
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Cleans them all. Slow, so not on the GTK thread. A body the
    /// sanitizer panics on comes back as a failed copy, so one bad body
    /// does not take the others with it. This needs panics to unwind;
    /// `no_profile_aborts_on_panic` keeps `panic = "abort"` out.
    pub fn clean(self) -> HashMap<String, Cleaned> {
        self.0
            .into_iter()
            .map(|dirty| {
                let (of, version) = (dirty.of(), dirty.version);
                let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match dirty.source {
                    Source::Body(body) => {
                        let html = html_of(&body).unwrap_or_default().to_string();
                        clean(&html, &dirty.pictures, Some(body), dirty.version)
                    }
                    Source::Words(html) => clean(&html, &dirty.pictures, None, dirty.version),
                }));
                let cleaned = made.unwrap_or_else(|_| {
                    tracing::warn!(message = %dirty.message_id, "the sanitizer failed on a message body");
                    Cleaned::failure(of, version)
                });
                (dirty.message_id, cleaned)
            })
            .collect()
    }

    /// What it takes to make failed copies of all of them once these have
    /// gone to a worker, without making the copies yet: a worker almost
    /// never fails to answer.
    pub fn stand_ins(&self) -> StandIns {
        StandIns(
            self.0
                .iter()
                .map(|dirty| (dirty.message_id.clone(), dirty.of(), dirty.version))
                .collect(),
        )
    }

    /// Failed copies for all of them, for a worker that never answered.
    #[cfg(test)]
    pub fn failures(&self) -> HashMap<String, Cleaned> {
        self.stand_ins().failures()
    }
}

/// The message, body and version of each body sent to be cleaned.
pub struct StandIns(Vec<(String, Option<Arc<MessageBody>>, u32)>);

impl StandIns {
    /// Failed copies for all of them, for a worker that never answered.
    /// Taken in place of the cleaned ones, they stop the same bodies from
    /// being sent out again.
    pub fn failures(self) -> HashMap<String, Cleaned> {
        self.0
            .into_iter()
            .map(|(id, of, version)| (id, Cleaned::failure(of, version)))
            .collect()
    }
}

impl OpenThread {
    /// Keeps cleaned copies made somewhere else, each only while it was
    /// made from the body and the version of its pictures the thread holds
    /// now. A body left without one stays loading until [`Self::to_clean`]
    /// names it again.
    pub fn take_cleaned(&mut self, cleaned: HashMap<String, Cleaned>) {
        for (id, copy) in cleaned {
            if self.is_fresh(&id, &copy) {
                self.cleaned.insert(id, copy);
            }
        }
    }

    /// The HTML bodies on screen with no cleaned copy of the body and the
    /// pictures the thread holds now.
    pub fn to_clean(&self) -> ToClean {
        ToClean(
            self.messages
                .iter()
                .filter_map(|meta| {
                    let Some(Ok(body)) = self.bodies.get(&meta.id) else {
                        return None;
                    };
                    if html_of(body).is_none() || self.cleaned_of(&meta.id).is_some() {
                        return None;
                    }
                    Some(Dirty {
                        message_id: meta.id.clone(),
                        source: Source::Body(Arc::clone(body)),
                        version: self.version_of(&meta.id),
                        pictures: self.picture_prefix(&meta.id),
                    })
                })
                .collect(),
        )
    }

    /// Whether `copy` was cleaned from the body this message holds now,
    /// with its pictures at the version they are at now.
    fn is_fresh(&self, id: &str, copy: &Cleaned) -> bool {
        let Some(Ok(body)) = self.bodies.get(id) else {
            return false;
        };
        copy.of.as_ref().is_some_and(|of| Arc::ptr_eq(of, body))
            && copy.version == self.version_of(id)
    }

    /// The cleaned copy of this message's body, while it still is one.
    fn cleaned_of(&self, id: &str) -> Option<&Cleaned> {
        self.cleaned.get(id).filter(|copy| self.is_fresh(id, copy))
    }

    /// What the page on screen needs for the thread as it stands, in
    /// `theme`: the whole document, when the head or the list of messages
    /// changed or nothing was drawn yet, and otherwise the articles whose
    /// inputs changed, which may be none. The answer assumes the caller
    /// puts it on screen.
    pub fn page(&mut self, theme: &Theme) -> Page {
        let mut drawn = std::mem::take(&mut self.drawn);
        let head = HeadInputs {
            subject: self.subject.clone(),
            count: self.messages.len(),
            allow_remote: self.images_allowed,
            theme: theme.clone(),
        };
        let (head_kept, head_html) = match drawn.head.take() {
            Some((before, html)) if before == head => (true, html),
            _ => {
                let html: Rc<str> = render::head(
                    &Head {
                        subject: &self.subject,
                        count: self.messages.len(),
                        allow_remote: self.images_allowed,
                    },
                    theme,
                )
                .into();
                (false, html)
            }
        };
        drawn.head = Some((head, Rc::clone(&head_html)));
        // Every article says "me" for the person's addresses and dates
        // itself against today.
        let today = chrono::Local::now().date_naive();
        if drawn
            .common
            .as_ref()
            .is_none_or(|(me, day)| *me != self.me || *day != today)
        {
            drawn.articles.clear();
            drawn.common = Some((self.me.clone(), today));
        }
        let same_messages = drawn
            .shown
            .as_ref()
            .is_some_and(|ids| ids.iter().eq(self.messages.iter().map(|m| &m.id)));
        let mut before = std::mem::take(&mut drawn.articles);
        let mut articles = Vec::with_capacity(self.messages.len());
        let mut changed = Vec::new();
        let mut earlier = Vec::with_capacity(self.messages.len());
        for meta in &self.messages {
            let address = render::sender_address(meta);
            let face = render::face(&self.photos, address, &earlier);
            earlier.push(address);
            let inputs = self.inputs(meta, face);
            let article = match before.remove(&meta.id) {
                Some((kept, article)) if kept == inputs => article,
                _ => {
                    let article = self.build(meta, face);
                    changed.push(article.clone());
                    article
                }
            };
            drawn
                .articles
                .insert(meta.id.clone(), (inputs, article.clone()));
            articles.push(article);
        }
        drawn.shown = Some(self.messages.iter().map(|m| m.id.clone()).collect());
        let page = match head_kept && same_messages {
            true => Page::Patch(changed),
            false => Page::Whole(Document {
                head: head_html,
                articles,
            }),
        };
        self.drawn = drawn;
        page
    }

    /// Puts the event card in the message `message_id` names, or takes it
    /// out of the page with `None`. The next [`Self::page`] patches the
    /// articles that gain or lose the card's place.
    pub fn take_invitation_place(&mut self, message_id: Option<String>) {
        self.invitation_at = message_id;
    }

    /// Forgets what the page on screen holds, so the next [`Self::page`]
    /// is the whole document: WebKit's process went away, or a patch could
    /// not be applied. The articles built for it are still good.
    pub fn page_lost(&mut self) {
        self.drawn.shown = None;
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

    /// Opens or closes a message, and builds its article again when that
    /// is all that changed in it, since the page did the same in place. An
    /// article with other changes waiting keeps them for the next page.
    fn set_open(&mut self, message_id: &str, open: bool) {
        match open {
            true => self.expanded.insert(message_id.to_string()),
            false => self.expanded.remove(message_id),
        };
        let Some(at) = self.messages.iter().position(|m| m.id == message_id) else {
            return;
        };
        let Some((kept, _)) = self.drawn.articles.get(message_id) else {
            return;
        };
        let meta = &self.messages[at];
        let earlier: Vec<&str> = self.messages[..at]
            .iter()
            .map(render::sender_address)
            .collect();
        let face = render::face(&self.photos, render::sender_address(meta), &earlier);
        let now = self.inputs(meta, face);
        if !kept.same_but_open(&now) {
            return;
        }
        let article = self.build(meta, face);
        self.drawn
            .articles
            .insert(message_id.to_string(), (now, article));
    }

    /// What one message's article is built from now.
    fn inputs(&self, meta: &MessageMeta, face: Option<Face>) -> Inputs {
        let (body, thumbnails) = match self.showing(&meta.id) {
            Showing::Loading => (Shows::Loading, 0),
            Showing::Failed(reason) => (Shows::Failed(reason.to_string()), 0),
            Showing::Unreadable => (Shows::Unreadable, 0),
            Showing::Body(body, cleaned) => {
                let mut hasher = DefaultHasher::new();
                // Which picture each row has, not only whether it has one:
                // a replaced picture draws the article again.
                for attachment in &body.attachments {
                    attachment
                        .attachment_id
                        .as_deref()
                        .and_then(|id| self.thumbnails.get(id))
                        .hash(&mut hasher);
                }
                let stamp = cleaned.map(|copy| copy.stamp);
                (Shows::Body(Arc::clone(body), stamp), hasher.finish())
            }
        };
        Inputs {
            meta: meta.clone(),
            body,
            expanded: self.expanded.contains(&meta.id),
            face: face.map(|face| {
                let (uri, paints) = face.photo();
                (Rc::clone(uri), paints)
            }),
            thumbnails,
            event_slot: self.invitation_at.as_deref() == Some(meta.id.as_str()),
            receipt: match self.receipt_view(meta) {
                None => Asks::Nothing,
                Some(Receipt::Noted) => Asks::Noted,
                Some(Receipt::Asked { to, working }) => Asks::Card(to.to_string(), working),
            },
            read_by: self.read_by(&meta.id).is_some(),
        }
    }

    /// One message's article as the page draws it now.
    fn build(&self, meta: &MessageMeta, face: Option<Face>) -> Article {
        Article {
            message_id: meta.id.clone(),
            html: render::article(&self.view(meta), &self.me, face).into(),
        }
    }

    /// The body the page draws for a message. A message showing its
    /// translation draws the translated body and the translated HTML; what
    /// arrived stays where it was, for the way back. An HTML body with no
    /// cleaned copy yet is still loading.
    fn showing(&self, id: &str) -> Showing<'_> {
        let translated = self
            .translations
            .get(id)
            .filter(|translation| translation.shown);
        let (body, cleaned) = match (translated, self.bodies.get(id)) {
            (Some(translation), _) => (&translation.body, translation.clean.as_ref()),
            (None, None) => return Showing::Loading,
            (None, Some(Err(reason))) => return Showing::Failed(reason),
            (None, Some(Ok(body))) => (body, self.cleaned_of(id)),
        };
        if cleaned.is_some_and(Cleaned::is_failed) {
            // Its text part, when it has one, in place of the HTML.
            return match body
                .text
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
            {
                true => Showing::Body(body, None),
                false => Showing::Unreadable,
            };
        }
        match html_of(body).is_some() && cleaned.is_none() {
            true => Showing::Loading,
            false => Showing::Body(body, cleaned),
        }
    }

    /// What the page draws for one message.
    fn view<'a>(&'a self, meta: &'a MessageMeta) -> MessageView<'a> {
        let (body, cleaned) = match self.showing(&meta.id) {
            Showing::Loading => (BodyState::Loading, None),
            Showing::Failed(reason) => (BodyState::Failed(reason), None),
            Showing::Unreadable => (BodyState::Unreadable, None),
            Showing::Body(body, cleaned) => (BodyState::Loaded(body), cleaned),
        };
        MessageView {
            meta,
            body,
            expanded: self.expanded.contains(&meta.id),
            thumbnails: &self.thumbnails,
            sanitized: cleaned.map(|cleaned| Sanitized {
                html: &cleaned.html,
                paints: cleaned.paints,
                history: cleaned.history.clone(),
            }),
            event_slot: self.invitation_at.as_deref() == Some(meta.id.as_str()),
            receipt: self.receipt_view(meta),
            read_by: self.read_by(&meta.id),
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
        let prose = match self.cleaned_of(&meta.id).filter(|cleaned| !cleaned.failed) {
            Some(cleaned) => Prose::read(Body::Html(&cleaned.html)),
            None => Prose::read(Body::Text(body.text.as_deref().unwrap_or(""))),
        };
        Some((meta.id.clone(), prose))
    }

    /// Whether a body the page drew loads anything from the web.
    pub fn has_remote_images(&self) -> bool {
        self.messages.iter().any(|meta| {
            self.cleaned_of(&meta.id)
                .is_some_and(|cleaned| cleaned.remote)
        })
    }
}

/// What the page draws for a message's body.
enum Showing<'a> {
    Loading,
    Failed(&'a str),
    /// Cleaning failed and the message has no text to fall back to.
    Unreadable,
    Body(&'a Arc<MessageBody>, Option<&'a Cleaned>),
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
    head: Rc<str>,
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

/// One message's `<article id="m-...">` element, whole. The HTML is shared
/// with the page's record of what it drew, so a whole page costs no copy
/// of an article that did not change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Article {
    pub message_id: String,
    pub html: Rc<str>,
}

/// What the head is built from.
#[derive(Debug, Clone, PartialEq)]
struct HeadInputs {
    subject: String,
    count: usize,
    allow_remote: bool,
    theme: Theme,
}

/// What one article is built from. Equal inputs build the same HTML, so an
/// article is built again only when its inputs change. Bodies, cleaned
/// copies and photos count by identity, which costs nothing to compare,
/// where hashing every body on each draw cost about as much as drawing.
#[derive(Debug, Clone)]
struct Inputs {
    meta: MessageMeta,
    body: Shows,
    expanded: bool,
    /// The photo and whether this article carries the rule that paints it.
    face: Option<(Rc<str>, bool)>,
    /// Which attachment rows have their picture.
    thumbnails: u64,
    event_slot: bool,
    receipt: Asks,
    read_by: bool,
}

impl PartialEq for Inputs {
    fn eq(&self, other: &Inputs) -> bool {
        let same_face = match (&self.face, &other.face) {
            (Some((a, x)), Some((b, y))) => Rc::ptr_eq(a, b) && x == y,
            (None, None) => true,
            _ => false,
        };
        same_face
            && self.body == other.body
            && self.expanded == other.expanded
            && self.thumbnails == other.thumbnails
            && self.event_slot == other.event_slot
            && self.receipt == other.receipt
            && self.read_by == other.read_by
            && self.meta == other.meta
    }
}

impl Inputs {
    /// Whether these and `now` differ in whether the message is open alone.
    fn same_but_open(&self, now: &Inputs) -> bool {
        let mut open = self.clone();
        open.expanded = now.expanded;
        open == *now
    }
}

/// The body an article draws, by identity.
#[derive(Debug, Clone)]
enum Shows {
    Loading,
    Failed(String),
    Unreadable,
    /// A body and the stamp of the cleaned copy drawn for it.
    Body(Arc<MessageBody>, Option<u64>),
}

impl PartialEq for Shows {
    fn eq(&self, other: &Shows) -> bool {
        match (self, other) {
            (Shows::Loading, Shows::Loading) | (Shows::Unreadable, Shows::Unreadable) => true,
            (Shows::Failed(a), Shows::Failed(b)) => a == b,
            (Shows::Body(a, x), Shows::Body(b, y)) => Arc::ptr_eq(a, b) && x == y,
            _ => false,
        }
    }
}

/// What an article says about a read receipt its sender asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Asks {
    Nothing,
    Noted,
    /// The card, asking about a receipt to this address, and whether its
    /// answer is on its way.
    Card(String, bool),
}

/// What the page on screen was built from, and what it holds.
#[derive(Debug, Default)]
pub(super) struct Drawn {
    /// The head as last built, with what it was built from.
    head: Option<(HeadInputs, Rc<str>)>,
    /// Each message's article as last built, with what it was built from.
    articles: HashMap<String, (Inputs, Article)>,
    /// What every article depends on: the person's addresses, and the day
    /// its dates are written against.
    common: Option<(Vec<String>, NaiveDate)>,
    /// The messages on the page on screen, in order. `None` before the
    /// first page and after one was lost, which loads the next one whole.
    shown: Option<Vec<String>>,
}

/// The HTML part of a body, when it has one worth drawing.
fn html_of(body: &MessageBody) -> Option<&str> {
    body.html.as_deref().filter(|h| !h.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::{Article, Cleaned, Page};
    use crate::open_thread::OpenThread;
    use crate::render::Theme;
    use mailrs_domain::{Address, MessageBody, MessageMeta, Target};
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::Arc;

    /// `html` cleaned on its own, for the tests of what cleaning finds.
    fn clean(html: &str) -> Cleaned {
        super::clean(html, "mailrs-cid:1/m1/0/", None, 0)
    }

    fn theme() -> Theme {
        Theme {
            dark: false,
            accent: "#3584e4".to_string(),
            accent_text: "#1a5fb4".to_string(),
            summarize: false,
            colors: None,
            font: String::new(),
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

    /// Cleaning is slow and belongs off the GTK thread, so drawing never
    /// does it: an HTML body says it is loading until its copy arrives.
    #[test]
    fn an_html_body_is_loading_until_its_cleaned_copy_arrives() {
        let mut open = uncleaned("<p>Kites</p>");
        let page = whole(open.page(&theme()));
        assert!(
            page.contains("Loading…") && !page.contains("Kites</p>"),
            "{page}"
        );
        let cleaned = open.to_clean().clean();
        open.take_cleaned(cleaned);
        assert_eq!(patched(open.page(&theme())), ["m1"]);
        assert!(open.to_clean().is_empty());
    }

    /// A thread of one message whose body has a text part and one
    /// attachment, `a1`.
    fn with_attachment(html: &str, text: Option<&str>) -> OpenThread {
        let mut open = uncleaned(html);
        let mut body = MessageBody::clone(open.bodies["m1"].as_ref().unwrap());
        body.text = text.map(str::to_string);
        body.attachments = vec![mailrs_domain::Attachment {
            part_id: "2".to_string(),
            filename: "kite.png".to_string(),
            mime_type: "image/png".to_string(),
            size: 10,
            attachment_id: Some("a1".to_string()),
            content_id: None,
        }];
        open.bodies.insert("m1".to_string(), Ok(Arc::new(body)));
        open
    }

    #[test]
    fn replacing_an_attachment_picture_draws_its_article_again() {
        let mut open = with_attachment("<p>Kites</p>", None);
        let cleaned = open.to_clean().clean();
        open.take_cleaned(cleaned);
        open.thumbnails
            .insert("a1".to_string(), "data:image/png;base64,AAAA".to_string());
        open.page(&theme());
        assert!(patched(open.page(&theme())).is_empty(), "nothing changed");
        open.thumbnails
            .insert("a1".to_string(), "data:image/png;base64,BBBB".to_string());
        assert_eq!(patched(open.page(&theme())), ["m1"]);
    }

    #[test]
    fn a_body_that_could_not_be_cleaned_shows_its_text_and_is_not_cleaned_again() {
        let mut open = with_attachment("<p>Kites</p>", Some("Kites at ten"));
        let failed = open.to_clean().failures();
        open.take_cleaned(failed);
        let page = whole(open.page(&theme()));
        assert!(
            page.contains("Kites at ten") && !page.contains("Loading…"),
            "{page}"
        );
        assert!(
            open.to_clean().is_empty(),
            "the same body is not sent out again"
        );
    }

    /// `ToClean::clean` and `compose::off_thread` catch a panic so one bad
    /// body or build does not end the app. With `panic = "abort"` in a
    /// profile they would catch nothing, and tests, which always unwind,
    /// would not notice; so the manifests are read instead.
    #[test]
    fn no_profile_aborts_on_panic() {
        for manifest in [
            include_str!("../../../Cargo.toml"),
            include_str!("../../Cargo.toml"),
        ] {
            let aborts = manifest
                .lines()
                .map(|line| line.split('#').next().unwrap_or_default().replace(' ', ""))
                .any(|line| line.starts_with("panic=") && line.contains("abort"));
            assert!(!aborts, "catch_unwind needs panics to unwind");
        }
    }

    #[test]
    fn a_body_with_no_text_that_could_not_be_cleaned_says_so() {
        let mut open = with_attachment("<p>Kites</p>", None);
        let failed = open.to_clean().failures();
        open.take_cleaned(failed);
        let page = whole(open.page(&theme()));
        assert!(
            page.contains("This message could not be displayed"),
            "{page}"
        );
        assert!(!page.contains("Loading…"), "{page}");
    }

    /// A thread of messages from Ann and Bo, in turn.
    fn conversation(count: usize) -> OpenThread {
        let mut open = thread("<p>Kites</p>");
        let first = open.messages[0].clone();
        open.messages = (0..count)
            .map(|n| {
                let mut meta = first.clone();
                meta.id = format!("m{n}");
                meta.from = Some(Address {
                    name: None,
                    email: ["ann@example.com", "bo@example.com"][n % 2].to_string(),
                });
                meta
            })
            .collect();
        open
    }

    #[test]
    fn a_photo_for_one_sender_patches_that_sender_s_articles_alone() {
        let mut open = conversation(3);
        open.page(&theme());
        let photo: Rc<str> = "data:image/png;base64,AAAA".into();
        open.photos = HashMap::from([("bo@example.com".to_string(), photo)]);
        assert_eq!(patched(open.page(&theme())), ["m1"]);
    }

    /// The same photo handed over again, or a picture for an attachment no
    /// message has, changes no article.
    #[test]
    fn a_change_that_touches_no_article_patches_nothing() {
        let mut open = conversation(3);
        let photo: Rc<str> = "data:image/png;base64,AAAA".into();
        open.photos = HashMap::from([("bo@example.com".to_string(), Rc::clone(&photo))]);
        open.page(&theme());
        open.photos = HashMap::from([("bo@example.com".to_string(), photo)]);
        open.thumbnails
            .insert("nobody".to_string(), "data:image/png;base64,".to_string());
        assert!(patched(open.page(&theme())).is_empty());
    }

    /// Loading the page again, after WebKit lost it, builds no article
    /// whose inputs stayed the same: it hands out the one built before.
    #[test]
    fn an_article_whose_inputs_did_not_change_is_not_built_again() {
        let mut open = conversation(3);
        let Page::Whole(before) = open.page(&theme()) else {
            panic!("the first page is whole");
        };
        open.take_invitation_place(Some("m2".to_string()));
        open.page_lost();
        let Page::Whole(after) = open.page(&theme()) else {
            panic!("a lost page loads whole");
        };
        let kept: Vec<bool> = before
            .articles
            .iter()
            .zip(&after.articles)
            .map(|(a, b)| Rc::ptr_eq(&a.html, &b.html))
            .collect();
        assert_eq!(kept, [true, true, false]);
    }

    /// A theme reaches the head alone, so a new one builds no article.
    #[test]
    fn a_new_theme_builds_the_head_again_and_no_article() {
        let mut open = conversation(2);
        let Page::Whole(light) = open.page(&theme()) else {
            panic!("the first page is whole");
        };
        let dark = Theme {
            dark: true,
            ..theme()
        };
        let Page::Whole(dark) = open.page(&dark) else {
            panic!("a new theme loads whole");
        };
        assert!(
            light
                .articles
                .iter()
                .zip(&dark.articles)
                .all(|(a, b)| Rc::ptr_eq(&a.html, &b.html))
        );
    }

    #[test]
    fn the_first_page_is_whole_and_an_unchanged_one_patches_nothing() {
        let mut open = thread("<p>Kites</p>");
        assert!(whole(open.page(&theme())).contains("<p>Kites</p>"));
        assert!(patched(open.page(&theme())).is_empty());
    }

    /// The card's place is in the article, so putting the card up or
    /// taking it down replaces that article and keeps the reader's place.
    #[test]
    fn the_card_s_place_patches_the_message_that_carries_it() {
        let mut open = thread("<p>Kites</p>");
        let mut second = open.messages[0].clone();
        second.id = "m2".to_string();
        open.messages.push(second);
        open.page(&theme());
        open.take_invitation_place(Some("m2".to_string()));
        let page = open.page(&theme());
        let Page::Patch(patch) = page else {
            panic!("a whole page for the card's place");
        };
        assert_eq!(patch.len(), 1);
        assert_eq!(patch[0].message_id, "m2");
        assert!(patch[0].html.contains("class=\"event-slot\""));
        open.take_invitation_place(None);
        assert_eq!(patched(open.page(&theme())), ["m2"]);
    }

    #[test]
    fn offering_summarize_loads_the_page_whole() {
        let mut open = thread("<p>Kites</p>");
        open.page(&theme());
        let offered = Theme {
            summarize: true,
            colors: None,
            ..theme()
        };
        assert!(whole(open.page(&offered)).contains("mailrs:summarize"));
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
        assert!(whole(open.page(&theme())).contains("img-src data: mailrs-cid: https: http:"));
    }

    #[test]
    fn a_changed_body_patches_its_article_alone() {
        let mut open = thread("<p>Kites</p>");
        let mut second = open.messages[0].clone();
        second.id = "m2".to_string();
        open.messages.push(second);
        open.page(&theme());
        open.take_bodies(
            vec![(
                "m2".to_string(),
                Ok(Arc::new(MessageBody {
                    text: Some("Tomorrow, then".to_string()),
                    ..MessageBody::default()
                })),
            )],
            HashMap::new(),
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
            html: "<article id=\"m-m1\">Kites, again</article>".into(),
        };
        document.patch(std::slice::from_ref(&fresh));
        let html = document.html(" data-load=\"2\"");
        assert!(
            html.starts_with("<!doctype html><html data-load=\"2\""),
            "{html}"
        );
        assert!(html.contains("Kites, again") && !html.contains("<p>Kites</p>"));
    }

    /// A thread of one open message whose body is this HTML, cleaned.
    fn thread(html: &str) -> OpenThread {
        let mut open = uncleaned(html);
        let cleaned = open.to_clean().clean();
        open.take_cleaned(cleaned);
        open
    }

    /// A thread of one open message whose body is this HTML, not cleaned
    /// yet.
    fn uncleaned(html: &str) -> OpenThread {
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
            held: mailrs_domain::Memberships::read(),
            roles: vec![],
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
    fn a_body_cleaned_elsewhere_is_drawn_as_it_came() {
        let mut open = uncleaned("<p>Kites</p>");
        let mut made = open.to_clean().clean();
        if let Some(copy) = made.get_mut("m1") {
            copy.html = "<p>Cleaned elsewhere</p>".to_string();
        }
        open.take_cleaned(made);
        assert!(whole(open.page(&theme())).contains("<p>Cleaned elsewhere</p>"));
    }

    /// Opening an encrypted message puts a new body under the same id; a
    /// copy cleaned from the old one must not stand in for it, even when
    /// the two read the same.
    #[test]
    fn a_copy_cleaned_from_another_body_is_turned_away() {
        let mut open = uncleaned("<p>Kites</p>");
        let made = open.to_clean().clean();
        let same_words = MessageBody {
            html: Some("<p>Kites</p>".to_string()),
            ..MessageBody::default()
        };
        open.take_bodies(
            vec![("m1".to_string(), Ok(Arc::new(same_words)))],
            HashMap::new(),
        );
        open.take_cleaned(made);
        let page = whole(open.page(&theme()));
        assert!(
            page.contains("Loading…") && !page.contains("<p>Kites</p>"),
            "{page}"
        );
        assert!(!open.to_clean().is_empty());
    }

    /// The engine's pictures come under a new version, and a copy that
    /// points at the old addresses is cleaned again.
    #[test]
    fn new_picture_addresses_need_a_new_copy() {
        let mut open = uncleaned("<img src=\"cid:logo\">");
        let made = open.to_clean().clean();
        open.replace_images("m1", HashMap::new());
        open.take_cleaned(made);
        assert!(!open.to_clean().is_empty());
    }

    /// What the scans find is kept with the cleaned copy: a cleaned copy
    /// that says so draws on its own white page, whatever its words.
    #[test]
    fn the_page_trusts_what_cleaning_found() {
        let mut open = uncleaned("<p>Kites</p>");
        let mut made = open.to_clean().clean();
        let copy = made.get_mut("m1").expect("a copy");
        assert!(!copy.paints && !copy.remote);
        copy.paints = true;
        open.take_cleaned(made);
        let page = whole(open.page(&theme()));
        assert!(page.contains("body html\""), "{page}");
    }

    /// Cleaning finds the quoted history, and the page folds it behind a
    /// button that says what it hides.
    #[test]
    fn a_reply_s_quoted_history_is_folded_on_the_page() {
        let mut open = thread(
            "<div dir=\"ltr\">Monday works.</div><div class=\"gmail_quote\">\
             <div class=\"gmail_attr\">On Friday, Ann wrote:</div>\
             <blockquote class=\"gmail_quote\">Lunch?</blockquote></div>",
        );
        let page = whole(open.page(&theme()));
        let fold = page.find("<details class=\"trimmed\">").expect("a fold");
        let named = "<summary title=\"Show trimmed content\" aria-label=\"Show trimmed content\">";
        assert!(page[fold..].contains(named), "{page}");
        assert!(page.find("Monday works.") < Some(fold), "{page}");
        assert!(
            page[fold..].find("Lunch?") < page[fold..].find("</details>"),
            "{page}"
        );
    }

    #[test]
    fn a_newsletter_paints_itself_and_a_note_does_not() {
        let note = clean("<div dir=\"ltr\">Monday works.</div>");
        let sale = clean("<table bgcolor=\"#ffffff\"><tr><td>Sale</td></tr></table>");
        assert!(!note.paints && sale.paints);
    }

    /// The colours a sender puts on `<body>` are a choice like any other,
    /// and a font alone is not.
    #[test]
    fn a_body_that_picks_its_colours_paints_itself() {
        let github = clean(
            "<html><body style=\"color:#24292e;background-color:#fff\"><p>Run failed</p></body></html>",
        );
        let font = clean(
            "<html><body style=\"font-family:Arial;line-height:1.5\"><p>Monday works.</p></body></html>",
        );
        assert!(github.paints && !font.paints);
    }

    /// The wrappers a `<body>` tag leaves do not hide a reply's history.
    #[test]
    fn a_wrapped_reply_still_folds_its_history() {
        let open = clean(
            "<html><body style=\"font-family:Arial\"><div dir=\"ltr\">Monday works.</div>\
             <div class=\"gmail_quote\"><div class=\"gmail_attr\">On Friday, Ann wrote:</div>\
             <blockquote class=\"gmail_quote\">Lunch?</blockquote></div></body></html>",
        );
        let history = open.history.expect("a fold");
        assert!(open.html[history].contains("Lunch?"));
    }

    /// A reply's quoted history is grey text with a border, as Penguin
    /// Mail and Gmail both write it. That is a note, not a newsletter, so
    /// it keeps the window's colours in the dark.
    #[test]
    fn a_quote_s_own_grey_does_not_make_mail_paint_itself() {
        for reply in [
            "<p>hey</p><blockquote style=\"margin:0 0 0 0.8ex;border-left:2px solid #ccc;padding-left:1ex;color:#555\"><p>test body</p></blockquote>",
            "<div>hey</div><blockquote class=\"gmail_quote\" style=\"border-left-width:1px;border-left-style:solid;border-left-color:rgb(204,204,204);padding-left:1ex\">test body</blockquote>",
        ] {
            assert!(!clean(reply).paints, "{reply}");
        }
        let red = clean("<p style=\"color:#c00\">Due today</p>");
        assert!(
            red.paints,
            "a colour the writer chose still keeps the sheet"
        );
    }

    /// A remote picture a sender left inside a comment never reaches the
    /// page, so it is no reason to offer Load Images.
    #[test]
    fn remote_images_are_looked_for_in_what_the_page_draws() {
        let hidden = clean("<p>Hi</p><!-- <img src=\"https://tracker.example/p.gif\"> -->");
        assert!(!hidden.remote);
        for shown in [
            "<img src='https://news.example/a.png'>",
            "<div style=\"background:url(https://news.example/b.png)\">x</div>",
        ] {
            assert!(clean(shown).remote, "{shown}");
        }
        let mut open = thread("<p>Hi</p><!-- <img src=\"https://tracker.example/p.gif\"> -->");
        open.page(&theme());
        assert!(!open.has_remote_images());
    }

    #[test]
    fn a_picture_in_the_page_is_asked_for_at_its_address() {
        let mut open = thread("<p>Logo</p><img src=\"cid:logo@kites\">");
        let page = whole(open.page(&theme()));
        assert!(
            page.contains("src=\"mailrs-cid:1/m1/0/logo@kites\""),
            "{page}"
        );
        assert!(
            page.contains("img-src data: mailrs-cid:;"),
            "the page's policy lets it ask: {page}"
        );
    }
}

#[cfg(test)]
mod measure {
    use super::Page;
    use crate::open_thread::OpenThread;
    use crate::render::Theme;
    use mailrs_domain::{Address, MessageBody, MessageMeta, Target};
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    /// A long newsletter thread: thirty open messages from two senders,
    /// each with about 30 KB of HTML.
    fn long_thread() -> OpenThread {
        let html: String = (0..200)
            .map(|n| {
                format!(
                    "<table bgcolor=\"#fff\"><tr><td><p style=\"color:#333\">Line {n} of the sale, \
                     with <a href=\"https://shop.example/{n}\">a link</a>.</p></td></tr></table>"
                )
            })
            .collect();
        let messages: Vec<MessageMeta> = (0..30)
            .map(|n| MessageMeta {
                account_id: 1,
                id: format!("m{n}"),
                thread_id: "t1".to_string(),
                rfc822_msgid: None,
                from: Some(Address {
                    name: Some("Ann".to_string()),
                    email: format!("ann{}@example.com", n % 2),
                }),
                to: Vec::new(),
                cc: Vec::new(),
                subject: "Sale".to_string(),
                date: 0,
                snippet: String::new(),
                size: 0,
                has_attachments: false,
                held: mailrs_domain::Memberships::read(),
                roles: vec![],
                list_unsubscribe: None,
                one_click: false,
            })
            .collect();
        let bodies = messages
            .iter()
            .map(|m| {
                let body = MessageBody {
                    html: Some(html.clone()),
                    ..MessageBody::default()
                };
                (m.id.clone(), body)
            })
            .collect();
        let mut open = OpenThread::new(
            &Target::thread(1, "t1"),
            "Sale".to_string(),
            messages,
            bodies,
            Vec::new(),
        );
        open.expanded = open.messages.iter().map(|m| m.id.clone()).collect();
        open
    }

    /// Times the redraw after one sender's photo arrives in a long thread.
    /// `cargo test -p mailrs --bin penguin-mail measure -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn a_photo_arriving_in_a_long_thread() {
        let theme = Theme {
            dark: false,
            accent: "#3584e4".to_string(),
            accent_text: "#1a5fb4".to_string(),
            summarize: false,
            colors: None,
            font: String::new(),
        };
        let mut open = long_thread();
        let first = Instant::now();
        let cleaned = open.to_clean().clean();
        open.take_cleaned(cleaned);
        assert!(matches!(open.page(&theme), Page::Whole(_)));
        let first = first.elapsed();
        let rounds = 20;
        let (mut photo, mut same) = (Duration::ZERO, Duration::ZERO);
        for round in 0..rounds {
            let uri: Rc<str> = format!("data:image/png;base64,{round}AAAA").into();
            open.photos = HashMap::from([("ann0@example.com".to_string(), uri)]);
            let at = Instant::now();
            let page = open.page(&theme);
            photo += at.elapsed();
            assert!(matches!(page, Page::Patch(p) if !p.is_empty()));
            let at = Instant::now();
            let page = open.page(&theme);
            same += at.elapsed();
            assert!(matches!(page, Page::Patch(p) if p.is_empty()));
        }
        println!(
            "first page {first:?}; photo arrives {:?}; nothing changed {:?}",
            photo / rounds,
            same / rounds
        );
    }
}
