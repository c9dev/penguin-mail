//! The window an AppImage shows when the desktop will not let it start
//! the sandbox WebKit and the image loaders run in. WebKit would abort the
//! app the first time a message opened, so this comes instead of it, says
//! why, and gives the one command that allows this copy, or the packages
//! that need no setup.

use std::os::unix::process::CommandExt;

use adw::prelude::*;
use gtk::gio;
use mailrs_domain::translate::gettext;

use crate::appimage::Sandbox;

/// Shows the window for `sandbox`. Once the sandbox starts, Penguin Mail
/// starts again in its place, with the arguments this launch had.
pub fn present(gio_app: &gio::Application, sandbox: &Sandbox) {
    crate::ensure_gtk();
    let hold = gio_app.hold();
    let page = adw::StatusPage::builder()
        .icon_name("penguin-mail-dialog-warning-symbolic")
        .css_classes(["compact"])
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .halign(gtk::Align::Center)
        .build();
    let check = gtk::Button::builder()
        .label(gettext("Check Again"))
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .build();
    let mut shown: Option<gtk::Label> = None;
    match sandbox {
        Sandbox::Blocked { command } => {
            page.set_title(&gettext("Allow Penguin Mail's Sandbox"));
            page.set_description(Some(&gettext(
                "Penguin Mail opens every message in a sandbox, and this system lets an app \
                 start one only when it is allowed. Run this in a terminal once to allow this \
                 copy, then choose Check Again.",
            )));
            let (card, text) = command_card(command);
            content.append(&card);
            shown = Some(text);
        }
        Sandbox::Missing | Sandbox::Ready => {
            page.set_title(&gettext("Install the Sandbox Tools"));
            page.set_description(Some(&gettext(
                "Penguin Mail opens every message in a sandbox made with Bubblewrap. Install \
                 the bubblewrap and xdg-dbus-proxy packages, then choose Check Again.",
            )));
        }
    }
    content.append(&check);
    let packages = gtk::Label::builder()
        .use_markup(true)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label"])
        .label(gettext(
            "Or install Penguin Mail from <a href=\"https://flathub.org/apps/io.github.c9dev.penguin-mail\">Flathub</a> \
             or the <a href=\"https://snapcraft.io/penguin-mail\">Snap Store</a>, which need no setup.",
        ))
        .build();
    content.append(&packages);
    page.set_child(Some(&content));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    let window = adw::Window::builder()
        .title("Penguin Mail")
        .default_width(640)
        .default_height(680)
        .content(&toolbar)
        .build();
    crate::ui::window::track_dark_class(&window);

    // AppArmor gives a profile only to a program started after it was
    // loaded, by the path it names, so this process cannot use the one the
    // command installs. Starting the image again, as the login item does,
    // can, and that copy checks the sandbox and shows this window again if
    // it still cannot start. A mount this copy held stays until it quits.
    check.connect_clicked(move |check| {
        if let Ok((image, flags)) = crate::appimage::login() {
            let err = std::process::Command::new(image)
                .args(flags)
                .args(std::env::args_os().skip(1))
                .exec();
            tracing::warn!(error = %err, "could not start again");
        }
        check.announce(
            &gettext("The sandbox still cannot start"),
            gtk::AccessibleAnnouncementPriority::Medium,
        );
    });
    let quit = gio_app.clone();
    window.connect_destroy(move |_| {
        let _ = &hold;
        quit.quit();
    });
    window.present();
    // A selectable label that takes the focus first selects all its text.
    check.grab_focus();
    if let Some(text) = shown {
        text.select_region(0, 0);
    }
}

/// The command, as text a person can select, and a button that copies it.
fn command_card(command: &str) -> (gtk::Box, gtk::Label) {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    let text = gtk::Label::builder()
        .label(command)
        .selectable(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .css_classes(["card", "monospace", "sandbox-command"])
        .build();
    crate::ui::name(&text, &gettext("Command"));
    card.append(&text);
    let content = adw::ButtonContent::builder()
        .icon_name("penguin-mail-edit-copy-symbolic")
        .label(gettext("Copy Command"))
        .build();
    let copy = gtk::Button::builder()
        .child(&content)
        .css_classes(["pill"])
        .halign(gtk::Align::Center)
        .build();
    crate::ui::name(&copy, &gettext("Copy Command"));
    let copied = crate::ui::copied::Copied::new(&copy);
    let command = command.to_string();
    copy.connect_clicked(move |button| {
        button.clipboard().set_text(&command);
        copied.show();
        button.announce(
            &gettext("Command copied"),
            gtk::AccessibleAnnouncementPriority::Medium,
        );
    });
    card.append(&copy);
    (card, text)
}
