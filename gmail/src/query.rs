//! Prints a query tree as Gmail search text, the text the folders and smart
//! mailboxes sent before they became trees, byte for byte.

use mailrs_domain::category;
use mailrs_domain::mailbox::keyword;
use mailrs_domain::query::{MEGABYTE, Query, Term, label_spelling};
use mailrs_domain::{MailSet, Role};

/// Where a subtree sits, which decides whether it needs brackets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    /// The whole search, or one item of an `And`: Gmail joins terms
    /// separated by spaces with AND, so nothing is needed.
    Top,
    /// One item of an `Or`, or the operand of a `Not`, where several terms
    /// need parentheses to stay together.
    Inside,
}

/// The Gmail search text for `query`. A term Gmail has no operator for
/// prints as nothing, and the operators around it leave it out.
pub fn print(query: &Query) -> String {
    printed(query, Place::Top)
}

fn printed(query: &Query, place: Place) -> String {
    match query {
        Query::Term(term) => printed_term(term, place),
        Query::And(items) => {
            let pieces = pieces(items, Place::Top);
            match (pieces.len(), place) {
                (0, _) => String::new(),
                (1, _) | (_, Place::Top) => pieces.join(" "),
                (_, Place::Inside) => format!("({})", pieces.join(" ")),
            }
        }
        Query::Or(items) => {
            let pieces = pieces(items, Place::Inside);
            match pieces.len() {
                0 => String::new(),
                1 => pieces.join(""),
                _ => format!("{{{}}}", pieces.join(" ")),
            }
        }
        Query::Not(inner) => {
            let inner = printed(inner, Place::Inside);
            if inner.is_empty() {
                inner
            } else {
                format!("-{inner}")
            }
        }
    }
}

/// Each item printed at `place`, leaving out the ones that print as nothing.
fn pieces(items: &[Query], place: Place) -> Vec<String> {
    items
        .iter()
        .map(|item| printed(item, place))
        .filter(|piece| !piece.is_empty())
        .collect()
}

fn printed_term(term: &Term, place: Place) -> String {
    match term {
        Term::From(text) => operator("from:", text),
        Term::To(text) => operator("to:", text),
        Term::Subject(text) => operator("subject:", text),
        Term::Words(text) => quoted(text).unwrap_or_default(),
        Term::Since(day) => format!("after:{}", day.format("%Y/%m/%d")),
        Term::Before(day) => format!("before:{}", day.format("%Y/%m/%d")),
        Term::NewerThan(days) => format!("newer_than:{days}d"),
        Term::HasAttachment => "has:attachment".into(),
        Term::Unread => "is:unread".into(),
        Term::Flagged => "is:starred".into(),
        Term::In(set) => printed_set(set, place),
        Term::MailboxNamed(name) => label(name),
        Term::Larger(bytes) if *bytes % MEGABYTE == 0 => format!("larger:{}M", bytes / MEGABYTE),
        Term::Larger(bytes) => format!("larger:{bytes}"),
    }
}

/// `prefix` and the text, or nothing when the text has nothing Gmail can
/// search for.
fn operator(prefix: &str, text: &str) -> String {
    quoted(text).map_or_else(String::new, |text| format!("{prefix}{text}"))
}

/// The text without the characters Gmail's syntax reserves, in double
/// quotes when it holds a space so Gmail reads it as one phrase. `None`
/// when nothing is left.
fn quoted(text: &str) -> Option<String> {
    let clean: String = text
        .chars()
        .filter(|c| !matches!(c, '"' | '(' | ')'))
        .collect();
    let trimmed = clean.trim();
    if trimmed.is_empty() {
        None
    } else if clean.contains(char::is_whitespace) {
        Some(format!("\"{trimmed}\""))
    } else {
        Some(trimmed.to_string())
    }
}

/// Gmail's search spells a label name in lower case, with spaces and
/// slashes as dashes.
fn label(name: &str) -> String {
    let spelled = label_spelling(name);
    if spelled.is_empty() {
        String::new()
    } else {
        format!("label:{spelled}")
    }
}

fn printed_set(set: &MailSet, place: Place) -> String {
    match set {
        MailSet::Role(Role::Inbox) => "in:inbox".into(),
        MailSet::Role(Role::Sent) => "in:sent".into(),
        MailSet::Role(Role::Drafts) => "in:drafts".into(),
        MailSet::Role(Role::Junk) => "in:spam".into(),
        MailSet::Role(Role::Trash) => "in:trash".into(),
        MailSet::Role(Role::Important) => "is:important".into(),
        // Gmail has no operator for these two, so they print as what they
        // hold: All Mail is everything outside Spam and the Trash, and the
        // archive is received mail outside the inbox as well.
        MailSet::Role(Role::All) => printed(&outside(&[Role::Junk, Role::Trash]), place),
        MailSet::Role(Role::Archive) => printed(
            &outside(&[
                Role::Inbox,
                Role::Sent,
                Role::Drafts,
                Role::Junk,
                Role::Trash,
            ]),
            place,
        ),
        // Gmail's search reads label names, not ids. Only a system label's
        // id is also its name, so this finds nothing for a person's label.
        MailSet::Mailbox(id) => format!("label:{id}"),
        MailSet::Keyword(k) if k == keyword::FLAGGED => "is:starred".into(),
        MailSet::Keyword(k) if k == keyword::SEEN => "is:read".into(),
        MailSet::Keyword(k) if k == keyword::MUTED => "is:muted".into(),
        MailSet::Keyword(_) => String::new(),
        MailSet::Unseen => "is:unread".into(),
        MailSet::Category(c) if c == category::PERSONAL => "category:primary".into(),
        MailSet::Category(c) => format!(
            "category:{}",
            c.strip_prefix("CATEGORY_").unwrap_or(c).to_lowercase()
        ),
    }
}

/// Mail in none of `roles`.
fn outside(roles: &[Role]) -> Query {
    Query::And(
        roles
            .iter()
            .map(|role| Query::not_in(MailSet::Role(*role)))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use mailrs_domain::query::{MEGABYTE, Query, Term};
    use mailrs_domain::{MailSet, Role};

    use super::print;

    fn from(text: &str) -> Query {
        Query::term(Term::From(text.into()))
    }

    #[test]
    fn an_and_joins_with_spaces_and_an_or_takes_braces() {
        let both = vec![from("ann@example.com"), Query::term(Term::Unread)];
        assert_eq!(
            print(&Query::And(both.clone())),
            "from:ann@example.com is:unread"
        );
        assert_eq!(print(&Query::Or(both)), "{from:ann@example.com is:unread}");
    }

    #[test]
    fn an_and_inside_an_or_or_a_not_takes_parentheses() {
        let pair = Query::And(vec![from("ann"), Query::term(Term::Flagged)]);
        assert_eq!(
            print(&Query::Or(vec![pair.clone(), Query::term(Term::Unread)])),
            "{(from:ann is:starred) is:unread}"
        );
        assert_eq!(print(&Query::Not(Box::new(pair))), "-(from:ann is:starred)");
        assert_eq!(
            print(&Query::Not(Box::new(Query::Or(vec![
                from("ann"),
                from("bo")
            ])))),
            "-{from:ann from:bo}"
        );
    }

    #[test]
    fn a_list_of_one_prints_its_item_alone() {
        assert_eq!(print(&Query::And(vec![from("ann")])), "from:ann");
        assert_eq!(print(&Query::Or(vec![from("ann")])), "from:ann");
        assert_eq!(
            print(&Query::Not(Box::new(Query::And(vec![from("ann")])))),
            "-from:ann"
        );
    }

    #[test]
    fn a_term_gmail_cannot_say_drops_out_with_its_not() {
        let answered = Query::is_in(MailSet::Keyword("$answered".into()));
        assert_eq!(print(&answered), "");
        assert_eq!(
            print(&Query::And(vec![
                Query::Not(Box::new(answered.clone())),
                Query::term(Term::Unread)
            ])),
            "is:unread"
        );
        assert_eq!(print(&Query::Or(vec![answered, from("\"()")])), "");
    }

    #[test]
    fn roles_without_an_operator_print_what_they_hold() {
        assert_eq!(
            print(&Query::is_in(MailSet::Role(Role::All))),
            "-in:spam -in:trash"
        );
        assert_eq!(
            print(&Query::And(vec![
                Query::is_in(MailSet::Role(Role::Archive)),
                Query::term(Term::Unread)
            ])),
            "-in:inbox -in:sent -in:drafts -in:spam -in:trash is:unread"
        );
        assert_eq!(
            print(&Query::not_in(MailSet::Role(Role::All))),
            "-(-in:spam -in:trash)"
        );
    }

    #[test]
    fn sets_print_as_gmail_operators() {
        let printed = |set: MailSet| print(&Query::is_in(set));
        assert_eq!(printed(MailSet::Role(Role::Junk)), "in:spam");
        assert_eq!(printed(MailSet::Role(Role::Important)), "is:important");
        assert_eq!(printed(MailSet::flagged()), "is:starred");
        assert_eq!(printed(MailSet::muted()), "is:muted");
        assert_eq!(printed(MailSet::Unseen), "is:unread");
        assert_eq!(
            printed(MailSet::Category("CATEGORY_SOCIAL".into())),
            "category:social"
        );
        assert_eq!(
            printed(MailSet::Category("CATEGORY_PERSONAL".into())),
            "category:primary"
        );
        assert_eq!(printed(MailSet::Mailbox("INBOX".into())), "label:INBOX");
    }

    #[test]
    fn dates_and_sizes_print_in_gmail_units() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 4).expect("a real day");
        assert_eq!(print(&Query::term(Term::Since(day))), "after:2026/09/04");
        assert_eq!(print(&Query::term(Term::Before(day))), "before:2026/09/04");
        assert_eq!(print(&Query::term(Term::NewerThan(7))), "newer_than:7d");
        assert_eq!(print(&Query::term(Term::Larger(5 * MEGABYTE))), "larger:5M");
        assert_eq!(print(&Query::term(Term::Larger(1500))), "larger:1500");
    }

    #[test]
    fn text_loses_reserved_characters_and_a_phrase_takes_quotes() {
        assert_eq!(print(&from("Ann Smith")), "from:\"Ann Smith\"");
        assert_eq!(print(&from("\"Ann\" (work)")), "from:\"Ann work\"");
        assert_eq!(
            print(&Query::term(Term::Words("lunch on thursday".into()))),
            "\"lunch on thursday\""
        );
        assert_eq!(
            print(&Query::term(Term::MailboxNamed("Work/Clients".into()))),
            "label:work-clients"
        );
        assert_eq!(print(&Query::term(Term::MailboxNamed("()".into()))), "");
    }

    /// What a person types reads as a tree that Gmail's printer turns back
    /// into text Gmail reads the same way, for every operator both know,
    /// and that text reads back as the same tree.
    #[test]
    fn typed_text_prints_back_as_text_gmail_reads_the_same_way() {
        use mailrs_domain::query::parse;

        let cases = [
            ("from:ann@example.com subject:kites", "from:ann@example.com subject:kites"),
            ("from:\"Ann Smith\" is:unread", "from:\"Ann Smith\" is:unread"),
            ("is:starred has:attachment", "is:starred has:attachment"),
            ("is:flagged", "is:starred"),
            ("is:read", "-is:unread"),
            ("newer_than:7d", "newer_than:7d"),
            ("older_than:30d", "-newer_than:30d"),
            ("after:2026/02/01 before:2026/03/01", "after:2026/02/01 before:2026/03/01"),
            ("larger:5M", "larger:5M"),
            ("larger:1500", "larger:1500"),
            ("label:work-clients", "label:work-clients"),
            ("in:inbox is:unread", "in:inbox is:unread"),
            ("in:spam", "in:spam"),
            ("category:social", "category:social"),
            ("from:ann OR from:bo kites", "{from:ann from:bo} kites"),
            ("{from:ann from:bo}", "{from:ann from:bo}"),
            ("-from:bo@example.org", "-from:bo@example.org"),
            ("-(from:ann is:starred)", "-(from:ann is:starred)"),
            ("\"lunch on thursday\" moss", "\"lunch on thursday\" moss"),
        ];
        for (typed, printed) in cases {
            let tree = parse(typed);
            assert_eq!(print(&tree), printed, "{typed}");
            assert_eq!(parse(printed), tree, "{printed} reads back as {typed} did");
        }
    }
}
