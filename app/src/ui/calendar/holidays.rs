//! Google's public holiday calendars, one per region, that Holiday
//! Calendars offers. Google names each by an id of the form
//! `en.<region>#holiday@group.v.calendar.google.com`; the region words
//! are Google's own, not country codes.

use mailrs_domain::translate::{fill, gettext};

/// One region's holiday calendar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// Google's word for the region, inside the calendar's id.
    pub key: &'static str,
    /// The region's name in the reader's language.
    pub name: String,
}

impl Region {
    /// The calendar's id on Google.
    pub fn calendar_id(&self) -> String {
        format!("en.{}#holiday@group.v.calendar.google.com", self.key)
    }

    /// What the calendar is called until Google's own name arrives.
    pub fn calendar_name(&self) -> String {
        fill(&gettext("Holidays in {region}"), &[("region", &self.name)])
    }
}

/// The region the list starts with: the owner lives in Portugal.
const FIRST: &str = "portuguese";

/// Every region, Portugal first and then the rest by name. Google's
/// religious calendars (Christian, Islamic, Jewish) are not regions and
/// are left out.
pub fn regions() -> Vec<Region> {
    let mut all: Vec<Region> = [
        ("australian", gettext("Australia")),
        ("austrian", gettext("Austria")),
        ("brazilian", gettext("Brazil")),
        ("canadian", gettext("Canada")),
        ("china", gettext("China")),
        ("danish", gettext("Denmark")),
        ("dutch", gettext("Netherlands")),
        ("finnish", gettext("Finland")),
        ("french", gettext("France")),
        ("german", gettext("Germany")),
        ("greek", gettext("Greece")),
        ("hong_kong", gettext("Hong Kong")),
        ("indian", gettext("India")),
        ("indonesian", gettext("Indonesia")),
        ("irish", gettext("Ireland")),
        ("italian", gettext("Italy")),
        ("japanese", gettext("Japan")),
        ("malaysia", gettext("Malaysia")),
        ("mexican", gettext("Mexico")),
        ("new_zealand", gettext("New Zealand")),
        ("norwegian", gettext("Norway")),
        ("philippines", gettext("Philippines")),
        ("polish", gettext("Poland")),
        ("portuguese", gettext("Portugal")),
        ("russian", gettext("Russia")),
        ("sa", gettext("South Africa")),
        ("singapore", gettext("Singapore")),
        ("south_korea", gettext("South Korea")),
        ("spain", gettext("Spain")),
        ("swedish", gettext("Sweden")),
        ("taiwan", gettext("Taiwan")),
        ("thai", gettext("Thailand")),
        ("uk", gettext("United Kingdom")),
        ("usa", gettext("United States")),
        ("vietnamese", gettext("Vietnam")),
    ]
    .into_iter()
    .map(|(key, name)| Region { key, name })
    .collect();
    all.sort_by_cached_key(|region| (region.key != FIRST, region.name.to_lowercase()));
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portugal_comes_first() {
        let first = &regions()[0];
        assert_eq!(first.name, "Portugal");
        assert_eq!(first.calendar_id(), "en.portuguese#holiday@group.v.calendar.google.com");
    }

    #[test]
    fn the_rest_follow_by_name() {
        let names: Vec<String> = regions().into_iter().skip(1).map(|r| r.name).collect();
        let mut sorted = names.clone();
        sorted.sort_by_key(|name| name.to_lowercase());
        assert_eq!(names, sorted);
        assert!(!names.contains(&"Portugal".to_string()), "Portugal shows once");
    }

    #[test]
    fn each_region_has_one_calendar() {
        let ids: std::collections::HashSet<String> = regions().iter().map(Region::calendar_id).collect();
        assert_eq!(ids.len(), regions().len());
    }

    #[test]
    fn a_holiday_calendar_is_named_for_its_region_until_google_names_it() {
        assert_eq!(regions()[0].calendar_name(), "Holidays in Portugal");
    }
}
