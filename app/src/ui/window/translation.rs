//! Offering to translate the message on screen, and translating it.
//!
//! The card goes up only for a message whose words are not the interface's
//! own, and nothing is sent until the button is pressed. What the card
//! says before that is where the words would go, which is the whole of the
//! decision: a model on this computer keeps the message here, and
//! Anthropic or a Claude subscription does not.
//!
//! A translation is kept beside the message for as long as the thread
//! stays open, so showing the original again and going back costs nothing.

use std::rc::Rc;

use gtk::glib;

use super::MainWindow;
use crate::sanitize::sanitize_html;
use crate::translation::{self, Language, Reading, Translation};
use crate::ui::composer::spell;
use crate::ui::conversation::ConversationView;
use mailrs_domain::translate::{fill, gettext};

impl MainWindow {
    /// Puts the card up when the message on screen is in another
    /// language, and takes it down when it is not. Runs whenever a thread
    /// is drawn, since the words only arrive with the body.
    pub(super) fn refresh_translation(self: &Rc<Self>, view: &Rc<ConversationView>) {
        let Some((message_id, prose)) = view.open_prose() else {
            view.translate.hide();
            return;
        };
        // A message that has been translated keeps its card, whatever a
        // second reading of the words would say about it.
        let done = view.find(|open| {
            open.translations
                .get(&message_id)
                .map(|said| (said.from, said.cut, said.shown))
        });
        if let Some((from, cut, shown)) = done {
            view.translate.done(from, cut, shown);
            return;
        }
        let Some(interface) = self.interface_language() else {
            view.translate.hide();
            return;
        };
        let Reading::Other(from) = translation::read_language(&prose.sample(), interface) else {
            view.translate.hide();
            return;
        };
        let ai = self.settings().ai;
        match translation::destination(&ai) {
            Ok((_, goes)) => view.translate.offer(from, Ok(&goes)),
            Err(problem) => view.translate.offer(from, Err(&problem)),
        }
    }

    /// The card's button: translate the message on screen, or turn over
    /// the translation it already has.
    pub(super) fn translate_message(self: &Rc<Self>, view: &Rc<ConversationView>) {
        let Some((message_id, prose)) = view.open_prose() else {
            return;
        };
        if view.turn_translation(&message_id) {
            return;
        }
        let Some(interface) = self.interface_language() else {
            return;
        };
        let (config, _) = match translation::destination(&self.settings().ai) {
            Ok(where_to) => where_to,
            Err(problem) => {
                view.translate.problem(&problem);
                self.toast(&problem);
                return;
            }
        };
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
        // The thread the reader asked about. They may move on while the
        // model works, and the card on screen then belongs to another
        // thread, which this answer says nothing about.
        let Some(asked_about) = view.read(|open| (open.account_id, open.thread_id.clone())) else {
            return;
        };
        view.translate.working();
        let (this, view) = (Rc::clone(self), Rc::clone(view));
        glib::spawn_future_local(async move {
            let (account_id, thread_id) = asked_about;
            let still_open = || view.is_showing(account_id, &thread_id);
            let answer = this
                .core
                .call(async move {
                    let asked: Vec<&str> = asked.iter().map(String::as_str).collect();
                    translation::ask(config, interface, &asked)
                        .await
                        .map_err(anyhow::Error::msg)
                })
                .await;
            let said = match answer {
                Ok(said) => said,
                Err(err) => {
                    let problem = fill(
                        &gettext("The model could not translate this: {reason}"),
                        &[("reason", &err.to_string())],
                    );
                    if still_open() {
                        view.translate.problem(&problem);
                    }
                    this.toast(&problem);
                    return;
                }
            };
            if said.iter().all(Option::is_none) {
                let problem = gettext("The model sent nothing back to put in the message.");
                if still_open() {
                    view.translate.problem(&problem);
                }
                this.toast(&problem);
                return;
            }
            let made = view.find(|open| {
                let Some(Ok(arrived)) = open.bodies.get(&message_id) else {
                    return None;
                };
                let images = open
                    .inline_images
                    .get(&message_id)
                    .cloned()
                    .unwrap_or_default();
                let rebuilt = prose.rebuild(&said);
                let mut body = arrived.clone();
                // Model output is cleaned like any other mail HTML before
                // it reaches the WebView.
                let clean = match arrived
                    .html
                    .as_deref()
                    .is_some_and(|h| !h.trim().is_empty())
                {
                    true => Some(sanitize_html(&rebuilt, &images)),
                    false => {
                        body.text = Some(rebuilt);
                        None
                    }
                };
                Some(Translation {
                    from,
                    body,
                    clean,
                    cut,
                    shown: true,
                })
            });
            let Some(translation) = made else {
                if still_open() {
                    view.translate.hide();
                }
                return;
            };
            view.translated(message_id, translation);
        });
    }

    /// The language the interface is in, when it is one whose words this
    /// app can count. `None` leaves every message alone.
    fn interface_language(&self) -> Option<Language> {
        let installed: Vec<String> = crate::language::choices()
            .into_iter()
            .map(|language| language.code)
            .collect();
        translation::interface_language(
            &self.settings().language,
            &spell::locale_language(),
            &installed,
        )
    }
}
