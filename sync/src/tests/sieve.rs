//! Rules and the automatic reply on a ManageSieve server, against
//! FakeSieve, with the IMAP adapter over FakeImap naming the folders.

use std::sync::Arc;

use mailrs_domain::{Filter, FilterCriteria, Vacation};
use mailrs_sieve::SCRIPT_NAME;
use mailrs_sieve::fake::FakeSieve;
use mailrs_store::{Db, accounts, rule_changes};

use crate::fake::{FakeImap, FakeSmtp};
use crate::services::{Imap, SieveRules};
use crate::settings::{Replaced, replace_via};
use crate::tests::fake_settings;
use crate::{AutoReplyService, BackendError, MailBackend, RulesService};

const DOVECOT: &str =
    "fileinto vacation imap4flags copy include body mime date relational mailbox";

type Adapter = SieveRules<FakeSieve, Imap<FakeImap, FakeSmtp>>;

async fn adapter(extensions: &str) -> (Arc<FakeSieve>, Adapter) {
    let imap = Imap::new(
        Arc::new(FakeImap::new()),
        Arc::new(FakeSmtp::default()),
        fake_settings(),
    );
    // The folders and their roles, as the first listing learns them.
    imap.mailboxes().await.unwrap();
    let sieve = Arc::new(FakeSieve::new(extensions));
    let rules = SieveRules::new(
        Arc::clone(&sieve),
        imap,
        "me@example.com".into(),
        "mailbox.org".into(),
    );
    (sieve, rules)
}

fn from(who: &str) -> Filter {
    Filter::block(who)
}

#[tokio::test]
async fn a_rule_goes_into_the_app_s_script_and_it_becomes_active() {
    let (sieve, rules) = adapter(DOVECOT).await;
    let made = rules.create_filter(&from("pest@example.com")).await.unwrap();
    assert!(made.id.is_some());
    assert_eq!(sieve.active().as_deref(), Some(SCRIPT_NAME));
    assert!(sieve.script(SCRIPT_NAME).unwrap().contains("fileinto \"Trash\";"));
    assert_eq!(rules.filters().await.unwrap(), std::slice::from_ref(&made));
    rules.delete_filter(made.id.as_deref().unwrap()).await.unwrap();
    assert!(rules.filters().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_script_the_person_runs_is_included() {
    let (sieve, rules) = adapter(DOVECOT).await;
    sieve.put_elsewhere("roundcube", "require \"fileinto\";\nfileinto \"Lists\";\n", true);
    rules.create_filter(&from("pest@example.com")).await.unwrap();
    assert_eq!(sieve.active().as_deref(), Some(SCRIPT_NAME));
    assert!(sieve.script(SCRIPT_NAME).unwrap().contains("include :personal \"roundcube\";"));
    assert!(sieve.script("roundcube").is_some(), "the person's script stays on the server");
}

#[tokio::test]
async fn without_include_the_rules_ask_before_replacing() {
    let (sieve, rules) = adapter("fileinto vacation imap4flags").await;
    sieve.put_elsewhere("mine", "keep;\n", true);
    let refused = rules.create_filter(&from("pest@example.com")).await.unwrap_err();
    assert!(
        matches!(refused, BackendError::WouldReplace { ref script } if script == "mine"),
        "{refused:?}"
    );
    assert_eq!(sieve.puts(), 0, "saying nothing yet changes nothing on the server");
    rules.take_over().await;
    rules.create_filter(&from("pest@example.com")).await.unwrap();
    assert_eq!(sieve.active().as_deref(), Some(SCRIPT_NAME));
}

#[tokio::test]
async fn an_edited_rule_asks_before_replacing_too() {
    let (sieve, rules) = adapter("fileinto vacation imap4flags").await;
    let made = rules.create_filter(&from("a@example.com")).await.unwrap();
    // Another client turns on a script of its own afterwards.
    sieve.put_elsewhere("mine", "keep;\n", true);
    let id = made.id.as_deref().unwrap();
    let puts = sieve.puts();
    let refused = rules.replace_filter(id, &from("b@example.com")).await.unwrap_err();
    assert!(matches!(refused, BackendError::WouldReplace { .. }), "{refused:?}");
    assert_eq!(sieve.puts(), puts);
    rules.take_over().await;
    rules.replace_filter(id, &from("b@example.com")).await.unwrap();
    assert_eq!(sieve.active().as_deref(), Some(SCRIPT_NAME));
}

#[tokio::test]
async fn a_script_the_server_refuses_changes_nothing_and_says_why() {
    let (sieve, rules) = adapter(DOVECOT).await;
    sieve.refuse_next_put("line 4: error: unknown command 'fileintoo'");
    let refused = rules.create_filter(&from("pest@example.com")).await.unwrap_err();
    assert!(matches!(refused, BackendError::Refused(ref words) if words.contains("fileintoo")));
    assert_eq!(sieve.active(), None);
}

#[tokio::test]
async fn the_automatic_reply_lives_in_the_same_script() {
    let (sieve, rules) = adapter(DOVECOT).await;
    rules.create_filter(&from("pest@example.com")).await.unwrap();
    let away = Vacation {
        enabled: true,
        subject: "Away".into(),
        body: "Back soon.".into(),
        ..Vacation::default()
    };
    rules.set_vacation(&away).await.unwrap();
    assert_eq!(rules.vacation().await.unwrap(), away);
    assert_eq!(rules.filters().await.unwrap().len(), 1, "the rules stay beside it");
    assert!(sieve.script(SCRIPT_NAME).unwrap().contains("vacation :days 1"));
}

#[tokio::test]
async fn a_server_down_is_offline() {
    let (sieve, rules) = adapter(DOVECOT).await;
    sieve.set_down(true);
    assert!(matches!(rules.filters().await, Err(BackendError::Offline(_))));
}

#[tokio::test]
async fn a_rule_the_server_cannot_run_says_what_it_lacks() {
    let (_, rules) = adapter("fileinto vacation").await;
    let words = Filter {
        criteria: FilterCriteria { query: Some("invoice".into()), ..Default::default() },
        ..from("x@example.com")
    };
    let refused = rules.create_filter(&words).await.unwrap_err();
    assert!(matches!(refused, BackendError::Refused(ref w) if w.contains("body")), "{refused:?}");
}

#[tokio::test]
async fn an_edited_sieve_rule_keeps_its_place_in_one_write() {
    let (sieve, rules) = adapter(DOVECOT).await;
    let first = rules.create_filter(&from("a@example.com")).await.unwrap();
    let second = rules.create_filter(&from("b@example.com")).await.unwrap();
    let third = rules.create_filter(&from("c@example.com")).await.unwrap();
    let before = sieve.puts();
    let edited = rules
        .replace_filter(second.id.as_deref().unwrap(), &from("z@example.com"))
        .await
        .unwrap();
    assert_eq!(sieve.puts(), before + 1, "one script write");
    assert_eq!(rules.filters().await.unwrap(), [first, edited.clone(), third]);
    assert_ne!(edited.id, second.id);
}

#[tokio::test]
async fn replacing_a_rule_the_script_lacks_finds_nothing() {
    let (sieve, rules) = adapter(DOVECOT).await;
    let before = sieve.puts();
    let missing = rules.replace_filter("sieve-nope", &from("z@example.com")).await;
    assert!(matches!(missing, Err(BackendError::NotFound)));
    assert_eq!(sieve.puts(), before);
}

#[tokio::test]
async fn a_read_only_rule_is_refused() {
    let (sieve, rules) = adapter(DOVECOT).await;
    let theirs = Filter { read_only: true, ..from("a@example.com") };
    assert!(matches!(rules.create_filter(&theirs).await, Err(BackendError::Refused(_))));
    assert_eq!(sieve.puts(), 0);
}

#[tokio::test]
async fn a_size_rule_says_over_or_under() {
    let (sieve, rules) = adapter(DOVECOT).await;
    let sized = |way: &str| Filter {
        criteria: FilterCriteria {
            size: Some(1000),
            size_comparison: Some(way.into()),
            ..Default::default()
        },
        ..from("x@example.com")
    };
    rules.create_filter(&sized("larger")).await.unwrap();
    rules.create_filter(&sized("smaller")).await.unwrap();
    let text = sieve.script(SCRIPT_NAME).unwrap();
    assert!(text.contains("size :over 1000") && text.contains("size :under 1000"), "{text}");
}

#[tokio::test]
async fn an_edit_made_offline_waits_and_goes_out() {
    let (sieve, rules) = adapter(DOVECOT).await;
    let made = rules.create_filter(&from("a@example.com")).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mail.db")).unwrap();
    let account_id = db
        .write(|c| accounts::insert_account(c, "me@example.com", 0))
        .await
        .unwrap();
    sieve.set_down(true);
    let old_id = made.id.clone().unwrap();
    let outcome = replace_via(&rules, &db, account_id, &old_id, from("b@example.com"))
        .await
        .unwrap();
    let Replaced::Swapped(new) = outcome else { panic!("the edit should wait") };
    assert!(new.id.as_deref().is_some_and(|id| id.starts_with("sieve-")));
    let queued = db
        .read(move |c| rule_changes::queued(c, account_id))
        .await
        .unwrap();
    assert_eq!(queued.len(), 2, "the new rule, then the delete of the old");
    assert_eq!(queued[0].change, rule_changes::RuleChange::Create(new));
    assert_eq!(queued[1].change, rule_changes::RuleChange::Delete(old_id));
}
