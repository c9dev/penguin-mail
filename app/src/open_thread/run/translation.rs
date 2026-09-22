//! Offering to translate the message on screen, and translating it, as
//! steps of the thread run.
//!
//! The card goes up only for a message whose words are not the
//! interface's own, and nothing is sent until the button is pressed. What
//! the card says before that is where the words would go: a model on this
//! computer keeps the message here, and Anthropic or a Claude subscription
//! does not. A translation is kept beside the message for as long as the
//! thread stays open, so turning back to what arrived and forward again
//! costs no second request.

use mailrs_domain::translate::{fill, gettext};

use super::ThreadRun;
use crate::sanitize::sanitize_html;
use crate::translation::{self, Language, Reading, Translation};

/// What the translation card shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Card {
    Hidden,
    /// The message reads as another language: where its words would go,
    /// or why they have nowhere to go.
    Offered {
        from: Option<Language>,
        goes: Result<String, String>,
    },
    /// The model is at work.
    Working,
    /// A translation is here, from `from`, cut short or whole, and shown
    /// or turned back to what arrived.
    Done {
        from: Option<Language>,
        cut: bool,
        shown: bool,
    },
    /// What went wrong the last time.
    Problem(String),
}

impl ThreadRun {
    /// What the card should say about the message on screen. A message
    /// translated here keeps its card, whatever a second reading of the
    /// words would say about it.
    pub(super) fn translation_offer(&self) -> Card {
        let Some((message_id, prose)) = self.desk.prose() else {
            return Card::Hidden;
        };
        if let Some((from, cut, shown)) = self.desk.translation_of(&message_id) {
            return Card::Done { from, cut, shown };
        }
        let Some(interface) = self.desk.interface_language() else {
            return Card::Hidden;
        };
        let Reading::Other(from) = translation::read_language(&prose.sample(), interface) else {
            return Card::Hidden;
        };
        Card::Offered {
            from,
            goes: self.desk.translation_destination(),
        }
    }

    /// The card's button: translate the message on screen, or turn over
    /// the translation it already has.
    pub async fn translate(&self) {
        let Some(wanted) = self.on_screen() else {
            return;
        };
        let Some((message_id, prose)) = self.desk.prose() else {
            return;
        };
        if wanted.on_screen(|effects| effects.turn_translation(&message_id)) == Some(true) {
            return;
        }
        let Some(interface) = self.desk.interface_language() else {
            return;
        };
        if let Err(problem) = self.desk.translation_destination() {
            wanted.on_screen(|effects| effects.translation_card(Card::Problem(problem.clone())));
            wanted.anyway(|effects| effects.toast(problem));
            return;
        }
        let from = match translation::read_language(&prose.sample(), interface) {
            Reading::Other(from) => from,
            _ => None,
        };
        let pieces = prose.pieces();
        if pieces.is_empty() {
            return;
        }
        let fits = translation::fits(&pieces);
        let cut = fits < pieces.len();
        let asked: Vec<String> = pieces[..fits]
            .iter()
            .map(|piece| piece.to_string())
            .collect();
        // The translation is built on the body as it is now, which is the
        // body the prose came from.
        let Some((arrived, images)) = self.desk.arrived(&message_id) else {
            return;
        };
        wanted.on_screen(|effects| effects.translation_card(Card::Working));
        // A failure is told whatever the reader has opened since; the card
        // only hears about it while it still belongs to this thread.
        let said = match wanted
            .anyway(|effects| effects.translate(interface, asked))
            .await
        {
            Ok(said) if said.iter().all(Option::is_none) => Err(gettext(
                "The model sent nothing back to put in the message.",
            )),
            Ok(said) => Ok(said),
            Err(err) => Err(fill(
                &gettext("The model could not translate this: {reason}"),
                &[("reason", &err)],
            )),
        };
        let said = match said {
            Ok(said) => said,
            Err(problem) => {
                wanted
                    .on_screen(|effects| effects.translation_card(Card::Problem(problem.clone())));
                wanted.anyway(|effects| effects.toast(problem));
                return;
            }
        };
        let rebuilt = prose.rebuild(&said);
        let mut body = arrived;
        // Model output is cleaned like any other mail HTML before it
        // reaches the page.
        let clean = match body.html.as_deref().is_some_and(|h| !h.trim().is_empty()) {
            true => Some(sanitize_html(&rebuilt, &images)),
            false => {
                body.text = Some(rebuilt);
                None
            }
        };
        let translation = Translation {
            from,
            body,
            clean,
            cut,
            shown: true,
        };
        wanted.on_screen(|effects| effects.translated(message_id, translation));
    }
}
