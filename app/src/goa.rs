//! The desktop's Online Accounts, as the window meets it: which window
//! opens it on this computer, and whether it holds an address. The D-Bus
//! client and its tokens live in `mailrs_sync::goa`; the window reaches
//! them only through `Core`.

use gtk::gio::prelude::AppInfoExt;
use gtk::glib::prelude::ToVariant;
use gtk::{gio, glib};

use crate::core::Core;
use crate::packaging::Packaging;

/// The desktop files of the app that adds an account, newest name first.
const SETTINGS: [&str; 2] = [
    "applications/org.gnome.Settings.desktop",
    "applications/gnome-control-center.desktop",
];

/// What GNOME Settings is started with, and the panel to open.
const PANEL: &str = "gnome-control-center online-accounts";

/// What Linux Mint and other desktops ship to add an account.
const STANDALONE: &str = "gnome-online-accounts-gtk";

/// How the window opens Online Accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opener {
    /// Starts GNOME Settings on its Online Accounts panel.
    SettingsCommand,
    /// Asks the running GNOME Settings to open that panel over D-Bus, which
    /// a Flatpak or a Snap can do without running a program.
    SettingsAction,
    /// Starts the standalone window.
    GoaGtk,
    /// Nothing to open: the page names the applications menu.
    Menu,
}

/// Which way opens Online Accounts for a build of `packaging`.
pub fn opener(packaging: Packaging, settings_installed: bool, goa_gtk_installed: bool) -> Opener {
    if matches!(packaging, Packaging::Flatpak | Packaging::Snap) {
        Opener::SettingsAction
    } else if settings_installed {
        Opener::SettingsCommand
    } else if goa_gtk_installed {
        Opener::GoaGtk
    } else {
        Opener::Menu
    }
}

/// What opening came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    /// A window opened.
    Window,
    /// Nothing opened; the person goes to the applications menu.
    Menu,
}

/// Opens Online Accounts. A failed D-Bus call from a sandbox falls back to
/// the menu, since the person can still get there by hand.
pub async fn open_online_accounts() -> Result<Opened, glib::Error> {
    let packaging = crate::packaging::BUILT_FOR;
    match opener(packaging, has_settings(), has_standalone()) {
        Opener::SettingsCommand => launch(PANEL).map(|()| Opened::Window),
        Opener::GoaGtk => launch(STANDALONE).map(|()| Opened::Window),
        Opener::SettingsAction => Ok(match open_panel().await {
            Ok(()) => Opened::Window,
            Err(err) => {
                tracing::warn!(error = %err, "could not open the Online Accounts panel");
                Opened::Menu
            }
        }),
        Opener::Menu => Ok(Opened::Menu),
    }
}

fn launch(command: &str) -> Result<(), glib::Error> {
    let app = gio::AppInfo::create_from_commandline(command, None, gio::AppInfoCreateFlags::NONE)?;
    app.launch(&[], gio::AppLaunchContext::NONE)
}

/// `org.freedesktop.Application.ActivateAction("launch-panel", ...)` on
/// GNOME Settings, which opens the panel. The call waits at most 20
/// seconds.
async fn open_panel() -> Result<(), glib::Error> {
    let bus = gio::bus_get_future(gio::BusType::Session).await?;
    let panel = ("online-accounts", Vec::<glib::Variant>::new()).to_variant();
    let args = vec![panel];
    let params = ("launch-panel", args, glib::VariantDict::new(None).end()).to_variant();
    bus.call_future(
        Some("org.gnome.Settings"),
        "/org/gnome/Settings",
        "org.freedesktop.Application",
        "ActivateAction",
        Some(&params),
        None,
        gio::DBusCallFlags::NONE,
        20_000,
    )
    .await
    .map(|_| ())
}

/// Whether this desktop has the app that adds an account.
fn has_settings() -> bool {
    let mut dirs = glib::system_data_dirs();
    dirs.push(glib::user_data_dir());
    dirs.iter()
        .any(|dir| SETTINGS.iter().any(|file| dir.join(file).exists()))
}

fn has_standalone() -> bool {
    glib::find_program_in_path(STANDALONE).is_some()
}

/// Where `email` stands with Online Accounts, from the list `core` last
/// read: `None` when it could not be reached, otherwise whether it holds
/// the address.
pub fn known(core: &Core, email: &str) -> Option<bool> {
    core.goa_holds(email)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flatpak_opens_the_panel_through_gnome_settings() {
        assert_eq!(
            opener(Packaging::Flatpak, false, false),
            Opener::SettingsAction
        );
        assert_eq!(opener(Packaging::Snap, true, true), Opener::SettingsAction);
    }

    #[test]
    fn gnome_settings_wins_over_the_standalone_window() {
        assert_eq!(
            opener(Packaging::Native, true, true),
            Opener::SettingsCommand
        );
    }

    #[test]
    fn off_gnome_the_standalone_window_opens() {
        assert_eq!(opener(Packaging::Arch, false, true), Opener::GoaGtk);
    }

    #[test]
    fn with_neither_the_page_names_the_menu() {
        assert_eq!(opener(Packaging::Rpm, false, false), Opener::Menu);
    }
}
