//! Which kinds of entry each provider's calendar can store: out of office
//! and focus time, and the decline choice that goes with them. The window
//! offers a type only where a save can keep it.

use std::sync::Arc;

use mailrs_dav::fake::FakeDav;

use crate::fake::{FakeGmail, FakeGraph, FakeImap, FakeSmtp};
use crate::services::CalDav;
use crate::{AccountServices, AnyCalendar, ImapSettings, Offers};

fn google(address: &str) -> Offers {
    let fake = Arc::new(FakeGmail::new());
    fake.with(|s| s.email = address.into());
    AccountServices::google(fake).offers()
}

#[test]
fn a_workspace_account_stores_out_of_office_and_focus_time_with_their_declines() {
    let offers = google("dana@fernwood.example");
    assert!(offers.out_of_office && offers.focus_time && offers.declines);
}

#[test]
fn a_personal_google_account_stores_only_events() {
    for address in ["dana@gmail.com", "Dana.Reyes@GMAIL.com", "old@googlemail.com"] {
        let offers = google(address);
        assert!(!offers.out_of_office && !offers.focus_time && !offers.declines, "{address}");
    }
}

#[test]
fn a_domain_that_only_ends_like_gmail_is_workspace() {
    assert!(google("dana@notgmail.com").focus_time);
}

#[test]
fn a_microsoft_account_stores_out_of_office_without_focus_time_or_declines() {
    let offers = AccountServices::fake_microsoft(Arc::new(FakeGraph::new())).offers();
    assert!(offers.out_of_office);
    assert!(!offers.focus_time, "Outlook has no focus time entry to write");
    assert!(!offers.declines, "showAs keeps no decline choice or message");
}

fn imap_settings(address: &str) -> ImapSettings {
    ImapSettings {
        address: address.into(),
        provider_name: "Fernwood".into(),
        files_sent_mail: true,
        window_days: crate::DEFAULT_WINDOW_DAYS,
    }
}

#[test]
fn a_caldav_account_stores_only_events_whatever_its_address() {
    let (imap, smtp) = (Arc::new(FakeImap::new()), Arc::new(FakeSmtp::new()));
    // An address on its own domain made the old check offer focus time.
    let settings = imap_settings("dana@fernwood.example");
    let services = AccountServices::imap(imap, smtp, settings);
    let mail = services.mail.clone();
    let dav = Arc::new(crate::AnyDav::from(Arc::new(FakeDav::new())));
    let offers = services.with_calendar(AnyCalendar::Dav(CalDav::new(dav, mail, vec![]))).offers();
    assert!(offers.calendar);
    assert!(!offers.out_of_office && !offers.focus_time && !offers.declines);
}

#[test]
fn an_account_with_no_calendar_stores_no_types() {
    let offers = AccountServices::fake_imap(Arc::new(FakeImap::new()), Arc::new(FakeSmtp::new())).offers();
    assert!(!offers.out_of_office && !offers.focus_time && !offers.declines);
}
