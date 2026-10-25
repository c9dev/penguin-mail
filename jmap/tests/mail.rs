//! The mail calls' requests, as wiremock receives them, and their answers.

mod common;

use mailrs_jmap::{EmailQuery, MailboxWrite, Submission};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::{client, session_on};

async fn answering(server: &MockServer, answer: Value) {
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(session_on(&server.uri(), json!({"maxCallsInRequest": 8}))),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer))
        .mount(server)
        .await;
}

async fn sent(server: &MockServer) -> Value {
    let posted = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.method.as_str() == "POST")
        .unwrap();
    serde_json::from_slice(&posted.body).unwrap()
}

#[tokio::test]
async fn the_change_feed_fetches_changed_places_in_the_same_request() {
    let server = MockServer::start().await;
    answering(&server, json!({"sessionState": "s1", "methodResponses": [
        ["Email/changes", {"accountId": "A1", "oldState": "e1", "newState": "e2", "hasMoreChanges": false,
            "created": ["M2"], "updated": ["M1"], "destroyed": ["M0"]}, "c0"],
        ["Email/get", {"accountId": "A1", "state": "e2", "notFound": [], "list": [
            {"id": "M2", "threadId": "T2", "mailboxIds": {"inbox": true}, "keywords": {}}]}, "c1"],
        ["Email/get", {"accountId": "A1", "state": "e2", "notFound": [], "list": [
            {"id": "M1", "threadId": "T1", "mailboxIds": {"archive": true}, "keywords": {"$seen": true}}]}, "c2"],
        ["Mailbox/changes", {"accountId": "A1", "oldState": "m1", "newState": "m1", "hasMoreChanges": false,
            "created": [], "updated": [], "destroyed": []}, "c3"]
    ]})).await;
    let changes = client(&server).mail_changes("e1", "m1", 500).await.unwrap();
    assert_eq!(changes.email.destroyed, ["M0"]);
    assert_eq!(changes.changed.len(), 2);
    let body = sent(&server).await;
    assert_eq!(
        body["methodCalls"][1][1]["#ids"],
        json!({"resultOf": "c0", "name": "Email/changes", "path": "/created"})
    );
    assert_eq!(body["methodCalls"][2][1]["#ids"]["path"], "/updated");
    assert_eq!(
        body["methodCalls"][0][1]["maxChanges"], 50,
        "no more than one Email/get takes"
    );
}

/// Each change comes back through a back-referenced `Email/get`, which a
/// server refuses past its `maxObjectsInGet`.
#[tokio::test]
async fn the_change_feed_asks_for_no_more_than_one_get_takes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(session_on(&server.uri(), json!({"maxObjectsInGet": 40}))),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"sessionState": "s1", "methodResponses": [
            ["Email/changes", {"accountId": "A1", "oldState": "e1", "newState": "e1", "hasMoreChanges": false,
                "created": [], "updated": [], "destroyed": []}, "c0"],
            ["Email/get", {"accountId": "A1", "state": "e1", "notFound": [], "list": []}, "c1"],
            ["Email/get", {"accountId": "A1", "state": "e1", "notFound": [], "list": []}, "c2"],
            ["Mailbox/changes", {"accountId": "A1", "oldState": "m1", "newState": "m1", "hasMoreChanges": false,
                "created": [], "updated": [], "destroyed": []}, "c3"]
        ]})))
        .mount(&server)
        .await;
    client(&server).mail_changes("e1", "m1", 500).await.unwrap();
    assert_eq!(sent(&server).await["methodCalls"][0][1]["maxChanges"], 40);
}

#[tokio::test]
async fn a_forgotten_state_answers_cannot_calculate_changes() {
    let server = MockServer::start().await;
    answering(
        &server,
        json!({"sessionState": "s1", "methodResponses": [
            ["error", {"type": "cannotCalculateChanges"}, "c0"],
            ["error", {"type": "invalidResultReference"}, "c1"],
            ["error", {"type": "invalidResultReference"}, "c2"],
            ["error", {"type": "cannotCalculateChanges"}, "c3"]
        ]}),
    )
    .await;
    let refused = client(&server)
        .mail_changes("old", "old", 500)
        .await
        .unwrap_err();
    assert_eq!(refused, mailrs_jmap::JmapError::CannotCalculateChanges);
}

#[tokio::test]
async fn a_query_sorts_newest_first_and_fetches_threads_by_back_reference() {
    let server = MockServer::start().await;
    answering(&server, json!({"sessionState": "s1", "methodResponses": [
        ["Email/query", {"accountId": "A1", "queryState": "q", "canCalculateChanges": false, "position": 0, "ids": ["M2", "M1"], "total": 2}, "c0"],
        ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [
            {"id": "M1", "threadId": "T1"}, {"id": "M2", "threadId": "T2"}]}, "c1"]
    ]})).await;
    let query = EmailQuery {
        filter: Some(json!({"inMailbox": "inbox"})),
        position: 0,
        limit: 50,
        total: true,
    };
    let found = client(&server).query(&query).await.unwrap();
    let ids: Vec<(&str, &str)> = found
        .hits
        .iter()
        .map(|h| (h.id.as_str(), h.thread_id.as_str()))
        .collect();
    assert_eq!(
        ids,
        [("M2", "T2"), ("M1", "T1")],
        "the query's order, each with its thread"
    );
    assert_eq!(found.total, Some(2));
    let body = sent(&server).await;
    assert_eq!(
        body["methodCalls"][0][1]["sort"],
        json!([{"property": "receivedAt", "isAscending": false}])
    );
    assert_eq!(body["methodCalls"][0][1]["calculateTotal"], true);
}

/// A hit gone before the back-referenced `Email/get` read it has no thread
/// to report, and is gone anyway.
#[tokio::test]
async fn a_hit_the_get_no_longer_finds_is_left_out() {
    let server = MockServer::start().await;
    answering(&server, json!({"sessionState": "s1", "methodResponses": [
        ["Email/query", {"accountId": "A1", "queryState": "q", "canCalculateChanges": false, "position": 0, "ids": ["M2", "M1"]}, "c0"],
        ["Email/get", {"accountId": "A1", "state": "e", "notFound": ["M2"], "list": [{"id": "M1", "threadId": "T1"}]}, "c1"]
    ]})).await;
    let query = EmailQuery {
        filter: None,
        position: 0,
        limit: 50,
        total: false,
    };
    let found = client(&server).query(&query).await.unwrap();
    let ids: Vec<(&str, &str)> = found
        .hits
        .iter()
        .map(|h| (h.id.as_str(), h.thread_id.as_str()))
        .collect();
    assert_eq!(ids, [("M1", "T1")]);
}

/// A page whose every hit went is no end of the listing: the caller tries
/// again rather than stop short.
#[tokio::test]
async fn a_page_whose_every_hit_went_is_a_changed_listing() {
    let server = MockServer::start().await;
    answering(&server, json!({"sessionState": "s1", "methodResponses": [
        ["Email/query", {"accountId": "A1", "queryState": "q", "canCalculateChanges": false, "position": 0, "ids": ["M2"]}, "c0"],
        ["Email/get", {"accountId": "A1", "state": "e", "notFound": ["M2"], "list": []}, "c1"]
    ]})).await;
    let query = EmailQuery {
        filter: None,
        position: 0,
        limit: 50,
        total: false,
    };
    assert_eq!(
        client(&server).query(&query).await.unwrap_err(),
        mailrs_jmap::JmapError::StateMismatch
    );
}

#[tokio::test]
async fn a_page_after_an_anchor_names_the_anchor_and_no_position() {
    let server = MockServer::start().await;
    answering(&server, json!({"sessionState": "s1", "methodResponses": [
        ["Email/query", {"accountId": "A1", "queryState": "q", "canCalculateChanges": false, "position": 7, "ids": ["M1"]}, "c0"],
        ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": "M1", "threadId": "T1"}]}, "c1"]
    ]})).await;
    let query = EmailQuery {
        filter: None,
        position: 7,
        limit: 50,
        total: false,
    };
    let found = client(&server).query_after(&query, "M2").await.unwrap();
    assert_eq!(found.position, 7);
    let body = sent(&server).await;
    assert_eq!(body["methodCalls"][0][1]["anchor"], "M2");
    assert_eq!(body["methodCalls"][0][1]["anchorOffset"], 1);
    assert!(body["methodCalls"][0][1].get("position").is_none());
}

#[tokio::test]
async fn sending_imports_submits_and_files_the_copy_in_one_request() {
    let server = MockServer::start().await;
    answering(&server, json!({"sessionState": "s1", "methodResponses": [
        ["Email/import", {"accountId": "A1", "created": {"m": {"id": "M9", "blobId": "G1", "threadId": "T9", "size": 10}}}, "c0"],
        ["EmailSubmission/set", {"accountId": "A1", "created": {"s": {"id": "S1"}}}, "c1"],
        ["Email/set", {"accountId": "A1", "updated": {"M9": null}}, "c1"]
    ]})).await;
    let submission = Submission {
        blob_id: "G1".into(),
        identity_id: "I1".into(),
        mail_from: "me@example.org".into(),
        rcpt_to: vec!["ada@example.org".into(), "secret@example.org".into()],
        drafts: Some("drafts".into()),
        sent: Some("sent".into()),
    };
    let imported = client(&server).submit(&submission).await.unwrap();
    assert_eq!(
        (imported.id.as_str(), imported.thread_id.as_str()),
        ("M9", "T9")
    );
    let body = sent(&server).await;
    let set = &body["methodCalls"][1][1];
    assert_eq!(set["create"]["s"]["emailId"], "#m");
    assert_eq!(
        set["create"]["s"]["envelope"]["rcptTo"][1]["email"],
        "secret@example.org"
    );
    assert_eq!(set["onSuccessUpdateEmail"]["#s"]["mailboxIds/sent"], true);
    assert_eq!(
        set["onSuccessUpdateEmail"]["#s"]["mailboxIds/drafts"],
        Value::Null
    );
    assert_eq!(
        set["onSuccessUpdateEmail"]["#s"]["keywords/$draft"],
        Value::Null
    );
    assert_eq!(
        body["methodCalls"][0][1]["emails"]["m"]["keywords"]["$draft"],
        true
    );
}

#[tokio::test]
async fn a_refused_submission_says_why() {
    let server = MockServer::start().await;
    answering(&server, json!({"sessionState": "s1", "methodResponses": [
        ["Email/import", {"accountId": "A1", "created": {"m": {"id": "M9", "blobId": "G1", "threadId": "T9", "size": 10}}}, "c0"],
        ["EmailSubmission/set", {"accountId": "A1", "notCreated": {"s": {"type": "forbiddenFrom", "description": "not yours"}}}, "c1"]
    ]})).await;
    let submission = Submission {
        blob_id: "G1".into(),
        identity_id: "I1".into(),
        mail_from: "x@example.org".into(),
        rcpt_to: vec!["a@example.org".into()],
        drafts: Some("drafts".into()),
        sent: Some("sent".into()),
    };
    let refused = client(&server).submit(&submission).await.unwrap_err();
    assert_eq!(
        refused,
        mailrs_jmap::JmapError::Method {
            kind: "forbiddenFrom".into(),
            description: "not yours".into()
        }
    );
}

#[tokio::test]
async fn a_new_mailbox_names_its_role_only_when_given() {
    let server = MockServer::start().await;
    answering(
        &server,
        json!({"sessionState": "s1", "methodResponses": [
            ["Mailbox/set", {"accountId": "A1", "created": {"b": {"id": "B9"}}}, "c0"]
        ]}),
    )
    .await;
    let made = MailboxWrite::Create {
        name: "Archive".into(),
        parent_id: None,
        role: Some("archive".into()),
    };
    assert_eq!(client(&server).set_mailbox(&made).await.unwrap(), "B9");
    let body = sent(&server).await;
    assert_eq!(
        body["methodCalls"][0][1]["create"]["b"],
        json!({"name": "Archive", "parentId": null, "role": "archive"})
    );
}

#[tokio::test]
async fn a_mailbox_rename_sends_its_new_parent() {
    let server = MockServer::start().await;
    answering(
        &server,
        json!({"sessionState": "s1", "methodResponses": [
            ["Mailbox/set", {"accountId": "A1", "updated": {"B1": null}}, "c0"]
        ]}),
    )
    .await;
    let id = client(&server)
        .set_mailbox(&MailboxWrite::Update {
            id: "B1".into(),
            name: "Trips".into(),
            parent_id: Some("B0".into()),
        })
        .await
        .unwrap();
    assert_eq!(id, "B1");
    let body = sent(&server).await;
    assert_eq!(
        body["methodCalls"][0][1]["update"]["B1"],
        json!({"name": "Trips", "parentId": "B0"})
    );
}

#[test]
fn a_patch_path_escapes_slashes_and_tildes() {
    assert_eq!(mailrs_jmap::pointer("a/b~c"), "a~1b~0c");
}

/// Answers the session, then each POST to the API with the next answer.
async fn answering_in_turn(server: &MockServer, answers: Vec<Value>) {
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(session_on(&server.uri(), json!({"maxCallsInRequest": 8}))),
        )
        .mount(server)
        .await;
    for answer in answers {
        Mock::given(method("POST"))
            .and(path("/api/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(answer))
            .up_to_n_times(1)
            .mount(server)
            .await;
    }
}

async fn posts(server: &MockServer) -> Vec<Value> {
    let all = server.received_requests().await.unwrap();
    all.into_iter()
        .filter(|r| r.method.as_str() == "POST")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

fn created_import() -> Value {
    json!({"sessionState": "s1", "methodResponses": [
        ["Email/import", {"accountId": "A1", "created": {"m": {"id": "M9", "blobId": "G1", "threadId": "T9", "size": 10}}}, "c0"]
    ]})
}

fn a_submission() -> Submission {
    Submission {
        blob_id: "G1".into(),
        identity_id: "I1".into(),
        mail_from: "x@example.org".into(),
        rcpt_to: vec!["a@example.org".into()],
        drafts: Some("drafts".into()),
        sent: Some("sent".into()),
    }
}

#[tokio::test]
async fn a_refused_import_leaves_the_old_draft_alone() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![json!({"sessionState": "s1", "methodResponses": [
        ["Email/import", {"accountId": "A1", "notCreated": {"m": {"type": "overQuota", "description": "full"}}}, "c0"]
    ]})]).await;
    let refused = client(&server)
        .import("G1", &["drafts".into()], &["$draft"], Some("OLD"))
        .await
        .unwrap_err();
    assert_eq!(
        refused,
        mailrs_jmap::JmapError::Method {
            kind: "overQuota".into(),
            description: "full".into()
        }
    );
    assert_eq!(
        posts(&server).await.len(),
        1,
        "no destroy followed the refused import"
    );
}

#[tokio::test]
async fn a_saved_draft_destroys_the_old_one_after_the_import() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        created_import(),
        json!({"sessionState": "s1", "methodResponses": [["Email/set", {"accountId": "A1", "destroyed": ["OLD"]}, "c0"]]}),
    ]).await;
    let saved = client(&server)
        .import("G1", &["drafts".into()], &["$draft"], Some("OLD"))
        .await
        .unwrap();
    assert_eq!(saved.email.id, "M9");
    assert_eq!(saved.leftover, None);
    let sent = posts(&server).await;
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["methodCalls"][0][1]["destroy"], json!(["OLD"]));
}

#[tokio::test]
async fn a_refused_destroy_is_reported_beside_the_new_message() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        created_import(),
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/set", {"accountId": "A1", "notDestroyed": {"OLD": {"type": "notFound"}}}, "c0"]]}),
    ]).await;
    let saved = client(&server)
        .import("G1", &["drafts".into()], &["$draft"], Some("OLD"))
        .await
        .unwrap();
    assert_eq!(saved.email.id, "M9");
    assert_eq!(saved.leftover, Some(mailrs_jmap::JmapError::NotFound));
}

#[tokio::test]
async fn a_refused_submission_destroys_the_imported_copy() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/import", {"accountId": "A1", "created": {"m": {"id": "M9", "blobId": "G1", "threadId": "T9", "size": 10}}}, "c0"],
            ["EmailSubmission/set", {"accountId": "A1", "notCreated": {"s": {"type": "forbiddenFrom"}}}, "c1"]]}),
        json!({"sessionState": "s1", "methodResponses": [["Email/set", {"accountId": "A1", "destroyed": ["M9"]}, "c0"]]}),
    ]).await;
    client(&server).submit(&a_submission()).await.unwrap_err();
    let sent = posts(&server).await;
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["methodCalls"][0][1]["destroy"], json!(["M9"]));
}

#[tokio::test]
async fn a_submission_method_error_destroys_the_imported_copy_and_is_returned() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/import", {"accountId": "A1", "created": {"m": {"id": "M9", "blobId": "G1", "threadId": "T9", "size": 10}}}, "c0"],
            ["error", {"type": "forbidden"}, "c1"]]}),
        json!({"sessionState": "s1", "methodResponses": [["Email/set", {"accountId": "A1", "destroyed": ["M9"]}, "c0"]]}),
    ]).await;
    let refused = client(&server).submit(&a_submission()).await.unwrap_err();
    assert_eq!(refused, mailrs_jmap::JmapError::Forbidden(String::new()));
    let sent = posts(&server).await;
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["methodCalls"][0][1]["destroy"], json!(["M9"]));
}

#[tokio::test]
async fn emails_are_fetched_in_chunks_of_the_servers_get_limit() {
    let server = MockServer::start().await;
    let page = |ids: &[&str]| {
        let list: Vec<Value> = ids.iter().map(|i| json!({"id": i})).collect();
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": list}, "c0"]]})
    };
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(session_on(&server.uri(), json!({"maxObjectsInGet": 2}))),
        )
        .mount(&server)
        .await;
    for answer in [page(&["a", "b"]), page(&["c"])] {
        Mock::given(method("POST"))
            .and(path("/api/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(answer))
            .up_to_n_times(1)
            .mount(&server)
            .await;
    }
    let ids: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
    let got = client(&server)
        .emails(&ids, mailrs_jmap::PLACES)
        .await
        .unwrap();
    assert_eq!(got.list.len(), 3);
    let sent = posts(&server).await;
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["methodCalls"][0][1]["ids"], json!(["a", "b"]));
    assert_eq!(sent[1]["methodCalls"][0][1]["ids"], json!(["c"]));
}

/// A server whose `maxObjectsInGet` is `limit`, holding `threads`, each
/// a thread id and its messages. It answers a back-referenced Email/get
/// with every message.
async fn holding_threads(
    limit: u64,
    threads: &'static [(&'static str, &'static [&'static str])],
) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(session_on(&server.uri(), json!({"maxObjectsInGet": limit}))),
        )
        .mount(&server)
        .await;
    let thread_of = |email: &str| {
        threads
            .iter()
            .find(|(_, emails)| emails.contains(&email))
            .map(|(t, _)| *t)
    };
    let answer = move |request: &wiremock::Request| {
        let asked: Value = serde_json::from_slice(&request.body).unwrap();
        let answers: Vec<Value> = asked["methodCalls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|call| {
                let (name, args, tag) = (&call[0], &call[1], &call[2]);
                let every: Vec<&str> = threads
                    .iter()
                    .flat_map(|(_, emails)| emails.iter().copied())
                    .collect();
                let asked: Vec<&str> = match args["ids"].as_array() {
                    Some(ids) => ids.iter().filter_map(Value::as_str).collect(),
                    None => every,
                };
                let list: Vec<Value> = match name.as_str().unwrap() {
                    "Thread/get" => threads
                        .iter()
                        .filter(|(t, _)| asked.contains(t))
                        .map(|(t, e)| json!({"id": t, "emailIds": e}))
                        .collect(),
                    _ => asked
                        .iter()
                        .map(|id| json!({"id": id, "threadId": thread_of(id)}))
                        .collect(),
                };
                json!([name, {"accountId": "A1", "state": "s", "notFound": [], "list": list}, tag])
            })
            .collect();
        ResponseTemplate::new(200)
            .set_body_json(json!({"sessionState": "s1", "methodResponses": answers}))
    };
    Mock::given(method("POST"))
        .and(path("/api/"))
        .respond_with(answer)
        .mount(&server)
        .await;
    server
}

/// The calls named `name` in every request the server took.
async fn calls_named(server: &MockServer, name: &str) -> Vec<Value> {
    let calls = posts(server)
        .await
        .into_iter()
        .flat_map(|r| r["methodCalls"].as_array().unwrap().clone());
    calls
        .filter(|c| c[0] == name)
        .map(|c| c[1].clone())
        .collect()
}

#[tokio::test]
async fn threads_are_fetched_in_chunks_of_the_servers_get_limit() {
    let server = holding_threads(2, &[("a", &["ma"]), ("b", &["mb"]), ("c", &["mc"])]).await;
    let ids: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
    let got = client(&server).threads(&ids).await.unwrap();
    assert_eq!((got.threads.len(), got.emails.len()), (3, 3));
    let asked: Vec<Value> = calls_named(&server, "Thread/get")
        .await
        .into_iter()
        .map(|args| args["ids"].clone())
        .collect();
    assert_eq!(asked, [json!(["a", "b"]), json!(["c"])]);
}

#[tokio::test]
async fn the_messages_of_long_threads_are_fetched_in_chunks_of_the_servers_get_limit() {
    let server = holding_threads(2, &[("a", &["m1", "m2", "m3"])]).await;
    let got = client(&server).threads(&["a".to_string()]).await.unwrap();
    assert_eq!(got.emails.len(), 3);
    for args in calls_named(&server, "Email/get").await {
        assert!(
            args["ids"].as_array().is_some_and(|ids| ids.len() <= 2),
            "an Email/get past the limit: {args}"
        );
    }
}

/// Cyrus, as Fastmail, keeps one copy of a message per account: importing
/// the same bytes again answers `alreadyExists` with the copy it has.
fn already_there(id: &str) -> Value {
    json!({"sessionState": "s1", "methodResponses": [
        ["Email/import", {"accountId": "A1", "notCreated": {"m": {"type": "alreadyExists", "existingId": id}}}, "c0"]
    ]})
}

#[tokio::test]
async fn saving_an_unchanged_draft_keeps_the_copy_the_server_has() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        already_there("OLD"),
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": "OLD", "threadId": "T1", "keywords": {"$draft": true}}]}, "c0"]]}),
        json!({"sessionState": "s1", "methodResponses": [["Email/set", {"accountId": "A1", "updated": {"OLD": null}}, "c0"]]}),
    ]).await;
    let saved = client(&server)
        .import("G1", &["drafts".into()], &["$draft"], Some("OLD"))
        .await
        .unwrap();
    assert_eq!(
        (saved.email.id.as_str(), saved.email.thread_id.as_str()),
        ("OLD", "T1")
    );
    assert_eq!(saved.leftover, None);
    let sent = posts(&server).await;
    assert_eq!(sent.len(), 3, "the copy is read and kept, not destroyed");
    let update = &sent[2]["methodCalls"][0][1]["update"]["OLD"];
    assert_eq!(
        (&update["mailboxIds/drafts"], &update["keywords/$draft"]),
        (&json!(true), &json!(true))
    );
}

/// The look at the server's copy before it is submitted: still a draft,
/// and no submission names it.
fn still_a_draft(id: &str) -> Value {
    json!({"sessionState": "s1", "methodResponses": [
        ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": id, "threadId": "T1", "keywords": {"$draft": true}}]}, "c0"],
        ["EmailSubmission/query", {"accountId": "A1", "ids": []}, "c1"]]})
}

/// The first answer to a send of bytes the server has as `D1`: the import
/// names the copy, and the submission of `#m` is not made.
fn exists_and_not_submitted() -> Value {
    json!({"sessionState": "s1", "methodResponses": [
        ["Email/import", {"accountId": "A1", "notCreated": {"m": {"type": "alreadyExists", "existingId": "D1"}}}, "c0"],
        ["EmailSubmission/set", {"accountId": "A1", "notCreated": {"s": {"type": "invalidProperties"}}}, "c1"]]})
}

fn submissions(sent: &[Value]) -> usize {
    sent.iter()
        .flat_map(|p| p["methodCalls"].as_array().unwrap().clone())
        .filter(|c| c[0] == "EmailSubmission/set")
        .count()
}

#[tokio::test]
async fn sending_bytes_the_server_has_submits_its_copy() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/import", {"accountId": "A1", "notCreated": {"m": {"type": "alreadyExists", "existingId": "D1"}}}, "c0"],
            ["EmailSubmission/set", {"accountId": "A1", "notCreated": {"s": {"type": "invalidProperties"}}}, "c1"]]}),
        still_a_draft("D1"),
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": "D1", "threadId": "T1"}]}, "c0"],
            ["EmailSubmission/set", {"accountId": "A1", "created": {"s": {"id": "S1"}}}, "c1"],
            ["Email/set", {"accountId": "A1", "updated": {"D1": null}}, "c1"]]}),
    ]).await;
    let imported = client(&server).submit(&a_submission()).await.unwrap();
    assert_eq!(
        (imported.id.as_str(), imported.thread_id.as_str()),
        ("D1", "T1")
    );
    let sent = posts(&server).await;
    assert_eq!(sent.len(), 3);
    let set = &sent[2]["methodCalls"][1][1];
    assert_eq!(set["create"]["s"]["emailId"], "D1");
    assert_eq!(set["onSuccessUpdateEmail"]["#s"]["mailboxIds/sent"], true);
    assert_eq!(
        set["onSuccessUpdateEmail"]["#s"]["keywords/$draft"],
        Value::Null
    );
}

#[tokio::test]
async fn a_refused_submission_of_the_server_s_copy_leaves_that_copy() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/import", {"accountId": "A1", "notCreated": {"m": {"type": "alreadyExists", "existingId": "D1"}}}, "c0"],
            ["EmailSubmission/set", {"accountId": "A1", "notCreated": {"s": {"type": "invalidProperties"}}}, "c1"]]}),
        still_a_draft("D1"),
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": "D1", "threadId": "T1"}]}, "c0"],
            ["EmailSubmission/set", {"accountId": "A1", "notCreated": {"s": {"type": "forbiddenFrom"}}}, "c1"]]}),
    ]).await;
    let refused = client(&server).submit(&a_submission()).await.unwrap_err();
    assert_eq!(
        refused,
        mailrs_jmap::JmapError::Method {
            kind: "forbiddenFrom".into(),
            description: String::new()
        }
    );
    assert_eq!(
        posts(&server).await.len(),
        3,
        "no destroy of a message the send did not make"
    );
}

#[tokio::test]
async fn a_server_that_submits_its_own_copy_at_once_sends_it_once() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/import", {"accountId": "A1", "notCreated": {"m": {"type": "alreadyExists", "existingId": "D1"}}}, "c0"],
            ["EmailSubmission/set", {"accountId": "A1", "created": {"s": {"id": "S1"}}}, "c1"]]}),
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/set", {"accountId": "A1", "updated": {"D1": null}}, "c0"],
            ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": "D1", "threadId": "T1"}]}, "c1"]]}),
    ]).await;
    let imported = client(&server).submit(&a_submission()).await.unwrap();
    assert_eq!(
        (imported.id.as_str(), imported.thread_id.as_str()),
        ("D1", "T1")
    );
    let sent = posts(&server).await;
    let submissions = sent
        .iter()
        .flat_map(|p| p["methodCalls"].as_array().unwrap().clone())
        .filter(|c| c[0] == "EmailSubmission/set")
        .count();
    assert_eq!(submissions, 1, "the message went out once");
    assert_eq!(
        sent[1]["methodCalls"][0][1]["update"]["D1"]["keywords/$draft"],
        Value::Null
    );
    assert_eq!(
        sent[1]["methodCalls"][0][1]["update"]["D1"]["mailboxIds/sent"],
        true
    );
}

#[tokio::test]
async fn the_server_s_copy_sent_without_a_sent_mailbox_stops_being_a_draft() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/import", {"accountId": "A1", "notCreated": {"m": {"type": "alreadyExists", "existingId": "D1"}}}, "c0"],
            ["EmailSubmission/set", {"accountId": "A1", "notCreated": {"s": {"type": "invalidProperties"}}}, "c1"]]}),
        still_a_draft("D1"),
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": "D1", "threadId": "T1"}]}, "c0"],
            ["EmailSubmission/set", {"accountId": "A1", "created": {"s": {"id": "S1"}}}, "c1"]]}),
    ]).await;
    // As the adapter asks with Drafts but no Sent or Archive: the copy is
    // filed in Drafts, without `$draft`.
    let submission = Submission {
        drafts: None,
        sent: Some("drafts".into()),
        ..a_submission()
    };
    client(&server).submit(&submission).await.unwrap();
    let sent = posts(&server).await;
    let moved = &sent[2]["methodCalls"][1][1]["onSuccessUpdateEmail"]["#s"];
    assert_eq!(moved["keywords/$draft"], Value::Null);
    assert!(moved.as_object().unwrap().contains_key("keywords/$draft"));
    assert_eq!(moved["mailboxIds/drafts"], true);
}

/// Cyrus after a send whose answer was lost: the server's copy was sent
/// and filed, so a retry files it again and submits nothing.
#[tokio::test]
async fn a_retry_finds_its_copy_already_submitted_and_sends_nothing() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        exists_and_not_submitted(),
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": "D1", "threadId": "T1", "keywords": {"$draft": true}}]}, "c0"],
            ["EmailSubmission/query", {"accountId": "A1", "ids": ["S1"]}, "c1"]]}),
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/set", {"accountId": "A1", "updated": {"D1": null}}, "c0"],
            ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": "D1", "threadId": "T1"}]}, "c1"]]}),
    ]).await;
    let imported = client(&server).submit(&a_submission()).await.unwrap();
    assert_eq!(imported.id, "D1");
    let sent = posts(&server).await;
    assert_eq!(
        sent[1]["methodCalls"][1][1]["filter"]["emailIds"],
        json!(["D1"])
    );
    assert_eq!(
        submissions(&sent),
        1,
        "only the first try's submission, which was not made"
    );
    assert_eq!(
        sent[2]["methodCalls"][0][1]["update"]["D1"]["mailboxIds/sent"],
        true
    );
}

/// A server without `EmailSubmission/query`: a copy that is no draft any
/// more was filed as sent by an earlier try.
#[tokio::test]
async fn a_retry_on_a_server_without_submission_query_trusts_the_missing_draft_keyword() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        exists_and_not_submitted(),
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": "D1", "threadId": "T1", "keywords": {"$seen": true}}]}, "c0"],
            ["error", {"type": "unknownMethod"}, "c1"]]}),
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/set", {"accountId": "A1", "updated": {"D1": null}}, "c0"],
            ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": "D1", "threadId": "T1"}]}, "c1"]]}),
    ]).await;
    client(&server).submit(&a_submission()).await.unwrap();
    assert_eq!(submissions(&posts(&server).await), 1);
}

#[tokio::test]
async fn saving_bytes_another_message_holds_is_refused_and_leaves_it_alone() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        already_there("INBOX1"),
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": "INBOX1", "threadId": "T1", "keywords": {"$seen": true}}]}, "c0"]]}),
    ]).await;
    let refused = client(&server)
        .import("G1", &["drafts".into()], &["$draft"], Some("OLD"))
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, mailrs_jmap::JmapError::Method { kind, .. } if kind == "alreadyExists"),
        "{refused:?}"
    );
    let sent = posts(&server).await;
    assert_eq!(sent.len(), 2, "no patch and no destroy");
}

#[tokio::test]
async fn a_query_page_stays_within_the_servers_get_limit() {
    // The query's ids feed a back-referenced Email/get, so a page longer
    // than maxObjectsInGet would be refused whole.
    let server = holding_threads(2, &[("a", &["ma", "mb", "mc"])]).await;
    let query = EmailQuery {
        filter: None,
        position: 0,
        limit: 1000,
        total: false,
    };
    let _ = client(&server).query(&query).await;
    let asked = calls_named(&server, "Email/query").await;
    assert_eq!(asked[0]["limit"], json!(2));
}

/// The import made `M9` and the submission's answer says nothing sure.
fn made_then(submission: Value) -> Value {
    json!({"sessionState": "s1", "methodResponses": [
        ["Email/import", {"accountId": "A1", "created": {"m": {"id": "M9", "blobId": "G1", "threadId": "T9", "size": 10}}}, "c0"],
        submission]})
}

fn destroys(sent: &[Value]) -> usize {
    sent.iter()
        .flat_map(|p| p["methodCalls"].as_array().unwrap().clone())
        .filter(|c| c[1].get("destroy").is_some())
        .count()
}

/// A `serverFail` may follow a submission the server queued: a look finds
/// it, so the copy is filed and nothing is sent again.
#[tokio::test]
async fn a_server_failure_after_a_submission_it_made_files_the_copy() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        made_then(json!(["error", {"type": "serverFail"}, "c1"])),
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": "M9", "threadId": "T9", "keywords": {"$draft": true}}]}, "c0"],
            ["EmailSubmission/query", {"accountId": "A1", "ids": ["S1"]}, "c1"]]}),
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/set", {"accountId": "A1", "updated": {"M9": null}}, "c0"],
            ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": "M9", "threadId": "T9"}]}, "c1"]]}),
    ]).await;
    let sent_copy = client(&server).submit(&a_submission()).await.unwrap();
    assert_eq!(sent_copy.id, "M9");
    let sent = posts(&server).await;
    assert_eq!(destroys(&sent), 0);
    assert_eq!(submissions(&sent), 1);
    assert_eq!(
        sent[2]["methodCalls"][0][1]["update"]["M9"]["mailboxIds/sent"],
        true
    );
}

/// A look that finds no submission and the copy still a draft proves
/// nothing went: the copy goes and the failure is the answer.
#[tokio::test]
async fn a_server_failure_with_no_submission_behind_it_destroys_the_copy() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        made_then(json!(["error", {"type": "serverFail"}, "c1"])),
        still_a_draft("M9"),
        json!({"sessionState": "s1", "methodResponses": [["Email/set", {"accountId": "A1", "destroyed": ["M9"]}, "c0"]]}),
    ]).await;
    let failed = client(&server).submit(&a_submission()).await.unwrap_err();
    assert!(
        matches!(failed, mailrs_jmap::JmapError::Method { ref kind, .. } if kind == "serverFail"),
        "{failed:?}"
    );
    assert_eq!(destroys(&posts(&server).await), 1);
}

/// When the look cannot tell, the copy stays and the outcome is unknown,
/// so the caller looks in Sent before it sends again.
#[tokio::test]
async fn a_server_failure_a_look_cannot_settle_keeps_the_copy_and_is_unknown() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        made_then(json!(["error", {"type": "serverFail"}, "c1"])),
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": "M9", "threadId": "T9", "keywords": {"$draft": true}}]}, "c0"],
            ["error", {"type": "unknownMethod"}, "c1"]]}),
    ]).await;
    assert_eq!(
        client(&server).submit(&a_submission()).await.unwrap_err(),
        mailrs_jmap::JmapError::SubmitUnknown
    );
    assert_eq!(destroys(&posts(&server).await), 0);
}

/// An answer that names no submission at all is no refusal either.
#[tokio::test]
async fn an_answer_without_the_submission_keeps_the_copy_when_a_look_cannot_settle_it() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/import", {"accountId": "A1", "created": {"m": {"id": "M9", "blobId": "G1", "threadId": "T9", "size": 10}}}, "c0"]]}),
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": "M9", "threadId": "T9", "keywords": {"$draft": true}}]}, "c0"],
            ["error", {"type": "unknownMethod"}, "c1"]]}),
    ]).await;
    assert_eq!(
        client(&server).submit(&a_submission()).await.unwrap_err(),
        mailrs_jmap::JmapError::SubmitUnknown
    );
    assert_eq!(destroys(&posts(&server).await), 0);
}

/// A 5xx after the request went may hide a sent message, and there is no
/// id to look at.
#[tokio::test]
async fn a_server_error_on_the_send_request_is_an_unknown_outcome() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(session_on(&server.uri(), json!({"maxCallsInRequest": 8}))),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/"))
        .respond_with(ResponseTemplate::new(502))
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).submit(&a_submission()).await.unwrap_err(),
        mailrs_jmap::JmapError::SubmitUnknown
    );
    assert_eq!(posts(&server).await.len(), 1);
}

/// The server's own copy sent again: a `serverFail` there is no refusal.
#[tokio::test]
async fn a_server_failure_on_resending_the_server_s_copy_is_unknown() {
    let server = MockServer::start().await;
    answering_in_turn(&server, vec![
        exists_and_not_submitted(),
        still_a_draft("D1"),
        json!({"sessionState": "s1", "methodResponses": [
            ["Email/get", {"accountId": "A1", "state": "e", "notFound": [], "list": [{"id": "D1", "threadId": "T1"}]}, "c0"],
            ["error", {"type": "serverFail"}, "c1"]]}),
    ]).await;
    assert_eq!(
        client(&server).submit(&a_submission()).await.unwrap_err(),
        mailrs_jmap::JmapError::SubmitUnknown
    );
}

#[tokio::test]
async fn a_conditional_delete_sends_the_state_and_reports_a_mismatch() {
    let server = MockServer::start().await;
    answering(
        &server,
        json!({"sessionState": "s1", "methodResponses": [
            ["error", {"type": "stateMismatch"}, "c0"]
        ]}),
    )
    .await;
    let result = client(&server)
        .set_emails(&serde_json::Map::new(), &["M1".into()], Some("e1"))
        .await;
    assert_eq!(result.unwrap_err(), mailrs_jmap::JmapError::StateMismatch);
    let body = sent(&server).await;
    assert_eq!(body["methodCalls"][0][1]["ifInState"], "e1");
    assert_eq!(body["methodCalls"][0][1]["destroy"], json!(["M1"]));
}
