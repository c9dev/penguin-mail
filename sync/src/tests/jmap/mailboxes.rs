use mailrs_domain::{MailSet, MailboxKind, Role};

use super::{fastmail, over};
use crate::fake::FakeJmap;
use crate::{MailBackend, MailCapabilities};

#[tokio::test]
async fn mailboxes_come_with_their_roles_and_nested_paths() {
    let h = fastmail().await;
    let trips = h.fake.add_mailbox("Trips", None);
    let year = h.fake.add_mailbox("2026", Some(&trips));
    let listed = h.mail().mailboxes().await.unwrap();
    let inbox = listed.iter().find(|m| m.role == Some(Role::Inbox)).unwrap();
    assert_eq!(
        (inbox.kind, inbox.name.as_str()),
        (MailboxKind::System, "Inbox")
    );
    let nested = listed.iter().find(|m| m.id == year).unwrap();
    assert_eq!(nested.name, "Trips/2026");
    assert_eq!(
        nested.kind,
        MailboxKind::Label,
        "Fastmail allows many mailboxes per message"
    );
    assert_eq!(
        h.mail().mailbox_for(Role::Junk),
        Some(h.fake.mailbox("junk"))
    );
    assert_eq!(
        h.mail().set_of(&h.fake.mailbox("sent")),
        MailSet::Role(Role::Sent)
    );
    assert!(h.mail().made_by_person(&trips) && !h.mail().made_by_person(&h.fake.mailbox("inbox")));
}

#[tokio::test]
async fn one_mailbox_per_message_makes_folders() {
    let h = over(FakeJmap::new().mailboxes_per_email(Some(1))).await;
    let trips = h.fake.add_mailbox("Trips", None);
    assert!(!h.mail().capabilities().labels);
    let listed = h.mail().mailboxes().await.unwrap();
    assert_eq!(
        listed.iter().find(|m| m.id == trips).unwrap().kind,
        MailboxKind::Folder
    );
}

#[tokio::test]
async fn the_capabilities_are_jmap_s() {
    let h = fastmail().await;
    assert_eq!(
        h.mail().capabilities(),
        MailCapabilities {
            labels: true,
            server_threads: true,
            files_sent_mail: true,
            categories: false,
            delete_forever: true,
            batch_limit: 500,
            keywords: &[
                "$seen",
                "$flagged",
                "$answered",
                "$draft",
                "$muted",
                "$mdnsent"
            ],
            native_search: false,
            paged_mailboxes: false,
            renames: false,
            restates: true,
            tags: false,
            focus: false,
            local_mailboxes: false,
            files_muted_replies: false,
        }
    );
}

#[tokio::test]
async fn unread_counts_come_from_the_listing_with_no_extra_call() {
    let h = fastmail().await;
    h.fake.deliver("inbox", &super::raw("u1@x", "One"), 1_000);
    h.mail().mailboxes().await.unwrap();
    let calls = h.fake.with(|s| s.calls.len());
    let counts = h.mail().unread_counts().await.unwrap();
    assert_eq!(counts.get(&MailSet::Role(Role::Inbox)), Some(&1));
    assert_eq!(h.fake.with(|s| s.calls.len()), calls);
}

#[tokio::test]
async fn a_mailbox_is_made_under_its_parent_renamed_and_deleted() {
    let h = fastmail().await;
    let trips = h.fake.add_mailbox("Trips", None);
    h.mail().mailboxes().await.unwrap();
    let made = h.mail().create_mailbox("Trips/Rome").await.unwrap();
    assert_eq!(made.name, "Trips/Rome");
    assert_eq!(
        h.fake.with(|s| s.mailboxes[&made.id].parent_id.clone()),
        Some(trips)
    );
    let renamed = h
        .mail()
        .rename_mailbox(&made.id, "Rome 2026")
        .await
        .unwrap();
    assert_eq!(renamed.name, "Rome 2026");
    assert_eq!(
        h.fake.with(|s| s.mailboxes[&made.id].parent_id.clone()),
        None
    );
    h.mail().delete_mailbox(&made.id).await.unwrap();
    assert!(h.fake.with(|s| !s.mailboxes.contains_key(&made.id)));
}

#[tokio::test]
async fn a_colour_is_refused_in_words() {
    let h = fastmail().await;
    let trips = h.fake.add_mailbox("Trips", None);
    let color = mailrs_gmail::LabelColor {
        background_color: "#16a765".into(),
        text_color: "#ffffff".into(),
    };
    let refused = h.mail().set_mailbox_color(&trips, &color).await;
    assert!(matches!(refused, Err(crate::BackendError::Refused(_))));
}
