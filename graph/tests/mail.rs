mod common;

use mailrs_graph::{Fields, GraphError, Listing, MessagePatch, Write};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[tokio::test]
async fn body_reads_select_content_ids_from_file_attachments() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/messages/m1"))
        .respond_with(|request: &Request| {
            let query: std::collections::HashMap<_, _> = request.url.query_pairs().collect();
            let fields = query["$expand"]
                .strip_prefix("attachments($select=")
                .unwrap()
                .strip_suffix(')')
                .unwrap();
            if fields.split(',').any(|field| field == "contentId") {
                return ResponseTemplate::new(400).set_body_json(json!({
                    "error": {"code": "BadRequest", "message":
                        "Could not find a property named 'contentId' on type 'microsoft.graph.attachment'."}
                }));
            }
            assert_eq!(query["$select"], "body,hasAttachments");
            assert!(fields.split(',').any(|field| field == "microsoft.graph.fileAttachment/contentId"));
            assert!(!fields.contains("contentBytes"));
            ResponseTemplate::new(200).set_body_json(json!({
                "body": {"contentType": "html", "content": "<img src=\"cid:logo\">"},
                "hasAttachments": true,
                "attachments": [
                    {"@odata.type": "#microsoft.graph.fileAttachment", "id": "image",
                     "name": "logo.png", "contentType": "image/png", "size": 128,
                     "isInline": true, "contentId": "logo"},
                    {"@odata.type": "#microsoft.graph.itemAttachment", "id": "mail",
                     "name": "Forwarded message", "contentType": "message/rfc822",
                     "size": 2048, "isInline": false}
                ]
            }))
        })
        .expect(1)
        .mount(&server)
        .await;
    let body = common::graph(&server).body("m1").await.unwrap();
    assert_eq!(body.body.content, "<img src=\"cid:logo\">");
    assert_eq!(body.attachments.len(), 2);
    assert_eq!(body.attachments[0].content_id.as_deref(), Some("logo"));
    assert!(body.attachments[0].is_inline);
    assert_eq!(body.attachments[1].id, "mail");
    assert_eq!(body.attachments[1].content_id, None);
}

#[tokio::test]
async fn the_well_known_folders_come_in_one_batch() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let answers: Vec<Value> = body["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    let url = r["url"].as_str().unwrap();
                    match url.contains("/archive") {
                        // A mailbox that never had an Archive.
                        true => json!({"id": r["id"], "status": 404, "body": {"error": {"code": "ErrorFolderNotFound"}}}),
                        false => json!({"id": r["id"], "status": 200, "body": {"id": format!("id-{url}"), "displayName": url}}),
                    }
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({"responses": answers}))
        })
        .mount(&server)
        .await;
    let found = common::graph(&server)
        .well_known(&["inbox", "sentitems", "archive"])
        .await
        .unwrap();
    assert_eq!(found.len(), 3);
    assert!(found[0].as_ref().unwrap().id.contains("inbox"));
    assert!(found[2].is_none(), "a missing folder is None, not an error");
}

#[tokio::test]
async fn a_first_delta_asks_for_the_window_and_small_pages() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/mailFolders/AAMk-inbox/messages/delta"))
        .and(query_param("$filter", "receivedDateTime ge 2026-08-28T00:00:00Z"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [
                {"id": "m1", "conversationId": "c1", "isRead": false, "categories": ["Red"],
                 "inferenceClassification": "other", "flag": {"flagStatus": "flagged"}},
                {"id": "m2", "@removed": {"reason": "deleted"}},
            ],
            "@odata.deltaLink": format!("{}/v1.0/me/mailFolders/AAMk-inbox/messages/delta?$deltatoken=d1", server.uri()),
        })))
        .mount(&server)
        .await;
    let page = common::graph(&server)
        .message_delta("AAMk-inbox", None, Some("2026-08-28T00:00:00Z"))
        .await
        .unwrap();
    assert_eq!(page.value.len(), 2);
    assert!(page.value[0].is_other() && page.value[0].is_flagged());
    assert!(page.value[1].removed.is_some());
    assert!(page.delta_link.unwrap().ends_with("$deltatoken=d1"));
    let asked = &server.received_requests().await.unwrap();
    let delta = asked
        .iter()
        .find(|r| r.url.path().ends_with("/delta"))
        .unwrap();
    let prefer: Vec<_> = delta
        .headers
        .get_all("prefer")
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    assert!(
        prefer.iter().any(|p| p == "odata.maxpagesize=50"),
        "{prefer:?}"
    );
}

#[tokio::test]
async fn a_whole_folder_delta_has_no_received_date_filter() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/mailFolders/inbox/messages/delta"))
        .and(|request: &Request| !request.url.query_pairs().any(|(key, _)| key == "$filter"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [{"id": "old", "isRead": true}],
            "@odata.deltaLink": format!("{}/v1.0/me/mailFolders/inbox/messages/delta?$deltatoken=d1", server.uri()),
        })))
        .expect(1)
        .mount(&server)
        .await;

    let page = common::graph(&server)
        .message_delta("inbox", None, None)
        .await
        .unwrap();

    assert_eq!(page.value.len(), 1);
    assert_eq!(page.value[0].id, "old");
}

#[tokio::test]
async fn a_search_leaves_out_the_order_graph_refuses_beside_it() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&server)
        .await;
    let listing = Listing {
        search: Some("from:ann subject:lunch".into()),
        top: 25,
        ..Listing::default()
    };
    common::graph(&server)
        .list_messages(&listing, None)
        .await
        .unwrap();
    let asked = server.received_requests().await.unwrap();
    let url = &asked
        .iter()
        .find(|r| r.url.path() == "/v1.0/me/messages")
        .unwrap()
        .url;
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(pairs["$search"], "\"from:ann subject:lunch\"");
    assert!(!pairs.contains_key("$orderby") && !pairs.contains_key("$filter"));
}

#[tokio::test]
async fn a_category_listing_filters_by_name_after_the_date() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&server)
        .await;
    let listing = Listing {
        received_since: Some("2026-08-28T00:00:00Z".into()),
        category: Some("Ann's".into()),
        top: 1000,
        ..Listing::default()
    };
    common::graph(&server)
        .list_messages(&listing, None)
        .await
        .unwrap();
    let asked = server.received_requests().await.unwrap();
    let url = &asked
        .iter()
        .find(|r| r.url.path() == "/v1.0/me/messages")
        .unwrap()
        .url;
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(
        pairs["$filter"],
        "receivedDateTime ge 2026-08-28T00:00:00Z and categories/any(c:c eq 'Ann''s')"
    );
    assert_eq!(pairs["$orderby"], "receivedDateTime desc");
}

#[tokio::test]
async fn metadata_fetches_keep_each_answer_in_its_place() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"responses": [
                {"id": "1", "status": 200, "body": {"id": "m1", "subject": "Hi",
                  "singleValueExtendedProperties": [{"id": "Integer 0xe08", "value": "5120"}]}},
                {"id": "2", "status": 404, "body": {"error": {"code": "ErrorItemNotFound"}}},
            ]})),
        )
        .mount(&server)
        .await;
    let found = common::graph(&server)
        .messages(&["m1".into(), "m2".into()])
        .await
        .unwrap();
    assert_eq!(found[0].as_ref().unwrap().size(), Some(5120));
    assert!(matches!(found[1], Err(GraphError::NotFound)));
}

#[tokio::test]
async fn writes_go_as_one_batch_in_graphs_shapes() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("POST"))
        .and(path("/v1.0/$batch"))
        .respond_with(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let answers: Vec<Value> = body["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| json!({"id": r["id"], "status": 200, "body": r}))
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({"responses": answers}))
        })
        .mount(&server)
        .await;
    let writes = [
        Write::Move {
            id: "m1".into(),
            folder: "AAMk-archive".into(),
        },
        Write::Patch {
            id: "m2".into(),
            patch: MessagePatch {
                is_read: Some(true),
                flagged: Some(false),
                categories: Some(vec!["Red".into()]),
                other: Some(true),
            },
        },
        Write::Delete { id: "m3".into() },
        Write::PermanentDelete { id: "m4".into() },
    ];
    let done = common::graph(&server).apply(&writes).await.unwrap();
    assert!(done.iter().all(Result::is_ok));
    let asked = server.received_requests().await.unwrap();
    let batch: Value = serde_json::from_slice(
        &asked
            .iter()
            .find(|r| r.url.path() == "/v1.0/$batch")
            .unwrap()
            .body,
    )
    .unwrap();
    let r = batch["requests"].as_array().unwrap();
    assert_eq!(
        (r[0]["method"].as_str(), r[0]["url"].as_str()),
        (Some("POST"), Some("/me/messages/m1/move"))
    );
    assert_eq!(r[0]["body"], json!({"destinationId": "AAMk-archive"}));
    assert_eq!(
        r[1]["body"],
        json!({"isRead": true, "flag": {"flagStatus": "notFlagged"}, "categories": ["Red"], "inferenceClassification": "other"})
    );
    assert_eq!(
        (r[2]["method"].as_str(), r[2]["url"].as_str()),
        (Some("DELETE"), Some("/me/messages/m3"))
    );
    assert_eq!(r[3]["url"], "/me/messages/m4/permanentDelete");
}

#[tokio::test]
async fn a_message_goes_out_as_base64_mime() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("POST"))
        .and(path("/v1.0/me/sendMail"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;
    common::graph(&server)
        .send_mime(b"From: a@outlook.com\r\n\r\nHi")
        .await
        .unwrap();
    let asked = server.received_requests().await.unwrap();
    let sent = asked
        .iter()
        .find(|r| r.url.path() == "/v1.0/me/sendMail")
        .unwrap();
    assert_eq!(sent.headers.get("content-type").unwrap(), "text/plain");
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&sent.body)
        .unwrap();
    assert_eq!(decoded, b"From: a@outlook.com\r\n\r\nHi");
}

#[tokio::test]
async fn an_upload_goes_only_to_microsofts_upload_host_and_carries_no_token() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    let graph = common::graph(&server);
    let refused = graph
        .upload_chunk("https://evil.example/upload/1", 0, 3, b"abc")
        .await;
    assert!(matches!(refused, Err(GraphError::OffHost(_))));
    Mock::given(method("PUT"))
        .and(path("/upload/1"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;
    // The test base is wiremock's, which the client takes as its own host.
    let done = graph
        .upload_chunk(&format!("{}/upload/1", server.uri()), 0, 3, b"abc")
        .await
        .unwrap();
    assert!(done);
    let put = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.method.as_str() == "PUT")
        .unwrap();
    assert!(put.headers.get("authorization").is_none());
    assert_eq!(put.headers.get("content-range").unwrap(), "bytes 0-2/3");
}

#[tokio::test]
async fn a_full_listing_expands_the_size() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/mailFolders/AAMk-inbox/messages"))
        .and(query_param("$orderby", "receivedDateTime desc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&server)
        .await;
    let listing = Listing {
        folder: Some("AAMk-inbox".into()),
        top: 10,
        fields: Fields::Meta,
        ..Listing::default()
    };
    common::graph(&server)
        .list_messages(&listing, None)
        .await
        .unwrap();
    let asked = server.received_requests().await.unwrap();
    let url = &asked
        .iter()
        .find(|r| r.url.path().ends_with("/messages"))
        .unwrap()
        .url;
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert!(pairs["$expand"].contains("Integer 0x0E08"));
    assert!(pairs["$select"].contains("internetMessageHeaders"));
}

#[tokio::test]
async fn an_unread_listing_filters_on_the_read_state() {
    let server = MockServer::start().await;
    common::token_endpoint(&server, "r").await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/mailFolders/inbox/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&server)
        .await;
    let listing = Listing {
        folder: Some("inbox".into()),
        unread: true,
        top: 200,
        ..Listing::default()
    };
    common::graph(&server)
        .list_messages(&listing, None)
        .await
        .unwrap();
    let asked = server.received_requests().await.unwrap();
    let url = &asked
        .iter()
        .find(|r| r.url.path() == "/v1.0/me/mailFolders/inbox/messages")
        .unwrap()
        .url;
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(pairs["$filter"], "isRead eq false");
}
