//! The language the interface is in: where its words come from, and how
//! gettext is pointed at them.
//!
//! gettext follows the desktop, reading `LANGUAGE`, `LC_ALL`,
//! `LC_MESSAGES` and `LANG` in that order. It has to be pointed at the
//! catalogues before the first word is built, so [`bind`] runs at the top
//! of `main`.

use std::path::{Path, PathBuf};

use mailrs_domain::translate::DOMAIN;

/// Where to read the translations from, when the usual places are wrong.
const LOCALE_DIR: &str = "PENGUIN_MAIL_LOCALE_DIR";

/// The directory holding the compiled translations. An installed copy
/// keeps them under the prefix it was installed into; a copy run from the
/// build tree reads `target/locale`, which `scripts/update-po.sh` fills,
/// so `--demo` speaks the same language as an installed app.
pub fn locale_dir() -> PathBuf {
    if let Some(named) = std::env::var_os(LOCALE_DIR) {
        return PathBuf::from(named);
    }
    // `<prefix>/bin/penguin-mail` once installed, `target/<profile>/
    // penguin-mail` in the build tree: both sit two directories below the
    // one that holds the catalogues.
    let above = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().and_then(Path::parent).map(Path::to_path_buf));
    let Some(above) = above else {
        return PathBuf::from("/usr/share/locale");
    };
    let installed = above.join("share/locale");
    match installed.is_dir() {
        true => installed,
        false => above.join("locale"),
    }
}

/// Points gettext at the translations, in whatever language the desktop
/// asked for. Nothing a person reads may be built before this runs, or the
/// first words out are English whatever the locale says.
pub fn bind() {
    // Startup, before any window or worker: nothing else is reading the
    // locale while this sets it.
    unsafe { gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "") };
    if gettextrs::bindtextdomain(DOMAIN, locale_dir()).is_err() {
        return;
    }
    let _ = gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8");
    let _ = gettextrs::textdomain(DOMAIN);
}
