//! The language the interface is in: where its words come from, what the
//! person may pick, and how a choice reaches gettext.
//!
//! Left alone, gettext follows the desktop, reading `LANGUAGE`,
//! `LC_ALL`, `LC_MESSAGES` and `LANG` in that order. A person who picks a
//! language instead gets `LANGUAGE` set from the preference, which is
//! ahead of all of them. Either way it has to happen before the first word
//! is built, so [`bind`] runs at the top of `main`.

use std::path::{Path, PathBuf};

use mailrs_domain::translate::DOMAIN;

include!(concat!(env!("OUT_DIR"), "/languages.rs"));

/// Where to read the translations from, when the usual places are wrong.
const LOCALE_DIR: &str = "PENGUIN_MAIL_LOCALE_DIR";

/// One language the Language preference offers.
pub struct Language {
    /// The locale code, such as `pt_PT`. Empty follows the desktop.
    pub code: String,
    /// The language's name in that language.
    pub name: String,
}

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
    let above = crate::exe::path()
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

/// Points gettext at the translations, in `chosen` or in whatever the
/// desktop asked for when `chosen` is empty. Nothing a person reads may be
/// built before this runs, or the first words out are English whatever the
/// locale says.
pub fn bind(chosen: &str) {
    // Startup, before any window or worker: nothing else is reading the
    // environment or the locale while these set them.
    unsafe {
        if !chosen.is_empty() {
            std::env::set_var("LANGUAGE", chosen);
        }
        gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "");
        // glibc ignores LANGUAGE while the message locale is the bare C
        // one, so a desktop that sets no locale at all needs telling.
        if !chosen.is_empty() && messages_locale_is_bare() {
            let named = gettextrs::setlocale(
                gettextrs::LocaleCategory::LcMessages,
                format!("{chosen}.UTF-8"),
            );
            // A translation ships as a catalogue; the matching locale is a
            // separate thing the system may never have generated. pt_PT
            // is missing from plenty of installs. Any locale but the bare
            // C one is enough for LANGUAGE to be read, and C.UTF-8 is
            // there when nothing else is.
            if named.is_none() {
                gettextrs::setlocale(gettextrs::LocaleCategory::LcMessages, "C.UTF-8");
            }
        }
    }
    if gettextrs::bindtextdomain(DOMAIN, locale_dir()).is_err() {
        return;
    }
    let _ = gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8");
    let _ = gettextrs::textdomain(DOMAIN);
}

/// Whether the message locale is `C` or `POSIX`, which translate nothing.
fn messages_locale_is_bare() -> bool {
    let name = unsafe { gettextrs::setlocale(gettextrs::LocaleCategory::LcMessages, "") };
    name.is_none_or(|name| name == b"C" || name == b"POSIX")
}

/// The languages the person may pick, English first, then every
/// translation whose catalogue is installed. A language is left out until
/// its `.mo` is there, so the list never offers one that would do nothing.
pub fn choices() -> Vec<Language> {
    let dir = locale_dir();
    let mut languages = vec![Language {
        code: "en".into(),
        name: "English".into(),
    }];
    for (code, name) in TRANSLATED {
        let catalogue = dir
            .join(code)
            .join("LC_MESSAGES")
            .join(format!("{DOMAIN}.mo"));
        if catalogue.is_file() {
            languages.push(Language {
                code: (*code).into(),
                name: (*name).into(),
            });
        }
    }
    languages
}

/// Where `code` sits in [`choices`], counting Follow System as the first
/// row. An unknown code falls back to Follow System.
pub fn row_of(languages: &[Language], code: &str) -> u32 {
    languages
        .iter()
        .position(|language| language.code == code)
        .map_or(0, |at| at as u32 + 1)
}

/// The code the row at `index` picks, counting Follow System as the first
/// row, whose code is empty.
pub fn code_at(languages: &[Language], index: u32) -> String {
    match index.checked_sub(1) {
        Some(at) => languages
            .get(at as usize)
            .map_or_else(String::new, |language| language.code.clone()),
        None => String::new(),
    }
}
