//! What a person types into the search box, read as a query tree for a
//! server that reads no Gmail syntax. The operators are the ones the
//! search box suggests and the assistant is told about, spelled as Gmail
//! spells them, so the same words search a Gmail account and an IMAP one.

use std::iter::Peekable;
use std::str::Chars;

use chrono::NaiveDate;

use super::{MEGABYTE, Query, Term, label_spelling, plain};
use crate::category::PERSONAL;
use crate::{MailSet, Role};

/// Days in the month and in the year that `newer_than:2m` and
/// `older_than:1y` count back. Gmail counts calendar months; a tree counts
/// days.
const MONTH: u32 = 30;
const YEAR: u32 = 365;

/// How many brackets and dashes deep a typed search may nest. Past this,
/// a bracket or a dash reads as nothing and the words after it are still
/// searched, so no text can build a tree deep enough to exhaust a 2 MB
/// thread's stack in the reader or in anything that walks the tree.
pub const MAX_DEPTH: usize = 32;

/// The operators this reader knows. Any other `name:value` is words.
const OPERATORS: &[&str] = &[
    "from",
    "to",
    "subject",
    "has",
    "is",
    "newer_than",
    "older_than",
    "after",
    "before",
    "newer",
    "older",
    "larger",
    "smaller",
    "size",
    "label",
    "in",
    "category",
];

/// `text` as a query tree. Words side by side must all match. `OR`
/// between two items, or braces around several, lets any of them match,
/// and binds tighter than a space, as in Gmail. A dash right before a
/// word, a phrase or a group negates it, and parentheses group. An
/// operator this reader does not know, or a value it cannot read, is
/// searched for as words. An empty search matches every message.
/// Brackets and dashes nest at most [`MAX_DEPTH`] deep.
pub fn parse(text: &str) -> Query {
    let mut reader = Reader {
        tokens: tokens(text),
        at: 0,
        depth: 0,
    };
    all(reader.sequence(None))
}

/// `query` with each typed mailbox name replaced by the name of the
/// mailbox in `names` that Gmail's search spells the same way, so
/// `label:work-clients` finds the mailbox `Work/Clients`. A name no
/// mailbox spells stays as typed.
///
/// This recurses once per level of the tree. A tree from [`parse`] is at
/// most about twice [`MAX_DEPTH`] deep, and one from a folder or a smart
/// mailbox a few levels deep, so the stack holds for any tree the
/// product builds.
pub fn resolve_names(query: Query, names: &[String]) -> Query {
    match query {
        Query::Term(Term::MailboxNamed(typed)) => {
            let wanted = label_spelling(&typed);
            let name = names
                .iter()
                .find(|name| label_spelling(name) == wanted)
                .cloned()
                .unwrap_or(typed);
            Query::Term(Term::MailboxNamed(name))
        }
        Query::Term(term) => Query::Term(term),
        Query::And(items) => Query::And(resolve_all(items, names)),
        Query::Or(items) => Query::Or(resolve_all(items, names)),
        Query::Not(inner) => Query::Not(Box::new(resolve_names(*inner, names))),
    }
}

fn resolve_all(items: Vec<Query>, names: &[String]) -> Vec<Query> {
    items
        .into_iter()
        .map(|item| resolve_names(item, names))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Open,
    Close,
    OpenAny,
    CloseAny,
    Minus,
    Or,
    /// A quoted phrase, searched for as it stands.
    Phrase(String),
    /// A bare word, or an operator with its value, quotes taken out.
    Word(String),
}

fn tokens(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '(' | ')' | '{' | '}' => {
                chars.next();
                out.push(match c {
                    '(' => Token::Open,
                    ')' => Token::Close,
                    '{' => Token::OpenAny,
                    _ => Token::CloseAny,
                });
            }
            '"' => {
                chars.next();
                out.push(Token::Phrase(quoted(&mut chars)));
            }
            '-' => {
                chars.next();
                // A dash negates only what follows it without a space.
                if chars.peek().is_some_and(|next| !next.is_whitespace()) {
                    out.push(Token::Minus);
                }
            }
            _ => {
                let word = word(&mut chars);
                out.push(match word.as_str() {
                    "OR" => Token::Or,
                    _ => Token::Word(word),
                });
            }
        }
    }
    out
}

/// The text up to the closing double quote, or to the end.
fn quoted(chars: &mut Peekable<Chars<'_>>) -> String {
    chars.by_ref().take_while(|c| *c != '"').collect()
}

/// One word, up to a space or a bracket. A quoted part inside it, as in
/// `from:"Ann Smith"`, keeps its spaces and loses its quotes.
fn word(chars: &mut Peekable<Chars<'_>>) -> String {
    let mut word = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() || matches!(c, '(' | ')' | '{' | '}') {
            break;
        }
        chars.next();
        match c {
            '"' => word.push_str(&quoted(chars)),
            _ => word.push(c),
        }
    }
    word
}

struct Reader {
    tokens: Vec<Token>,
    at: usize,
    /// The brackets and dashes open around the token at `at`.
    depth: usize,
}

impl Reader {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        token
    }

    /// The items up to `end`, which it takes, or to the end of the text.
    /// A closing bracket that closes nothing is passed over.
    fn sequence(&mut self, end: Option<Token>) -> Vec<Query> {
        let mut items = Vec::new();
        while let Some(token) = self.peek().cloned() {
            if Some(&token) == end.as_ref() {
                self.at += 1;
                break;
            }
            if matches!(token, Token::Close | Token::CloseAny) {
                self.at += 1;
                continue;
            }
            items.extend(self.either());
        }
        items
    }

    /// One item, or several joined by `OR`.
    fn either(&mut self) -> Option<Query> {
        let mut any: Vec<Query> = self.unary().into_iter().collect();
        while self.peek() == Some(&Token::Or) {
            self.at += 1;
            any.extend(self.unary());
        }
        match any.len() {
            0 => None,
            1 => any.pop(),
            _ => Some(Query::Or(any)),
        }
    }

    fn unary(&mut self) -> Option<Query> {
        let mut token = self.next()?;
        // Each dash and each bracket costs a level of recursion here and
        // one in the tree. Past the cap they read as nothing.
        if self.depth >= MAX_DEPTH {
            while matches!(token, Token::Minus | Token::Open | Token::OpenAny) {
                token = self.next()?;
            }
        }
        self.depth += 1;
        let item = match token {
            Token::Minus => self.unary().map(not),
            Token::Open => {
                let items = self.sequence(Some(Token::Close));
                (!items.is_empty()).then(|| all(items))
            }
            Token::OpenAny => {
                let mut items = self.sequence(Some(Token::CloseAny));
                match items.len() {
                    0 => None,
                    1 => items.pop(),
                    _ => Some(Query::Or(items)),
                }
            }
            Token::Phrase(text) => words(&text),
            Token::Word(word) => term(&word),
            Token::Or | Token::Close | Token::CloseAny => None,
        };
        self.depth -= 1;
        item
    }
}

/// Items that must all match, as one query. An empty `And` holds for
/// every message, so it adds nothing beside other items.
fn all(items: Vec<Query>) -> Query {
    let mut items: Vec<Query> = items
        .into_iter()
        .filter(|item| !matches!(item, Query::And(inner) if inner.is_empty()))
        .collect();
    if items.len() == 1
        && let Some(one) = items.pop()
    {
        return one;
    }
    Query::And(items)
}

fn not(query: Query) -> Query {
    Query::Not(Box::new(query))
}

/// Text searched for anywhere in a message, or nothing when only the
/// characters Gmail's syntax reserves were typed.
fn words(text: &str) -> Option<Query> {
    (!plain(text).is_empty()).then(|| Query::Term(Term::Words(text.trim().to_string())))
}

fn term(word: &str) -> Option<Query> {
    // Gmail reads AND where a space already says it.
    if word == "AND" {
        return None;
    }
    let Some((name, value)) = word.split_once(':') else {
        return words(word);
    };
    let name = name.to_ascii_lowercase();
    if !OPERATORS.contains(&name.as_str()) {
        return words(word);
    }
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    operator(&name, value).or_else(|| words(word))
}

fn operator(name: &str, value: &str) -> Option<Query> {
    let lower = value.to_lowercase();
    let term = match name {
        "from" => Term::From(value.to_string()),
        "to" => Term::To(value.to_string()),
        "subject" => Term::Subject(value.to_string()),
        "has" if lower == "attachment" => Term::HasAttachment,
        "is" => return is(&lower),
        "newer_than" => Term::NewerThan(days(&lower)?),
        "older_than" => return Some(not(Query::Term(Term::NewerThan(days(&lower)?)))),
        "after" | "newer" => Term::Since(day(value)?),
        "before" | "older" => Term::Before(day(value)?),
        "larger" | "size" => Term::Larger(size(&lower)?),
        // Smaller than n bytes is not larger than n - 1.
        "smaller" => return Some(not(Query::Term(Term::Larger(size(&lower)? - 1)))),
        "label" => Term::MailboxNamed(value.to_string()),
        "in" => return Some(in_place(value, &lower)),
        "category" => Term::In(MailSet::Category(category(&lower))),
        _ => return None,
    };
    Some(Query::Term(term))
}

fn is(value: &str) -> Option<Query> {
    Some(match value {
        "unread" => Query::Term(Term::Unread),
        "read" => not(Query::Term(Term::Unread)),
        "starred" | "flagged" => Query::Term(Term::Flagged),
        "muted" => Query::is_in(MailSet::muted()),
        "important" => Query::is_in(MailSet::Role(Role::Important)),
        _ => return None,
    })
}

/// A role mailbox as Gmail spells it, or any other mailbox by its name.
/// `in:anywhere` adds nothing: a tree has no term for "the Junk and the
/// Trash too", so the search looks where it always looks.
fn in_place(value: &str, lower: &str) -> Query {
    let role = match lower {
        "inbox" => Role::Inbox,
        "sent" => Role::Sent,
        "draft" | "drafts" => Role::Drafts,
        "trash" => Role::Trash,
        "spam" | "junk" => Role::Junk,
        "archive" => Role::Archive,
        "starred" | "flagged" => return Query::Term(Term::Flagged),
        "anywhere" => return Query::And(Vec::new()),
        _ => return Query::Term(Term::MailboxNamed(value.to_string())),
    };
    Query::is_in(MailSet::Role(role))
}

/// Gmail's category names as the ids the store keeps.
fn category(value: &str) -> String {
    match value {
        "primary" | "personal" => PERSONAL.to_string(),
        other => format!("CATEGORY_{}", other.to_uppercase()),
    }
}

/// `7d`, `2m` or `1y` as days. A bare number counts days.
fn days(value: &str) -> Option<u32> {
    let (count, per) = match value.char_indices().last()? {
        (i, 'd') => (&value[..i], 1),
        (i, 'm') => (&value[..i], MONTH),
        (i, 'y') => (&value[..i], YEAR),
        _ => (value, 1),
    };
    count.parse::<u32>().ok()?.checked_mul(per)
}

/// `2026/02/01` or `2026-02-01`.
fn day(value: &str) -> Option<NaiveDate> {
    ["%Y/%m/%d", "%Y-%m-%d"]
        .iter()
        .find_map(|format| NaiveDate::parse_from_str(value, format).ok())
}

/// `5m`, `500k` or a count of bytes, already in lower case.
fn size(value: &str) -> Option<i64> {
    let (count, per) = match value.char_indices().last()? {
        (i, 'm') => (&value[..i], MEGABYTE),
        (i, 'k') => (&value[..i], 1024),
        _ => (value, 1),
    };
    count
        .parse::<i64>()
        .ok()
        .filter(|n| *n >= 0)?
        .checked_mul(per)
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::{MAX_DEPTH, parse, resolve_names};
    use crate::query::{MEGABYTE, Query, Term};
    use crate::{MailSet, Role};

    fn term(term: Term) -> Query {
        Query::Term(term)
    }

    fn not(query: Query) -> Query {
        Query::Not(Box::new(query))
    }

    fn words(text: &str) -> Query {
        term(Term::Words(text.into()))
    }

    fn from(who: &str) -> Query {
        term(Term::From(who.into()))
    }

    #[test]
    fn plain_words_must_all_match() {
        assert_eq!(parse("kites"), words("kites"));
        assert_eq!(
            parse("  kites   moss "),
            Query::And(vec![words("kites"), words("moss")])
        );
    }

    #[test]
    fn an_empty_search_matches_every_message() {
        assert_eq!(parse(""), Query::And(vec![]));
        assert_eq!(parse("  \"\" () in:anywhere "), Query::And(vec![]));
    }

    #[test]
    fn each_operator_reads_as_its_term() {
        let day = |month, day| NaiveDate::from_ymd_opt(2026, month, day).unwrap();
        let cases = [
            ("from:ann@example.com", from("ann@example.com")),
            ("FROM:Ann", from("Ann")),
            ("to:bo", term(Term::To("bo".into()))),
            ("subject:kites", term(Term::Subject("kites".into()))),
            ("has:attachment", term(Term::HasAttachment)),
            ("is:unread", term(Term::Unread)),
            ("is:read", not(term(Term::Unread))),
            ("is:starred", term(Term::Flagged)),
            ("is:flagged", term(Term::Flagged)),
            ("is:muted", Query::is_in(MailSet::muted())),
            ("newer_than:7d", term(Term::NewerThan(7))),
            ("newer_than:2m", term(Term::NewerThan(60))),
            ("older_than:1y", not(term(Term::NewerThan(365)))),
            ("after:2026/02/01", term(Term::Since(day(2, 1)))),
            ("before:2026-03-01", term(Term::Before(day(3, 1)))),
            ("larger:5M", term(Term::Larger(5 * MEGABYTE))),
            ("larger:1500", term(Term::Larger(1500))),
            ("smaller:10k", not(term(Term::Larger(10 * 1024 - 1)))),
            (
                "label:work-clients",
                term(Term::MailboxNamed("work-clients".into())),
            ),
            ("in:inbox", Query::is_in(MailSet::Role(Role::Inbox))),
            ("in:spam", Query::is_in(MailSet::Role(Role::Junk))),
            ("in:receipts", term(Term::MailboxNamed("receipts".into()))),
            (
                "category:social",
                Query::is_in(MailSet::Category("CATEGORY_SOCIAL".into())),
            ),
        ];
        for (typed, wanted) in cases {
            assert_eq!(parse(typed), wanted, "{typed}");
        }
    }

    #[test]
    fn a_quoted_phrase_stays_whole_and_an_operator_takes_a_quoted_value() {
        assert_eq!(parse("\"lunch on thursday\""), words("lunch on thursday"));
        assert_eq!(
            parse("from:\"Ann Smith\" moss"),
            Query::And(vec![from("Ann Smith"), words("moss")])
        );
    }

    #[test]
    fn or_binds_tighter_than_a_space_and_braces_list_alternatives() {
        assert_eq!(
            parse("from:ann OR from:bo kites"),
            Query::And(vec![
                Query::Or(vec![from("ann"), from("bo")]),
                words("kites")
            ])
        );
        assert_eq!(
            parse("{from:ann from:bo}"),
            Query::Or(vec![from("ann"), from("bo")])
        );
        assert_eq!(
            parse("from:ann or from:bo"),
            Query::And(vec![from("ann"), words("or"), from("bo")])
        );
    }

    #[test]
    fn a_dash_negates_the_word_phrase_or_group_right_after_it() {
        assert_eq!(parse("-kites"), not(words("kites")));
        assert_eq!(
            parse("-\"lunch on thursday\""),
            not(words("lunch on thursday"))
        );
        assert_eq!(
            parse("-(from:ann is:starred)"),
            not(Query::And(vec![from("ann"), term(Term::Flagged)]))
        );
        assert_eq!(
            parse("e-mail - kites"),
            Query::And(vec![words("e-mail"), words("kites")])
        );
    }

    #[test]
    fn what_this_reader_cannot_read_is_searched_as_words() {
        assert_eq!(parse("re:"), words("re:"));
        assert_eq!(parse("10:30"), words("10:30"));
        assert_eq!(parse("newer_than:soon"), words("newer_than:soon"));
        assert_eq!(parse("is:snoozed"), words("is:snoozed"));
        assert_eq!(parse("larger:-5"), words("larger:-5"));
        assert_eq!(parse("from: kites"), words("kites"));
    }

    #[test]
    fn brackets_left_open_or_closed_twice_read_what_is_there() {
        assert_eq!(parse("(from:ann"), from("ann"));
        assert_eq!(
            parse("from:ann)) moss"),
            Query::And(vec![from("ann"), words("moss")])
        );
        assert_eq!(parse("OR kites OR"), words("kites"));
    }

    /// Levels in `query`, a lone term counting one.
    fn depth(query: &Query) -> usize {
        match query {
            Query::Term(_) => 1,
            Query::Not(inner) => 1 + depth(inner),
            Query::And(items) | Query::Or(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
        }
    }

    /// `parse(text)` on a thread with the 2 MB stack a tokio worker has.
    fn parse_on_a_small_stack(text: String) -> Query {
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || parse(&text))
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn deep_nesting_reads_as_a_bounded_tree_on_a_small_stack() {
        let texts = [
            "(".repeat(10_000),
            "-a -".repeat(10_000),
            format!("{}a", "-".repeat(10_000)),
            format!("{}a", "-(".repeat(10_000)),
            format!("{}a", "{".repeat(10_000)),
        ];
        for text in texts {
            let tree = parse_on_a_small_stack(text.clone());
            assert!(depth(&tree) <= 2 * MAX_DEPTH + 2, "{}", &text[..8]);
        }
    }

    #[test]
    fn words_past_the_depth_cap_are_still_searched() {
        let text = format!("{}kites", "(".repeat(100));
        assert_eq!(parse_on_a_small_stack(text), words("kites"));
    }

    #[test]
    fn an_unclosed_quote_and_text_in_any_script_read_as_words() {
        assert_eq!(parse("\"lunch"), words("lunch"));
        assert_eq!(
            parse("café -é"),
            Query::And(vec![words("café"), not(words("é"))])
        );
        assert_eq!(
            parse("日本語 from:東京"),
            Query::And(vec![words("日本語"), from("東京")])
        );
    }

    #[test]
    fn a_typed_label_takes_the_name_of_the_mailbox_it_spells() {
        let names = ["INBOX".to_string(), "Work/Clients".to_string()];
        assert_eq!(
            resolve_names(parse("label:work-clients kites"), &names),
            Query::And(vec![
                term(Term::MailboxNamed("Work/Clients".into())),
                words("kites")
            ])
        );
        assert_eq!(
            resolve_names(parse("-in:Receipts"), &names),
            not(term(Term::MailboxNamed("Receipts".into())))
        );
    }
}
