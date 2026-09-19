//! Suggestions while typing a search, as Apple Mail offers them.

use mailrs_store::contacts::Contact;

use crate::contacts::suggest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// What the list shows.
    pub label: String,
    /// The whole search to run when picked.
    pub query: String,
}

/// Suggestions for the word being typed at the end of `text`: the subject,
/// people it could be from or to, and labels it could name.
pub fn suggestions(text: &str, contacts: &[Contact], labels: &[String]) -> Vec<Suggestion> {
    if text.ends_with(char::is_whitespace) {
        return Vec::new();
    }
    let start = text.rfind(char::is_whitespace).map_or(0, |i| i + 1);
    let (head, word) = (&text[..start], &text[start..]);
    if word.len() < 2 || word.contains(':') {
        return Vec::new();
    }
    let with = |term: String| format!("{head}{term}");
    let mut out = vec![Suggestion {
        label: format!("Subject contains “{word}”"),
        query: with(format!("subject:{word}")),
    }];
    let people = suggest(contacts, word, &[], 4);
    for person in &people {
        let name = person.name.as_deref().unwrap_or(&person.email);
        out.push(Suggestion {
            label: format!("From {name}"),
            query: with(format!("from:{}", person.email)),
        });
    }
    if let Some(first) = people.first() {
        let name = first.name.as_deref().unwrap_or(&first.email);
        out.push(Suggestion {
            label: format!("To {name}"),
            query: with(format!("to:{}", first.email)),
        });
    }
    let lower = word.to_lowercase();
    for label in labels
        .iter()
        .filter(|l| l.to_lowercase().contains(&lower))
        .take(3)
    {
        let slug: String = label
            .to_lowercase()
            .chars()
            .map(|c| {
                if c.is_whitespace() || c == '/' {
                    '-'
                } else {
                    c
                }
            })
            .collect();
        out.push(Suggestion {
            label: format!("In {}", label.replace('/', " › ")),
            query: with(format!("label:{slug}")),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_word_becomes_subject_people_and_labels() {
        let contacts = [Contact {
            name: Some("Ann Lee".into()),
            email: "ann@example.com".into(),
            score: 3,
            last_seen: 0,
        }];
        let labels = ["Work/Annual".to_string(), "Travel".to_string()];
        let found = suggestions("invoice an", &contacts, &labels);
        let queries: Vec<&str> = found.iter().map(|s| s.query.as_str()).collect();
        assert_eq!(
            queries,
            [
                "invoice subject:an",
                "invoice from:ann@example.com",
                "invoice to:ann@example.com",
                "invoice label:work-annual",
            ]
        );
        assert_eq!(found[1].label, "From Ann Lee");
        assert!(suggestions("invoice ", &contacts, &labels).is_empty());
        assert!(suggestions("from:an", &contacts, &labels).is_empty());
        assert!(suggestions("a", &contacts, &labels).is_empty());
    }
}
