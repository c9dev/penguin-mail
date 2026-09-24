//! Gmail's search text for every folder and for smart mailboxes with each
//! condition, each kind of bad value and both ways of joining, recorded
//! from the code that wrote the text by hand before folders and smart
//! mailboxes became query trees. The printed trees must match it byte for
//! byte: Gmail answers the same searches it answered before.
//!
//! `PENGUIN_MAIL_BLESS=1` records the text again. Set it only after a
//! person has read the new text and judged it right.

use std::fmt::Write as _;
use std::path::Path;

use mailrs_domain::smart::{Condition, Field};
use mailrs_domain::{Folder, SmartMailbox};
use mailrs_gmail::query::print;

/// The search text a folder lists with.
fn folder_text(folder: Folder) -> Option<String> {
    Some(print(&folder.query()))
}

/// The search text a smart mailbox lists with.
fn smart_text(smart: &SmartMailbox) -> Option<String> {
    smart.query().map(|query| print(&query))
}

fn cond(field: Field, value: &str) -> Condition {
    Condition {
        field,
        value: value.into(),
    }
}

fn smart(match_all: bool, conditions: Vec<Condition>) -> SmartMailbox {
    SmartMailbox {
        id: "s1".into(),
        name: "Golden".into(),
        account: None,
        match_all,
        conditions,
    }
}

fn cases() -> Vec<(&'static str, SmartMailbox)> {
    use Field::*;
    let every = || {
        vec![
            cond(From, "ann@example.com"),
            cond(To, "bo@example.org"),
            cond(Subject, "quarterly report"),
            cond(Words, "invoice"),
            cond(Label, "Work/Clients"),
            cond(NewerThanDays, "7"),
            cond(LargerThanMb, "5"),
            cond(HasAttachment, ""),
            cond(Unread, ""),
            cond(Flagged, ""),
        ]
    };
    vec![
        (
            "from an address",
            smart(true, vec![cond(From, "ann@example.com")]),
        ),
        (
            "from a name with a space",
            smart(true, vec![cond(From, "Ann Smith")]),
        ),
        (
            "from with padding",
            smart(true, vec![cond(From, "  ann@example.com  ")]),
        ),
        (
            "from with quotes and parentheses",
            smart(true, vec![cond(From, "\"Ann\" (work)")]),
        ),
        (
            "from ending in a quote",
            smart(true, vec![cond(From, "ann \"")]),
        ),
        (
            "from of punctuation only",
            smart(true, vec![cond(From, "\"()")]),
        ),
        (
            "from in another script",
            smart(true, vec![cond(From, "Zé Ninguém")]),
        ),
        (
            "to an address",
            smart(true, vec![cond(To, "bo@example.org")]),
        ),
        (
            "subject phrase",
            smart(true, vec![cond(Subject, "quarterly report")]),
        ),
        ("words one", smart(true, vec![cond(Words, "invoice")])),
        (
            "words phrase",
            smart(true, vec![cond(Words, "lunch on thursday")]),
        ),
        ("words with a tab", smart(true, vec![cond(Words, "a\tb")])),
        ("label plain", smart(true, vec![cond(Label, "Work")])),
        (
            "label nested",
            smart(true, vec![cond(Label, "Work/Clients")]),
        ),
        (
            "label with spaces",
            smart(true, vec![cond(Label, "  Travel Plans 2026 ")]),
        ),
        (
            "label with a parenthesis and a space",
            smart(true, vec![cond(Label, "( Work)")]),
        ),
        ("label of parentheses", smart(true, vec![cond(Label, "()")])),
        (
            "label of spaced parentheses",
            smart(true, vec![cond(Label, "( )")]),
        ),
        (
            "label in another script",
            smart(true, vec![cond(Label, "Música")]),
        ),
        ("newer than 7", smart(true, vec![cond(NewerThanDays, "7")])),
        ("newer than 0", smart(true, vec![cond(NewerThanDays, "0")])),
        (
            "newer than words",
            smart(true, vec![cond(NewerThanDays, "a week")]),
        ),
        (
            "newer than padded",
            smart(true, vec![cond(NewerThanDays, " 30 ")]),
        ),
        (
            "newer than the largest",
            smart(true, vec![cond(NewerThanDays, "4294967295")]),
        ),
        ("larger than 5", smart(true, vec![cond(LargerThanMb, "5")])),
        (
            "larger than minus 5",
            smart(true, vec![cond(LargerThanMb, "-5")]),
        ),
        (
            "larger than the largest",
            smart(true, vec![cond(LargerThanMb, "4294967295")]),
        ),
        ("has attachment", smart(true, vec![cond(HasAttachment, "")])),
        (
            "unread with a stray value",
            smart(true, vec![cond(Unread, "ignored")]),
        ),
        ("flagged", smart(true, vec![cond(Flagged, "")])),
        (
            "any of one",
            smart(false, vec![cond(From, "ann@example.com")]),
        ),
        ("no conditions", smart(true, vec![])),
        ("no conditions, any", smart(false, vec![])),
        (
            "every condition dropped",
            smart(true, vec![cond(From, "  "), cond(LargerThanMb, "lots")]),
        ),
        (
            "one survivor of three",
            smart(
                true,
                vec![
                    cond(From, "  "),
                    cond(LargerThanMb, "lots"),
                    cond(Label, "Work/Clients"),
                ],
            ),
        ),
        (
            "one survivor of two, any",
            smart(false, vec![cond(Words, "\"()"), cond(Unread, "")]),
        ),
        (
            "all of four",
            smart(
                true,
                vec![
                    cond(From, "ann@example.com"),
                    cond(Subject, "quarterly report"),
                    cond(Unread, "ignored"),
                    cond(NewerThanDays, "7"),
                ],
            ),
        ),
        (
            "any of four",
            smart(
                false,
                vec![
                    cond(From, "ann@example.com"),
                    cond(Subject, "quarterly report"),
                    cond(Unread, "ignored"),
                    cond(NewerThanDays, "7"),
                ],
            ),
        ),
        (
            "any of three senders and labels",
            smart(
                false,
                vec![
                    cond(From, "ann@example.com"),
                    cond(From, "bo@example.org"),
                    cond(Label, "Work"),
                ],
            ),
        ),
        ("all of every field", smart(true, every())),
        ("any of every field", smart(false, every())),
    ]
}

#[test]
fn folders_and_smart_mailboxes_print_the_recorded_gmail_text() {
    let mut now = String::new();
    for folder in Folder::ALL {
        writeln!(now, "folder {folder:?} => {:?}", folder_text(folder)).unwrap();
    }
    for (name, smart) in cases() {
        writeln!(now, "smart {name} => {:?}", smart_text(&smart)).unwrap();
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/queries.txt");
    if std::env::var_os("PENGUIN_MAIL_BLESS").is_some() {
        std::fs::write(&path, &now).unwrap();
    }
    let then = std::fs::read_to_string(&path)
        .expect("tests/golden/queries.txt, recorded with PENGUIN_MAIL_BLESS=1");
    for (line, (now, then)) in now.lines().zip(then.lines()).enumerate() {
        assert_eq!(now, then, "line {} of golden/queries.txt", line + 1);
    }
    assert_eq!(
        now.lines().count(),
        then.lines().count(),
        "golden/queries.txt line count"
    );
}
