//! A message carried in from another account lands on an IMAP server
//! with the marks it wore and the date it arrived under at home, since
//! neither can be put back by hand afterwards.

use super::{adapter, days_ago, message};
use crate::fake::FakeImap;
use crate::services::{Landing, MailBackend};

#[tokio::test]
async fn a_landed_copy_keeps_its_flags_and_arrives_under_its_old_date() {
    let imap = FakeImap::new();
    imap.add_mailbox("Work", None);
    let (imap, mail) = adapter(imap);
    assert!(mail.lands_copies(), "an IMAP server takes a copy");
    let raw = message("x", "Note", "");
    let at = days_ago(1);
    let name = mail
        .land_copy(
            &raw,
            &Landing {
                mailboxes: &["Work".to_string()],
                keywords: &["$seen".to_string(), "$flagged".to_string()],
                at: Some(at),
            },
        )
        .await
        .unwrap();
    assert!(name.starts_with("Work/"), "the copy's name: {name}");
    let held = imap.with(|s| s.mailboxes["Work"].messages.values().next().cloned());
    let Some(held) = held else {
        panic!("the copy is filed in Work");
    };
    assert_eq!(held.internal_date, at, "the date it arrived under at home");
    assert_eq!(
        held.flags,
        ["\\Seen".to_string(), "\\Flagged".to_string()].into(),
        "the marks spelled the IMAP way"
    );
    assert_eq!(held.raw, raw, "the bytes landed as they arrived");
}
