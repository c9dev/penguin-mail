//! Finding the servers beside mail, with a network and a probe held in
//! memory.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use mailrs_dav::Kind;
use mailrs_discover::fake::FakeNet;
use mailrs_sieve::script::Extensions;
use mailrs_store::services::{self, Miss, ServiceKind};

use crate::services::finding::{ServiceProbe, confirm_found, find_services, keep_found, use_typed};
use crate::BackendError;

/// Takes the logins in `open` and refuses the rest; records every try.
#[derive(Default)]
struct Probe {
    open: HashSet<(String, String)>,
    sieve: Option<&'static str>,
    tried: Mutex<Vec<String>>,
}

impl ServiceProbe for Probe {
    async fn dav(&self, url: &str, user: &str, _password: &str, _kind: Kind) -> Result<(), BackendError> {
        self.tried.lock().unwrap().push(url.to_string());
        match self.open.contains(&(url.to_string(), user.to_string())) {
            true => Ok(()),
            false => Err(BackendError::NeedsReauth),
        }
    }

    async fn sieve(&self, host: &str, _port: u16, _user: &str, _password: &str) -> Result<Extensions, BackendError> {
        self.tried.lock().unwrap().push(host.to_string());
        self.sieve.map(Extensions::parse).ok_or(BackendError::Offline("closed".into()))
    }
}

#[tokio::test]
async fn a_table_provider_is_found_with_the_user_name_that_works() {
    let probe = Probe {
        open: [("https://caldav.fastmail.com/dav/calendars".to_string(), "me@fastmail.com".to_string())].into(),
        ..Probe::default()
    };
    let found = find_services(&FakeNet::default(), &probe, "me@fastmail.com", "Fastmail", "imap.fastmail.com", "me", "pw").await;
    let caldav = found.caldav.expect("found");
    assert_eq!(caldav.user_name, "me@fastmail.com");
    assert!(caldav.confirmed);
    assert_eq!(found.sieve, None, "Fastmail's port 4190 is closed");
}

#[tokio::test]
async fn a_host_outside_the_domain_is_kept_for_a_yes_and_never_sent_the_password() {
    let net = FakeNet::default().answer_srv(
        "_caldavs._tcp.example.org",
        vec![mailrs_discover::SrvRecord { priority: 0, weight: 0, port: 443, target: "dav.hoster.net.".into() }],
    );
    let probe = Probe::default();
    let found = find_services(&net, &probe, "me@example.org", "example.org", "mail.example.org", "me@example.org", "pw").await;
    let caldav = found.caldav.expect("kept");
    assert!(!caldav.confirmed);
    assert!(probe.tried.lock().unwrap().iter().all(|u| !u.contains("hoster.net")));
}

#[tokio::test]
async fn sieve_counts_only_with_fileinto_and_vacation() {
    let weak = Probe { sieve: Some("fileinto"), ..Probe::default() };
    let found = find_services(&FakeNet::default(), &weak, "me@example.org", "example.org", "mail.example.org", "me", "pw").await;
    assert_eq!(found.sieve, None);
    let full = Probe { sieve: Some("fileinto vacation"), ..Probe::default() };
    let found = find_services(&FakeNet::default(), &full, "me@example.org", "example.org", "mail.example.org", "me", "pw").await;
    assert_eq!(found.sieve.expect("found").url, "mail.example.org:4190");
}

#[tokio::test]
async fn a_typed_url_is_checked_and_kept_and_discovery_leaves_it_alone() {
    let db = crate::tests::store_with_imap_account().await;
    let probe = Probe { open: [("https://dav.example.org/".to_string(), "me".to_string())].into(), ..Probe::default() };
    assert!(use_typed(&probe, &db.0, db.1, ServiceKind::CalDav, "http://dav.example.org/", "me", "pw").await.is_err(), "no plain HTTP");
    let typed = use_typed(&probe, &db.0, db.1, ServiceKind::CalDav, "dav.example.org", "me", "pw").await.unwrap();
    assert_eq!(typed.url, "https://dav.example.org/");
    let found = crate::services::finding::FoundServices {
        caldav: Some(services::FoundService { kind: ServiceKind::CalDav, url: "https://caldav.other/".into(), user_name: "me".into(), confirmed: true, source: "table".into() }),
        ..Default::default()
    };
    keep_found(&db.0, db.1, &found).await.unwrap();
    let account_id = db.1;
    let kept = db.0.read(move |c| services::load(c, account_id)).await.unwrap();
    assert_eq!(kept[0].url, "https://dav.example.org/", "what the person typed wins");
}

#[tokio::test]
async fn saying_yes_probes_then_confirms() {
    let db = crate::tests::store_with_imap_account().await;
    let account_id = db.1;
    let row = services::FoundService { kind: ServiceKind::CardDav, url: "https://dav.hoster.net/".into(), user_name: "me".into(), confirmed: false, source: "srv".into() };
    db.0.write(move |c| services::save(c, account_id, &row)).await.unwrap();
    let probe = Probe { open: [("https://dav.hoster.net/".to_string(), "me".to_string())].into(), ..Probe::default() };
    confirm_found(&probe, &db.0, account_id, ServiceKind::CardDav, "pw").await.unwrap();
    let kept = db.0.read(move |c| services::load(c, account_id)).await.unwrap();
    assert!(kept[0].confirmed);
}

/// Answers each user name's try with its own error.
struct Failing(HashMap<&'static str, BackendError>);

impl ServiceProbe for Failing {
    async fn dav(&self, _url: &str, user: &str, _password: &str, _kind: Kind) -> Result<(), BackendError> {
        Err(self.0.get(user).cloned().unwrap_or(BackendError::Unsupported))
    }

    async fn sieve(&self, _host: &str, _port: u16, _user: &str, _password: &str) -> Result<Extensions, BackendError> {
        Err(BackendError::Offline("closed".into()))
    }
}

async fn missed_with(errors: [(&'static str, BackendError); 2]) -> Option<Miss> {
    let probe = Failing(errors.into_iter().collect());
    find_services(&FakeNet::default(), &probe, "me@fastmail.com", "Fastmail", "imap.fastmail.com", "me", "pw").await.caldav_missed
}

#[tokio::test]
async fn a_refused_login_is_why_nothing_was_found_over_no_answer() {
    let missed = missed_with([("me", BackendError::NeedsReauth), ("me@fastmail.com", BackendError::Offline("down".into()))]).await;
    assert_eq!(missed, Some(Miss::Refused));
}

#[tokio::test]
async fn nothing_found_is_why_over_no_answer() {
    let missed = missed_with([("me", BackendError::Offline("down".into())), ("me@fastmail.com", BackendError::Unsupported)]).await;
    assert_eq!(missed, Some(Miss::NotFound));
}

#[tokio::test]
async fn no_answer_anywhere_is_why() {
    let down = || BackendError::Offline("down".into());
    let missed = missed_with([("me", down()), ("me@fastmail.com", down())]).await;
    assert_eq!(missed, Some(Miss::Unreachable));
}

#[tokio::test]
async fn no_place_to_look_is_nothing_found() {
    let found = find_services(&FakeNet::default(), &Probe::default(), "me@example.org", "example.org", "mail.example.org", "me", "pw").await;
    assert_eq!(found.caldav, None);
    assert_eq!(found.carddav_missed, Some(Miss::NotFound));
}

#[tokio::test]
async fn a_found_server_has_no_miss() {
    let probe = Probe {
        open: [("https://caldav.fastmail.com/dav/calendars".to_string(), "me".to_string())].into(),
        ..Probe::default()
    };
    let found = find_services(&FakeNet::default(), &probe, "me@fastmail.com", "Fastmail", "imap.fastmail.com", "me", "pw").await;
    assert!(found.caldav.is_some());
    assert_eq!(found.caldav_missed, None);
}

#[tokio::test]
async fn the_store_keeps_why_until_a_server_is_found() {
    let db = crate::tests::store_with_imap_account().await;
    let account_id = db.1;
    let missed = crate::services::finding::FoundServices { caldav_missed: Some(Miss::Refused), ..Default::default() };
    keep_found(&db.0, account_id, &missed).await.unwrap();
    let kept = db.0.read(services::all_misses).await.unwrap();
    assert_eq!(kept, [(account_id, ServiceKind::CalDav, Miss::Refused)]);
    let found = crate::services::finding::FoundServices {
        caldav: Some(services::FoundService { kind: ServiceKind::CalDav, url: "https://dav.example.org/".into(), user_name: "me".into(), confirmed: true, source: "table".into() }),
        ..Default::default()
    };
    keep_found(&db.0, account_id, &found).await.unwrap();
    assert!(db.0.read(services::all_misses).await.unwrap().is_empty());
}
