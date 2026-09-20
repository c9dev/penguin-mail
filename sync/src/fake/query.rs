//! The part of Gmail's search language that `FakeGmail` answers: labels and
//! folders, read and flag state, attachments, addresses, dates, sizes, and
//! plain words. Braces mean "any of these", a leading `-` means "not this",
//! and quotes hold a phrase together.
//!
//! A term Gmail knows and this does not, such as `after:2026/01/01`, matches
//! its value as a plain word rather than failing the search.

use mailrs_domain::{EpochMillis, MessageMeta, system_label};
use mailrs_gmail::RemoteLabel;

const DAY_MILLIS: i64 = 24 * 60 * 60 * 1000;

/// A parsed search. Every node must hold for a message to match.
pub struct Query {
    nodes: Vec<Node>,
    /// Whether the search asked for Spam or Trash by name. Gmail leaves both
    /// out of every other search.
    spam: bool,
    trash: bool,
    anywhere: bool,
}

enum Node {
    One {
        negated: bool,
        term: Term,
    },
    /// `{a b c}`: one of these is enough.
    Any(Vec<Node>),
}

enum Term {
    /// A system label id, or a user label's name in Gmail's dashed form.
    Label(String),
    Unread,
    Starred,
    Attachment,
    Anywhere,
    From(String),
    To(String),
    Subject(String),
    NewerThanDays(i64),
    OlderThanDays(i64),
    LargerThan(i64),
    SmallerThan(i64),
    Word(String),
}

impl Query {
    pub fn parse(text: &str) -> Query {
        let nodes: Vec<Node> = tokens(text).iter().filter_map(|t| node(t)).collect();
        let mut query = Query {
            nodes,
            spam: false,
            trash: false,
            anywhere: false,
        };
        let asked = &mut (false, false, false);
        for node in &query.nodes {
            node.asked_for(asked);
        }
        (query.spam, query.trash, query.anywhere) = *asked;
        query
    }

    /// Whether `meta` belongs in this search's results.
    pub fn matches(&self, meta: &MessageMeta, labels: &[RemoteLabel], now: EpochMillis) -> bool {
        let hidden = [
            (system_label::SPAM, self.spam),
            (system_label::TRASH, self.trash),
        ];
        for (label, asked) in hidden {
            if !asked && !self.anywhere && meta.has_label(label) {
                return false;
            }
        }
        self.nodes.iter().all(|n| n.matches(meta, labels, now))
    }
}

impl Node {
    fn matches(&self, meta: &MessageMeta, labels: &[RemoteLabel], now: EpochMillis) -> bool {
        match self {
            Node::One { negated, term } => term.matches(meta, labels, now) != *negated,
            Node::Any(nodes) => nodes.iter().any(|n| n.matches(meta, labels, now)),
        }
    }

    /// Records the folders the search names, so Spam and Trash stay hidden
    /// from every other search.
    fn asked_for(&self, found: &mut (bool, bool, bool)) {
        match self {
            Node::One { negated: true, .. } => {}
            Node::One { term, .. } => match term {
                Term::Label(id) if id == system_label::SPAM => found.0 = true,
                Term::Label(id) if id == system_label::TRASH => found.1 = true,
                Term::Anywhere => found.2 = true,
                _ => {}
            },
            Node::Any(nodes) => {
                for node in nodes {
                    node.asked_for(found);
                }
            }
        }
    }
}

impl Term {
    fn matches(&self, meta: &MessageMeta, labels: &[RemoteLabel], now: EpochMillis) -> bool {
        let contains = |haystack: &str, needle: &str| haystack.to_lowercase().contains(needle);
        let addresses = |list: &[mailrs_domain::Address], needle: &str| {
            list.iter().any(|a| {
                contains(&a.email, needle) || a.name.as_deref().is_some_and(|n| contains(n, needle))
            })
        };
        match self {
            Term::Label(wanted) => has_label(meta, labels, wanted),
            Term::Unread => meta.is_unread(),
            Term::Starred => meta.has_label(system_label::STARRED),
            Term::Attachment => meta.has_attachments,
            Term::Anywhere => true,
            Term::From(text) => meta
                .from
                .as_ref()
                .is_some_and(|a| addresses(std::slice::from_ref(a), text)),
            Term::To(text) => addresses(&meta.to, text) || addresses(&meta.cc, text),
            Term::Subject(text) => contains(&meta.subject, text),
            Term::NewerThanDays(days) => meta.date >= now - days * DAY_MILLIS,
            Term::OlderThanDays(days) => meta.date < now - days * DAY_MILLIS,
            Term::LargerThan(bytes) => meta.size > *bytes,
            Term::SmallerThan(bytes) => meta.size < *bytes,
            Term::Word(text) => {
                contains(&meta.subject, text)
                    || contains(&meta.snippet, text)
                    || meta
                        .from
                        .as_ref()
                        .is_some_and(|a| addresses(std::slice::from_ref(a), text))
            }
        }
    }
}

/// Whether `meta` carries a label, named either by its id or the way Gmail
/// writes it in a search.
fn has_label(meta: &MessageMeta, labels: &[RemoteLabel], wanted: &str) -> bool {
    meta.label_ids.iter().any(|id| {
        id == wanted
            || labels
                .iter()
                .any(|l| &l.id == id && search_name(&l.name) == wanted)
    })
}

/// A label name as a search writes it: lower case, with spaces and slashes
/// as dashes.
fn search_name(name: &str) -> String {
    name.to_lowercase()
        .replace(|c: char| c.is_whitespace() || c == '/', "-")
}

/// Splits a search into terms, keeping a quoted phrase and a braced group
/// whole.
fn tokens(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut depth = 0u32;
    for ch in text.chars() {
        if ch == '"' {
            quoted = !quoted;
            continue;
        }
        if !quoted {
            if ch == '{' {
                depth += 1;
                if depth == 1 {
                    take(&mut current, &mut found);
                    current.push(ch);
                    continue;
                }
            } else if ch == '}' && depth > 0 {
                depth -= 1;
                if depth == 0 {
                    current.push(ch);
                    take(&mut current, &mut found);
                    continue;
                }
            } else if ch.is_whitespace() && depth == 0 {
                take(&mut current, &mut found);
                continue;
            }
        }
        current.push(ch);
    }
    take(&mut current, &mut found);
    found
}

fn take(current: &mut String, found: &mut Vec<String>) {
    if !current.is_empty() {
        found.push(std::mem::take(current));
    }
}

fn node(token: &str) -> Option<Node> {
    if let Some(inner) = token.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
        let nodes: Vec<Node> = tokens(inner).iter().filter_map(|t| node(t)).collect();
        return (!nodes.is_empty()).then_some(Node::Any(nodes));
    }
    let (negated, rest) = match token.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, token),
    };
    let (flipped, term) = term(rest)?;
    Some(Node::One {
        negated: negated != flipped,
        term,
    })
}

/// One term, and whether it means the opposite of what it names, as
/// `is:read` means "not unread".
fn term(text: &str) -> Option<(bool, Term)> {
    let Some((prefix, value)) = text.split_once(':') else {
        return word(text).map(|t| (false, t));
    };
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let lower = value.to_lowercase();
    let term = match prefix.to_lowercase().as_str() {
        "in" | "label" | "category" => folder(&lower)?,
        "is" => match lower.as_str() {
            "unread" => Term::Unread,
            "read" => return Some((true, Term::Unread)),
            "starred" | "flagged" => Term::Starred,
            "muted" => Term::Label(system_label::MUTE.into()),
            _ => return None,
        },
        "has" if lower == "attachment" => Term::Attachment,
        "from" => Term::From(lower),
        "to" | "cc" | "bcc" => Term::To(lower),
        "subject" => Term::Subject(lower),
        "newer_than" => Term::NewerThanDays(days(&lower)?),
        "older_than" => Term::OlderThanDays(days(&lower)?),
        "larger" | "size" => Term::LargerThan(bytes(&lower)?),
        "smaller" => Term::SmallerThan(bytes(&lower)?),
        // A term this fake does not know: match its value as a word, as the
        // demo's search did before.
        _ => word(&lower)?,
    };
    Some((false, term))
}

fn folder(name: &str) -> Option<Term> {
    Some(match name {
        "inbox" => Term::Label(system_label::INBOX.into()),
        "sent" => Term::Label(system_label::SENT.into()),
        "spam" | "junk" => Term::Label(system_label::SPAM.into()),
        "trash" => Term::Label(system_label::TRASH.into()),
        "draft" | "drafts" => Term::Label(system_label::DRAFT.into()),
        "important" => Term::Label(system_label::IMPORTANT.into()),
        "starred" => Term::Starred,
        "unread" => Term::Unread,
        "anywhere" | "all" => Term::Anywhere,
        "primary" => Term::Label(system_label::CATEGORY_PERSONAL.into()),
        "updates" => Term::Label(system_label::CATEGORY_UPDATES.into()),
        "promotions" => Term::Label(system_label::CATEGORY_PROMOTIONS.into()),
        "social" => Term::Label(system_label::CATEGORY_SOCIAL.into()),
        "forums" => Term::Label(system_label::CATEGORY_FORUMS.into()),
        other => Term::Label(other.into()),
    })
}

fn word(text: &str) -> Option<Term> {
    let text = text.trim().to_lowercase();
    (!text.is_empty()).then_some(Term::Word(text))
}

/// `30d`, `2m`, or `1y` as a number of days.
fn days(value: &str) -> Option<i64> {
    let (digits, unit) = value.split_at(value.find(|c: char| !c.is_ascii_digit())?);
    let count = digits.parse::<i64>().ok()?;
    Some(match unit {
        "d" => count,
        "m" => count * 30,
        "y" => count * 365,
        _ => return None,
    })
}

/// `5M`, `500K`, or a plain byte count.
fn bytes(value: &str) -> Option<i64> {
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let (digits, unit) = value.split_at(split);
    let count = digits.parse::<i64>().ok()?;
    Some(match unit {
        "" | "b" => count,
        "k" => count * 1024,
        "m" => count * 1024 * 1024,
        _ => return None,
    })
}
