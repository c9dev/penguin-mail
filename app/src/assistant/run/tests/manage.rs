//! Looking after labels, smart mailboxes, templates, contacts, the senders
//! whose images load, and exported mail.

use mailrs_domain::smart::{Condition, Field, SmartMailbox};
use mailrs_gmail::labels as gmail;
use mailrs_store::{address_book, image_senders, labels, templates};
use serde_json::json;

use super::super::Permission;
use super::super::fake::{Harness, ME, NOW, labelled, meta};
use super::{harness, mail, target};

/// The fixture mail, with the Kites label on two of its conversations.
async fn with_kites() -> Harness {
    let mut all = mail();
    all.push(labelled(
        meta("k1", "tk1", "theo@example.com", "Kite string", NOW - 5_000),
        &[gmail::INBOX, "Label_kites"],
    ));
    all.push(labelled(
        meta("k2", "tk2", "ann@example.com", "Kite tails", NOW - 6_000),
        &["Label_kites"],
    ));
    Harness::with(all).await
}

/// The labels the person made in the fake Gmail, leaving out Gmail's own.
fn persons_labels(h: &Harness) -> Vec<mailrs_gmail::RemoteLabel> {
    h.gmail.with(|s| {
        s.labels
            .iter()
            .filter(|l| l.kind.as_deref() == Some("user"))
            .cloned()
            .collect()
    })
}

async fn stored_labels(h: &Harness) -> Vec<String> {
    let id = h.account_id;
    h.db.read(move |c| labels::list_labels(c, id))
        .await
        .expect("the labels are stored")
        .into_iter()
        .map(|l| l.name)
        .collect()
}

#[tokio::test]
async fn rename_label_asks_then_renames_in_gmail_and_the_store() {
    let h = harness().await;
    let done = h
        .ok(
            "rename_label",
            json!({"account": ME, "label": "kites", "new_name": "Hobbies/Kites"}),
        )
        .await;
    assert_eq!(done, json!({"renamed": "Kites", "to": "Hobbies/Kites"}));
    assert_eq!(
        h.asked().questions,
        [format!(
            "Rename the label “Kites” in {ME} to “Hobbies/Kites”? Labels nested under it move along."
        )]
    );
    assert_eq!(persons_labels(&h)[0].name, "Hobbies/Kites");
    assert!(stored_labels(&h).await.contains(&"Hobbies/Kites".into()));
}

#[tokio::test]
async fn a_label_the_account_lacks_is_named_back() {
    let h = harness().await;
    assert_eq!(
        h.run(
            "rename_label",
            json!({"account": ME, "label": "Ferns", "new_name": "Plants"})
        )
        .await,
        Err(format!("{ME} has no label called Ferns."))
    );
    assert!(h.asked().questions.is_empty());
}

#[tokio::test]
async fn recolor_label_gives_it_a_colour_from_gmails_palette() {
    let h = harness().await;
    h.ok(
        "recolor_label",
        json!({"account": ME, "label": "Kites", "color": "blue"}),
    )
    .await;
    assert_eq!(
        h.asked().questions,
        [format!("Color the label “Kites” in {ME} blue?")]
    );
    let color = persons_labels(&h)[0].color.clone().expect("a colour");
    assert_eq!(color.background_color, "#4a86e8");
    assert_eq!(color.text_color, "#ffffff");

    assert!(
        h.run(
            "recolor_label",
            json!({"account": ME, "label": "Kites", "color": "mauve"})
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn delete_label_says_how_many_conversations_carry_it() {
    let h = with_kites().await;
    h.effects.asked.borrow_mut().approves = false;
    assert_eq!(
        h.run("delete_label", json!({"account": ME, "label": "Kites"}))
            .await,
        Err("The user declined.".into())
    );
    assert_eq!(
        h.asked().questions,
        [format!(
            "Delete the label “Kites” from {ME}? 2 conversations carry it. \
             The mail stays in Gmail, without the label."
        )]
    );
    assert_eq!(persons_labels(&h).len(), 1, "a no keeps the label");

    h.effects.asked.borrow_mut().approves = true;
    let done = h
        .ok("delete_label", json!({"account": ME, "label": "Kites"}))
        .await;
    assert_eq!(done, json!({"deleted": "Kites", "conversations": 2}));
    assert!(persons_labels(&h).is_empty());
    assert!(!stored_labels(&h).await.contains(&"Kites".into()));
    assert!(!h.labels_of("k1").await.contains(&"Label_kites".into()));
    assert!(
        h.labels_of("k1")
            .await
            .contains(&gmail::INBOX.into())
    );
}

fn smart(id: &str, name: &str, from: &str) -> SmartMailbox {
    SmartMailbox {
        id: id.into(),
        name: name.into(),
        account: None,
        match_all: true,
        conditions: vec![Condition {
            field: Field::From,
            value: from.into(),
        }],
    }
}

#[tokio::test]
async fn smart_mailboxes_list_change_in_place_and_delete_after_asking() {
    let h = harness().await;
    h.desk.0.borrow_mut().settings.smart_mailboxes = vec![
        smart("s1", "From Ann", "ann@example.com"),
        smart("s2", "From Theo", "theo@example.com"),
    ];
    let listed = h.ok("list_smart_mailboxes", json!({})).await;
    assert_eq!(listed["smart_mailboxes"][0]["id"], "s1");
    assert_eq!(
        listed["smart_mailboxes"][0]["gmail_query"],
        "from:ann@example.com"
    );

    h.ok(
        "update_smart_mailbox",
        json!({
            "mailbox": "from ann",
            "name": "Ann, unread",
            "account": ME,
            "conditions": [{"field": "from", "value": "ann@example.com"}, {"field": "unread"}],
        }),
    )
    .await;
    let kept = h.desk.0.borrow().settings.smart_mailboxes.clone();
    assert_eq!(kept.len(), 2);
    assert_eq!(kept[0].id, "s1", "the change keeps its place");
    assert_eq!(kept[0].name, "Ann, unread");
    assert_eq!(kept[0].account.as_deref(), Some(ME));
    assert_eq!(
        kept[0].query().as_deref(),
        Some("from:ann@example.com is:unread")
    );
    assert!(
        h.asked().questions.is_empty(),
        "a change in place asks nothing"
    );

    h.ok("delete_smart_mailbox", json!({"mailbox": "s2"})).await;
    assert_eq!(
        h.asked().questions,
        ["Delete the smart mailbox “From Theo”? The mail it lists stays where it is."]
    );
    let names: Vec<String> = h
        .desk
        .0
        .borrow()
        .settings
        .smart_mailboxes
        .iter()
        .map(|m| m.name.clone())
        .collect();
    assert_eq!(names, ["Ann, unread"]);
}

async fn stored_templates(h: &Harness) -> Vec<templates::Template> {
    h.db.read(templates::list)
        .await
        .expect("the templates list")
}

#[tokio::test]
async fn save_template_says_when_it_replaces_one_and_delete_removes_it() {
    let h = harness().await;
    let saved = h
        .ok(
            "save_template",
            json!({"name": "Thanks", "body": "Thank you, {{first_name}}.", "subject": "Thanks"}),
        )
        .await;
    assert_eq!(saved, json!({"saved": "Thanks", "replaced": false}));
    assert_eq!(
        h.asked().questions[0],
        "Save a template called “Thanks”?\n\nThank you, {{first_name}}."
    );

    let saved = h
        .ok(
            "save_template",
            json!({"name": "thanks", "body": "Many thanks, {{first_name}}."}),
        )
        .await;
    assert_eq!(saved["replaced"], true);
    assert!(
        h.asked().questions[1]
            .starts_with("Replace the template “Thanks” with this one? The saved one is lost.")
    );
    let kept = stored_templates(&h).await;
    assert_eq!(kept.len(), 1, "the new one takes the old one's place");
    assert_eq!(kept[0].markdown, "Many thanks, {{first_name}}.");
    assert_eq!(kept[0].subject, "Thanks", "a subject left out stays");

    h.ok("delete_template", json!({"name": "THANKS"})).await;
    assert_eq!(h.asked().questions[2], "Delete the template “Thanks”?");
    assert!(stored_templates(&h).await.is_empty());
    assert_eq!(
        h.run("delete_template", json!({"name": "Thanks"})).await,
        Err("There is no template called Thanks.".into())
    );
}

/// Turns on the fixture account's contacts, as Preferences does.
fn contacts_on(h: &Harness) {
    h.desk.0.borrow_mut().settings.contact_accounts = vec![ME.into()];
}

#[tokio::test]
async fn create_contact_writes_to_google_and_the_address_book() {
    let h = harness().await;
    contacts_on(&h);
    let made = h
        .ok(
            "create_contact",
            json!({
                "name": "Priya Shah",
                "emails": ["priya@fernwood.example"],
                "phones": ["+351 21 000 0000"],
                "organization": "Fernwood",
            }),
        )
        .await;
    assert_eq!(made["created"]["id"], "people/c1");
    assert_eq!(
        h.asked().questions,
        [format!(
            "Add Priya Shah to the Google contacts of {ME}?\n\n\
             Name: Priya Shah\nAddresses: priya@fernwood.example\n\
             Phones: +351 21 000 0000\nOrganization: Fernwood"
        )]
    );
    let google = h.gmail.with(|s| s.contacts.clone());
    assert_eq!(google.len(), 1);
    assert_eq!(google[0].organization.as_deref(), Some("Fernwood"));

    let found = h.ok("find_contact", json!({"query": "fernwood"})).await;
    assert_eq!(found["contacts"][0]["name"], "Priya Shah");
    assert_eq!(found["contacts"][0]["id"], "people/c1");
}

#[tokio::test]
async fn an_account_with_contacts_off_keeps_no_copy_of_a_new_contact() {
    let h = harness().await;
    h.ok("create_contact", json!({"emails": ["bo@example.com"]}))
        .await;
    assert_eq!(h.gmail.with(|s| s.contacts.len()), 1);
    let stored = h.db.read(address_book::list).await.expect("the book");
    assert!(stored.is_empty());
}

#[tokio::test]
async fn update_contact_finds_the_contact_by_address_and_changes_what_it_names() {
    let h = harness().await;
    contacts_on(&h);
    h.ok(
        "create_contact",
        json!({"name": "Priya Shah", "emails": ["priya@fernwood.example"]}),
    )
    .await;

    let changed = h
        .ok(
            "update_contact",
            json!({"contact": "PRIYA@fernwood.example", "phones": ["+351 91 000 0000"]}),
        )
        .await;
    assert_eq!(changed["updated"]["phone"], "+351 91 000 0000");
    assert_eq!(
        h.asked().questions[1],
        format!("Change Priya Shah in the Google contacts of {ME}?\n\nPhones: +351 91 000 0000")
    );
    let google = h.gmail.with(|s| s.contacts[0].clone());
    assert_eq!(google.phone.as_deref(), Some("+351 91 000 0000"));
    assert_eq!(google.name.as_deref(), Some("Priya Shah"), "the name stays");
    let stored = h.db.read(address_book::list).await.expect("the book");
    assert_eq!(stored[0].phone.as_deref(), Some("+351 91 000 0000"));

    assert!(
        h.run(
            "update_contact",
            json!({"contact": "nobody@example.com", "name": "No One"})
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn writing_a_contact_asks_for_the_permission_it_lacks() {
    let h = harness().await;
    h.gmail.withhold(mailrs_gmail::CONTACTS_WRITE_SCOPE);
    let answer = h.run("create_contact", json!({"name": "Priya Shah"})).await;
    assert_eq!(
        answer,
        Err(format!(
            "Penguin Mail needs permission to add and change contacts for {ME}. \
             The user was asked to grant it; try again once they have."
        ))
    );
    assert_eq!(
        h.asked().permission_asked,
        [(h.account_id, Permission::ChangeContacts)]
    );
    assert!(h.gmail.with(|s| s.contacts.is_empty()));
}

#[tokio::test]
async fn create_contact_on_an_account_without_contacts_says_why() {
    let h = Harness::with_services(|_, services| services.contacts = None).await;
    let answer = h.run("create_contact", json!({"name": "Priya Shah"})).await;
    assert_eq!(
        answer,
        Ok(json!({"unavailable": "Gmail keeps no contacts that other apps can reach."}))
    );
}

async fn allowed(h: &Harness) -> Vec<(String, bool)> {
    h.db.read(image_senders::list)
        .await
        .expect("the list")
        .into_iter()
        .map(|s| (s.sender, s.whole_domain))
        .collect()
}

#[tokio::test]
async fn images_load_from_a_sender_or_a_domain_and_stop_again() {
    let h = harness().await;
    h.ok("allow_images", json!({"sender": "Ann@Example.com"}))
        .await;
    h.ok(
        "allow_images",
        json!({"sender": "news@trail.example", "whole_domain": true}),
    )
    .await;
    assert_eq!(
        h.asked().questions,
        [
            "Always load images from ann@example.com? Loading a remote image tells the \
             sender when you opened their mail.",
            "Always load images from anyone at trail.example? Loading a remote image tells \
             the sender when you opened their mail.",
        ]
    );
    let mut list = allowed(&h).await;
    list.sort();
    assert_eq!(
        list,
        [
            ("ann@example.com".to_string(), false),
            ("trail.example".to_string(), true)
        ]
    );
    assert_eq!(h.asked().image_senders_changed, 2);

    let listed = h.ok("list_image_senders", json!({})).await;
    assert_eq!(listed["senders"].as_array().map(Vec::len), Some(2));

    h.ok("forget_image_sender", json!({"sender": "trail.example"}))
        .await;
    assert_eq!(
        h.asked().questions[2],
        "Stop loading images from trail.example?"
    );
    assert_eq!(allowed(&h).await, [("ann@example.com".to_string(), false)]);
    assert_eq!(h.asked().image_senders_changed, 3);
    assert!(
        h.run("forget_image_sender", json!({"sender": "bo@example.com"}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn export_mail_writes_an_mbox_to_downloads_and_never_over_an_earlier_one() {
    let h = harness().await;
    let downloads = h.desk.0.borrow().downloads.clone();
    let done = h
        .ok("export_mail", json!({"targets": [target("t3")]}))
        .await;
    let file = downloads.join("Fern cuttings 2026-01-01.mbox");
    assert_eq!(done["file"], file.display().to_string());
    assert_eq!(
        h.asked().questions,
        [format!("Export 1 conversation to {}?", file.display())]
    );
    let mbox = std::fs::read_to_string(&file).expect("the file");
    assert_eq!(mbox.matches("\nFrom ").count() + 1, 2, "{mbox}");

    let again = h
        .ok("export_mail", json!({"targets": [target("t3")]}))
        .await;
    assert_eq!(
        again["file"],
        downloads
            .join("Fern cuttings 2026-01-01 2.mbox")
            .display()
            .to_string()
    );
    assert_eq!(again["replaced"], false);
}

#[tokio::test]
async fn export_mail_says_when_it_replaces_a_file_the_user_named() {
    let h = harness().await;
    let downloads = h.desk.0.borrow().downloads.clone();
    let named = downloads.join("backup.mbox");
    std::fs::write(&named, b"old").expect("a file to replace");
    h.effects.asked.borrow_mut().approves = false;
    assert!(
        h.run(
            "export_mail",
            json!({"targets": [target("t1"), target("t2")], "path": "backup.mbox"}),
        )
        .await
        .is_err()
    );
    assert_eq!(
        h.asked().questions,
        [format!(
            "Export 2 conversations to {}? A file with that name is there already, and \
             exporting replaces it.",
            named.display()
        )]
    );
    assert_eq!(std::fs::read(&named).expect("the file"), b"old");
}

#[tokio::test]
async fn export_mail_writes_one_message_as_eml() {
    let h = harness().await;
    let downloads = h.desk.0.borrow().downloads.clone();
    assert!(
        h.run(
            "export_mail",
            json!({"targets": [target("t3")], "format": "eml"})
        )
        .await
        .expect_err("an .eml needs a message")
        .contains("message_id")
    );
    let done = h
        .ok(
            "export_mail",
            json!({
                "targets": [{"account": ME, "thread_id": "t3", "message_id": "m3"}],
                "format": "eml",
            }),
        )
        .await;
    let file = downloads.join("Fern cuttings 2025-12-30.eml");
    assert_eq!(done["file"], file.display().to_string());
    assert_eq!(
        h.asked().questions[0],
        format!("Export the message “Fern cuttings” to {}?", file.display())
    );
    let raw = std::fs::read_to_string(&file).expect("the file");
    assert!(raw.contains("Subject: Fern cuttings"), "{raw}");
    assert!(!raw.starts_with("From "), "an .eml has no mbox separator");
}
