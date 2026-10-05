//! What each automatic reply adapter keeps, as `Offers` states it: a
//! subject of its own, and a limit to the person's contacts.

use std::sync::Arc;

use mailrs_sieve::fake::FakeSieve;

use crate::fake::{FakeGmail, FakeGraph, FakeImap, FakeSmtp};
use crate::services::SieveRules;
use crate::{AccountServices, AnyAutoReply, Offers};

fn kept(offers: Offers) -> (bool, bool) {
    (offers.auto_reply_subject, offers.auto_reply_contacts_only)
}

#[test]
fn gmail_keeps_a_subject_and_can_limit_to_contacts() {
    let offers = AccountServices::fake(Arc::new(FakeGmail::new())).offers();
    assert_eq!(kept(offers), (true, true));
}

#[test]
fn outlook_keeps_the_text_but_no_subject_and_can_limit_to_contacts() {
    let offers = AccountServices::fake_microsoft(Arc::new(FakeGraph::new())).offers();
    assert_eq!(kept(offers), (false, true));
}

#[test]
fn sieve_keeps_a_subject_and_has_no_contacts_limit() {
    let imap = AccountServices::fake_imap(Arc::new(FakeImap::new()), Arc::new(FakeSmtp::default()));
    let adapter = imap.mail.clone();
    let sieve = Arc::new(FakeSieve::new("fileinto vacation"));
    let rules = SieveRules::new(sieve, adapter, "me@example.com".into(), "Example".into());
    let offers = imap.with_auto_reply(AnyAutoReply::FakeSieve(rules)).offers();
    assert!(offers.auto_reply);
    assert_eq!(kept(offers), (true, false));
}

#[test]
fn an_account_with_no_automatic_reply_keeps_nothing() {
    let offers = AccountServices::fake_imap(Arc::new(FakeImap::new()), Arc::new(FakeSmtp::default())).offers();
    assert!(!offers.auto_reply);
    assert_eq!(kept(offers), (false, false));
}
