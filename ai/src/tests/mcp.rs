//! The MCP client against servers written for the test: this test binary
//! itself run as a stdio server, and wiremock for Streamable HTTP.

use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::ToolOutcome;
use crate::mcp::{
    HANDSHAKE_VERSIONS, McpClient, McpError, PROTOCOL_VERSION, Transport, header_marks,
    mirrored_params, outcome,
};

/// Set on the child process to make it the fake server, naming its era.
const FAKE: &str = "PENGUIN_MAIL_FAKE_MCP";

/// The test binary, asked to run only [`fake_mcp_server`], as a server of
/// the given era.
fn fake(mode: &str) -> Transport {
    Transport::Stdio {
        command: std::env::current_exe()
            .expect("the test binary has a path")
            .display()
            .to_string(),
        args: [
            "tests::mcp::fake_mcp_server",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ]
        .map(String::from)
        .to_vec(),
        env: vec![(FAKE.into(), mode.into())],
    }
}

/// Does nothing in a normal run. With [`FAKE`] set, this process is an MCP
/// server on stdin and stdout until stdin closes. The test harness prints a
/// line of its own first, which the client skips as it skips any line of
/// stdout that is not JSON, and leaves it open, which the newline below
/// ends.
#[test]
fn fake_mcp_server() {
    let Ok(mode) = std::env::var(FAKE) else {
        return;
    };
    let modern = mode == "modern";
    if mode == "crash" {
        eprintln!("boom: no config file");
        std::process::exit(3);
    }
    let mut tools = vec!["echo", "fail", "grow", "picture", "pinged"];
    let mut initialized = false;
    let mut subscription: Option<Value> = None;
    let mut pinged = false;
    let mut out = std::io::stdout();
    writeln!(out).unwrap();
    let mut send = |message: Value| {
        writeln!(out, "{message}").unwrap();
        out.flush().unwrap();
    };
    for line in std::io::stdin().lock().lines() {
        let Ok(message) = serde_json::from_str::<Value>(&line.unwrap()) else {
            continue;
        };
        let id = message["id"].clone();
        let method = message["method"].as_str().unwrap_or_default().to_string();
        let params = &message["params"];
        if method.is_empty() {
            // The client's answer to our ping.
            pinged |= id == json!("s1") && message["result"] == json!({});
            continue;
        }
        if id.is_null() {
            if method == "notifications/initialized" {
                initialized = true;
                send(json!({"jsonrpc": "2.0", "id": "s1", "method": "ping"}));
            }
            continue;
        }
        let reply = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
        let error = |code: i64, text: &str| json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": text}});
        let meta = &params["_meta"]["io.modelcontextprotocol/protocolVersion"];
        if modern && method != "server/discover" && *meta != json!(PROTOCOL_VERSION) {
            send(error(-32602, "missing _meta"));
            continue;
        }
        let answer = match method.as_str() {
            "server/discover" if modern => reply(json!({
                "resultType": "complete",
                "supportedVersions": [PROTOCOL_VERSION],
                "capabilities": {"tools": {"listChanged": true}},
            })),
            "initialize" if !modern => {
                assert_eq!(params["protocolVersion"], json!(HANDSHAKE_VERSIONS[0]));
                // An older server answers with the revision it speaks.
                reply(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {"listChanged": true}},
                    "serverInfo": {"name": "fake", "version": "1"},
                }))
            }
            _ if !modern && !initialized => error(-32600, "not initialized"),
            "subscriptions/listen" => {
                subscription = Some(id.clone());
                send(
                    json!({"jsonrpc": "2.0", "method": "notifications/subscriptions/acknowledged",
                    "params": {"_meta": {"io.modelcontextprotocol/subscriptionId": id},
                        "notifications": {"toolsListChanged": true}}}),
                );
                continue;
            }
            "tools/list" => {
                // One tool a page, to make the client follow the cursor.
                let at: usize = params["cursor"].as_str().map_or(0, |c| c.parse().unwrap());
                let mut page = json!({"tools": [{
                    "name": tools[at],
                    "description": format!("The {} tool.", tools[at]),
                    "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
                }]});
                if at + 1 < tools.len() {
                    page["nextCursor"] = json!((at + 1).to_string());
                }
                reply(page)
            }
            "tools/call" => {
                let text = |t: &str| json!({"content": [{"type": "text", "text": t}]});
                match params["name"].as_str().unwrap() {
                    "echo" => reply(text(params["arguments"]["text"].as_str().unwrap())),
                    "fail" => reply(
                        json!({"content": [{"type": "text", "text": "no such file"}],
                        "isError": true}),
                    ),
                    "picture" => reply(json!({"content": [
                        {"type": "text", "text": "Here it is."},
                        {"type": "image", "data": "iVBOR", "mimeType": "image/png"},
                    ]})),
                    "pinged" => reply(text(if pinged { "yes" } else { "no" })),
                    "grow" => {
                        tools.push("extra");
                        let mut changed =
                            json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"});
                        if let Some(subscription) = &subscription {
                            changed["params"] = json!({"_meta":
                                {"io.modelcontextprotocol/subscriptionId": subscription}});
                        }
                        if modern == subscription.is_some() {
                            send(changed);
                        }
                        reply(text("grown"))
                    }
                    other => error(-32602, &format!("Unknown tool: {other}")),
                }
            }
            _ => error(-32601, "method not found"),
        };
        send(answer);
    }
    std::process::exit(0);
}

fn names(client: &McpClient) -> Vec<String> {
    client.tools().into_iter().map(|t| t.name).collect()
}

/// Waits until the tool list has been read again since `changes` was taken.
async fn changed(changes: &mut tokio::sync::watch::Receiver<u64>) {
    tokio::time::timeout(Duration::from_secs(10), changes.changed())
        .await
        .expect("the tool list changed in time")
        .unwrap();
}

#[tokio::test]
async fn a_modern_stdio_server_lists_tools_across_pages_and_runs_them() {
    let client = McpClient::connect("fake", fake("modern")).await.unwrap();
    assert_eq!(client.protocol_version(), PROTOCOL_VERSION);
    assert_eq!(
        names(&client),
        ["echo", "fail", "grow", "picture", "pinged"]
    );
    assert_eq!(client.tools()[0].description, "The echo tool.");

    assert_eq!(
        client.call("echo", json!({"text": "hello"})).await,
        ToolOutcome::Ok(json!("hello"))
    );
    assert_eq!(
        client.call("fail", json!({})).await,
        ToolOutcome::Err("no such file".into())
    );
    let ToolOutcome::Ok(Value::String(picture)) = client.call("picture", json!({})).await else {
        panic!("the picture tool answers");
    };
    assert_eq!(picture, "Here it is.\n\n[An image, image/png, not shown]");
    let ToolOutcome::Err(unknown) = client.call("nothing", json!({})).await else {
        panic!("an unknown tool fails");
    };
    assert!(unknown.contains("Unknown tool"), "{unknown}");
}

#[tokio::test]
async fn a_modern_stdio_server_refreshes_its_tools_when_they_change() {
    let client = McpClient::connect("fake", fake("modern")).await.unwrap();
    let mut changes = client.changes();
    assert_eq!(
        client.call("grow", json!({})).await,
        ToolOutcome::Ok(json!("grown"))
    );
    changed(&mut changes).await;
    assert!(names(&client).contains(&"extra".to_string()));
}

#[tokio::test]
async fn a_handshake_stdio_server_agrees_on_its_older_revision() {
    let client = McpClient::connect("fake", fake("handshake")).await.unwrap();
    assert_eq!(client.protocol_version(), "2025-06-18");
    assert_eq!(
        names(&client),
        ["echo", "fail", "grow", "picture", "pinged"]
    );
    assert_eq!(
        client.call("echo", json!({"text": "olá"})).await,
        ToolOutcome::Ok(json!("olá"))
    );
    // The server pinged the client after the handshake, and heard back.
    assert_eq!(
        client.call("pinged", json!({})).await,
        ToolOutcome::Ok(json!("yes"))
    );
    let mut changes = client.changes();
    client.call("grow", json!({})).await;
    changed(&mut changes).await;
    assert!(names(&client).contains(&"extra".to_string()));
}

#[tokio::test]
async fn a_command_that_does_not_exist_fails_to_start() {
    let transport = Transport::Stdio {
        command: "/nonexistent/penguin-mcp".into(),
        args: Vec::new(),
        env: Vec::new(),
    };
    let Err(McpError::Unreachable(why)) = McpClient::connect("gone", transport).await else {
        panic!("a missing command cannot start");
    };
    assert!(why.contains("could not start"), "{why}");
}

#[tokio::test]
async fn a_server_that_exits_says_what_it_printed() {
    let Err(McpError::Unreachable(why)) = McpClient::connect("crash", fake("crash")).await else {
        panic!("a server that exits cannot connect");
    };
    assert!(why.contains("boom: no config file"), "{why}");
}

#[tokio::test]
async fn dropping_the_client_stops_the_server() {
    let client = McpClient::connect("fake", fake("modern")).await.unwrap();
    assert!(client.is_alive());
    drop(client);
    // Nothing to observe from here but that dropping returns at once and
    // the process goes; a leaked child would hold the harness's stdout.
}

fn sse(messages: &[Value]) -> ResponseTemplate {
    let body: String = messages
        .iter()
        .map(|m| format!("event: message\ndata: {m}\n\n"))
        .collect();
    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
}

/// Answers each request with its own id, which wiremock cannot template.
struct Echo(Value, bool);

impl wiremock::Respond for Echo {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let message = json!({"jsonrpc": "2.0", "id": body["id"], "result": self.0});
        if self.1 {
            sse(&[
                json!({"jsonrpc": "2.0", "method": "notifications/progress",
                    "params": {"progressToken": 1, "progress": 1}}),
                message,
            ])
        } else {
            ResponseTemplate::new(200).set_body_json(message)
        }
    }
}

fn tool_list(names: &[&str]) -> Value {
    json!({"tools": names.iter().map(|n| json!({
        "name": n, "description": "", "inputSchema": {"type": "object"}})).collect::<Vec<_>>()})
}

#[tokio::test]
async fn a_modern_http_server_answers_in_json_and_in_a_stream() {
    let server = MockServer::start().await;
    let modern = || {
        Mock::given(method("POST"))
            .and(header("mcp-protocol-version", PROTOCOL_VERSION))
            .and(header("authorization", "Bearer s3cret"))
    };
    modern()
        .and(header("mcp-method", "server/discover"))
        .respond_with(Echo(
            json!({"supportedVersions": [PROTOCOL_VERSION, "2025-11-25"],
                "capabilities": {"tools": {"listChanged": true}}}),
            false,
        ))
        .mount(&server)
        .await;
    // The first listing, then the one after the change.
    modern()
        .and(header("mcp-method", "tools/list"))
        .respond_with(Echo(tool_list(&["search"]), true))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    modern()
        .and(header("mcp-method", "tools/list"))
        .respond_with(Echo(tool_list(&["search", "extra"]), true))
        .mount(&server)
        .await;
    modern()
        .and(header("mcp-method", "tools/call"))
        .and(header("mcp-name", "search"))
        .and(body_partial_json(json!({"params": {"_meta":
            {"io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION}}})))
        .respond_with(Echo(
            json!({"resultType": "complete",
                "content": [{"type": "text", "text": "3 issues"}]}),
            false,
        ))
        .mount(&server)
        .await;
    modern()
        .and(header("mcp-method", "subscriptions/listen"))
        .and(body_partial_json(
            json!({"params": {"notifications": {"toolsListChanged": true}}}),
        ))
        .respond_with(sse(&[
            json!({"jsonrpc": "2.0", "method": "notifications/subscriptions/acknowledged"}),
            json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
        ]))
        .mount(&server)
        .await;

    let transport = Transport::Http {
        url: format!("{}/mcp", server.uri()),
        token: Some("s3cret".into()),
    };
    let client = McpClient::connect("web", transport).await.unwrap();
    assert_eq!(client.protocol_version(), PROTOCOL_VERSION);
    assert_eq!(
        client.call("search", json!({"q": "bugs"})).await,
        ToolOutcome::Ok(json!("3 issues"))
    );
    // The listen stream said the tools changed, perhaps already.
    let mut changes = client.changes();
    if names(&client).len() == 1 {
        changed(&mut changes).await;
    }
    assert_eq!(names(&client), ["search", "extra"]);
}

#[tokio::test]
async fn a_handshake_http_server_gets_initialize_and_keeps_its_session() {
    let server = MockServer::start().await;
    // A server of the handshake era turns the unknown request away with a
    // plain 400.
    Mock::given(method("POST"))
        .and(header("mcp-method", "server/discover"))
        .respond_with(ResponseTemplate::new(400).set_body_string("Bad Request: no session"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({"method": "initialize",
            "params": {"protocolVersion": HANDSHAKE_VERSIONS[0]}})))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("mcp-session-id", "abc")
                // The client's second request, after the probe.
                .set_body_json(json!({"jsonrpc": "2.0", "id": 2, "result": {
                    "protocolVersion": "2025-11-25", "capabilities": {"tools": {}}}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(header("mcp-session-id", "abc"))
        .and(header("mcp-protocol-version", "2025-11-25"))
        .and(body_partial_json(
            json!({"method": "notifications/initialized"}),
        ))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(header("mcp-session-id", "abc"))
        .and(body_partial_json(json!({"method": "tools/list"})))
        .respond_with(Echo(tool_list(&["lookup"]), true))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(header("mcp-session-id", "abc"))
        .and(body_partial_json(json!({"method": "tools/call"})))
        .respond_with(Echo(
            json!({"content": [{"type": "text", "text": "found it"}]}),
            false,
        ))
        .mount(&server)
        .await;

    let transport = Transport::Http {
        url: server.uri(),
        token: None,
    };
    let client = McpClient::connect("old", transport).await.unwrap();
    assert_eq!(client.protocol_version(), "2025-11-25");
    assert_eq!(names(&client), ["lookup"]);
    assert_eq!(
        client.call("lookup", json!({})).await,
        ToolOutcome::Ok(json!("found it"))
    );
}

#[tokio::test]
async fn an_http_server_that_wants_a_sign_in_says_so() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let transport = Transport::Http {
        url: server.uri(),
        token: None,
    };
    assert_eq!(
        McpClient::connect("private", transport).await.err(),
        Some(McpError::Unauthorized)
    );
}

#[tokio::test]
async fn a_modern_server_without_our_revision_is_named_as_such() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "jsonrpc": "2.0", "id": 1,
            "error": {"code": -32022, "message": "Unsupported protocol version",
                "data": {"supported": ["2027-01-01"], "requested": PROTOCOL_VERSION}}})))
        .mount(&server)
        .await;
    let transport = Transport::Http {
        url: server.uri(),
        token: None,
    };
    assert_eq!(
        McpClient::connect("future", transport).await.err(),
        Some(McpError::Unsupported("2027-01-01".into()))
    );
}

#[test]
fn results_become_text_the_model_reads() {
    assert_eq!(
        outcome(&json!({"content": [
            {"type": "text", "text": "one"},
            {"type": "resource_link", "uri": "file:///a.txt", "name": "a.txt"},
            {"type": "resource", "resource": {"uri": "file:///b.txt", "text": "bee"}},
            {"type": "audio", "data": "", "mimeType": "audio/wav"},
        ]})),
        ToolOutcome::Ok(json!(
            "one\n\n[A link to a.txt: file:///a.txt]\n\nfile:///b.txt\nbee\n\n\
             [A sound, audio/wav, not played]"
        ))
    );
    assert_eq!(
        outcome(&json!({"content": [], "structuredContent": {"n": 2}})),
        ToolOutcome::Ok(json!("{\"n\":2}"))
    );
    assert_eq!(
        outcome(&json!({"content": [], "isError": true})),
        ToolOutcome::Err("The tool failed and said nothing more.".into())
    );
    assert!(matches!(
        outcome(&json!({"resultType": "input_required", "inputRequests": {}})),
        ToolOutcome::Err(_)
    ));
}

#[test]
fn marked_arguments_become_headers_and_bad_marks_are_refused() {
    let schema = json!({"type": "object", "properties": {
        "region": {"type": "string", "x-mcp-header": "Region"},
        "query": {"type": "string"},
        "where": {"type": "object", "properties": {
            "shard": {"type": "integer", "x-mcp-header": "Shard"}}},
    }});
    let marks = header_marks(&schema).unwrap();
    assert_eq!(
        mirrored_params(
            &marks,
            &json!({"region": "Lisboa ão", "where": {"shard": 4}})
        ),
        [
            ("Region".to_string(), "Lisboa ão".to_string()),
            ("Shard".to_string(), "4".to_string()),
        ]
    );
    assert_eq!(
        crate::mcp::http_header_value("Lisboa ão"),
        "=?base64?TGlzYm9hIMOjbw==?="
    );
    assert_eq!(crate::mcp::http_header_value("us-west1"), "us-west1");
    let spaced = json!({"properties": {"a": {"type": "string", "x-mcp-header": "Two Words"}}});
    assert!(header_marks(&spaced).is_err());
    let number = json!({"properties": {"a": {"type": "number", "x-mcp-header": "N"}}});
    assert!(header_marks(&number).is_err());
}
