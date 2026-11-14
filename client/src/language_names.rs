//! A dictionary's locale code as a person reads it: "en_AU" as "English
//! (Australia)", in the language of the interface.
//!
//! The names come from the iso-codes package, which GNOME's own language
//! settings read too: its JSON lists of ISO 639 languages and ISO 3166
//! countries, translated through its gettext domains. Without the package
//! a code stays a code.

use std::collections::HashMap;

use mailrs_domain::translate::{fill, gettext};

const LANGUAGES: &str = "/usr/share/iso-codes/json/iso_639-2.json";
const COUNTRIES: &str = "/usr/share/iso-codes/json/iso_3166-1.json";

/// Language and country names by their two-letter codes.
#[derive(Debug, Default)]
pub struct IsoNames {
    /// "en" to "English".
    languages: HashMap<String, String>,
    /// "AU" to "Australia".
    countries: HashMap<String, String>,
}

impl IsoNames {
    /// Reads the iso-codes lists, with each name in the interface's
    /// language where iso-codes has a translation. Empty when the package
    /// is missing.
    pub fn load() -> Self {
        IsoNames {
            languages: read(LANGUAGES, "639-2", "iso_639-2"),
            countries: read(COUNTRIES, "3166-1", "iso_3166-1"),
        }
    }

    /// "English (Australia)" for "en_AU", "French" for "fr", and the code
    /// itself when the lists do not know it.
    pub fn name(&self, code: &str) -> String {
        let (language, country) = match code.split_once(['_', '-']) {
            Some((language, country)) => (language, Some(country)),
            None => (code, None),
        };
        let Some(language) = self.languages.get(&language.to_lowercase()) else {
            return code.to_string();
        };
        match country.and_then(|c| self.countries.get(&c.to_uppercase())) {
            Some(country) => fill(
                &gettext("{language} ({country})"),
                &[("language", language), ("country", country)],
            ),
            None if country.is_some() => code.to_string(),
            None => language.clone(),
        }
    }
}

/// Reads one iso-codes list: the entries under `key` that have a
/// two-letter code, named in the interface's language through `domain`.
/// ISO 639-2 gives some languages two names ("Spanish; Castilian"); the
/// first is the one people use.
fn read(path: &str, key: &str, domain: &str) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return HashMap::new();
    };
    let Some(entries) = json.get(key).and_then(|v| v.as_array()) else {
        return HashMap::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let code = entry.get("alpha_2")?.as_str()?;
            let name = entry.get("name")?.as_str()?;
            let translated = gettextrs::dgettext(domain, name);
            let first = translated.split(';').next().unwrap_or(&translated).trim();
            Some((code.to_string(), first.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> IsoNames {
        IsoNames {
            languages: [("en", "English"), ("fr", "French")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .into(),
            countries: [("AU", "Australia")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .into(),
        }
    }

    #[test]
    fn a_dictionary_with_a_country_names_both() {
        assert_eq!(names().name("en_AU"), "English (Australia)");
    }

    #[test]
    fn a_dictionary_with_no_country_names_the_language() {
        assert_eq!(names().name("fr"), "French");
    }

    #[test]
    fn a_code_the_lists_do_not_know_stays_a_code() {
        assert_eq!(names().name("xx_YY"), "xx_YY");
        assert_eq!(names().name("en_ZZ"), "en_ZZ");
    }

    #[test]
    fn the_package_names_english_in_australia_when_it_is_installed() {
        if !std::path::Path::new(LANGUAGES).exists() {
            return;
        }
        assert_eq!(IsoNames::load().name("en_AU"), "English (Australia)");
    }
}
