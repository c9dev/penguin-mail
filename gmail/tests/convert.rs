use mailrs_domain::Address;
use mailrs_gmail::HistoryChange;
use mailrs_gmail::address::{parse_address_list, parse_address_list_keeping_invalid};
use mailrs_gmail::convert::{history_page, message_meta, unescape_snippet};
use mailrs_gmail::model::{HistoryList, Message};

fn addr(name: Option<&str>, email: &str) -> Address {
    Address {
        name: name.map(str::to_string),
        email: email.into(),
    }
}

#[test]
fn parses_a_bare_address() {
    assert_eq!(
        parse_address_list("bob@example.com"),
        vec![addr(None, "bob@example.com")]
    );
}

#[test]
fn parses_a_named_address() {
    assert_eq!(
        parse_address_list("Ann Lee <ann@example.com>"),
        vec![addr(Some("Ann Lee"), "ann@example.com")]
    );
}

#[test]
fn quoted_names_may_contain_commas() {
    assert_eq!(
        parse_address_list(r#""Lee, Ann" <ann@example.com>, bob@example.com"#),
        vec![
            addr(Some("Lee, Ann"), "ann@example.com"),
            addr(None, "bob@example.com")
        ]
    );
}

#[test]
fn escaped_quotes_are_unescaped() {
    assert_eq!(
        parse_address_list(r#""Ann \"The Boss\" Lee" <ann@example.com>"#),
        vec![addr(Some(r#"Ann "The Boss" Lee"#), "ann@example.com")]
    );
}

#[test]
fn group_syntax_and_empty_input_yield_nothing() {
    assert!(parse_address_list("undisclosed-recipients:;").is_empty());
    assert!(parse_address_list("").is_empty());
}

#[test]
fn snippet_entities_are_decoded() {
    assert_eq!(
        unescape_snippet("Tom &amp; Jerry&#39;s &lt;show&gt; &#x41; &bogus; & more"),
        "Tom & Jerry's <show> A &bogus; & more"
    );
}

#[test]
fn metadata_converts_to_domain() {
    let msg: Message = serde_json::from_str(
        r#"{
          "id":"m1","threadId":"t1","labelIds":["INBOX","UNREAD"],"snippet":"Hi &amp; bye",
          "internalDate":"1700000000000","sizeEstimate":2048,
          "payload":{"mimeType":"multipart/mixed","headers":[
            {"name":"From","value":"Ann Lee <ann@example.com>"},
            {"name":"To","value":"me@example.com, Bob <bob@example.com>"},
            {"name":"Subject","value":"Hello"},
            {"name":"Message-Id","value":"<abc@example.com>"}
          ]}
        }"#,
    )
    .unwrap();
    let meta = message_meta(&msg, 7);
    assert_eq!(meta.account_id, 7);
    assert_eq!(meta.from, Some(addr(Some("Ann Lee"), "ann@example.com")));
    assert_eq!(meta.to.len(), 2);
    assert!(meta.cc.is_empty());
    assert_eq!(meta.subject, "Hello");
    assert_eq!(meta.rfc822_msgid.as_deref(), Some("<abc@example.com>"));
    assert_eq!(meta.snippet, "Hi & bye");
    assert_eq!(meta.date, 1_700_000_000_000);
    assert!(meta.has_attachments);
    assert!(meta.is_unread());
}

#[test]
fn message_without_payload_still_converts() {
    let msg: Message = serde_json::from_str(r#"{"id":"m1","threadId":"t1"}"#).unwrap();
    let meta = message_meta(&msg, 1);
    assert_eq!(meta.subject, "");
    assert!(meta.from.is_none());
    assert!(!meta.has_attachments);
}

#[test]
fn history_flattens_in_order() {
    let list: HistoryList = serde_json::from_str(
        r#"{"history":[
            {"id":"1","messagesAdded":[{"message":{"id":"a","threadId":"ta"}}]},
            {"id":"2","labelsAdded":[{"message":{"id":"a","threadId":"ta"},"labelIds":["STARRED"]}]},
            {"id":"3","labelsRemoved":[{"message":{"id":"a","threadId":"ta"},"labelIds":["UNREAD"]}]},
            {"id":"4","messagesDeleted":[{"message":{"id":"b","threadId":"tb"}}]}
          ],"historyId":"50","nextPageToken":"p2"}"#,
    )
    .unwrap();
    let page = history_page(list);
    assert_eq!(page.history_id, 50);
    assert_eq!(page.next_page_token.as_deref(), Some("p2"));
    assert_eq!(
        page.changes,
        vec![
            HistoryChange::MessageAdded {
                id: "a".into(),
                thread_id: "ta".into()
            },
            HistoryChange::LabelsAdded {
                id: "a".into(),
                thread_id: "ta".into(),
                label_ids: vec!["STARRED".into()]
            },
            HistoryChange::LabelsRemoved {
                id: "a".into(),
                thread_id: "ta".into(),
                label_ids: vec!["UNREAD".into()]
            },
            HistoryChange::MessageDeleted {
                id: "b".into(),
                thread_id: "tb".into()
            },
        ]
    );
    assert_eq!(page.changes[3].message_id(), "b");
}

#[test]
fn the_lenient_parser_keeps_mistakes() {
    assert_eq!(
        parse_address_list_keeping_invalid("ann@example.com, not an address, "),
        vec![addr(None, "ann@example.com"), addr(None, "not an address")]
    );
}
