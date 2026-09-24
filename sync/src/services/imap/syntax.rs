//! Pieces of IMAP's command syntax the adapter prints.

use chrono::NaiveDate;

/// A day as IMAP's SEARCH writes it, `1-Feb-2026`. chrono writes `%b` in
/// English whatever the locale.
pub(super) fn imap_date(day: NaiveDate) -> String {
    day.format("%-d-%b-%Y").to_string()
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::imap_date;

    #[test]
    fn a_day_is_written_the_way_search_reads_it() {
        let day = NaiveDate::from_ymd_opt(2026, 2, 1).unwrap();
        assert_eq!(imap_date(day), "1-Feb-2026");
    }
}
