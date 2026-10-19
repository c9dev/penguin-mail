//! The language the interface is in: where its words come from, what the
//! person may pick, and how a choice reaches gettext.
//!
//! Left alone, gettext follows the desktop, reading `LANGUAGE`,
//! `LC_ALL`, `LC_MESSAGES` and `LANG` in that order. A person who picks a
//! language instead gets `LANGUAGE` set from the preference, which is
//! ahead of all of them. Either way it has to happen before the first word
//! is built, so [`bind`] runs at the top of `main`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use mailrs_domain::translate::DOMAIN;

include!(concat!(env!("OUT_DIR"), "/languages.rs"));

/// Where to read the translations from, when the usual places are wrong.
const LOCALE_DIR: &str = "PENGUIN_MAIL_LOCALE_DIR";

/// The desktop's own `LANGUAGE`, empty when it set none, as the first copy
/// of the app found it. A copy the app starts again inherits it, and
/// [`environment`] says why that matters.
const DESKTOP_LANGUAGE: &str = "PENGUIN_MAIL_DESKTOP_LANGUAGE";

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

/// What the environment holds once a start has settled the language: the
/// desktop's own `LANGUAGE`, to hand on to the next copy, and the
/// `LANGUAGE` this copy runs with, `None` for none at all.
#[derive(Debug, PartialEq)]
struct Environment {
    desktop: OsString,
    language: Option<OsString>,
}

/// Settles the language from the desktop's `LANGUAGE` as an earlier copy
/// handed it on (`inherited`), the `LANGUAGE` this copy was started with,
/// and the preference.
///
/// The app restarts itself for a new language, to return memory and after
/// an update, and each new copy inherits the environment of the old one,
/// `LANGUAGE` included. Taken at face value, that would keep a language the
/// person has since switched away from, because Follow System leaves
/// `LANGUAGE` alone. So the first copy notes what the desktop set, and every
/// copy after it starts from that note rather than from what it inherited.
fn environment(inherited: Option<&OsStr>, language: Option<&OsStr>, chosen: &str) -> Environment {
    let desktop = inherited
        .or(language)
        .map(OsStr::to_os_string)
        .unwrap_or_default();
    let language = if !chosen.is_empty() {
        Some(OsString::from(chosen))
    } else if desktop.is_empty() {
        None
    } else {
        Some(desktop.clone())
    };
    Environment { desktop, language }
}

/// Points gettext at the translations, in `chosen` or in whatever the
/// desktop asked for when `chosen` is empty. Nothing a person reads may be
/// built before this runs, or the first words out are English whatever the
/// locale says.
pub fn bind(chosen: &str) {
    // Startup, before any window or worker: nothing else is reading the
    // environment or the locale while these set them.
    unsafe {
        let settled = environment(
            std::env::var_os(DESKTOP_LANGUAGE).as_deref(),
            std::env::var_os("LANGUAGE").as_deref(),
            chosen,
        );
        std::env::set_var(DESKTOP_LANGUAGE, &settled.desktop);
        match &settled.language {
            Some(language) => std::env::set_var("LANGUAGE", language),
            None => std::env::remove_var("LANGUAGE"),
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

/// The languages the person may pick, American English first, since the
/// source strings are written in it, then every translation whose
/// catalogue is installed, British English among them. A language is left out until
/// its `.mo` is there, so the list never offers one that would do nothing.
pub fn choices() -> Vec<Language> {
    let dir = locale_dir();
    let mut languages = vec![Language {
        code: "en".into(),
        name: "English (United States)".into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn start(inherited: Option<&str>, language: Option<&str>, chosen: &str) -> Environment {
        environment(
            inherited.map(OsStr::new),
            language.map(OsStr::new),
            chosen,
        )
    }

    /// A first start keeps the desktop's own LANGUAGE for a later restart
    /// and leaves it in force while the preference follows the desktop.
    #[test]
    fn a_first_start_keeps_what_the_desktop_asked_for() {
        let env = start(None, Some("de_DE:de"), "");
        assert_eq!(env.desktop, OsString::from("de_DE:de"));
        assert_eq!(env.language, Some(OsString::from("de_DE:de")));
    }

    #[test]
    fn a_chosen_language_goes_ahead_of_the_desktop() {
        let env = start(None, Some("de_DE:de"), "ru");
        assert_eq!(env.desktop, OsString::from("de_DE:de"));
        assert_eq!(env.language, Some(OsString::from("ru")));
    }

    /// A restart from the app inherits the LANGUAGE the old copy set. Back
    /// on Follow System, the new copy speaks the desktop's language again,
    /// not the one the person just left.
    #[test]
    fn following_the_system_after_a_restart_drops_the_old_choice() {
        let env = start(Some("de_DE:de"), Some("ru"), "");
        assert_eq!(env.language, Some(OsString::from("de_DE:de")));
        assert_eq!(env.desktop, OsString::from("de_DE:de"));
    }

    #[test]
    fn a_desktop_without_language_gets_none_back() {
        let env = start(Some(""), Some("ru"), "");
        assert_eq!(env.language, None);
        assert_eq!(env.desktop, OsString::new());
    }

    #[test]
    fn a_desktop_without_language_stays_without_it() {
        let env = start(None, None, "");
        assert_eq!(env.language, None);
        assert_eq!(env.desktop, OsString::new());
    }

    #[test]
    fn a_new_choice_after_a_restart_still_wins() {
        let env = start(Some(""), Some("ru"), "el_GR");
        assert_eq!(env.language, Some(OsString::from("el_GR")));
        assert_eq!(env.desktop, OsString::new());
    }

    /// The source strings are American English, and British English is a
    /// catalogue beside them, so the list says which English each is.
    #[test]
    fn the_source_language_says_it_is_american_english() {
        let first = &choices()[0];
        assert_eq!(
            (first.code.as_str(), first.name.as_str()),
            ("en", "English (United States)")
        );
    }
}
