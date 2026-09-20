//! Matching typed text against known correspondents.

use mailrs_store::contacts::Suggestion;

/// People whose address or any word of whose name starts with `query`,
/// best first, leaving out addresses already in `entered`.
pub fn suggest<'a>(
    contacts: &'a [Suggestion],
    query: &str,
    entered: &[String],
    limit: usize,
) -> Vec<&'a Suggestion> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    contacts
        .iter()
        .filter(|c| !entered.iter().any(|e| e.eq_ignore_ascii_case(&c.email)))
        .filter(|c| {
            let email = c.email.to_lowercase();
            let name = c.name.as_deref().unwrap_or_default().to_lowercase();
            email.starts_with(&query)
                || name.starts_with(&query)
                || name.split_whitespace().any(|w| w.starts_with(&query))
                || email
                    .split(['.', '_', '-', '@'])
                    .any(|part| part.starts_with(&query))
        })
        .take(limit)
        .collect()
}

/// The part of a recipient field being typed: the text after the last
/// comma or semicolon, trimmed.
pub fn current_token(text: &str) -> (usize, &str) {
    let start = text.rfind([',', ';']).map_or(0, |i| i + 1);
    let token = &text[start..];
    let trimmed = token.trim_start();
    (start + token.len() - trimmed.len(), trimmed.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(name: Option<&str>, email: &str) -> Suggestion {
        Suggestion {
            name: name.map(str::to_string),
            email: email.into(),
            organization: None,
            photo_file: None,
            known: false,
            score: 1,
            last_seen: 0,
        }
    }

    #[test]
    fn names_and_addresses_match_by_prefix() {
        let all = [
            contact(Some("Ann Lee"), "ann@example.com"),
            contact(Some("Priya Raman"), "priya.raman@work.example"),
            contact(None, "leeroy@example.com"),
        ];
        let emails = |found: Vec<&Suggestion>| -> Vec<String> {
            found.into_iter().map(|c| c.email.clone()).collect()
        };
        assert_eq!(
            emails(suggest(&all, "lee", &[], 5)),
            ["ann@example.com", "leeroy@example.com"]
        );
        assert_eq!(
            emails(suggest(&all, "RAMAN", &[], 5)),
            ["priya.raman@work.example"]
        );
        assert_eq!(
            emails(suggest(&all, "work", &[], 5)),
            ["priya.raman@work.example"]
        );
        assert!(suggest(&all, " ", &[], 5).is_empty());
        assert_eq!(
            emails(suggest(&all, "lee", &["ANN@example.com".into()], 5)),
            ["leeroy@example.com"]
        );
    }

    #[test]
    fn the_token_is_whatever_follows_the_last_separator() {
        assert_eq!(current_token("Ann <ann@x.com>, pri"), (17, "pri"));
        assert_eq!(current_token("pri"), (0, "pri"));
        assert_eq!(current_token("a@x.com;  "), (10, ""));
    }
}
