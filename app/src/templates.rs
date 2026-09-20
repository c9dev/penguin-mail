//! Templates in the composer: what a placeholder stands for, and filling a
//! saved body in with it.
//!
//! A template is stored as it was written, placeholders and all, and the
//! expansion happens on the way into a message. That way one saved body
//! greets whoever it is sent to, and editing the template later still shows
//! `{{first_name}}` rather than the last person it went to.

use chrono::{DateTime, Local};
use mailrs_domain::Address;

use crate::richtext::{Block, RichBody, Span};

/// What the placeholders stand for at the moment a template goes in.
pub struct Filling {
    /// The first recipient, when the message has one.
    pub recipient: Option<Address>,
    pub subject: String,
    /// Today, as [`today`] writes it.
    pub date: String,
}

/// The day `{{date}}` becomes: "9 June 2025".
pub fn today(now: DateTime<Local>) -> String {
    now.format("%-d %B %Y").to_string()
}

/// Fills every placeholder in `text`. One nothing stands for stays as it
/// was written, because a body may hold braces of its own.
pub fn expand(text: &str, filling: &Filling) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("{{") {
        let (before, marked) = rest.split_at(at);
        out.push_str(before);
        let Some(end) = marked.find("}}") else {
            out.push_str(marked);
            return out;
        };
        match value(marked[2..end].trim(), filling) {
            Some(filled) => out.push_str(&filled),
            None => out.push_str(&marked[..end + 2]),
        }
        rest = &marked[end + 2..];
    }
    out.push_str(rest);
    out
}

/// `body` with its placeholders filled, every style left where it was.
pub fn fill(body: &RichBody, filling: &Filling) -> RichBody {
    let blocks = body
        .blocks
        .iter()
        .map(|block| Block {
            kind: block.kind,
            spans: block
                .spans
                .iter()
                .map(|span| Span {
                    text: expand(&span.text, filling),
                    ..span.clone()
                })
                .collect(),
        })
        .collect();
    RichBody { blocks }
}

/// What one placeholder stands for, or nothing when the name is not one of
/// ours. A recipient who gave no display name stands in with their address,
/// so a greeting names something they recognize either way.
fn value(name: &str, filling: &Filling) -> Option<String> {
    let recipient = filling.recipient.as_ref();
    Some(match name {
        "first_name" => recipient.map_or_else(String::new, |to| {
            to.display().split_whitespace().next().unwrap_or("").into()
        }),
        "name" => recipient.map_or_else(String::new, |to| to.display().into()),
        "email" => recipient.map_or_else(String::new, |to| to.email.clone()),
        "subject" => filling.subject.clone(),
        "date" => filling.date.clone(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::richtext::{BlockKind, Style};

    fn filling(name: Option<&str>, email: &str) -> Filling {
        Filling {
            recipient: Some(Address {
                name: name.map(str::to_string),
                email: email.into(),
            }),
            subject: "Lunch plans".into(),
            date: "9 June 2025".into(),
        }
    }

    #[test]
    fn every_placeholder_becomes_what_it_stands_for() {
        let filled = expand(
            "Hi {{first_name}}, about {{subject}} — {{name}} <{{email}}>, {{date}}.",
            &filling(Some("Ann Silva"), "ann@example.com"),
        );
        assert_eq!(
            filled,
            "Hi Ann, about Lunch plans — Ann Silva <ann@example.com>, 9 June 2025."
        );
    }

    #[test]
    fn a_recipient_with_no_name_stands_in_with_their_address() {
        let filling = filling(None, "ann.silva@example.com");
        assert_eq!(
            expand("Hi {{first_name}},", &filling),
            "Hi ann.silva@example.com,"
        );
        assert_eq!(
            expand("{{name}} {{email}}", &filling),
            "ann.silva@example.com ann.silva@example.com"
        );
    }

    #[test]
    fn a_name_of_one_word_is_the_first_name_too() {
        let filling = filling(Some("Ann"), "ann@example.com");
        assert_eq!(expand("Hi {{first_name}},", &filling), "Hi Ann,");
        assert_eq!(expand("Hi {{name}},", &filling), "Hi Ann,");
    }

    #[test]
    fn braces_that_name_nothing_stay_as_they_were_written() {
        let filling = filling(Some("Ann"), "ann@example.com");
        let body = "Use {{count}} in {braces}, send {\"a\": 1}, and end on {{";
        assert_eq!(expand(body, &filling), body);
        assert_eq!(
            expand("{{ name }} reads the same as {{name}}", &filling),
            "Ann reads the same as Ann"
        );
    }

    #[test]
    fn a_message_with_no_recipient_yet_drops_the_name() {
        let filling = Filling {
            recipient: None,
            subject: String::new(),
            date: "9 June 2025".into(),
        };
        assert_eq!(expand("Hi {{first_name}}!", &filling), "Hi !");
    }

    #[test]
    fn a_filled_body_keeps_its_styling() {
        let body = RichBody::from_markdown("Hi **{{first_name}}**,\n\n- on {{date}}");
        let filled = fill(&body, &filling(Some("Ann Silva"), "ann@example.com"));
        assert_eq!(filled.blocks[0].text(), "Hi Ann,");
        assert_eq!(
            filled.blocks[0].spans[1].style,
            Style {
                bold: true,
                ..Style::default()
            }
        );
        let last = filled.blocks.last().unwrap();
        assert_eq!(last.kind, BlockKind::Bullet);
        assert_eq!(last.text(), "on 9 June 2025");
    }
}
