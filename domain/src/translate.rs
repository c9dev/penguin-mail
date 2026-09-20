//! The words a person reads, in their own language. Every crate that
//! writes such a word calls through here, so there is one text domain and
//! one way to put a value into a sentence.
//!
//! `gettext` and `ngettext` come straight from the C library the desktop
//! already uses. A string with a value in it goes through [`fill`] rather
//! than `format!`, because a translator has to be able to move the value
//! to wherever the sentence wants it.

pub use gettextrs::{gettext, ngettext};

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

#[cfg(test)]
mod tests {
    use super::fill;

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
