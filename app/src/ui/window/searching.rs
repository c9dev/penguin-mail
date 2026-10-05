//! Mail search as the person types. A pause in typing runs the search
//! over the mail on this computer, and from three letters on over the
//! servers too, with the stored rows on screen while the servers answer.
//! `search::typing` decides when a search runs and how far it reaches.
//! Every answer lands through the list feed's ticket, so a search that a
//! newer one replaced never reaches the list, and a key stops the server
//! search still out, so the servers hear nothing more of it.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::AccountId;
use mailrs_sync::{Listing, Loaded, Stop};

use super::on_screen::OnScreen;
use super::{MainWindow, load_failed};
use crate::search::typing::{self, Reach, Run, Typed};
use crate::ui::Mailbox;
use crate::ui::list_feed::Ticket;

impl MainWindow {
    /// Runs the search the field holds once typing pauses, and puts the
    /// mailbox back when the field empties.
    pub(super) fn install_search_typing(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.list.search_entry.connect_changed(move |entry| {
            let Some(win) = weak.upgrade() else { return };
            // The answer to the old text is no longer wanted.
            if let Some((_, stop)) = win.search_stop.take() {
                stop.stop();
                win.typing.borrow_mut().interrupted();
            }
            let typed = win.typing.borrow_mut().typed(&entry.text());
            match typed {
                Typed::Cleared => win.change_screen(OnScreen::search_closed),
                Typed::Wait(pause) => {
                    let weak = Rc::downgrade(&win);
                    glib::timeout_add_local_once(typing::PAUSE, move || {
                        let Some(win) = weak.upgrade() else { return };
                        let run = win.typing.borrow_mut().paused(pause);
                        if let Some(run) = run {
                            win.run_search(run);
                        }
                    });
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.list.search_button.connect_toggled(move |button| {
            let Some(win) = weak.upgrade() else { return };
            if !button.is_active() {
                win.typing.borrow_mut().closed();
                if let Some((_, stop)) = win.search_stop.take() {
                    stop.stop();
                }
            }
        });
    }

    /// Enter in the search field, or a picked suggestion: runs `text` at
    /// once, as far as the servers.
    pub(super) fn search_entered(self: &Rc<Self>, text: String) {
        let run = self.typing.borrow_mut().entered(&text);
        if let Some(run) = run {
            self.run_search(run);
        }
    }

    fn run_search(self: &Rc<Self>, run: Run) {
        let query = run.query.clone();
        *self.searched.borrow_mut() = Some(run);
        self.search(query);
    }

    /// Lists the first page of a search under `ticket`: the stored mail
    /// first, then, when the search reaches them, the servers' answer in
    /// its place. A search the field did not run, such as one the
    /// assistant opened, reaches the servers. With no stored rows to show,
    /// `spinner` puts the spinner up while the servers answer; a refresh
    /// leaves the rows on screen instead.
    pub(super) fn search_first_page(
        self: &Rc<Self>,
        ticket: Ticket,
        query: String,
        only: Option<AccountId>,
        spinner: bool,
    ) {
        let reach = match &*self.searched.borrow() {
            Some(run) if run.query == query => run.reach,
            _ => Reach::Servers,
        };
        let stop = Stop::default();
        let held = (reach == Reach::Servers).then(|| (ticket, stop.clone()));
        if let Some((_, older)) = self.search_stop.replace(held) {
            older.stop();
        }
        let (scope, view) = (self.scope(), self.view());
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let lists = this.core.lists();
            let (asked, within, seen) = (query.clone(), scope.clone(), view.clone());
            let stored = this
                .core
                .call(async move { lists.stored_search(&asked, only, &within, &seen).await })
                .await;
            if reach == Reach::Store {
                return this.land_search(ticket, stored);
            }
            if stop.stopped() || !this.screen.borrow_mut().feed().current(ticket) {
                return;
            }
            match &stored {
                Ok(found) if !found.rows.is_empty() => this.show_listing(found.clone()),
                Err(err) => tracing::warn!(error = %err, "could not search the stored mail"),
                Ok(_) if spinner => this.list.show_loading(),
                Ok(_) => {}
            }
            let lists = this.core.lists();
            let mailbox = Mailbox::Search {
                query,
                account_id: only,
            };
            let until = stop.clone();
            let found = this
                .core
                .call(async move {
                    lists
                        .list_until(&mailbox, &scope, &view, Loaded::nothing(), &until)
                        .await
                })
                .await;
            {
                let mut held = this.search_stop.borrow_mut();
                if held.as_ref().is_some_and(|(at, _)| *at == ticket) {
                    *held = None;
                }
            }
            // A stopped search answers with what it had, which is less than
            // the stored rows already on screen.
            if !stop.stopped() {
                this.land_search(ticket, found);
            }
        });
    }

    /// Puts a search's first page on screen while `ticket` is current.
    fn land_search(self: &Rc<Self>, ticket: Ticket, loaded: anyhow::Result<Listing>) {
        let landed = self.screen.borrow_mut().feed().first_page(ticket, &loaded);
        let Some(landed) = landed else {
            return;
        };
        match loaded {
            Ok(listing) => self.show_listing(listing),
            Err(err) => self.toast(&load_failed(&err)),
        }
        if let Some((account_id, thread_id, then)) = landed.reveal {
            self.select_revealed(account_id, thread_id, then);
        }
    }
}
