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
    pub addresses: Vec<String>,
    pub urls: Vec<String>,
    pub groups: Vec<String>,
    pub dates: Vec<Field>,
    pub custom: Vec<Field>,
}

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
    pub addresses: Option<Vec<String>>,
    pub urls: Option<Vec<String>>,
    pub groups: Option<Vec<String>>,
    pub dates: Option<Vec<Field>>,
    pub custom: Option<Vec<Field>>,
}

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
        .map(|date| (date, has_year))
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
}
