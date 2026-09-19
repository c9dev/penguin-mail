use mailrs_gmail::model::{HistoryList, LabelList, Message, MessagePage, Profile, Thread};

#[test]
fn profile_parses_string_encoded_history_id() {
    let p: Profile = serde_json::from_str(
        r#"{"emailAddress":"me@example.com","messagesTotal":10,"threadsTotal":5,"historyId":"123456"}"#,
    )
    .unwrap();
    assert_eq!(p.email_address, "me@example.com");
    assert_eq!(p.history_id, 123456);
}

#[test]
fn empty_message_list_has_no_messages() {
    let page: MessagePage = serde_json::from_str(r#"{"resultSizeEstimate":0}"#).unwrap();
    assert!(page.messages.is_empty());
    assert!(page.next_page_token.is_none());
}

#[test]
fn metadata_message_parses_headers_and_dates() {
    let m: Message = serde_json::from_str(
        r#"{
          "id":"18c1","threadId":"18c0","labelIds":["INBOX","UNREAD"],"snippet":"Hi",
          "historyId":"999","internalDate":"1700000000000","sizeEstimate":2048,
          "payload":{"mimeType":"multipart/mixed","headers":[{"name":"Subject","value":"Hello"}]}
        }"#,
    )
    .unwrap();
    assert_eq!(m.internal_date, Some(1_700_000_000_000));
    assert_eq!(m.size_estimate, 2048);
    let payload = m.payload.unwrap();
    assert_eq!(payload.mime_type, "multipart/mixed");
    assert_eq!(payload.headers[0].value, "Hello");
}

#[test]
fn history_list_parses_every_change_kind() {
    let h: HistoryList = serde_json::from_str(
        r#"{
          "history":[
            {"id":"1","messages":[],"messagesAdded":[{"message":{"id":"a","threadId":"ta","labelIds":["INBOX"]}}]},
            {"id":"2","labelsRemoved":[{"message":{"id":"a","threadId":"ta"},"labelIds":["UNREAD"]}]}
          ],
          "historyId":"42"
        }"#,
    )
    .unwrap();
    assert_eq!(h.history_id, 42);
    assert_eq!(h.history[0].messages_added[0].message.id, "a");
    assert_eq!(
        h.history[1].labels_removed[0].label_ids,
        vec!["UNREAD".to_string()]
    );
}

#[test]
fn labels_and_threads_parse() {
    let l: LabelList = serde_json::from_str(
        r#"{"labels":[{"id":"INBOX","name":"INBOX","type":"system"},{"id":"Label_1","name":"Work","type":"user"}]}"#,
    )
    .unwrap();
    assert_eq!(l.labels[1].kind.as_deref(), Some("user"));
    let t: Thread =
        serde_json::from_str(r#"{"id":"t","messages":[{"id":"a","threadId":"t"}]}"#).unwrap();
    assert_eq!(t.messages.len(), 1);
}
