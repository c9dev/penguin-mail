//! Contact details shared by address book services.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Details {
    /// The revision read from CardDAV, for detecting edits on another device.
    pub revision: String,
    pub uid: String,
    /// Group cards have members instead of representing a person.
    pub members: Option<Vec<String>>,
    pub phones: Vec<String>,
    pub birthday: String,
    pub notes: String,
    pub addresses: Vec<Address>,
    pub urls: Vec<String>,
    pub groups: Vec<String>,
    pub dates: Vec<Field>,
    pub custom: Vec<Field>,
    /// The card sits in an address book this account may only read.
    pub read_only: bool,
}

/// A postal address as vCard's seven ADR parts, in order: PO box, extended
/// address, street, locality, region, postal code, country. A part may hold
/// a newline, as a street of two lines does.
pub type Address = [String; 7];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub label: String,
    pub value: String,
}

/// Only the fields the person changed. None leaves a server's data alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub birthday: Option<String>,
    pub notes: Option<String>,
    pub addresses: Option<Vec<Address>>,
    pub urls: Option<Vec<String>>,
    pub groups: Option<Vec<String>>,
    pub dates: Option<Vec<Field>>,
    pub custom: Option<Vec<Field>>,
}

/// The largest contact photo a save sends.
pub const MOST_PHOTO_BYTES: usize = 512 * 1024;

/// A date as `YYYY-MM-DD` or `YYYYMMDD`, or `--MM-DD` or `--MMDD` without a
/// year, and whether it has one. Apple writes year 1604 for a birthday
/// without a year, so that year counts as none.
fn contact_date(value: &str) -> Option<(chrono::NaiveDate, bool)> {
    let value = value.trim();
    let (value, has_year) = match value.strip_prefix("--") {
        Some(part) => (format!("2000{}", part.replace('-', "")), false),
        None => (value.replace('-', ""), true),
    };
    if value.len() != 8 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    chrono::NaiveDate::parse_from_str(&value, "%Y%m%d")
        .ok()
        .map(|date| (date, has_year && !value.starts_with("1604")))
}

/// Whether `value` is a date a contact may hold.
pub fn is_date(value: &str) -> bool {
    contact_date(value).is_some()
}

/// The year, if known, the month and the day of a contact date.
pub fn date_parts(value: &str) -> Option<(Option<i32>, u32, u32)> {
    use chrono::Datelike;
    let (date, has_year) = contact_date(value)?;
    Some((has_year.then(|| date.year()), date.month(), date.day()))
}

/// Why a save would be refused before it reaches a server, in the reader's
/// language: a malformed date, a date without its label, or a photo that is
/// not a small PNG. An empty `photo` removes the photo.
pub fn check(changes: &Changes, photo: Option<&[u8]>) -> Result<(), String> {
    use crate::translate::gettext;
    let dates = changes.dates.iter().flatten();
    if changes
        .birthday
        .iter()
        .chain(dates.clone().map(|f| &f.value))
        .any(|date| !date.is_empty() && !is_date(date))
    {
        return Err(gettext(
            "Use YYYY-MM-DD for dates, or --MM-DD when the year is unknown.",
        ));
    }
    if dates.clone().any(|d| d.label.is_empty() || d.value.is_empty()) {
        return Err(gettext("A date needs a label and a value."));
    }
    if photo.is_some_and(|p| {
        p.len() > MOST_PHOTO_BYTES || (!p.is_empty() && !p.starts_with(b"\x89PNG\r\n\x1a\n"))
    }) {
        return Err(gettext(
            "Contact photos must be PNG images no larger than 512 KB.",
        ));
    }
    Ok(())
}

/// The birthday in a displayed year and the age reached, if a year is known.
/// February 29 has no occurrence in a non-leap year.
pub fn birthday_in_year(value: &str, year: i32) -> Option<(chrono::NaiveDate, Option<u32>)> {
    use chrono::Datelike;
    let (birth, has_year) = contact_date(value)?;
    let date = chrono::NaiveDate::from_ymd_opt(year, birth.month(), birth.day())?;
    let age = if has_year {
        Some(date.years_since(birth)?)
    } else {
        None
    };
    Some((date, age))
}

/// Use the same editable form for compact vCard and dashed service dates.
pub fn date_for_edit(value: &str) -> String {
    match contact_date(value) {
        Some((date, has_year)) => date
            .format(if has_year { "%Y-%m-%d" } else { "--%m-%d" })
            .to_string(),
        None => value.to_string(),
    }
}

pub fn birthday_label(value: &str, today: chrono::NaiveDate) -> String {
    use crate::translate::{date_locale, fill, fill_plural, gettext};
    let Some((date, has_year)) = contact_date(value) else {
        return value.to_string();
    };
    let day = date
        .format_localized(&gettext("%-d %B"), date_locale())
        .to_string();
    if !has_year {
        return day;
    }
    let year = date.format("%Y").to_string();
    let Some(age) = today.years_since(date) else {
        return fill(
            &gettext("{date} ({year})"),
            &[("date", &day), ("year", &year)],
        );
    };
    fill_plural(
        "{date} ({year}), {age} year old",
        "{date} ({year}), {age} years old",
        age as usize,
        &[("date", &day), ("year", &year), ("age", &age.to_string())],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn birthdays_show_the_day_and_current_age_without_inventing_a_year() {
        crate::translate::set_date_locale("en");
        let today = NaiveDate::from_ymd_opt(2026, 4, 23).unwrap();
        assert_eq!(date_for_edit("19850423"), "1985-04-23");
        assert_eq!(date_for_edit("--0229"), "--02-29");
        for value in ["19850423", "1985-04-23"] {
            assert_eq!(
                birthday_label(value, today),
                "23 April (1985), 41 years old"
            );
        }
        assert_eq!(
            birthday_label("1985-04-24", today),
            "24 April (1985), 40 years old"
        );
        assert_eq!(
            birthday_label("2025-04-23", today),
            "23 April (2025), 1 year old"
        );
        for value in ["--0229", "--02-29"] {
            assert_eq!(birthday_label(value, today), "29 February");
        }
        assert_eq!(birthday_label("2030-04-23", today), "23 April (2030)");
        for value in ["", "unknown", "1985-02-29", "--02-30", "éé"] {
            assert_eq!(date_for_edit(value), value);
            assert_eq!(birthday_label(value, today), value);
        }
    }

    #[test]
    fn apples_placeholder_year_1604_reads_as_no_year() {
        crate::translate::set_date_locale("en");
        let today = NaiveDate::from_ymd_opt(2026, 4, 23).unwrap();
        assert_eq!(birthday_label("1604-03-15", today), "15 March");
        assert_eq!(birthday_in_year("16040315", 2027).unwrap().1, None);
        assert_eq!(date_for_edit("1604-03-15"), "--03-15");
    }

    #[test]
    fn a_save_is_checked_for_dates_and_photos_before_it_leaves() {
        let birthday = |value: &str| Changes {
            birthday: Some(value.into()),
            ..Default::default()
        };
        for value in ["2023-02-29", "--13-01", "broken"] {
            assert!(check(&birthday(value), None).is_err(), "{value}");
        }
        for value in ["", "1985-04-23", "--0229", "--02-29"] {
            assert!(check(&birthday(value), None).is_ok(), "{value}");
        }
        let unlabelled = Changes {
            dates: Some(vec![Field {
                label: String::new(),
                value: "2020-01-01".into(),
            }]),
            ..Default::default()
        };
        assert!(check(&unlabelled, None).is_err());
        assert!(check(&Changes::default(), Some(&[0; 32])).is_err());
        assert!(check(&Changes::default(), Some(&[])).is_ok());
    }
}
