//! Whether GNOME already knows an account.
//!
//! GNOME Online Accounts signs an address in once and hands it to the rest
//! of the desktop, which is how GNOME Calendar gets the user's meetings
//! and the shell clock lists them. Penguin Mail signs in for its own mail
//! and tells GNOME nothing, so an address added here alone leaves the
//! calendar empty however many invitations the inbox holds.
//!
//! This reads the file GNOME keeps its accounts in rather than asking the
//! daemon over D-Bus: the answer settles one line in the event card, and a
//! file read that fails is an answer of its own.

use std::path::{Path, PathBuf};

use gtk::gio;
use gtk::gio::prelude::AppInfoExt;

/// The file GNOME Online Accounts keeps its accounts in, under the config
/// directory.
const ACCOUNTS: &str = "goa-1.0/accounts.conf";

/// The desktop files of the app that adds an account, newest name first.
/// Nothing is offered on a desktop that has neither.
const SETTINGS: [&str; 2] = [
    "applications/org.gnome.Settings.desktop",
    "applications/gnome-control-center.desktop",
];

/// What GNOME Settings is started with, and the panel to open.
const PANEL: &str = "gnome-control-center online-accounts";

/// Whether to offer `email` to GNOME: true when this desktop has Online
/// Accounts and that address is not in them yet.
pub fn worth_offering(email: &str) -> bool {
    has_settings() && !known_in(&gtk::glib::user_config_dir(), email)
}

/// Opens Online Accounts in GNOME Settings.
pub fn open_online_accounts() -> Result<(), gtk::glib::Error> {
    let settings =
        gio::AppInfo::create_from_commandline(PANEL, None, gio::AppInfoCreateFlags::NONE)?;
    settings.launch(&[], gio::AppLaunchContext::NONE)
}

/// Whether this desktop has the app that adds an account.
fn has_settings() -> bool {
    let mut dirs = gtk::glib::system_data_dirs();
    dirs.push(gtk::glib::user_data_dir());
    dirs.iter()
        .any(|dir| SETTINGS.iter().any(|file| dir.join(file).exists()))
}

/// Whether the accounts file under `config` names `email`. Every provider
/// writes the address as an `Identity`, so one key covers Google and the
/// rest alike. A file that is missing or unreadable names nobody.
fn known_in(config: &Path, email: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(PathBuf::from(config).join(ACCOUNTS)) else {
        return false;
    };
    text.lines().any(|line| {
        line.split_once('=').is_some_and(|(key, value)| {
            key.trim().ends_with("Identity") && value.trim().eq_ignore_ascii_case(email.trim())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::known_in;

    fn accounts(dir: &std::path::Path, text: &str) {
        std::fs::create_dir_all(dir.join("goa-1.0")).unwrap();
        std::fs::write(dir.join(super::ACCOUNTS), text).unwrap();
    }

    #[test]
    fn an_address_gnome_signed_in_is_known() {
        let dir = tempfile::tempdir().unwrap();
        accounts(
            dir.path(),
            "[Account account_1702051234_0]\n\
             Provider=google\n\
             Identity=Dana.Reyes@example.com\n\
             PresentationIdentity=dana.reyes@example.com\n\
             CalendarEnabled=true\n",
        );
        assert!(known_in(dir.path(), "dana.reyes@example.com"));
        assert!(!known_in(dir.path(), "someone@example.com"));
    }

    #[test]
    fn a_desktop_with_no_online_accounts_knows_nobody() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!known_in(dir.path(), "dana.reyes@example.com"));
        accounts(dir.path(), "not a key file at all");
        assert!(!known_in(dir.path(), "dana.reyes@example.com"));
    }
}
