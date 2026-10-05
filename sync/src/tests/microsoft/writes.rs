use mailrs_domain::mailbox::keyword::{FLAGGED, SEEN};
use mailrs_domain::{Target, category};
use mailrs_gmail::LabelColor;

use super::outlook;
use crate::fake::FakeMail;
use crate::{MailBackend, MailOp, TriageAction, now_millis};

fn fresh() -> FakeMail {
    FakeMail { at: now_millis(), ..FakeMail::default() }
}

#[tokio::test]
async fn archiving_moves_the_message_and_keeps_its_id() {
    let h = outlook().await;
    let id = h.fake.deliver(&h.fake.folder_id("inbox"), fresh());
    h.bootstrap_all().await;
    let thread = h.fake.with(|s| s.messages[&id].message.conversation_id.clone().unwrap());
    h.sync.triage_all(&[Target::thread(h.account_id, thread)], &TriageAction::Archive, None).await.unwrap();
    assert_eq!(h.fake.with(|s| s.messages[&id].folder.clone()), h.fake.folder_id("archive"));
    assert_eq!(h.held(&id).await.mailboxes, [h.fake.folder_id("archive")]);
}

#[tokio::test]
async fn archiving_tagged_mail_keeps_its_categories() {
    let h = outlook().await;
    h.fake.add_category("Red", "preset0");
    let id = h.fake.deliver(&h.fake.folder_id("inbox"), fresh());
    h.fake.tag(&id, &["Red"]);
    h.bootstrap_all().await;
    let thread = h.fake.with(|s| s.messages[&id].message.conversation_id.clone().unwrap());
    h.sync.triage_all(&[Target::thread(h.account_id, thread)], &TriageAction::Archive, None).await.unwrap();
    assert_eq!(h.fake.with(|s| s.messages[&id].message.categories.clone()), Some(vec!["Red".into()]));
    assert!(h.held(&id).await.mailboxes.contains(&"category:Red".to_string()));
}

#[tokio::test]
async fn a_tag_goes_on_and_comes_off_beside_the_others() {
    let h = outlook().await;
    h.fake.add_category("Red", "preset0");
    h.fake.add_category("Blue", "preset7");
    let id = h.fake.deliver(&h.fake.folder_id("inbox"), fresh());
    h.fake.tag(&id, &["Blue"]);
    h.bootstrap_all().await;
    let thread = h.fake.with(|s| s.messages[&id].message.conversation_id.clone().unwrap());
    let target = [Target::thread(h.account_id, thread)];
    h.sync.triage_all(&target, &TriageAction::AddLabel("category:Red".into()), None).await.unwrap();
    let mut on = h.fake.with(|s| s.messages[&id].message.categories.clone().unwrap());
    on.sort();
    assert_eq!(on, ["Blue", "Red"]);
    h.sync.triage_all(&target, &TriageAction::RemoveLabel("category:Blue".into()), None).await.unwrap();
    assert_eq!(h.fake.with(|s| s.messages[&id].message.categories.clone()), Some(vec!["Red".into()]));
    assert_eq!(h.fake.with(|s| s.messages[&id].folder.clone()), h.fake.folder_id("inbox"), "tagging never moves");
}

#[tokio::test]
async fn marks_and_focus_are_one_patch() {
    let h = outlook().await;
    let id = h.fake.deliver(&h.fake.folder_id("inbox"), fresh());
    h.bootstrap_all().await;
    let mail = &h.sync.services().mail;
    mail.apply(
        std::slice::from_ref(&id),
        &[
            MailOp::SetKeyword { keyword: SEEN.into(), on: true },
            MailOp::SetKeyword { keyword: FLAGGED.into(), on: true },
            MailOp::SetCategory { category: category::OTHER.into(), on: true },
        ],
    )
    .await
    .unwrap();
    let message = h.fake.with(|s| s.messages[&id].message.clone());
    assert_eq!(message.is_read, Some(true));
    assert!(message.is_flagged() && message.is_other());
}

#[tokio::test]
async fn undoing_a_move_moves_it_back() {
    let h = outlook().await;
    let (inbox, archive) = (h.fake.folder_id("inbox"), h.fake.folder_id("archive"));
    let id = h.fake.deliver(&archive, fresh());
    h.bootstrap_all().await;
    // Undo reverses a move as adding the old place back and taking the new
    // one away.
    h.sync.services().mail
        .apply(std::slice::from_ref(&id), &[MailOp::AddToMailbox(inbox.clone()), MailOp::RemoveFromMailbox(archive)])
        .await
        .unwrap();
    assert_eq!(h.fake.with(|s| s.messages[&id].folder.clone()), inbox);
}

#[tokio::test]
async fn erasing_is_for_good() {
    let h = outlook().await;
    let id = h.fake.deliver(&h.fake.folder_id("deleteditems"), fresh());
    h.bootstrap_all().await;
    h.sync.services().mail.apply(std::slice::from_ref(&id), &[MailOp::Destroy]).await.unwrap();
    assert!(h.fake.with(|s| !s.messages.contains_key(&id)));
}

#[tokio::test]
async fn a_refused_message_stops_the_count_where_it_failed() {
    let h = outlook().await;
    let inbox = h.fake.folder_id("inbox");
    let a = h.fake.deliver(&inbox, fresh());
    let b = h.fake.deliver(&inbox, fresh());
    h.bootstrap_all().await;
    h.fake.delete(&b);
    let refused = h
        .sync
        .services()
        .mail
        .apply(&[a.clone(), b.clone()], &[MailOp::MoveToRole(mailrs_domain::Role::Archive)])
        .await
        .unwrap_err();
    assert_eq!(refused.taken, 1);
    assert!(matches!(refused.error, crate::BackendError::NotFound));
}

#[tokio::test]
async fn folders_nest_by_slash_and_tags_take_a_preset_colour() {
    let h = outlook().await;
    h.fake.add_category("Red", "preset0");
    let mail = &h.sync.services().mail;
    mail.mailboxes().await.unwrap();
    let made = mail.create_mailbox("Trips/2026").await.unwrap();
    assert_eq!(made.name, "Trips/2026");
    let parent = h.fake.with(|s| s.folders[&made.id].parent.clone()).unwrap();
    assert_eq!(h.fake.with(|s| s.folders[&parent].name.clone()), "Trips");
    let color = LabelColor { background_color: "#4a8ee0".into(), text_color: "#ffffff".into() };
    mail.set_mailbox_color("category:Red", &color).await.unwrap();
    assert_eq!(h.fake.with(|s| s.categories[0].color.clone()), "preset7");
    assert!(matches!(
        mail.set_mailbox_color(&made.id, &color).await,
        Err(crate::BackendError::Unsupported)
    ));
}

#[tokio::test]
async fn a_folder_renames_in_place_and_goes_away_but_a_tag_does_neither() {
    let h = outlook().await;
    h.fake.add_category("Red", "preset0");
    let mail = &h.sync.services().mail;
    mail.mailboxes().await.unwrap();
    let made = mail.create_mailbox("Trips").await.unwrap();
    let renamed = mail.rename_mailbox(&made.id, "Journeys").await.unwrap();
    assert_eq!(renamed.name, "Journeys");
    assert_eq!(h.fake.with(|s| s.folders[&made.id].name.clone()), "Journeys");
    assert!(matches!(mail.rename_mailbox(&made.id, "A/Journeys").await, Err(crate::BackendError::Unsupported)));
    mail.delete_mailbox(&made.id).await.unwrap();
    assert!(h.fake.with(|s| !s.folders.contains_key(&made.id)));
    assert!(matches!(mail.rename_mailbox("category:Red", "Blue").await, Err(crate::BackendError::Unsupported)));
    assert!(matches!(mail.delete_mailbox("category:Red").await, Err(crate::BackendError::Unsupported)));
}
