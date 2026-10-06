//! A calendar file opened from Files: the event card in a small window of
//! its own, with Add to Calendar.
//!
//! Nothing here reads a message. A file that is a saved invitation has no
//! mail to answer, so every request in it reads as an event to keep, and
//! the card offers what it offers for a ticket: pick a calendar, add.

use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::invitation::{self, Invitation, Method};
use mailrs_domain::translate::gettext;
use mailrs_domain::AccountId;
use mailrs_sync::Permitted;

use crate::app::App;
use crate::ui::invitation::{Action, AddTo, EventCard, Showing};

/// The largest file read. A calendar file is text, and a few hundred
/// kilobytes already holds years of events.
const MOST_BYTES: u64 = 4 * 1024 * 1024;

/// The events of a file, as the card offers them. A request asks somebody
/// to answer it, and a file has nobody to answer to, so it reads as an
/// event to keep like the rest.
pub fn events_of(text: &str) -> Vec<Invitation> {
    invitation::read_all(text)
        .into_iter()
        .map(|event| match event.method {
            Method::Request => Invitation { method: Method::Publish, ..event },
            _ => event,
        })
        .collect()
}

/// Why a file gave no events, in words for the window.
fn unreadable(path: &Path) -> Option<String> {
    let size = std::fs::metadata(path).map(|m| m.len());
    match size {
        Err(err) => Some(err.to_string()),
        Ok(size) if size > MOST_BYTES => Some(gettext("This file is too large to be a calendar.")),
        Ok(_) => None,
    }
}

/// Opens `path` in a window of its own and hands the window back.
pub fn open(app: &Rc<App>, path: &Path) -> adw::Window {
    crate::ensure_gtk();
    let window = adw::Window::builder()
        .title(gettext("Calendar File"))
        .default_width(520)
        .default_height(-1)
        .build();
    // The card's colors follow the window's light or dark class, which the
    // main window sets for itself.
    crate::ui::window::track_dark_class(&window);
    let toasts = adw::ToastOverlay::new();
    let header = adw::HeaderBar::new();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toasts));
    window.set_content(Some(&toolbar));

    let events = match unreadable(path) {
        Some(why) => Err(why),
        None => std::fs::read(path)
            .map(|bytes| events_of(&String::from_utf8_lossy(&bytes)))
            .map_err(|err| err.to_string()),
    };
    match events {
        Ok(events) if !events.is_empty() => show_events(app, &window, &toasts, path, events),
        Ok(_) => toasts.set_child(Some(&status(
            &gettext("No Events in This File"),
            &gettext("Penguin Mail found no event to add in it."),
        ))),
        Err(why) => toasts.set_child(Some(&status(&gettext("Could Not Open This File"), &why))),
    }
    window.present();
    window
}

fn status(title: &str, description: &str) -> adw::StatusPage {
    adw::StatusPage::builder()
        .icon_name("penguin-mail-calendar-symbolic")
        .title(glib::markup_escape_text(title).as_str())
        .description(glib::markup_escape_text(description).as_str())
        .build()
}

/// The accounts that can take an event: a calendar offered and the
/// permission not withheld. An account still starting is left out.
fn calendar_accounts(app: &App) -> Vec<(AccountId, String)> {
    app.accounts()
        .into_iter()
        .filter(|account| {
            app.core.account(account.id).is_some_and(|running| {
                running.services().offers().calendar && !running.services().withheld().calendar
            })
        })
        .map(|account| (account.id, account.email))
        .collect()
}

fn show_events(
    app: &Rc<App>,
    window: &adw::Window,
    toasts: &adw::ToastOverlay,
    path: &Path,
    events: Vec<Invitation>,
) {
    let accounts = calendar_accounts(app);
    let (weak_app, weak_toasts) = (Rc::downgrade(app), toasts.downgrade());
    let card_slot: Rc<std::cell::RefCell<Option<Rc<EventCard>>>> = Rc::default();
    let file = path.to_path_buf();
    let card = EventCard::new({
        let card_slot = Rc::clone(&card_slot);
        let window = window.downgrade();
        move |action| {
            let (Some(app), Some(toasts), Some(card)) =
                (weak_app.upgrade(), weak_toasts.upgrade(), card_slot.borrow().clone())
            else {
                return;
            };
            let toast = |text: &str| toasts.add_toast(crate::ui::toast(text));
            match action {
                Action::Import(events, target) => import(&app, &card, &toasts, events, target),
                Action::ShowInCalendar => {
                    if let Some(Some(spot)) = card.with_showing(|showing| showing.on_calendar.clone()) {
                        app.show_window().show_spot(&spot);
                        if let Some(window) = window.upgrade() {
                            window.close();
                        }
                    }
                }
                // A file whose events name no id has nothing to match a
                // second import on, so the desktop's own calendar takes it.
                Action::AddToCalendar => {
                    let launcher = gtk::FileLauncher::new(Some(&gtk::gio::File::for_path(&file)));
                    let failed = gettext("No app on this desktop opens calendar files");
                    launcher.launch(window.upgrade().as_ref(), gtk::gio::Cancellable::NONE, {
                        let toasts = toasts.clone();
                        move |result| {
                            if result.is_err() {
                                toasts.add_toast(crate::ui::toast(&failed));
                            }
                        }
                    });
                }
                Action::Answer(..) | Action::Propose(_) | Action::GrantAccess => {
                    toast(&gettext("A file has nobody to answer to"));
                }
            }
        }
    });
    *card_slot.borrow_mut() = Some(Rc::clone(&card));

    let mut rest = events.into_iter();
    let Some(first) = rest.next() else {
        return;
    };
    let uid = first.uid.clone();
    card.show(Showing {
        message_id: String::new(),
        invitation: first,
        also: rest.collect(),
        change: None,
        answer: None,
        me: accounts.iter().map(|(_, email)| email.clone()).collect(),
        on_calendar: None,
    });

    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(6)
        .margin_bottom(18)
        .margin_start(12)
        .margin_end(12)
        .build();
    page.append(&card.widget);
    if accounts.is_empty() {
        // The card would only hand the file to the desktop, which is not
        // what Add to Calendar means here.
        card.cannot_add();
        let note = gtk::Label::builder()
            .label(gettext(
                "No account here has a calendar Penguin Mail can add to. Add an account with \
                 a calendar, or turn one on in Preferences, under Contacts & Calendar, then \
                 open the file again.",
            ))
            .wrap(true)
            .xalign(0.0)
            .css_classes(["invitation-meta"])
            .build();
        page.append(&note);
    }
    toasts.set_child(Some(&page));

    if !accounts.is_empty() {
        offer_calendars(app, &card, uid, accounts);
    }
}

/// Puts the writable calendars of every calendar account into the card's
/// picker, each account's own first. The address follows a calendar's
/// name only when there is more than one account to tell apart.
fn offer_calendars(app: &Rc<App>, card: &Rc<EventCard>, uid: String, accounts: Vec<(AccountId, String)>) {
    let (calendar, core) = (app.core.calendar(), Rc::clone(&app.core));
    let (card, several) = (Rc::downgrade(card), accounts.len() > 1);
    glib::spawn_future_local(async move {
        let mut targets: Vec<AddTo> = Vec::new();
        for (account_id, email) in accounts {
            let calendar = Arc::clone(&calendar);
            let listed = core.call(async move { calendar.calendars(account_id).await }).await;
            if let Ok(Permitted::Done(list)) = listed {
                let label = several.then_some(email.as_str());
                targets.extend(crate::ui::invitation::targets_of(account_id, list, label));
            }
        }
        if let Some(card) = card.upgrade() {
            match targets.is_empty() {
                true => card.cannot_add(),
                false => card.set_targets(&uid, targets),
            }
        }
    });
}

use std::sync::Arc;

fn import(
    app: &Rc<App>,
    card: &Rc<EventCard>,
    toasts: &adw::ToastOverlay,
    events: Vec<Invitation>,
    target: AddTo,
) {
    let Some(uid) = card.with_showing(|showing| showing.invitation.uid.clone()) else {
        return;
    };
    let calendar = app.core.calendar();
    let (core, app) = (Rc::clone(&app.core), Rc::clone(app));
    let (card, toasts) = (Rc::downgrade(card), toasts.downgrade());
    glib::spawn_future_local(async move {
        let account_id = target.account_id;
        let done = core
            .call(async move { calendar.import(account_id, Some(&target.calendar), &events).await })
            .await;
        let (Some(card), Some(toasts)) = (card.upgrade(), toasts.upgrade()) else {
            return;
        };
        let say = |text: &str| toasts.add_toast(crate::ui::toast(text));
        match done {
            Ok(Permitted::Done(added)) if added.spots.is_empty() => say(&gettext(
                "Nothing in this file can be added, since its events have no id or start time",
            )),
            Ok(Permitted::Done(added)) => {
                card.set_added(&uid, &added.calendar, &added.spots);
                if added.skipped > 0 {
                    say(&gettext("Some events were left out, since they have no id or start time"));
                }
                // The main window reads its calendar from the copy the
                // import just wrote to.
                if let Some(window) = app.window() {
                    window.calendar_changed();
                }
            }
            Ok(Permitted::NeedsPermission) => say(&gettext(
                "Penguin Mail has no permission to use this calendar. Allow it in Preferences.",
            )),
            Err(err) => say(&fill(
                &gettext("Could not add to your calendar: {reason}"),
                &[("reason", &err.to_string())],
            )),
        }
    });
}

use mailrs_domain::translate::fill;

#[cfg(test)]
mod tests {
    use super::*;

    const REQUEST: &str = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:a@x\r\n\
        DTSTART:20261105T083000Z\r\nSUMMARY:Review\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn a_saved_invitation_reads_as_an_event_to_keep() {
        let events = events_of(REQUEST);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].card(), invitation::Card::Add);
    }

    #[test]
    fn a_cancellation_stays_news() {
        let events = events_of(&REQUEST.replace("REQUEST", "CANCEL"));
        assert_eq!(events[0].card(), invitation::Card::News);
    }

    #[test]
    fn text_that_is_no_calendar_holds_no_events() {
        assert!(events_of("just some words").is_empty());
    }
}
