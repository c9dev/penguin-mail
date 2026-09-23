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

use super::OpenThread;
use crate::render::{BodyState, Conversation, MessageView, Theme, render};
use crate::sanitize::sanitize_html;
use crate::translation::{Body, Prose};

/// A message body after cleaning, with a mark of the HTML and the inline
/// images it was made from. A different mark means the body needs
/// cleaning again.
#[derive(Debug, Clone)]
pub(super) struct Cleaned {
    mark: u64,
    html: String,
}

impl OpenThread {
    /// The whole document for the thread as it stands, in `theme`.
    /// Cleaning a long body costs milliseconds, so each cleaned copy is
    /// kept until its body or its pictures change.
    pub fn page(&mut self, theme: &Theme) -> String {
        self.clean_bodies();
        let empty = HashMap::new();
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
                    inline_images: self.inline_images.get(&meta.id).unwrap_or(&empty),
                    thumbnails: &self.thumbnails,
                    sanitized: match showing {
                        Some(translation) => translation.clean.as_deref(),
                        None => self.cleaned.get(&meta.id).map(|body| body.html.as_str()),
                    },
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

    /// Cleans every HTML body whose cleaned copy is missing or was made
    /// from something else, and forgets the copies of bodies that left.
    fn clean_bodies(&mut self) {
        let empty = HashMap::new();
        let bodies = &self.bodies;
        self.cleaned.retain(|id, _| bodies.contains_key(id));
        for meta in &self.messages {
            let Some(Ok(body)) = self.bodies.get(&meta.id) else {
                continue;
            };
            let Some(html) = html_of(body) else {
                continue;
            };
            let images = self.inline_images.get(&meta.id).unwrap_or(&empty);
            let mark = body_mark(html, images);
            if self
                .cleaned
                .get(&meta.id)
                .is_none_or(|seen| seen.mark != mark)
            {
                let cleaned = Cleaned {
                    mark,
                    html: sanitize_html(html, images),
                };
                self.cleaned.insert(meta.id.clone(), cleaned);
            }
        }
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
    use super::body_mark;
    use std::collections::HashMap;

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
