//! The words a person reads, in their own language. Every crate that
//! writes such a word calls through here, so there is one text domain and
//! one way to put a value into a sentence.
//!
//! `gettext`, `ngettext` and `pgettext` come straight from the C library
//! the desktop already uses. `pgettext` takes a context for an English word
//! that other languages split in two, such as "Archive" the button and
//! "Archive" the mailbox. A string with a value in it goes through [`fill`] rather
//! than `format!`, because a translator has to be able to move the value
//! to wherever the sentence wants it. A date's weekday and month names
//! come from [`date_locale`], so they match the words around them.

use std::cell::Cell;

use chrono::Locale;
pub use gettextrs::{gettext, ngettext, pgettext};

/// The text domain, which is also the name of the `.mo` files.
pub const DOMAIN: &str = "penguin-mail";

/// `text` with every `{name}` replaced by the value given for that name.
///
/// A name with no value stays as it is: a translator who mistypes one
/// should see it in the sentence rather than lose the rest of it.
///
/// ```
/// # use mailrs_domain::translate::fill;
/// assert_eq!(fill("Sent to {who}", &[("who", "Ann")]), "Sent to Ann");
/// ```
pub fn fill(text: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        let Some(close) = rest.find('}') else {
            break;
        };
        let name = &rest[1..close];
        match values.iter().find(|(key, _)| *key == name) {
            Some((_, value)) => out.push_str(value),
            None => out.push_str(&rest[..=close]),
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// The plural form of `text`, with every `{name}` filled in as [`fill`]
/// does it. `count` chooses the form, so the language decides how many
/// forms there are rather than the code.
pub fn fill_plural(one: &str, many: &str, count: usize, values: &[(&str, &str)]) -> String {
    fill(&ngettext(one, many, count as u32), values)
}

/// `said` with the error in place of `{reason}` and every other `{name}`
/// filled from `values`, for sentences such as "Could not save {file}:
/// {reason}". The caller passes `gettext` of the literal, so xgettext still
/// finds the msgid where the sentence is used.
///
/// One pass fills every name, so a file called `{reason}.pdf` keeps its
/// name rather than taking the error's text.
pub fn with_reason(said: &str, reason: &impl std::fmt::Display, values: &[(&str, &str)]) -> String {
    let reason = reason.to_string();
    let mut all = Vec::with_capacity(values.len() + 1);
    all.extend_from_slice(values);
    all.push(("reason", reason.as_str()));
    fill(said, &all)
}

thread_local! {
    /// The locale the names in a date come from, looked up on first use.
    static DATE_LOCALE: Cell<Option<Locale>> = const { Cell::new(None) };
}

/// The locale for the weekday and month names a date pattern's `%A`,
/// `%a`, `%B` and `%b` stand for, to pass to chrono's `format_localized`.
///
/// It follows the catalogue gettext picked rather than `LC_TIME`. A
/// person who chose English on a Portuguese desktop reads "Today at", and
/// "Sep" belongs beside it, not "set".
pub fn date_locale() -> Locale {
    DATE_LOCALE.with(|cell| {
        cell.get().unwrap_or_else(|| {
            let locale = locale_named(&catalogue_language());
            cell.set(Some(locale));
            locale
        })
    })
}

/// Fixes the locale [`date_locale`] gives on this thread, as a test does
/// to see a date in another language.
pub fn set_date_locale(code: &str) {
    DATE_LOCALE.with(|cell| cell.set(Some(locale_named(code))));
}

/// The `Language` field of the catalogue in use, such as `pt_PT`, or
/// nothing when the interface is in the English of the source.
fn catalogue_language() -> String {
    // gettext answers the empty message id with the header of the
    // catalogue it chose. The id goes in as a value rather than a literal
    // so xgettext does not take it for a word to translate.
    let header = gettext(String::new());
    header
        .lines()
        .find_map(|line| line.strip_prefix("Language:"))
        .map(|code| code.trim().to_string())
        .unwrap_or_default()
}

/// The chrono locale for a language code. A bare language such as `de`
/// takes the country of the same name, and a code chrono does not know
/// writes English names.
fn locale_named(code: &str) -> Locale {
    let code = code.split(['.', '@']).next().unwrap_or_default();
    Locale::try_from(code)
        .or_else(|_| Locale::try_from(format!("{code}_{}", code.to_uppercase()).as_str()))
        .unwrap_or(Locale::POSIX)
}

#[cfg(test)]
mod tests {
    use chrono::Locale;

    use super::{fill, locale_named, with_reason};

    #[test]
    fn a_language_code_finds_its_locale() {
        assert_eq!(locale_named("pt_PT"), Locale::pt_PT);
        assert_eq!(locale_named("pt_PT.UTF-8"), Locale::pt_PT);
        assert_eq!(locale_named("de"), Locale::de_DE);
        assert_eq!(locale_named(""), Locale::POSIX);
        assert_eq!(locale_named("xx"), Locale::POSIX);
    }

    #[test]
    fn the_error_goes_where_the_reason_is() {
        assert_eq!(
            with_reason("Could not save: {reason}", &"disk full", &[]),
            "Could not save: disk full"
        );
    }

    #[test]
    fn other_names_fill_in_the_same_pass_as_the_reason() {
        assert_eq!(
            with_reason(
                "Could not open {file}: {reason}",
                &"gone",
                &[("file", "{reason}.pdf")]
            ),
            "Could not open {reason}.pdf: gone"
        );
    }

    #[test]
    fn values_land_where_their_names_are() {
        assert_eq!(
            fill("{count} of {total}", &[("total", "9"), ("count", "3")]),
            "3 of 9"
        );
    }

    #[test]
    fn a_name_nobody_gave_a_value_stays_in_the_sentence() {
        assert_eq!(fill("Hello {who}", &[("name", "Ann")]), "Hello {who}");
    }

    #[test]
    fn text_with_no_names_comes_back_whole() {
        assert_eq!(fill("Archived", &[]), "Archived");
        assert_eq!(fill("An unclosed { brace", &[]), "An unclosed { brace");
    }
}
