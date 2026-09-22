//! The tool loop, headless. Each test drives `Tools::run` the way the model
//! does and checks the JSON that goes back, plus what the ports were asked
//! to do.

use mailrs_domain::{FlagColor, MessageBody, MessageMeta, Vacation, system_label};
use mailrs_sync::{MailAction, Outcome, Permitted, TriageAction};
use serde_json::{Value, json};

use super::catalog::{Run, catalog};
use super::fake::{Connected, Harness, ME, NOW, YOU, labelled, meta};
use super::{OpenConversation, Permission};
use crate::settings::{Change, TextSize};
use mailrs_sync::hidden;

mod calendar;
mod mail;

const DAY: i64 = 24 * 60 * 60 * 1000;

/// Three inbox conversations: one unread from Theo, a promotion, and a
/// two-message thread from Ann.
fn mail() -> Vec<MessageMeta> {
    vec![
        labelled(
            meta("m1", "t1", "theo@example.com", "Kite plans", NOW - DAY),
            &[
                system_label::INBOX,
                system_label::UNREAD,
                system_label::CATEGORY_PERSONAL,
            ],
        ),
        labelled(
            meta(
                "m2",
                "t2",
                "shop@example.com",
                "Half price kites",
                NOW - 2 * DAY,
            ),
            &[system_label::INBOX, system_label::CATEGORY_PROMOTIONS],
        ),
        labelled(
            meta(
                "m3",
                "t3",
                "ann@example.com",
                "Fern cuttings",
                NOW - 3 * DAY,
            ),
            &[system_label::INBOX, system_label::CATEGORY_PERSONAL],
        ),
        labelled(
            meta(
                "m4",
                "t3",
                "ann@example.com",
                "Re: Fern cuttings",
                NOW - DAY - DAY / 2,
            ),
            &[system_label::INBOX, system_label::CATEGORY_PERSONAL],
        ),
    ]
}

async fn harness() -> Harness {
    Harness::with(mail()).await
}

/// The thread ids a listing gave back, in order.
fn thread_ids(result: &Value) -> Vec<String> {
    result["conversations"]
        .as_array()
        .expect("conversations")
        .iter()
        .map(|c| c["thread_id"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn target(thread_id: &str) -> Value {
    json!({"account": ME, "thread_id": thread_id})
}

#[tokio::test]
async fn an_unknown_tool_says_so() {
    let h = harness().await;
    assert_eq!(
        h.run("fly_a_kite", json!({})).await,
        Err("There is no tool called fly_a_kite.".into())
    );
}

#[tokio::test]
async fn get_context_reports_the_screen_the_accounts_and_the_labels() {
    let h = harness().await;
    {
        let mut screen = h.desk.0.borrow_mut();
        screen.on_screen.mailbox = "Inbox".into();
        screen.on_screen.open = Some(OpenConversation {
            account_id: h.account_id,
            thread_id: "t1".into(),
            message_id: None,
            subject: "Kite plans".into(),
        });
        screen
            .settings
            .vips
            .insert("theo@example.com".into(), "Theo".into());
        screen
            .settings
            .account_names
            .insert(ME.into(), "Home".into());
    }

    let context = h.ok("get_context", json!({})).await;
    assert_eq!(context["mailbox_on_screen"], "Inbox");
    assert_eq!(context["vips"], json!(["theo@example.com"]));
    assert_eq!(context["open_conversation"]["thread_id"], "t1");
    assert_eq!(context["open_conversation"]["account"], ME);
    assert_eq!(
        context["accounts"],
        json!([{"email": ME, "name": "Home", "labels": ["Kites"]}])
    );
    assert_eq!(context["selected"], json!([]));
    assert!(
        context["now"].as_str().is_some_and(|s| s.len() > 8),
        "the model needs the time"
    );
}

#[tokio::test]
async fn list_mail_reads_a_mailbox_and_narrows_to_a_category() {
    let h = harness().await;

    let all = h.ok("list_mail", json!({"mailbox": "inbox"})).await;
    assert_eq!(all["count"], 3);
    assert_eq!(thread_ids(&all), ["t1", "t3", "t2"], "newest first");

    let promotions = h
        .ok(
            "list_mail",
            json!({"mailbox": "inbox", "category": "promotions"}),
        )
        .await;
    assert_eq!(thread_ids(&promotions), ["t2"]);
    let row = &promotions["conversations"][0];
    assert_eq!(row["account"], ME);
    assert_eq!(row["subject"], "Half price kites");
    assert_eq!(row["from_email"], "shop@example.com");
    assert_eq!(row["unread"], false);

    let unread = h
        .ok(
            "list_mail",
            json!({"mailbox": "inbox", "unread_only": true}),
        )
        .await;
    assert_eq!(thread_ids(&unread), ["t1"]);

    assert_eq!(
        h.run("list_mail", json!({"mailbox": "outbox"})).await,
        Err("Unknown mailbox outbox.".into())
    );
    assert_eq!(
        h.run("list_mail", json!({})).await,
        Err("`mailbox` is missing".into())
    );
    assert_eq!(
        h.run(
            "list_mail",
            json!({"mailbox": "inbox", "category": "nonsense"})
        )
        .await,
        Err("Unknown category nonsense.".into())
    );
}

#[tokio::test]
async fn list_mail_finds_a_label_by_name() {
    let h = harness().await;
    let listed = h
        .ok("list_mail", json!({"mailbox": "label", "label": "kites"}))
        .await;
    assert_eq!(listed["count"], 0, "no mail carries the label yet");
    assert_eq!(
        h.run("list_mail", json!({"mailbox": "label", "label": "Boats"}))
            .await,
        Err("There is no label called Boats.".into())
    );
}

#[tokio::test]
async fn search_mail_asks_gmail() {
    let h = harness().await;
    let found = h.ok("search_mail", json!({"query": "kite"})).await;
    let mut ids = thread_ids(&found);
    ids.sort();
    assert_eq!(ids, ["t1", "t2"], "the subject holds the word");
    assert_eq!(
        h.run("search_mail", json!({})).await,
        Err("`query` is missing".into())
    );
}

#[tokio::test]
async fn read_conversation_gives_every_message_with_its_body() {
    let h = harness().await;
    h.gmail.with(|i| {
        i.bodies.insert(
            "m3".into(),
            MessageBody {
                text: Some("Cuttings on Friday.".into()),
                ..MessageBody::default()
            },
        );
    });

    let read = h
        .ok(
            "read_conversation",
            json!({"account": ME, "thread_id": "t3"}),
        )
        .await;
    assert_eq!(read["account"], ME);
    assert_eq!(read["thread_id"], "t3");
    let messages = read["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["from"], "ann <ann@example.com>");
    assert_eq!(messages[0]["subject"], "Fern cuttings");
    assert_eq!(messages[0]["text"], "Cuttings on Friday.");
    assert_eq!(
        messages[1]["text"], "about Re: Fern cuttings",
        "the snippet stands in when Gmail has no body"
    );

    assert_eq!(
        h.run(
            "read_conversation",
            json!({"account": ME, "thread_id": "gone"})
        )
        .await,
        Err("That conversation was not found.".into())
    );
    assert_eq!(
        h.run(
            "read_conversation",
            json!({"account": "nobody@example.com", "thread_id": "t3"})
        )
        .await,
        Err("There is no account nobody@example.com.".into())
    );
}

#[tokio::test]
async fn organize_archives_and_reports_what_changed() {
    let h = harness().await;
    let done = h
        .ok(
            "organize",
            json!({"targets": [target("t1")], "action": "archive"}),
        )
        .await;
    assert_eq!(
        done,
        json!({"done": 1, "undo": "The user can press Ctrl+Z to undo this."})
    );
    assert!(!h.labels_of("m1").await.contains(&"INBOX".to_string()));
    assert_eq!(
        h.gmail.with(|s| s.remote_writes.clone()),
        ["modify m1 + -INBOX"]
    );
    let asked = h.asked();
    assert_eq!(asked.mail_changed.len(), 1, "the window redraws once");
    assert_eq!(
        asked.mail_changed[0].0,
        MailAction::Triage(TriageAction::Archive)
    );
    assert!(asked.questions.is_empty(), "archiving needs no approval");
}

#[tokio::test]
async fn organize_flags_one_message_of_a_thread() {
    let h = harness().await;
    h.ok(
        "organize",
        json!({
            "targets": [{"account": ME, "thread_id": "t3", "message_id": "m4"}],
            "action": "flag",
            "color": "blue",
        }),
    )
    .await;
    assert!(h.labels_of("m4").await.contains(&"STARRED".to_string()));
    assert!(
        !h.labels_of("m3").await.contains(&"STARRED".to_string()),
        "the other message of the thread is untouched"
    );
    assert_eq!(
        h.asked().mail_changed[0].0,
        MailAction::Flag(Some(FlagColor::Blue))
    );
}

#[tokio::test]
async fn organize_refuses_an_action_it_does_not_know() {
    let h = harness().await;
    assert_eq!(
        h.run(
            "organize",
            json!({"targets": [target("t1")], "action": "shred"})
        )
        .await,
        Err("Unknown action shred.".into())
    );
    assert_eq!(
        h.run("organize", json!({"action": "archive"})).await,
        Err("`targets` is missing".into())
    );
}

#[tokio::test]
async fn label_adds_and_removes_by_name() {
    let h = harness().await;
    let done = h
        .ok(
            "label",
            json!({"targets": [target("t1")], "add": ["Kites"]}),
        )
        .await;
    assert_eq!(done["done"], 1);
    assert!(h.labels_of("m1").await.contains(&"Label_kites".to_string()));

    h.ok(
        "label",
        json!({"targets": [target("t1")], "remove": ["Kites"]}),
    )
    .await;
    assert!(!h.labels_of("m1").await.contains(&"Label_kites".to_string()));
    assert!(h.asked().questions.is_empty(), "no label was made");
}

/// Dana's account has Kites; Sam's, the second, has no labels. Each holds
/// one inbox conversation.
async fn two_accounts() -> Harness {
    let theirs = labelled(
        meta("s1", "u1", "kim@example.com", "Kite club", NOW - DAY),
        &[system_label::INBOX],
    );
    Harness::with_second(mail(), vec![theirs]).await
}

/// One conversation in each account.
fn both() -> Value {
    json!([target("t1"), {"account": YOU, "thread_id": "u1"}])
}

/// Whether Sam's Gmail has a label by that name.
fn second_has(h: &Harness, name: &str) -> bool {
    let (_, gmail) = h.second.as_ref().expect("two accounts");
    gmail.with(|s| s.labels.iter().any(|l| l.name == name))
}

#[tokio::test]
async fn labelling_across_accounts_asks_once_before_making_a_label() {
    let h = two_accounts().await;
    let done = h
        .ok("label", json!({"targets": both(), "add": ["Kites"]}))
        .await;
    assert_eq!(done["done"], 2);
    let questions = h.asked().questions.clone();
    assert_eq!(questions.len(), 1, "{questions:?}");
    assert!(questions[0].contains("“Kites”"), "{}", questions[0]);
    assert!(questions[0].contains(YOU), "names Sam: {}", questions[0]);
    assert!(!questions[0].contains(ME), "Dana has Kites: {}", questions[0]);
    assert!(second_has(&h, "Kites"));
    assert!(h.labels_of("m1").await.contains(&"Label_kites".to_string()));
    let (second, _) = h.second.clone().expect("two accounts");
    let theirs = h.labels_in(second, "s1").await;
    assert!(
        theirs.iter().any(|l| l != system_label::INBOX),
        "Sam's mail carries the new label: {theirs:?}"
    );
}

#[tokio::test]
async fn declining_a_new_label_labels_only_where_it_exists() {
    let h = two_accounts().await;
    h.effects.asked.borrow_mut().approves = false;
    let done = h
        .ok("label", json!({"targets": both(), "add": ["Kites"]}))
        .await;
    assert_eq!(done["done"], 1);
    assert!(done["declined"].is_string(), "the model hears why: {done}");
    assert!(h.labels_of("m1").await.contains(&"Label_kites".to_string()));
    assert!(!second_has(&h, "Kites"), "Sam's account gets no new label");
    let (second, _) = h.second.clone().expect("two accounts");
    assert_eq!(h.labels_in(second, "s1").await, [system_label::INBOX]);

    assert_eq!(
        h.run("label", json!({"targets": both(), "add": ["Boats"]}))
            .await,
        Err("The user declined.".into()),
        "no account has Boats, so a no leaves nothing to label"
    );
}

#[tokio::test]
async fn without_ask_before_acting_a_missing_label_is_made() {
    let h = two_accounts().await;
    h.desk.0.borrow_mut().settings.ai.confirm_actions = false;
    h.effects.asked.borrow_mut().approves = false;
    let done = h
        .ok("label", json!({"targets": both(), "add": ["Kites"]}))
        .await;
    assert_eq!(done["done"], 2);
    assert!(h.asked().questions.is_empty());
    assert!(second_has(&h, "Kites"));
}

#[tokio::test]
async fn remind_me_takes_a_future_time_and_says_when_it_returns() {
    let h = harness().await;
    let later = chrono::Local::now() + chrono::Duration::days(2);
    let at = later.format("%Y-%m-%dT%H:%M").to_string();

    let done = h
        .ok("remind_me", json!({"targets": [target("t1")], "at": at}))
        .await;
    assert_eq!(done["done"], 1);
    assert!(done["returns"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(matches!(
        h.asked().mail_changed[0].0,
        MailAction::Remind { .. }
    ));

    assert_eq!(
        h.run(
            "remind_me",
            json!({"targets": [target("t1")], "at": "2020-01-01T09:00"})
        )
        .await,
        Err("That time is in the past.".into())
    );
    assert_eq!(
        h.run(
            "remind_me",
            json!({"targets": [target("t1")], "at": "next Tuesday"})
        )
        .await,
        Err("Could not read the time next Tuesday; use YYYY-MM-DDTHH:MM.".into())
    );
}

#[tokio::test]
async fn draft_email_opens_a_composer_and_never_sends() {
    let h = harness().await;
    let opened = h
        .ok(
            "draft_email",
            json!({"to": ["ann@example.com"], "subject": "Kites", "body": "Saturday?"}),
        )
        .await;
    assert_eq!(
        opened,
        json!({"opened": "A composer window shows the draft for the user to review."})
    );
    let asked = h.asked();
    assert_eq!(asked.sent.len(), 0);
    let draft = &asked.composed[0];
    assert_eq!(draft.to[0].email, "ann@example.com");
    assert_eq!(draft.subject, "Kites");
    assert_eq!(draft.markdown, "Saturday?");
    assert!(asked.questions.is_empty(), "a draft needs no approval");
}

#[tokio::test]
async fn draft_email_replies_in_the_thread() {
    let h = harness().await;
    h.ok(
        "draft_email",
        json!({
            "to": ["ann@example.com"],
            "body": "Friday works.",
            "reply_to": {"account": ME, "thread_id": "t3"},
        }),
    )
    .await;
    let asked = h.asked();
    let draft = &asked.composed[0];
    assert_eq!(draft.thread_id.as_deref(), Some("t3"));
    assert_eq!(
        draft.subject, "Re: Fern cuttings",
        "the parent already says Re:"
    );
    assert_eq!(draft.in_reply_to.as_deref(), Some("<m4@example.com>"));
    assert_eq!(draft.references, ["<m3@example.com>", "<m4@example.com>"]);
}

#[tokio::test]
async fn send_email_goes_out_once_the_user_approves() {
    let h = harness().await;
    let sent = h
        .ok(
            "send_email",
            json!({"to": ["ann@example.com"], "subject": "Kites", "body": "Saturday?"}),
        )
        .await;
    assert_eq!(sent, json!({"sent": true, "undo_seconds": 10}));
    let asked = h.asked();
    assert_eq!(asked.questions, ["Send “Kites” to ann@example.com?"]);
    assert_eq!(asked.sent.len(), 1);
}

#[tokio::test]
async fn a_declined_send_reports_the_decline_and_changes_nothing() {
    let h = harness().await;
    h.effects.asked.borrow_mut().approves = false;

    assert_eq!(
        h.run(
            "send_email",
            json!({"to": ["ann@example.com"], "subject": "Kites", "body": "Saturday?"})
        )
        .await,
        Err("The user declined.".into())
    );
    let asked = h.asked();
    assert_eq!(asked.questions.len(), 1, "the user was asked once");
    assert!(asked.sent.is_empty(), "nothing went out");
    assert!(asked.composed.is_empty());
}

#[tokio::test]
async fn send_email_checks_the_recipients_before_asking() {
    let h = harness().await;
    assert_eq!(
        h.run("send_email", json!({"subject": "Kites", "body": "Hi"}))
            .await,
        Err("Add at least one recipient.".into())
    );
    assert!(h.asked().questions.is_empty());
}

#[tokio::test]
async fn approval_is_skipped_when_the_user_turned_it_off() {
    let h = harness().await;
    h.desk.0.borrow_mut().settings.ai.confirm_actions = false;
    h.effects.asked.borrow_mut().approves = false;

    let sent = h
        .ok(
            "send_email",
            json!({"to": ["ann@example.com"], "body": "Hi"}),
        )
        .await;
    assert_eq!(sent["sent"], true);
    assert!(h.asked().questions.is_empty());
}

#[tokio::test]
async fn the_automatic_reply_reads_back_what_it_stored() {
    let h = harness().await;
    let off = h.ok("get_automatic_reply", json!({"account": ME})).await;
    assert_eq!(off["enabled"], false);

    let set = h
        .ok(
            "set_automatic_reply",
            json!({
                "account": ME,
                "enabled": true,
                "subject": "Away",
                "message": "Back on Monday.",
                "first_day": "2026-03-01",
                "last_day": "2026-03-03",
            }),
        )
        .await;
    assert_eq!(set["enabled"], true);
    assert_eq!(set["subject"], "Away");
    assert_eq!(set["message"], "Back on Monday.");
    assert_eq!(set["first_day"], "2026-03-01");
    assert_eq!(set["last_day"], "2026-03-03");
    assert!(h.asked().questions[0].starts_with("Turn on the automatic reply for"));

    let read = h.ok("get_automatic_reply", json!({"account": ME})).await;
    assert_eq!(read, set);

    assert_eq!(
        h.run(
            "set_automatic_reply",
            json!({"account": ME, "first_day": "the first of March"})
        )
        .await,
        Err("Could not read the date the first of March; use YYYY-MM-DD.".into())
    );
}

#[tokio::test]
async fn gmail_settings_ask_for_the_permission_instead_of_failing() {
    let h = harness().await;
    h.gmail.withhold(mailrs_gmail::SETTINGS_SCOPE);

    let answer = h.run("get_automatic_reply", json!({"account": ME})).await;
    assert_eq!(
        answer,
        Err(format!(
            "Penguin Mail needs permission to change Gmail settings for {ME}. \
             The user was asked to grant it; try again once they have."
        ))
    );
    assert_eq!(
        h.asked().permission_asked,
        [(h.account_id, Permission::Settings)]
    );
}

#[tokio::test]
async fn rules_are_made_listed_and_deleted() {
    let h = harness().await;
    assert_eq!(
        h.ok("list_rules", json!({"account": ME})).await,
        json!({"rules": []})
    );

    let made = h
        .ok(
            "create_rule",
            json!({"account": ME, "from": "shop@example.com", "label": "Kites", "skip_inbox": true}),
        )
        .await;
    let id = made["created"].as_str().expect("a rule id").to_string();
    assert_eq!(
        h.asked().questions,
        [format!(
            "Create a Gmail rule for {ME}: From shop@example.com → skip the inbox, apply kites?"
        )]
    );

    let listed = h.ok("list_rules", json!({"account": ME})).await;
    assert_eq!(listed["rules"][0]["id"], id.as_str());
    assert_eq!(listed["rules"][0]["when"], "From shop@example.com");

    assert_eq!(
        h.ok("delete_rule", json!({"account": ME, "id": id})).await,
        json!({"deleted": true})
    );
    assert_eq!(
        h.ok("list_rules", json!({"account": ME})).await,
        json!({"rules": []})
    );
}

#[tokio::test]
async fn change_setting_names_the_change_it_made() {
    let h = harness().await;
    let changed = h
        .ok(
            "change_setting",
            json!({"name": "text_size", "value": "larger"}),
        )
        .await;
    assert_eq!(changed, json!({"changed": "text_size", "value": "larger"}));
    assert_eq!(h.asked().changes, [Change::TextSize(TextSize::Larger)]);

    assert_eq!(
        h.run("change_setting", json!({"name": "wallpaper", "value": 1}))
            .await,
        Err("wallpaper is not a setting the assistant can change.".into())
    );
    assert!(
        h.run(
            "change_setting",
            json!({"name": "text_size", "value": "enormous"})
        )
        .await
        .is_err_and(|e| e.starts_with("\"enormous\" is not a valid value for text_size:")),
    );
    assert_eq!(h.asked().changes.len(), 1, "a bad value changes nothing");
}

#[tokio::test]
async fn get_settings_reports_every_value_with_its_choices() {
    let h = harness().await;
    let settings = h.ok("get_settings", json!({})).await;
    assert_eq!(settings["settings"]["text_size"], "normal");
    assert_eq!(settings["settings"]["threading"], true);
    assert!(
        settings["choices"]["undo_send"]
            .as_array()
            .is_some_and(|c| !c.is_empty())
    );
}

#[tokio::test]
async fn vip_adds_and_removes_an_address() {
    let h = harness().await;
    let added = h
        .ok("vip", json!({"email": "Theo@Example.com", "name": "Theo"}))
        .await;
    assert_eq!(added, json!({"email": "theo@example.com", "vip": true}));
    assert_eq!(
        h.asked().changes,
        [Change::SetVip {
            email: "theo@example.com".into(),
            name: "Theo".into(),
            add: true,
        }]
    );

    let removed = h
        .ok("vip", json!({"email": "theo@example.com", "add": false}))
        .await;
    assert_eq!(removed, json!({"email": "theo@example.com", "vip": false}));
}

#[tokio::test]
async fn categorize_sender_asks_then_moves_their_mail_and_sorts_the_rest() {
    let h = harness().await;
    let done = h
        .ok(
            "categorize_sender",
            json!({"account": ME, "email": "shop@example.com", "name": "The Kite Shop", "category": "social"}),
        )
        .await;
    assert_eq!(
        done,
        json!({"sender": "shop@example.com", "category": "social"})
    );
    assert_eq!(
        h.asked().questions,
        [format!(
            "Move mail from The Kite Shop to Social in {ME}, and add a Gmail rule for their future mail?"
        )]
    );
    let categorized = h.categorized().await;
    assert_eq!(categorized.len(), 1);
    assert_eq!(
        categorized[0].sorted.as_ref().ok(),
        Some(&Permitted::Done(()))
    );

    let labels = h.labels_of("m2").await;
    assert!(labels.iter().any(|l| l == system_label::CATEGORY_SOCIAL));
    assert!(
        !labels
            .iter()
            .any(|l| l == system_label::CATEGORY_PROMOTIONS)
    );
    let rules = h.gmail.with(|s| s.filters.clone());
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].criteria.from.as_deref(), Some("shop@example.com"));
    assert_eq!(
        rules[0].action.add_label_ids,
        [system_label::CATEGORY_SOCIAL]
    );
}

#[tokio::test]
async fn a_declined_categorize_changes_nothing() {
    let h = harness().await;
    h.effects.asked.borrow_mut().approves = false;
    assert_eq!(
        h.run(
            "categorize_sender",
            json!({"account": ME, "email": "shop@example.com", "category": "social"})
        )
        .await,
        Err("The user declined.".into())
    );
    assert!(h.categorized().await.is_empty());
    assert!(
        h.labels_of("m2")
            .await
            .iter()
            .any(|l| l == system_label::CATEGORY_PROMOTIONS)
    );
    assert!(h.gmail.with(|s| s.filters.is_empty()));
}

#[tokio::test]
async fn dismiss_follow_up_takes_the_thread_off_the_list() {
    let h = harness().await;
    let done = h
        .ok(
            "dismiss_follow_up",
            json!({"account": ME, "thread_id": "t1"}),
        )
        .await;
    assert_eq!(done, json!({"dismissed": "t1"}));
    assert_eq!(
        h.asked().mail_changed,
        [(
            MailAction::DismissFollowUp,
            Outcome {
                done: vec![mailrs_domain::Target::thread(h.account_id, "t1")],
                failed: vec![],
            }
        )],
        "the window redraws what the action changed"
    );

    let account_id = h.account_id;
    let stored: i64 = h
        .db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM follow_up_dismissals WHERE account_id = ?1 AND thread_id = ?2",
                rusqlite::params![account_id, "t1"],
                |row| row.get(0),
            )?)
        })
        .await
        .expect("the store answers");
    assert_eq!(stored, 1, "the store remembers the dismissal");
}

#[tokio::test]
async fn open_conversation_shows_the_thread() {
    let h = harness().await;
    assert_eq!(
        h.ok(
            "open_conversation",
            json!({"account": ME, "thread_id": "t2"})
        )
        .await,
        json!({"opened": true})
    );
    let asked = h.asked();
    assert_eq!(asked.opened[0].id, "t2");
    assert_eq!(asked.opened[0].account_id, h.account_id);
}

#[tokio::test]
async fn hide_my_email_makes_an_address_and_copies_it() {
    let h = harness().await;
    let made = h
        .ok(
            "create_hidden_address",
            json!({"account": ME, "note": "kite shop"}),
        )
        .await;
    assert_eq!(made["copied"], true);
    let address = made["address"].as_str().expect("an address").to_string();
    assert!(hidden::is_alias(&address), "{address}");
    assert_eq!(h.asked().copied, [address.as_str()]);
    let filters = h.gmail.with(|s| s.filters.clone());
    assert_eq!(filters.len(), 1, "one filter labels the alias's mail");
    assert!(
        filters[0].criteria.to.as_deref() == Some(address.as_str()),
        "{filters:?}"
    );

    let listed = h.ok("list_hidden_addresses", json!({})).await;
    assert_eq!(listed["addresses"][0]["address"], address.as_str());
    assert_eq!(listed["addresses"][0]["note"], "kite shop");
    assert_eq!(listed["addresses"][0]["active"], true);

    let turned_off = h
        .ok(
            "set_hidden_address",
            json!({"address": address, "active": false}),
        )
        .await;
    assert_eq!(turned_off, json!({"address": address, "active": false}));
    assert_eq!(
        h.gmail.with(|s| s.filters.len()),
        2,
        "a second filter sends the alias's mail to the Trash"
    );
    let kept = h.desk.0.borrow().settings.hidden_addresses.clone();
    assert!(kept[0].trash_filter.is_some() && !kept[0].active);

    assert_eq!(
        h.run(
            "set_hidden_address",
            json!({"address": "nobody@example.com", "active": true})
        )
        .await,
        Err("nobody@example.com is not a Hide My Email address.".into())
    );
}

#[tokio::test]
async fn create_label_makes_one_in_gmail() {
    let h = harness().await;
    let made = h
        .ok("create_label", json!({"account": ME, "name": "Boats"}))
        .await;
    assert_eq!(made, json!({"account": ME, "created": "Boats"}));
    assert!(h.gmail.with(|s| s.labels.iter().any(|l| l.name == "Boats")));
}

#[tokio::test]
async fn block_sender_asks_then_files_a_rule() {
    let h = harness().await;
    let blocked = h
        .ok(
            "block_sender",
            json!({"account": ME, "email": "shop@example.com"}),
        )
        .await;
    assert_eq!(blocked, json!({"blocked": "shop@example.com"}));
    assert_eq!(
        h.asked().questions,
        ["Block shop@example.com? Their future mail goes straight to the Trash."]
    );
    assert_eq!(h.gmail.with(|s| s.filters.len()), 1);
}

#[tokio::test]
async fn create_smart_mailbox_saves_its_gmail_query() {
    let h = harness().await;
    let made = h
        .ok(
            "create_smart_mailbox",
            json!({
                "name": "From Ann",
                "conditions": [{"field": "from", "value": "ann@example.com"}],
            }),
        )
        .await;
    assert_eq!(made["created"], "From Ann");
    assert_eq!(made["gmail_query"], "from:ann@example.com");
    assert!(matches!(h.asked().changes[0], Change::SaveSmartMailbox(_)));

    assert_eq!(
        h.run(
            "create_smart_mailbox",
            json!({"name": "Empty", "conditions": []})
        )
        .await,
        Err("Give at least one condition with a value.".into())
    );
}

#[tokio::test]
async fn set_signature_names_the_account() {
    let h = harness().await;
    assert_eq!(
        h.ok("set_signature", json!({"account": ME, "text": "Dana"}))
            .await,
        json!({"signature_set_for": ME})
    );
    assert_eq!(
        h.asked().changes,
        [Change::Signature {
            email: ME.into(),
            text: "Dana".into(),
        }]
    );
}

/// An input each tool accepts in the fixture mailbox, or `None` for a tool
/// that needs fixtures of its own (attachments, events, invitations, the
/// delete permission) or an earlier call's answer. Those have tests in
/// `tests/mail.rs`, `tests/calendar.rs` and the end of the table test.
fn sample(name: &str, later: &str) -> Option<Value> {
    Some(match name {
        "get_context" | "get_settings" | "list_hidden_addresses" | "list_templates" => json!({}),
        "list_mail" => json!({"mailbox": "inbox"}),
        "search_mail" => json!({"query": "kite"}),
        "read_conversation" | "open_conversation" | "dismiss_follow_up" => {
            json!({"account": ME, "thread_id": "t1"})
        }
        "organize" => json!({"targets": [target("t2")], "action": "mark_read"}),
        "label" => json!({"targets": [target("t2")], "add": ["Kites"]}),
        "remind_me" => json!({"targets": [target("t2")], "at": later}),
        "draft_email" | "send_email" => json!({"to": ["ann@example.com"], "body": "Hi"}),
        "block_sender" => json!({"account": ME, "email": "spam@example.com"}),
        "get_automatic_reply" | "list_rules" => json!({"account": ME}),
        "set_automatic_reply" => json!({"account": ME, "enabled": false}),
        "create_rule" => json!({"account": ME, "from": "ann@example.com", "mark_read": true}),
        "create_label" => json!({"account": ME, "name": "Boats"}),
        "change_setting" => json!({"name": "threading", "value": false}),
        "set_signature" => json!({"account": ME, "text": "Dana"}),
        "vip" => json!({"email": "theo@example.com", "name": "Theo"}),
        "create_smart_mailbox" => {
            json!({"name": "Ann", "conditions": [{"field": "from", "value": "ann"}]})
        }
        "categorize_sender" => {
            json!({"account": ME, "email": "shop@example.com", "category": "promotions"})
        }
        "mute" => json!({"targets": [target("t3")]}),
        "find_contact" => json!({"query": "ann"}),
        "send_later" => json!({"to": ["ann@example.com"], "body": "Hi", "at": later}),
        "list_events" => json!({"from": "2030-03-11", "to": "2030-03-12"}),
        "find_free_time" => json!({"from": "2030-03-11", "to": "2030-03-12", "minutes": 30}),
        "delete_rule"
        | "create_hidden_address"
        | "set_hidden_address"
        | "delete_forever"
        | "insert_template"
        | "unsubscribe"
        | "read_attachment"
        | "create_event"
        | "update_event"
        | "delete_event"
        | "answer_invitation" => return None,
        other => panic!("{other} has no sample input; add one to `sample`"),
    })
}

fn later() -> String {
    (chrono::Local::now() + chrono::Duration::days(1))
        .format("%Y-%m-%dT%H:%M")
        .to_string()
}

async fn with_a_reply_to_change() -> Harness {
    let h = harness().await;
    h.gmail.with(|i| {
        i.vacation = Vacation {
            subject: "Away".into(),
            ..Vacation::default()
        }
    });
    h
}

/// Every tool in the catalog with an input it accepts, none of them needing
/// a window. A tool that runs at once must not ask on the way.
#[tokio::test]
async fn every_tool_answers_without_a_window() {
    let h = with_a_reply_to_change().await;
    let later = later();
    for tool in catalog::<Connected>() {
        let Some(input) = sample(tool.name, &later) else {
            continue;
        };
        let asked_before = h.asked().questions.len();
        let answer = h.run(tool.name, input).await;
        assert!(answer.is_ok(), "{} answered {answer:?}", tool.name);
        if matches!(tool.run, Run::Now(_)) {
            assert_eq!(
                h.asked().questions.len(),
                asked_before,
                "{} runs at once but asked first",
                tool.name
            );
        }
    }

    // The two left over need an earlier call's output, and one of them
    // shows the permission path.
    let rule = h.ok("list_rules", json!({"account": ME})).await;
    let id = rule["rules"][0]["id"].as_str().expect("a rule to delete");
    assert!(
        h.run("delete_rule", json!({"account": ME, "id": id}))
            .await
            .is_ok()
    );
    h.gmail.withhold(mailrs_gmail::SETTINGS_SCOPE);
    assert!(
        h.run("create_hidden_address", json!({"account": ME}))
            .await
            .is_err_and(|e| e.contains("needs permission"))
    );
}

/// A tool that asks first and hears Don't Allow makes no change: nothing
/// sent, scheduled, saved or sorted, and Gmail's rules and automatic reply
/// stay as they were.
#[tokio::test]
async fn a_declined_question_changes_nothing() {
    let h = with_a_reply_to_change().await;
    h.effects.asked.borrow_mut().approves = false;
    let later = later();
    let rules = h.ok("list_rules", json!({"account": ME})).await;
    let reply = h.ok("get_automatic_reply", json!({"account": ME})).await;
    for tool in catalog::<Connected>() {
        let (Run::AsksFirst(_), Some(input)) = (&tool.run, sample(tool.name, &later)) else {
            continue;
        };
        let asked_before = h.asked().questions.len();
        let answer = h.run(tool.name, input).await;
        if h.asked().questions.len() > asked_before {
            assert_eq!(answer, Err("The user declined.".into()), "{}", tool.name);
        }
    }
    {
        let asked = h.asked();
        assert!(!asked.questions.is_empty(), "the samples include questions");
        assert!(asked.sent.is_empty() && asked.scheduled.is_empty());
        assert!(asked.changes.is_empty() && asked.left.is_empty());
    }
    assert!(h.categorized().await.is_empty());
    assert_eq!(h.ok("list_rules", json!({"account": ME})).await, rules);
    assert_eq!(
        h.ok("get_automatic_reply", json!({"account": ME})).await,
        reply
    );
}
