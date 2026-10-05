//! Rule changes made while the ManageSieve server is down wait in the
//! store, show in the list, and go out once it answers.

use std::sync::Arc;

use mailrs_domain::Filter;
use mailrs_sieve::fake::FakeSieve;

use crate::services::SieveRules;
use crate::tests::{ImapHarness, ServedBy, imap_harness};
use crate::{AccountSettings, AnyAutoReply, AnyRules, Permitted, Replaced, RulesPlace};

async fn settings() -> (Arc<FakeSieve>, AccountSettings<ServedBy>, i64, ImapHarness) {
    let h = imap_harness().await;
    let sieve = Arc::new(FakeSieve::new("fileinto vacation imap4flags copy include"));
    let imap = h.sync.services().mail.clone();
    let rules = SieveRules::new(Arc::clone(&sieve), imap, "me@example.com".into(), "Example".into());
    let services = h
        .sync
        .services()
        .clone()
        .with_rules(AnyRules::FakeSieve(rules.clone()))
        .with_auto_reply(AnyAutoReply::FakeSieve(rules));
    let settings =
        AccountSettings::new(Arc::new(ServedBy::new(h.account_id, Arc::clone(&h.sync), services)), h.db.clone());
    (sieve, settings, h.account_id, h)
}

#[tokio::test]
async fn a_rule_made_while_the_server_is_down_waits_and_shows() {
    let (sieve, settings, account, _h) = settings().await;
    settings.add_rule(account, Filter::block("a@example.com")).await.unwrap();
    // The list as read while the server answered is what shows when it
    // stops.
    settings.rule_list(account).await.unwrap();
    sieve.set_down(true);
    let Permitted::Done(made) = settings.add_rule(account, Filter::block("b@example.com")).await.unwrap() else {
        panic!()
    };
    let Permitted::Done(list) = settings.rule_list(account).await.unwrap() else { panic!() };
    assert!(list.waiting);
    assert_eq!(list.place, RulesPlace::Server);
    assert_eq!(list.rules.len(), 2, "the rule read last and the one waiting");
    sieve.set_down(false);
    let sent = settings.send_rule_changes(account).await.unwrap();
    assert_eq!(sent.sent, 1);
    let Permitted::Done(list) = settings.rule_list(account).await.unwrap() else { panic!() };
    assert!(!list.waiting);
    assert!(list.rules.iter().any(|r| r.id == made.id), "the rule kept the id it waited under");
}

#[tokio::test]
async fn a_delete_made_offline_goes_out_later() {
    let (sieve, settings, account, _h) = settings().await;
    let Permitted::Done(made) = settings.add_rule(account, Filter::block("a@example.com")).await.unwrap() else {
        panic!()
    };
    settings.rule_list(account).await.unwrap();
    sieve.set_down(true);
    settings.delete_rule(account, made.id.as_deref().unwrap()).await.unwrap();
    let Permitted::Done(list) = settings.rule_list(account).await.unwrap() else { panic!() };
    assert!(list.rules.is_empty());
    sieve.set_down(false);
    settings.send_rule_changes(account).await.unwrap();
    assert!(!sieve.script(mailrs_sieve::SCRIPT_NAME).unwrap().contains("a@example.com"));
}

#[tokio::test]
async fn a_waiting_rule_the_server_refuses_is_dropped_with_its_words() {
    let (sieve, settings, account, _h) = settings().await;
    sieve.set_down(true);
    settings.add_rule(account, Filter::block("a@example.com")).await.unwrap();
    sieve.set_down(false);
    sieve.refuse_next_put("line 2: error");
    let sent = settings.send_rule_changes(account).await.unwrap();
    assert_eq!(sent.refused.len(), 1);
    assert!(sent.refused[0].contains("line 2"));
    assert_eq!(settings.send_rule_changes(account).await.unwrap().sent, 0, "nothing left waiting");
}

#[tokio::test]
async fn blocks_written_elsewhere_list_read_only() {
    let (sieve, settings, account, _h) = settings().await;
    settings.add_rule(account, Filter::block("a@example.com")).await.unwrap();
    let text = sieve.script(mailrs_sieve::SCRIPT_NAME).unwrap() + "\nif header :contains \"x-spam\" \"yes\" { discard; }\n";
    sieve.put_elsewhere(mailrs_sieve::SCRIPT_NAME, &text, true);
    let Permitted::Done(list) = settings.rule_list(account).await.unwrap() else { panic!() };
    assert_eq!(list.elsewhere.len(), 1);
    assert!(list.elsewhere[0].contains("x-spam"));
}

#[tokio::test]
async fn an_edit_made_offline_waits_and_goes_out() {
    let (sieve, settings, account, _h) = settings().await;
    let Permitted::Done(old) = settings.add_rule(account, Filter::block("a@example.com")).await.unwrap() else {
        panic!()
    };
    settings.rule_list(account).await.unwrap();
    sieve.set_down(true);
    let Permitted::Done(Replaced::Swapped(new)) =
        settings.replace_rule(account, &old, Filter::block("b@example.com")).await.unwrap()
    else {
        panic!("the edit should wait")
    };
    let Permitted::Done(list) = settings.rule_list(account).await.unwrap() else { panic!() };
    assert!(list.waiting);
    assert_eq!(list.rules.len(), 1, "the new rule shows, the old one is gone");
    assert_eq!(list.rules[0].id, new.id);
    sieve.set_down(false);
    assert_eq!(settings.send_rule_changes(account).await.unwrap().sent, 2);
    let text = sieve.script(mailrs_sieve::SCRIPT_NAME).unwrap();
    assert!(text.contains("b@example.com") && !text.contains("a@example.com"), "{text}");
}

#[tokio::test]
async fn a_change_stays_queued_while_the_server_still_does_not_answer() {
    let (sieve, settings, account, _h) = settings().await;
    sieve.set_down(true);
    settings.add_rule(account, Filter::block("a@example.com")).await.unwrap();
    assert_eq!(settings.send_rule_changes(account).await.unwrap().sent, 0);
    sieve.set_down(false);
    assert_eq!(settings.send_rule_changes(account).await.unwrap().sent, 1, "it stayed queued");
}
