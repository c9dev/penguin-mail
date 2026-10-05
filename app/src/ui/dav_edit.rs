//! Typing an IMAP account's CalDAV and CardDAV URLs, for a server
//! discovery did not find. Each URL is checked with the account's login
//! before it is kept.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::Account;
use mailrs_domain::translate::{fill, gettext, with_reason};
use mailrs_store::services::ServiceKind;

use crate::app::App;

pub fn present(app: &Rc<App>, account: &Account, calendar: &str, contacts: &str, parent: &impl IsA<gtk::Widget>, done: impl Fn() + 'static) {
    let caldav = adw::EntryRow::builder().title(gettext("Calendar (CalDAV) URL")).text(calendar).build();
    let carddav = adw::EntryRow::builder().title(gettext("Contacts (CardDAV) URL")).text(contacts).build();
    let problem = gtk::Label::builder().wrap(true).xalign(0.0).css_classes(["error"]).visible(false).build();
    let group = adw::PreferencesGroup::builder()
        .description(fill(&gettext("Penguin Mail signs in to these with {account}'s password."), &[("account", &account.email)]))
        .build();
    group.add(&caldav);
    group.add(&carddav);
    let page = adw::PreferencesPage::new();
    page.add(&group);
    let save = gtk::Button::builder().label(gettext("Save")).css_classes(["suggested-action"]).build();
    let header = adw::HeaderBar::new();
    header.pack_end(&save);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&page);
    content.append(&problem);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .title(gettext("Calendar and Contacts Servers"))
        .content_width(480)
        .child(&toolbar)
        .build();
    let (app, account, weak) = (Rc::clone(app), account.clone(), dialog.downgrade());
    let (was_calendar, was_contacts) = (calendar.to_string(), contacts.to_string());
    let done = Rc::new(done);
    save.connect_clicked(move |button| {
        let wanted: Vec<(ServiceKind, String)> = [
            (ServiceKind::CalDav, caldav.text().trim().to_string(), &was_calendar),
            (ServiceKind::CardDav, carddav.text().trim().to_string(), &was_contacts),
        ]
        .into_iter()
        .filter(|(_, now, was)| !now.is_empty() && now != *was)
        .map(|(kind, now, _)| (kind, now))
        .collect();
        button.set_sensitive(false);
        let (app, account, weak, problem, button, done) = (Rc::clone(&app), account.clone(), weak.clone(), problem.clone(), button.clone(), Rc::clone(&done));
        glib::spawn_future_local(async move {
            for (kind, url) in wanted {
                if let Err(err) = app.core.use_typed_server(account.clone(), kind, url).await {
                    problem.set_label(&with_reason(&gettext("Could not use that server: {reason}"), &err, &[]));
                    problem.set_visible(true);
                    button.set_sensitive(true);
                    return;
                }
            }
            done();
            if let Some(dialog) = weak.upgrade() {
                dialog.close();
            }
        });
    });
    dialog.present(Some(parent));
}
