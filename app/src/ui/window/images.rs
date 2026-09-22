//! Remembering which senders may load remote images.
//!
//! The banner loads images for the thread in front of the reader and
//! forgets it. This is the other answer: allow this sender, or everyone
//! who writes from their domain, and stop being asked. The rule that
//! decides lives in `crate::images`; this is the part that reads and
//! writes the list and puts the choice in front of the reader.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_store::image_senders;

use super::MainWindow;
use crate::images;
use crate::settings::RemoteImages;
use crate::ui::conversation::ConversationView;
use mailrs_domain::translate::{fill, gettext};

impl MainWindow {
    /// Whether a message from these senders may load remote images. Every
    /// sender has to be allowed, because the page carries one policy for
    /// the whole conversation: one unknown sender in a thread would
    /// otherwise load on the strength of the others.
    pub(super) fn images_allowed_for(&self, senders: &[String]) -> bool {
        if self.settings().remote_images == RemoteImages::Always {
            return true;
        }
        let list = self.image_senders.borrow();
        !senders.is_empty()
            && senders
                .iter()
                .all(|from| images::allowed(&list, Some(from.as_str())))
    }

    /// Reads the list in from the store. Called at startup and after every
    /// change, so the copy held here is the one the store holds.
    pub fn reload_image_senders(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if let Ok(list) = this.core.read(image_senders::list).await {
                *this.image_senders.borrow_mut() = list;
            }
        });
    }

    /// Loads this thread's images and offers to remember the sender. The
    /// offer rides on the toast, because the moment someone asks for the
    /// images is the moment the question makes sense; the same choice
    /// stays in the More menu for anyone who lets the toast go.
    pub(super) fn load_images_once(self: &Rc<Self>, view: &Rc<ConversationView>) {
        view.allow_images();
        let from = view
            .find(|open| {
                open.messages
                    .last()
                    .and_then(|m| m.from.as_ref())
                    .map(|a| a.email.to_lowercase())
            })
            .filter(|a| !a.trim().is_empty());
        let Some(from) = from else { return };
        if images::allowed(&self.image_senders.borrow(), Some(&from)) {
            return;
        }
        let ask = fill(
            &gettext("Always load images from {sender}?"),
            &[("sender", &from)],
        );
        let toast = adw::Toast::builder()
            .title(glib::markup_escape_text(&ask))
            .button_label(gettext("Always"))
            .timeout(8)
            .build();
        let (this, view) = (Rc::clone(self), Rc::clone(view));
        toast.connect_button_clicked(move |_| {
            let (this, view, from) = (Rc::clone(&this), Rc::clone(&view), from.clone());
            glib::spawn_future_local(async move {
                this.allow_images_from(&view, from, false).await;
            });
        });
        self.toasts.add_toast(toast);
    }

    /// Asks whether to allow this sender or their whole domain, then
    /// records the answer and redraws the conversation without the banner.
    pub(super) fn always_load_images(self: &Rc<Self>, view: &Rc<ConversationView>) {
        let Some(from) = view.read(|open| {
            open.messages
                .last()
                .and_then(|m| m.from.as_ref())
                .map(|a| a.email.to_lowercase())
                .unwrap_or_default()
        }) else {
            return;
        };
        if from.trim().is_empty() {
            self.toast(&gettext("This message has no sender to remember."));
            return;
        }
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Always Load Images?"))
            .body(gettext(
                "Loading a remote image tells the sender when you opened their mail.",
            ))
            .build();
        dialog.add_response("cancel", &gettext("Cancel"));
        dialog.add_response(
            "address",
            &fill(&gettext("From {sender}"), &[("sender", &from)]),
        );
        let domain = images::domain_of(&from).map(str::to_string);
        if let Some(domain) = &domain {
            dialog.add_response(
                "domain",
                &fill(&gettext("From Anyone at {domain}"), &[("domain", domain)]),
            );
        }
        dialog.set_default_response(Some("address"));
        dialog.set_close_response("cancel");

        let (this, view) = (Rc::clone(self), Rc::clone(view));
        glib::spawn_future_local(async move {
            let answer = dialog.choose_future(Some(&this.window)).await;
            let (sender, whole_domain) = match answer.as_str() {
                "address" => (from.clone(), false),
                "domain" => match domain {
                    Some(domain) => (domain, true),
                    None => return,
                },
                _ => return,
            };
            this.allow_images_from(&view, sender, whole_domain).await;
        });
    }

    /// Records one sender and shows the conversation with its images.
    pub(super) async fn allow_images_from(
        self: &Rc<Self>,
        view: &Rc<ConversationView>,
        sender: String,
        whole_domain: bool,
    ) {
        let now = chrono::Utc::now().timestamp_millis();
        let saved = sender.clone();
        let written = self
            .core
            .write(move |c| {
                image_senders::allow(c, &saved, whole_domain, now)?;
                image_senders::list(c)
            })
            .await;
        match written {
            Ok(list) => {
                *self.image_senders.borrow_mut() = list;
                view.allow_images();
                self.toast(&if whole_domain {
                    fill(
                        &gettext("Images from anyone at {domain} will load from now on"),
                        &[("domain", &sender)],
                    )
                } else {
                    fill(
                        &gettext("Images from {sender} will load from now on"),
                        &[("sender", &sender)],
                    )
                });
            }
            Err(err) => self.toast(&fill(
                &gettext("Could not remember that sender: {reason}"),
                &[("reason", &err.to_string())],
            )),
        }
    }
}
