//! Every rule Penguin Mail can make goes into the script and comes back
//! out the same; a block edited by hand comes back as written elsewhere.

use mailrs_domain::{Filter, FilterAction, FilterCriteria, MailSet, Role, Vacation};
use mailrs_sieve::script::{Extensions, Script, WriteError, read, write};

const DOVECOT: &str = "fileinto reject envelope encoded-character vacation subaddress comparator-i;ascii-numeric \
    relational regex imap4flags copy include variables body enotify environment mailbox date index ihave duplicate mime";

fn ext() -> Extensions {
    Extensions::parse(DOVECOT)
}

fn folder(set: &MailSet) -> Option<String> {
    match set {
        MailSet::Role(Role::Trash) => Some("Trash".into()),
        MailSet::Role(Role::Archive) => Some("Archive".into()),
        MailSet::Role(Role::Junk) => Some("Junk".into()),
        MailSet::Mailbox(id) => Some(format!("Folders/{id}")),
        _ => None,
    }
}

fn rule(id: &str, criteria: FilterCriteria, action: FilterAction) -> Filter {
    Filter { id: Some(id.into()), criteria, action, ..Filter::default() }
}

fn every_rule_the_app_makes() -> Vec<Filter> {
    let from = || FilterCriteria { from: Some("news@example.com".into()), ..FilterCriteria::default() };
    vec![
        rule("r1", from(), FilterAction { remove: vec![MailSet::Role(Role::Inbox)], ..FilterAction::default() }),
        rule("r2", from(), FilterAction { remove: vec![MailSet::Unseen], ..FilterAction::default() }),
        rule("r3", from(), FilterAction { add: vec![MailSet::flagged()], ..FilterAction::default() }),
        rule("r4", from(), FilterAction { add: vec![MailSet::Mailbox("Travel".into())], ..FilterAction::default() }),
        rule("r5", from(), FilterAction { forward: Some("me@elsewhere.example".into()), ..FilterAction::default() }),
        Filter { id: Some("r6".into()), ..Filter::block("pest@example.com") },
        rule(
            "r7",
            FilterCriteria {
                to: Some("me+shop@example.com".into()),
                subject: Some("Invoice \"May\"".into()),
                query: Some("receipt".into()),
                negated_query: Some("draft".into()),
                has_attachment: true,
                ..FilterCriteria::default()
            },
            FilterAction {
                add: vec![MailSet::Mailbox("Receipts".into())],
                remove: vec![MailSet::Role(Role::Inbox), MailSet::Unseen],
                forward: None,
            },
        ),
        rule(
            "r8",
            FilterCriteria { size: Some(5_000_000), size_comparison: Some("larger".into()), ..FilterCriteria::default() },
            FilterAction { add: vec![MailSet::Mailbox("Big".into())], ..FilterAction::default() },
        ),
    ]
}

#[test]
fn every_rule_the_app_makes_comes_back_the_same() {
    for filter in every_rule_the_app_makes() {
        let script = Script { rules: vec![filter.clone()], ..Script::default() };
        let text = write(&script, "me@example.com", &folder, &ext()).unwrap();
        assert_eq!(read(&text).rules, [filter], "{text}");
        assert!(read(&text).foreign.is_empty(), "{text}");
    }
}

#[test]
fn a_size_rule_uses_over_and_under() {
    let big = every_rule_the_app_makes()[7].clone();
    let text = write(&Script { rules: vec![big.clone()], ..Script::default() }, "me@example.com", &folder, &ext()).unwrap();
    assert!(text.contains("size :over 5000000"), "{text}");
    let small = Filter { criteria: FilterCriteria { size_comparison: Some("smaller".into()), ..big.criteria.clone() }, ..big };
    let text = write(&Script { rules: vec![small], ..Script::default() }, "me@example.com", &folder, &ext()).unwrap();
    assert!(text.contains("size :under 5000000"), "{text}");
}

#[test]
fn deleting_files_into_the_trash() {
    let block = Script { rules: vec![Filter { id: Some("b".into()), ..Filter::block("pest@example.com") }], ..Script::default() };
    let text = write(&block, "me@example.com", &folder, &ext()).unwrap();
    assert!(text.contains("fileinto \"Trash\";"), "{text}");
}

#[test]
fn deleting_without_a_trash_folder_makes_one_and_never_discards() {
    let block = Script { rules: vec![Filter { id: Some("b".into()), ..Filter::block("pest@example.com") }], ..Script::default() };
    let text = write(&block, "me@example.com", &|_| None, &ext()).unwrap();
    assert!(text.contains("fileinto :create \"Trash\";"), "{text}");
    assert!(!text.contains("discard"), "{text}");
    let without_mailbox = Extensions::parse("fileinto vacation imap4flags");
    assert!(matches!(
        write(&block, "me@example.com", &|_| None, &without_mailbox),
        Err(WriteError::NoFolder(_))
    ));
}

#[test]
fn the_automatic_reply_runs_between_its_days() {
    let vacation = Vacation {
        enabled: true,
        subject: "Away".into(),
        body: "Back on Monday.".into(),
        start: Some(1_790_000_000_000),
        end: Some(1_790_600_000_000),
        ..Vacation::default()
    };
    let script = Script { vacation: Some(vacation.clone()), ..Script::default() };
    let text = write(&script, "me@example.com", &folder, &ext()).unwrap();
    assert!(text.contains("vacation :days 1 :subject \"Away\" :addresses [\"me@example.com\"] \"Back on Monday.\";"), "{text}");
    assert!(text.contains("currentdate :value \"ge\" \"date\""));
    assert_eq!(read(&text).vacation, Some(vacation));
}

#[test]
fn a_reply_that_is_off_is_kept_and_sends_nothing() {
    let vacation = Vacation { enabled: false, subject: "Away".into(), body: "x".into(), ..Vacation::default() };
    let text = write(&Script { vacation: Some(vacation.clone()), ..Script::default() }, "me@example.com", &folder, &ext()).unwrap();
    assert!(!text.contains("vacation :days"));
    assert_eq!(read(&text).vacation, Some(vacation));
}

#[test]
fn a_reply_that_is_off_does_not_hide_the_rule_after_it() {
    let vacation = Vacation { enabled: false, subject: "Away".into(), body: "x".into(), ..Vacation::default() };
    let rules = every_rule_the_app_makes()[..2].to_vec();
    let script = Script { vacation: Some(vacation.clone()), rules: rules.clone(), ..Script::default() };
    let back = read(&write(&script, "me@example.com", &folder, &ext()).unwrap());
    assert_eq!(back.vacation, Some(vacation));
    assert_eq!(back.rules, rules);
}

#[test]
fn a_rule_read_back_is_never_read_only() {
    let held = Filter { read_only: true, ..every_rule_the_app_makes()[0].clone() };
    let text = write(&Script { rules: vec![held], ..Script::default() }, "me@example.com", &folder, &ext()).unwrap();
    assert!(!read(&text).rules[0].read_only);
}

#[test]
fn a_rule_edited_by_hand_comes_back_as_written_elsewhere() {
    let script = Script { rules: every_rule_the_app_makes()[..1].to_vec(), ..Script::default() };
    let text = write(&script, "me@example.com", &folder, &ext()).unwrap();
    let edited = text.replace("news@example.com\"", "newsletter@example.com\"");
    let back = read(&edited);
    assert!(back.rules.is_empty());
    assert_eq!(back.foreign.len(), 1);
    assert!(back.foreign[0].contains("newsletter@example.com"));
    let rewritten = write(&back, "me@example.com", &folder, &ext()).unwrap();
    assert!(rewritten.contains("newsletter@example.com"), "a block written elsewhere stays when the script is written again");
}

#[test]
fn a_script_written_elsewhere_keeps_its_requires_and_its_include() {
    let theirs = "require [\"fileinto\", \"regex\"];\nif header :regex \"subject\" \"^\\\\[list\\\\]\" { fileinto \"Lists\"; }\n";
    let back = read(theirs);
    assert_eq!(back.foreign.len(), 1);
    assert!(back.kept_requires.contains(&"regex".to_string()));
    let ours = Script { include: Some("roundcube".into()), ..back };
    let text = write(&ours, "me@example.com", &folder, &ext()).unwrap();
    let require_line = text.lines().find(|l| l.starts_with("require")).unwrap();
    assert!(require_line.contains("\"regex\"") && require_line.contains("\"include\""), "{text}");
    assert!(text.contains("include :personal \"roundcube\";"));
    assert_eq!(read(&text).include.as_deref(), Some("roundcube"));
}

#[test]
fn a_server_without_body_cannot_sort_by_words() {
    let script = Script { rules: vec![every_rule_the_app_makes()[6].clone()], ..Script::default() };
    let small = Extensions::parse("fileinto vacation imap4flags");
    assert_eq!(write(&script, "me@example.com", &folder, &small), Err(WriteError::Needs("body")));
}

#[test]
fn strings_are_quoted_so_a_quote_cannot_end_them() {
    let script = Script { rules: vec![every_rule_the_app_makes()[6].clone()], ..Script::default() };
    let text = write(&script, "me@example.com", &folder, &ext()).unwrap();
    assert!(text.contains("\"Invoice \\\"May\\\"\""), "{text}");
}

#[test]
fn a_reply_of_several_lines_comes_back_with_the_rule_after_it() {
    let vacation = Vacation { enabled: true, subject: "Away".into(), body: "Hi\nAway until Monday".into(), ..Vacation::default() };
    let rules = every_rule_the_app_makes()[..1].to_vec();
    let script = Script { vacation: Some(vacation.clone()), rules: rules.clone(), ..Script::default() };
    let back = read(&write(&script, "me@example.com", &folder, &ext()).unwrap());
    assert_eq!(back.vacation, Some(vacation));
    assert_eq!(back.rules, rules);
    assert!(back.foreign.is_empty(), "{:?}", back.foreign);
}

#[test]
fn a_dated_reply_whose_lines_hold_braces_and_hashes_comes_back() {
    let vacation = Vacation {
        enabled: true,
        subject: "Away".into(),
        body: "Hi;\n} back soon {\n# not a comment\nwrite to text:\nme".into(),
        start: Some(1_790_000_000_000),
        end: Some(1_790_600_000_000),
        ..Vacation::default()
    };
    let rules = every_rule_the_app_makes()[..2].to_vec();
    let script = Script { vacation: Some(vacation.clone()), rules: rules.clone(), ..Script::default() };
    let back = read(&write(&script, "me@example.com", &folder, &ext()).unwrap());
    assert_eq!(back.vacation, Some(vacation));
    assert_eq!(back.rules, rules);
    assert!(back.foreign.is_empty(), "{:?}", back.foreign);
}
