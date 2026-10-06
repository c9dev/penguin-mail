//! The offer in the sent toast to save a message's new recipients to the
//! contacts of the account that sent it, and what Save does.
//!
//! Sync decides who is new (`ContactBook::new_recipients`) and makes the
//! contacts; this module words the toast and reacts to it. Nothing is
//! written until the person presses Save. Closing the toast counts as No,
//! so the same people are not offered again on that account. A toast that
//! runs out of time records nothing: whoever looked away is asked again
//! the next time they write to those people.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::translate::gettext;
use mailrs_domain::{AccountId, Address};
use mailrs_sync::Permitted;

use super::MainWindow;
use crate::contacts::{OfferEnd, OfferTimer, declines, failed_title, offer_timer, offer_title, saved_title};
use crate::format::account_label;
use crate::permission::{Occasion, Permission};

/// How long the offer stays, in seconds: longer than a plain toast, since
/// it asks something and the question takes a moment to read.
const OFFER_SECONDS: u32 = 10;

/// How long the offer stays after the pointer or the focus leaves it.
const AFTER_READING_SECONDS: u32 = 3;

/// The toast's own widget around `title`, whose state says whether the
/// pointer is over any part of the toast, its Save button included.
fn toast_widget(title: gtk::Widget) -> gtk::Widget {
    let mut at = Some(title.clone());
    while let Some(widget) = at {
        if widget.css_name() == "toast" {
            return widget;
        }
        at = widget.parent();
    }
    title
}

impl MainWindow {
    /// Says a message went out, and offers to save whoever it went to
    /// that the sending account's contacts lack. An account whose contacts
    /// are off in Preferences keeps no address book here to compare with,
    /// so it gets no offer.
    pub(super) fn sent(
        self: &Rc<Self>,
        said: String,
        account_id: AccountId,
        recipients: Vec<Address>,
    ) {
        let reads = self.account(account_id).is_some_and(|account| {
            self.app
                .upgrade()
                .is_some_and(|app| app.settings().reads_contacts(&account.email))
        });
        if !reads || recipients.is_empty() {
            return self.toast(&said);
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let book = this.core.contacts();
            let found = this
                .core
                .call(async move { book.new_recipients(account_id, &recipients).await })
                .await;
            let people = found.unwrap_or_else(|err| {
                tracing::warn!(error = %err, "could not tell which recipients are new");
                Vec::new()
            });
            match people.is_empty() {
                true => this.toast(&said),
                false => this.offer_to_save(&said, account_id, people),
            }
        });
    }

    fn offer_to_save(self: &Rc<Self>, said: &str, account_id: AccountId, people: Vec<Address>) {
        let question = offer_title(&people, &account_label(account_id).short);
        // A toast's own title stays on one line, which cut the question
        // off in a narrow window. The news and the question take a line
        // each, and the question wraps when even that is too wide.
        let title = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .build();
        title.append(
            &gtk::Label::builder()
                .label(said)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["heading"])
                .build(),
        );
        title.append(
            &gtk::Label::builder()
                .label(&question)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .max_width_chars(36)
                .xalign(0.0)
                .build(),
        );
        let toast = adw::Toast::builder()
            .custom_title(&title)
            .button_label(gettext("Save"))
            // The toast's own timeout ends it the way its close button
            // does, and the dismissed signal cannot tell the two apart.
            // It waits for nobody here; the timer below ends it instead
            // and says so. High priority shows it at once, so the time
            // runs while it is on screen rather than in the queue.
            .timeout(0)
            .priority(adw::ToastPriority::High)
            .build();
        let people = Rc::new(people);
        let ended: Rc<Cell<Option<OfferEnd>>> = Rc::new(Cell::new(None));
        let (win, chosen, who) = (Rc::downgrade(self), Rc::clone(&ended), Rc::clone(&people));
        toast.connect_button_clicked(move |_| {
            chosen.set(Some(OfferEnd::Saved));
            if let Some(win) = win.upgrade() {
                win.save_recipients(account_id, who.to_vec());
            }
        });
        // The timer looks once a second. Once the time is up it waits while
        // the pointer is over the toast or the focus is in it, as
        // libadwaita's own timeout does, and then gives a few seconds more.
        let (timer, held, anchor) = (toast.downgrade(), Rc::clone(&ended), title.downgrade());
        let waited = Cell::new(0);
        glib::timeout_add_seconds_local(1, move || {
            if held.get().is_some() {
                return glib::ControlFlow::Break;
            }
            let Some(toast) = timer.upgrade() else { return glib::ControlFlow::Break };
            waited.set(waited.get() + 1);
            if waited.get() < OFFER_SECONDS {
                return glib::ControlFlow::Continue;
            }
            let shown = anchor.upgrade().map(|title| toast_widget(title.upcast()));
            let flags = shown.map(|w| w.state_flags()).unwrap_or(gtk::StateFlags::NORMAL);
            let hovered = flags.contains(gtk::StateFlags::PRELIGHT);
            let focused = flags.contains(gtk::StateFlags::FOCUS_WITHIN);
            match offer_timer(hovered, focused) {
                OfferTimer::Wait => {
                    waited.set(OFFER_SECONDS - AFTER_READING_SECONDS);
                    glib::ControlFlow::Continue
                }
                OfferTimer::End => {
                    held.set(Some(OfferEnd::TimedOut));
                    toast.dismiss();
                    glib::ControlFlow::Break
                }
            }
        });
        let win = Rc::downgrade(self);
        toast.connect_dismissed(move |_| {
            let Some(win) = win.upgrade() else { return };
            let emails: Vec<String> = people.iter().map(|p| p.email.clone()).collect();
            let book = win.core.contacts();
            let core = Rc::clone(&win.core);
            let ended = Rc::clone(&ended);
            glib::spawn_future_local(async move {
                // Save also dismisses the toast, and the two signals may
                // come in either order. The future first runs after both,
                // so it sees whether Save was pressed. Nothing else said
                // how it ended, so the person closed it.
                if !declines(ended.get().unwrap_or(OfferEnd::Closed)) {
                    return;
                }
                let declined = core
                    .call(async move { book.decline_recipients(account_id, &emails).await })
                    .await;
                if let Err(err) = declined {
                    tracing::warn!(error = %err, "could not remember a declined offer");
                }
            });
        });
        self.toasts.add_toast(toast);
    }

    /// Makes a contact of each of `people` on `account_id`, asking for the
    /// permission to change contacts first when the account lacks it.
    fn save_recipients(self: &Rc<Self>, account_id: AccountId, people: Vec<Address>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let book = this.core.contacts();
            let asked = people.clone();
            let saved = this
                .core
                .call(async move { book.save_recipients(account_id, &asked).await })
                .await;
            let label = account_label(account_id).short;
            match saved {
                Ok(Permitted::Done(done)) => {
                    if !done.saved.is_empty()
                        && let Some(app) = this.app.upgrade()
                    {
                        app.contacts_added();
                    }
                    match done.failed.is_empty() {
                        true => this.toast(&saved_title(&people, &label)),
                        false => this.toast(&failed_title(&done.failed, &label)),
                    }
                }
                Ok(Permitted::NeedsPermission) => {
                    this.ask_permission(account_id, Permission::ChangeContacts, Occasion::Needed);
                }
                Err(err) => {
                    tracing::warn!(error = %err, "could not save the recipients to contacts");
                    this.toast(&failed_title(&people, &label));
                }
            }
        });
    }
}
