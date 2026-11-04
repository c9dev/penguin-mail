//! Attach a shared mailbox through an existing Microsoft 365 sign-in.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::Account;
use mailrs_domain::translate::{fill, gettext};
use mailrs_sync::sign_in::{Browser, Wanted};

use crate::app::App;

pub(super) fn row(app: &Rc<App>, owner: &Account) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(gettext("Add Shared Mailbox"))
        .subtitle(gettext(
            "Open a shared mailbox using this account's sign-in.",
        ))
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("list-add-symbolic"));
    let (app, owner) = (Rc::downgrade(app), owner.clone());
    row.connect_activated(move |row| {
        if let Some(app) = app.upgrade() {
            present(&app, &owner, row);
        }
    });
    row
}

fn present(app: &Rc<App>, owner: &Account, parent: &impl IsA<gtk::Widget>) {
    let dialog = adw::Dialog::builder()
        .title(gettext("Add Shared Mailbox"))
        .content_width(460)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    for edge in ["margin-top", "margin-bottom", "margin-start", "margin-end"] {
        content.set_property(edge, 18i32);
    }
    let description = gtk::Label::builder().wrap(true).xalign(0.0)
        .label(fill(&gettext("Enter the shared mailbox address. Sign in as {account} to allow access. Your administrator must have granted you Full Access to the mailbox."), &[("account", &owner.email)])).build();
    let group = adw::PreferencesGroup::new();
    let address = adw::EntryRow::builder()
        .title(gettext("Shared Mailbox Address"))
        .input_purpose(gtk::InputPurpose::Email)
        .build();
    group.add(&address);
    let status = gtk::Label::builder().wrap(true).xalign(0.0).build();
    let add = gtk::Button::with_label(&gettext("Add Shared Mailbox"));
    add.add_css_class("suggested-action");
    for child in [
        description.upcast_ref::<gtk::Widget>(),
        group.upcast_ref(),
        status.upcast_ref(),
        add.upcast_ref(),
    ] {
        content.append(child);
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));
    dialog.set_child(Some(&toolbar));
    let (app, owner, window) = (Rc::downgrade(app), owner.clone(), dialog.downgrade());
    add.connect_clicked(move |button| {
        let Some(parsed) = crate::add_account::Address::parse(&address.text()) else {
            status.set_label(&crate::add_account::not_an_address());
            return;
        };
        let (Some(app), Some(dialog)) = (app.upgrade(), window.upgrade()) else {
            return;
        };
        button.set_sensitive(false);
        address.set_sensitive(false);
        status.set_label(&gettext(
            "Finish signing in with Microsoft, then return here.",
        ));
        let (cancel, canceled) = async_channel::bounded(1);
        dialog.connect_closed(move |_| {
            let _ = cancel.try_send(());
        });
        let (urls, opened) = async_channel::unbounded::<String>();
        let launcher_parent = dialog.downgrade();
        glib::spawn_future_local(async move {
            while let Ok(url) = opened.recv().await {
                let Some(dialog) = launcher_parent.upgrade().filter(|d| d.is_visible()) else {
                    break;
                };
                gtk::UriLauncher::new(&url).launch(
                    dialog.root().and_downcast::<gtk::Window>().as_ref(),
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }
        });
        let (owner, button, address, status) = (
            owner.clone(),
            button.clone(),
            address.clone(),
            status.clone(),
        );
        glib::spawn_future_local(async move {
            let result = async {
                let (signed, _) = app
                    .core
                    .sign_in_in_browser(
                        Browser::Microsoft,
                        urls,
                        Wanted::SharedMail(owner.email.clone()),
                        canceled,
                        None,
                    )
                    .await?;
                if !dialog.is_visible() {
                    anyhow::bail!(gettext("Canceled."));
                }
                status.set_label(&gettext("Checking…"));
                app.core.add_shared_mailbox(signed.id, parsed.full()).await
            }
            .await;
            if !dialog.is_visible() {
                return;
            }
            match result {
                Ok(_) => {
                    dialog.close();
                }
                Err(err) => {
                    status.set_label(&err.to_string());
                    button.set_sensitive(true);
                    address.set_sensitive(true);
                }
            }
        });
    });
    dialog.present(Some(parent));
}
