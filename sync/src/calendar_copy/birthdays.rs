//! Read-only birthday occurrences derived from the local address books.

use std::sync::Arc;

use chrono::{DateTime, Datelike};
use mailrs_domain::calendar::{Calendar, Event, Kind, Occurrence};
use mailrs_domain::contact::birthday_in_year;
use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_store::address_book::{self, Contact};

use super::CalendarCopy;
use crate::{Accounts, SyncError};

pub const ID: &str = "penguin-mail:contact-birthdays";
const DAY: EpochMillis = 86_400_000;

pub fn calendar() -> Calendar {
    Calendar {
        id: ID.into(),
        name: gettext("Birthdays"),
        color: "#9141ac".into(),
        shown: true,
        ..Calendar::default()
    }
}

fn occurrence(contact: &Contact, year: i32) -> Option<Occurrence> {
    let (date, age) = birthday_in_year(&contact.details.birthday, year)?;
    let name = if contact.display().is_empty() {
        gettext("Unnamed Contact")
    } else {
        contact.display().to_string()
    };
    let title = match age {
        Some(age) => fill(
            &gettext("{name}'s birthday ({age})"),
            &[("name", &name), ("age", &age.to_string())],
        ),
        None => fill(&gettext("{name}'s birthday"), &[("name", &name)]),
    };
    let start = date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis();
    let event = Event {
        calendar: ID.into(),
        id: format!("{year}/{}", contact.resource),
        start,
        end: start + DAY,
        all_day: true,
        zone: "UTC".into(),
        title,
        kind: Kind::Birthday,
        reminders: Some(Vec::new()),
        ..Event::default()
    };
    Some(Occurrence {
        account_id: contact.account_id,
        start: event.start,
        end: event.end,
        event: Arc::new(event),
    })
}

fn in_range(
    contacts: &[Contact],
    accounts: &[AccountId],
    from: EpochMillis,
    to: EpochMillis,
) -> Vec<Occurrence> {
    let (Some(first), Some(last)) = (
        DateTime::from_timestamp_millis(from),
        DateTime::from_timestamp_millis(to),
    ) else {
        return Vec::new();
    };
    if from >= to {
        return Vec::new();
    }
    let mut found = Vec::new();
    for contact in contacts
        .iter()
        .filter(|c| accounts.contains(&c.account_id) && c.details.members.is_none())
    {
        // Calendar reads cover at most a year, including both boundary years.
        for year in first.year()..=last.year().min(first.year() + 1) {
            if let Some(o) = occurrence(contact, year).filter(|o| o.start < to && o.end > from) {
                found.push(o);
            }
        }
    }
    found.sort_by_key(|o| o.start);
    found
}

impl<A: Accounts> CalendarCopy<A> {
    /// Birthdays of contacts on enabled accounts, without writing calendar rows.
    pub async fn birthdays(
        &self,
        accounts: &[AccountId],
        from: EpochMillis,
        to: EpochMillis,
    ) -> Result<Vec<Occurrence>, SyncError> {
        if accounts.is_empty() {
            return Ok(Vec::new());
        }
        let accounts = accounts.to_vec();
        Ok(self
            .db
            .read(move |c| Ok(in_range(&address_book::birthdays(c)?, &accounts, from, to)))
            .await?)
    }

    /// Resolves a generated id against the current contact, so deleted contacts disappear.
    pub async fn birthday(&self, account: AccountId, id: &str) -> Result<Option<Event>, SyncError> {
        let Some((year, resource)) = id.split_once('/') else {
            return Ok(None);
        };
        let Ok(year) = year.parse::<i32>() else {
            return Ok(None);
        };
        let resource = resource.to_string();
        Ok(self
            .db
            .read(move |c| {
                Ok(address_book::one(c, account, &resource)?
                    .and_then(|c| occurrence(&c, year))
                    .map(|o| (*o.event).clone()))
            })
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn person(resource: &str, birthday: &str) -> Contact {
        Contact {
            account_id: 1,
            resource: resource.into(),
            name: Some("Sample Person".into()),
            details: mailrs_domain::contact::Details {
                birthday: birthday.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn midnight(value: &str) -> EpochMillis {
        NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    #[test]
    fn birthdays_follow_the_displayed_year_and_keep_yearless_dates() {
        let known = person("people/one", "1982-07-27");
        for value in ["1982-07-27", "19820727"] {
            assert_eq!(birthday_in_year(value, 2027).unwrap().1, Some(45));
        }
        assert!(birthday_in_year(&known.details.birthday, 1981).is_none());
        let o = occurrence(&known, 2027).unwrap();
        assert_eq!(o.start, midnight("2027-07-27"));
        assert_eq!(o.end - o.start, DAY);
        assert!(o.event.all_day);
        assert_eq!(o.event.zone, "UTC");
        assert_eq!(o.event.kind, Kind::Birthday);
        assert!(!calendar().access.can_write());
        assert_eq!(o.event.reminders, Some(vec![]));
        assert!(o.event.title.ends_with("(45)"));
        for value in ["--07-27", "--0727"] {
            assert_eq!(birthday_in_year(value, 2027).unwrap().1, None);
            assert!(
                !occurrence(&person("yearless", value), 2027)
                    .unwrap()
                    .event
                    .title
                    .contains('(')
            );
        }
        assert!(occurrence(&person("invalid", "not a date"), 2027).is_none());
    }

    #[test]
    fn ranges_filter_accounts_and_handle_leap_days_and_year_boundaries() {
        let contacts = vec![person("leap", "--02-29"), person("new-year", "2000-01-01")];
        let enabled = [1];
        assert!(
            in_range(
                &contacts,
                &[],
                midnight("2028-01-01"),
                midnight("2029-01-01")
            )
            .is_empty()
        );
        assert_eq!(
            in_range(
                &contacts,
                &enabled,
                midnight("2028-02-01"),
                midnight("2028-03-01")
            )
            .len(),
            1
        );
        assert!(
            in_range(
                &contacts,
                &enabled,
                midnight("2027-02-01"),
                midnight("2027-03-01")
            )
            .is_empty()
        );
        let found = in_range(
            &contacts,
            &enabled,
            midnight("2027-12-25"),
            midnight("2028-01-08"),
        );
        assert_eq!(found.len(), 1);
        assert!(found[0].event.title.ends_with("(28)"));
        assert!(
            in_range(
                &contacts,
                &enabled,
                midnight("2028-01-02"),
                midnight("2028-01-03")
            )
            .is_empty()
        );
        assert!(
            in_range(
                &contacts,
                &enabled,
                midnight("2027-12-31"),
                midnight("2028-01-01")
            )
            .is_empty()
        );
        assert_eq!(
            in_range(
                &contacts,
                &enabled,
                midnight("2028-01-01") + DAY / 2,
                midnight("2028-01-02")
            )
            .len(),
            1
        );
    }
}
