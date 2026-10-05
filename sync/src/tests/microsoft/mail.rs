use mailrs_domain::mailbox::keyword::{FLAGGED, SEEN};
use mailrs_domain::{ChangeEvent, MailboxKind, Role, category};

use super::outlook;
use crate::fake::FakeMail;
use crate::{MailBackend, now_millis};

fn fresh() -> FakeMail {
    FakeMail { at: now_millis(), ..FakeMail::default() }
}

#[tokio::test]
async fn folders_come_with_their_roles_and_tags_come_as_tags() {
    let h = outlook().await;
    h.fake.add_category("Red", "preset0");
    let trips = h.fake.add_folder("Trips", None);
    let listed = h.sync.services().mail.mailboxes().await.unwrap();
    let inbox = listed.iter().find(|m| m.role == Some(Role::Inbox)).unwrap();
    assert_eq!(inbox.kind, MailboxKind::System);
    let trips = listed.iter().find(|m| m.id == trips).unwrap();
    assert_eq!((trips.kind, trips.name.as_str()), (MailboxKind::Folder, "Trips"));
    let red = listed.iter().find(|m| m.id == "category:Red").unwrap();
    assert_eq!(red.kind, MailboxKind::Tag);
    assert!(red.color.is_some());
}

#[tokio::test]
async fn new_mail_arrives_with_its_marks_and_is_announced() {
    let h = outlook().await;
    h.fake.add_category("Red", "preset0");
    h.bootstrap_all().await;
    let inbox = h.fake.folder_id("inbox");
    let id = h.fake.deliver(&inbox, fresh());
    h.fake.mark(&id, None, Some(true));
    h.fake.tag(&id, &["Red"]);
    h.fake.classify(&id, true);
    h.look().await;
    let held = h.held(&id).await;
    assert!(held.mailboxes.contains(&inbox) && held.mailboxes.contains(&"category:Red".to_string()));
    assert_eq!(held.keywords, [FLAGGED]);
    assert_eq!(held.categories, [category::OTHER]);
    assert!(h.drain().iter().any(|e| matches!(e, ChangeEvent::NewMail { message_ids, .. } if message_ids == std::slice::from_ref(&id))));
}

#[tokio::test]
async fn a_move_between_synced_folders_keeps_the_stored_message() {
    let h = outlook().await;
    let (inbox, archive) = (h.fake.folder_id("inbox"), h.fake.folder_id("archive"));
    let id = h.fake.deliver(&inbox, fresh());
    h.fake.tag(&id, &["Red"]);
    h.bootstrap_all().await;
    assert!(h.stored(&id).await);
    h.fake.move_message(&id, &archive);
    h.look().await;
    assert!(h.stored(&id).await, "the same id, never deleted");
    let held = h.held(&id).await;
    assert!(held.mailboxes.contains(&archive) && !held.mailboxes.contains(&inbox), "{held:?}");
    assert!(held.mailboxes.contains(&"category:Red".to_string()), "the tag stays");
    assert!(!h.drain().iter().any(|e| matches!(e, ChangeEvent::NewMail { .. })), "a move is not new mail");
}

#[tokio::test]
async fn a_move_to_an_unsynced_folder_keeps_the_message() {
    let h = outlook().await;
    let inbox = h.fake.folder_id("inbox");
    let trips = h.fake.add_folder("Trips", None);
    let id = h.fake.deliver(&inbox, fresh());
    h.bootstrap_all().await;
    h.fake.move_message(&id, &trips);
    h.look().await;
    assert!(h.stored(&id).await);
    assert_eq!(h.held(&id).await.mailboxes, [trips]);
}

#[tokio::test]
async fn mail_erased_elsewhere_leaves_the_store() {
    let h = outlook().await;
    let id = h.fake.deliver(&h.fake.folder_id("inbox"), fresh());
    h.bootstrap_all().await;
    h.fake.delete(&id);
    h.look().await;
    assert!(!h.stored(&id).await);
}

#[tokio::test]
async fn marks_changed_elsewhere_come_back_whole() {
    let h = outlook().await;
    h.fake.add_category("Red", "preset0");
    let id = h.fake.deliver(&h.fake.folder_id("inbox"), fresh());
    h.fake.tag(&id, &["Red"]);
    h.fake.mark(&id, None, Some(true));
    h.bootstrap_all().await;
    h.fake.tag(&id, &[]);
    h.fake.mark(&id, Some(true), Some(false));
    h.look().await;
    let held = h.held(&id).await;
    assert_eq!(held.keywords, [SEEN]);
    assert!(!held.mailboxes.contains(&"category:Red".to_string()));
}

#[tokio::test]
async fn a_refused_delta_link_lists_the_mail_again_under_the_same_ids() {
    let h = outlook().await;
    let id = h.fake.deliver(&h.fake.folder_id("inbox"), fresh());
    h.bootstrap_all().await;
    h.fake.expire_links();
    h.look().await;
    assert!(h.stored(&id).await, "listed again under the id it had");
    // The feed has its place back: the next look answers from new links.
    h.fake.mark(&id, Some(true), None);
    h.look().await;
    assert_eq!(h.held(&id).await.keywords, [mailrs_domain::mailbox::keyword::SEEN]);
}

#[tokio::test]
async fn a_long_backlog_carries_on_at_the_next_look() {
    let h = outlook().await;
    h.bootstrap_all().await;
    let inbox = h.fake.folder_id("inbox");
    let ids: Vec<String> = (0..1100).map(|i| h.fake.deliver(&inbox, FakeMail { at: now_millis() - i, ..FakeMail::default() })).collect();
    h.look().await;
    let first = futures::future::join_all(ids.iter().map(|id| h.stored(id))).await.iter().filter(|s| **s).count();
    assert_eq!(first, 1000, "one look reads twenty pages of fifty");
    h.look().await;
    let all = futures::future::join_all(ids.iter().map(|id| h.stored(id))).await.iter().all(|s| *s);
    assert!(all);
}

#[tokio::test]
async fn metadata_reads_graphs_fields_into_the_store_shape() {
    let h = outlook().await;
    let inbox = h.fake.folder_id("inbox");
    let id = h.fake.deliver(&inbox, FakeMail { subject: "Lunch", conversation: Some("conv-7"), ..fresh() });
    h.fake.with(|s| {
        s.messages.get_mut(&id).unwrap().message.internet_message_headers = Some(vec![mailrs_graph::Header {
            name: "List-Unsubscribe".into(),
            value: "<https://news.example/u>".into(),
        }]);
    });
    let found = h.sync.services().mail.fetch(vec![crate::Want::message(&id)]).await.unwrap();
    let meta = &found.metas[0];
    assert_eq!(meta.thread_id, "conv-7");
    assert_eq!(meta.subject, "Lunch");
    assert!(meta.rfc822_msgid.as_deref().is_some_and(|m| !m.starts_with('<')));
    assert!(meta.size > 0);
    assert_eq!(meta.roles, [Role::Inbox]);
    assert_eq!(meta.list_unsubscribe.as_deref(), Some("<https://news.example/u>"));
}

#[tokio::test]
async fn a_preview_with_line_breaks_is_stored_as_one_line() {
    // Graph's bodyPreview keeps the body's line breaks and blank lines;
    // the list row clamps a preview to two lines only within one
    // paragraph, so a preview with breaks filled the row.
    let h = outlook().await;
    let id = h.fake.deliver(&h.fake.folder_id("inbox"), FakeMail { subject: "Apps", ..fresh() });
    h.fake.with(|s| {
        s.messages.get_mut(&id).unwrap().message.body_preview =
            Some("Microsoft account\r\nNew app(s) have access\r\n\r\n  Manage your apps".into());
    });
    let found = h.sync.services().mail.fetch(vec![crate::Want::message(&id)]).await.unwrap();
    assert_eq!(found.metas[0].snippet, "Microsoft account New app(s) have access Manage your apps");
}

#[tokio::test]
async fn a_large_message_reads_by_its_structure_with_part_paths() {
    let h = outlook().await;
    let id = h.fake.deliver(
        &h.fake.folder_id("inbox"),
        FakeMail { files: vec![("plan.pdf", "application/pdf", b"%PDF-1.7".to_vec())], ..fresh() },
    );
    let mail = &h.sync.services().mail;
    let parts = mail.fetch_structure(&id).await.unwrap();
    assert_eq!(parts.root.children[0].path, "1");
    assert!(parts.root.children[0].data.is_some(), "the text comes along");
    let file = &parts.root.children[1];
    assert_eq!((file.path.as_str(), file.filename.as_deref()), ("2", Some("plan.pdf")));
    assert!(file.data.is_none(), "a file waits until it is opened");
    assert_eq!(mail.fetch_part(&id, "2").await.unwrap(), b"%PDF-1.7");
}

#[tokio::test]
async fn the_grant_decides_what_is_withheld() {
    let h = outlook().await;
    assert!(h.sync.services().withheld().is_empty());
    h.fake.withhold("Calendars.ReadWrite");
    assert!(h.sync.services().withheld().calendar);
}

#[tokio::test]
async fn a_microsoft_account_offers_folders_tags_and_focus() {
    let h = outlook().await;
    let offers = h.sync.services().offers();
    assert!(!offers.labels && offers.tags && offers.focused && !offers.categories);
    assert!(offers.calendar && offers.contacts && offers.rules && offers.auto_reply);
    assert!(!offers.event_files && !offers.moves_events && offers.calendar_list);
}
