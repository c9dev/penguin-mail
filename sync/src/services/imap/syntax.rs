//! Pieces of IMAP's command syntax the adapter prints.

use chrono::NaiveDate;

/// A day as IMAP's SEARCH writes it, `1-Feb-2026`. chrono writes `%b` in
/// English whatever the locale.
pub(super) fn imap_date(day: NaiveDate) -> String {
    day.format("%-d-%b-%Y").to_string()
}

/// Text as an IMAP quoted string, its quotes and backslashes escaped. A
/// quoted string carries no line break (RFC 3501 section 4.3), so each
/// becomes a space. Text beyond ASCII stays in the quotes: the client
/// sends such a string as a literal and opens the keys with
/// `CHARSET UTF-8` itself, and the fake reads it as it stands.
pub(super) fn string(text: &str) -> String {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\r', '\n'], " ");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::imap_date;
    use super::string;

    #[test]
    fn a_day_is_written_the_way_search_reads_it() {
        let day = NaiveDate::from_ymd_opt(2026, 2, 1).unwrap();
        assert_eq!(imap_date(day), "1-Feb-2026");
    }

    #[test]
    fn text_is_quoted_with_its_quotes_escaped_and_its_line_breaks_gone() {
        assert_eq!(string("Ann"), "\"Ann\"");
        assert_eq!(string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(string("José"), "\"José\"");
        assert_eq!(string("two\r\nlines"), "\"two  lines\"");
    }
}
