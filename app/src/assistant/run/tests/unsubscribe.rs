//! The newsletters list, and leaving three lists in one call: one that
//! takes a one-click request, one that only lets go through its page,
//! and one that wants a mail.

use mailrs_domain::{Address, EpochMillis, MessageMeta, system_label};
use serde_json::{Value, json};

use super::super::fake::{Harness, ME, labelled, meta};
use crate::unsubscribe_page::PageForm;

const DAY: i64 = 24 * 60 * 60 * 1000;

/// Shop News lets go only through this page.
const PAGE: &str = "https://shop.example/leave/9";

/// The newsletters sit in the last few days of the real clock, because
/// the listing counts ninety days back from it rather than from the
/// hour the fixture mail is pinned to.
fn days_ago(days: i64) -> EpochMillis {
    chrono::Local::now().timestamp_millis() - days * DAY
}

/// One newsletter's newest message, from a sender with a name, in the
/// bulk category it arrived in. The header it carries is the caller's to
/// put on.
fn from(id: &str, thread: &str, name: &str, email: &str, days: i64, category: &str) -> MessageMeta {
    MessageMeta {
        from: Some(Address {
            name: Some(name.into()),
            email: email.into(),
        }),
        ..labelled(
            meta(id, thread, email, "This week", days_ago(days)),
            &[system_label::INBOX, category],
        )
    }
}

/// Three lists, one of each way out.
fn newsletters() -> Vec<MessageMeta> {
    vec![
        MessageMeta {
            list_unsubscribe: Some("<https://trail.example/u/1>".into()),
            one_click: true,
            ..from(
                "n1",
                "tn1",
                "Trail Notes",
                "news@trail.example",
                1,
                system_label::CATEGORY_UPDATES,
            )
        },
        MessageMeta {
            list_unsubscribe: Some(format!("<{PAGE}>")),
            ..from(
                "n2",
                "tn2",
                "Shop News",
                "hello@shop.example",
                2,
                system_label::CATEGORY_PROMOTIONS,
            )
        },
        MessageMeta {
            list_unsubscribe: Some("<mailto:leave@forum.example?subject=bye>".into()),
            ..from(
                "n3",
                "tn3",
                "Old Forum",
                "digest@forum.example",
                3,
                system_label::CATEGORY_FORUMS,
            )
        },
    ]
}

/// The shop's page: one form, an empty address field and a button that
/// reads as leaving, which is what the rules fill in and press.
fn shop_page() -> PageForm {
    serde_json::from_value(json!({
        "url": PAGE,
        "title": "Leave Shop News",
        "text": "Type the address you get Shop News at.",
        "forms": [{
            "id": 0,
            "fields": [{"id": 1, "kind": "email", "label": "Email address", "required": true}],
            "buttons": [{"id": 2, "label": "Unsubscribe"}]
        }],
    }))
    .expect("a PageForm")
}

/// A harness whose store holds the three newsletters, with the shop's
/// page behind the hidden view and a page after the submission that says
/// the request worked.
async fn with_three() -> Harness {
    let h = Harness::with(newsletters()).await;
    h.serve(PAGE, shop_page());
    h.after_submitting("You have been unsubscribed from Shop News.");
    h
}

fn conversation(thread_id: &str) -> Value {
    json!({"account": ME, "thread_id": thread_id})
}

/// What each list came back as, in the order the call named them.
fn outcomes(answer: &Value) -> Vec<(String, String)> {
    answer["lists"]
        .as_array()
        .expect("lists")
        .iter()
        .map(|list| {
            (
                list["name"].as_str().unwrap_or_default().to_string(),
                list["outcome"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_newsletters_come_back_newest_first_and_narrow_to_a_query() {
    let h = with_three().await;

    let listed = h.ok("list_newsletters", json!({})).await;
    let lists = listed["newsletters"].as_array().expect("newsletters");
    let named: Vec<&str> = lists.iter().map(|l| l["name"].as_str().unwrap()).collect();
    assert_eq!(named, ["Trail Notes", "Shop News", "Old Forum"]);
    let ways: Vec<&str> = lists
        .iter()
        .map(|l| l["way_out"].as_str().unwrap())
        .collect();
    assert_eq!(ways, ["one_click", "page", "email"]);
    assert_eq!(lists[1]["email"], "hello@shop.example");
    assert_eq!(lists[1]["messages"], 1);
    assert_eq!(lists[1]["account"], ME);
    assert_eq!(
        lists[1]["thread_id"], "tn2",
        "the thread unsubscribe is called with"
    );

    let narrowed = h.ok("list_newsletters", json!({"query": "shop"})).await;
    let lists = narrowed["newsletters"].as_array().expect("newsletters");
    assert_eq!(lists.len(), 1, "{lists:?}");
    assert_eq!(lists[0]["name"], "Shop News");
}

#[tokio::test]
async fn three_lists_leave_in_one_call_once_the_dialog_says_yes() {
    let h = with_three().await;

    let done = h
        .ok(
            "unsubscribe",
            json!({"conversations": [
                conversation("tn1"),
                conversation("tn2"),
                conversation("tn3"),
            ]}),
        )
        .await;

    assert_eq!(
        outcomes(&done),
        [
            ("Trail Notes".to_string(), "done".to_string()),
            ("Shop News".to_string(), "done".to_string()),
            ("Old Forum".to_string(), "done".to_string()),
        ]
    );
    assert_eq!(
        h.one_click.posted(),
        ["https://trail.example/u/1"],
        "the one-click list hears from Gmail"
    );
    assert_eq!(
        h.asked().requests,
        [(
            h.account_id,
            "leave@forum.example".to_string(),
            "bye".to_string(),
            "unsubscribe".to_string(),
        )],
        "the window sends the forum's request from the account"
    );
    let submitted = h.submissions();
    assert_eq!(submitted.len(), 1, "{submitted:?}");
    assert_eq!(submitted[0].fill, [(1, ME.to_string())]);
    assert_eq!(submitted[0].press, 2);
    assert_eq!(
        h.typed(),
        [ME],
        "the page is typed one address, the owner's"
    );

    assert_eq!(
        h.asked().lists_asked,
        [[
            "Trail Notes: ask the sender to take you off the list".to_string(),
            "Shop News: press “Unsubscribe” on shop.example with d…@example.com".to_string(),
            "Old Forum: send a request from d…@example.com".to_string(),
        ]],
        "the dialog names the button and the address before anything goes in"
    );
    assert!(
        h.asked().questions.is_empty(),
        "the dialog is the question, so the pane shows no card as well"
    );
}

#[tokio::test]
async fn a_refused_dialog_leaves_every_list_alone() {
    let h = with_three().await;
    h.effects.asked.borrow_mut().approves_lists = false;

    let answer = h
        .ok(
            "unsubscribe",
            json!({"conversations": [
                conversation("tn1"),
                conversation("tn2"),
                conversation("tn3"),
            ]}),
        )
        .await;

    let said: Vec<String> = outcomes(&answer).into_iter().map(|(_, o)| o).collect();
    assert_eq!(said, ["declined", "declined", "declined"]);
    assert!(h.submissions().is_empty(), "the page was read, not pressed");
    assert!(h.one_click.posted().is_empty());
    assert!(h.asked().requests.is_empty(), "no request went out");
    assert_eq!(
        h.asked().lists_asked.len(),
        1,
        "it asked once and took the answer"
    );
}

#[tokio::test]
async fn more_than_twenty_lists_are_refused_before_a_page_loads() {
    let h = with_three().await;
    let conversations: Vec<Value> = (0..21).map(|_| conversation("tn2")).collect();

    let refused = h
        .run("unsubscribe", json!({"conversations": conversations}))
        .await;

    assert_eq!(
        refused,
        Err(
            "unsubscribe leaves at most 20 lists at a time, and that call named 21. Ask about the rest afterwards."
                .into()
        )
    );
    assert!(
        h.effects.browser.borrow().is_none(),
        "nothing was loaded, so no sender heard about it"
    );
    assert!(h.asked().lists_asked.is_empty());
}

#[tokio::test]
async fn a_conversation_with_no_way_out_says_so_and_stops_nothing_else() {
    let h = with_three().await;
    let plain = labelled(
        meta("m9", "t9", "theo@example.com", "Kites", days_ago(1)),
        &[system_label::INBOX],
    );
    h.gmail.seed(plain);

    let answer = h
        .ok(
            "unsubscribe",
            json!({"conversations": [conversation("t9"), conversation("tn1")]}),
        )
        .await;

    let lists = answer["lists"].as_array().expect("lists");
    assert_eq!(lists[0]["outcome"], "failed");
    assert_eq!(
        lists[0]["reason"],
        "That conversation has no unsubscribe link Penguin Mail can use."
    );
    assert_eq!(lists[1]["outcome"], "done");
    assert_eq!(
        h.asked().lists_asked,
        [["Trail Notes: ask the sender to take you off the list".to_string()]],
        "the dialog is asked only about the list there is a way out of"
    );
}
