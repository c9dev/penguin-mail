use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::FakeHost;
use crate::bridge::{serve, serve_mcp};

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
async fn mcp_server_relays_tools_over_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let host = Arc::new(FakeHost::default());
    let bridge = serve(dir.path(), host.clone()).await.unwrap();
    assert_eq!(mode(&bridge.socket), 0o600);
    assert_eq!(mode(bridge.socket.parent().unwrap()), 0o700);

    let (mut client, server_side) = tokio::io::duplex(64 * 1024);
    let (server_read, server_write) = tokio::io::split(server_side);
    let socket = bridge.socket.clone();
    let server = tokio::spawn(async move { serve_mcp(&socket, server_read, server_write).await });

    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "claude-code", "version": "2.1.278"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "search_mail", "arguments": {"query": "invoice"}}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "fail"}}),
        json!({"jsonrpc": "2.0", "id": 5, "method": "ping"}),
        json!({"jsonrpc": "2.0", "id": 6, "method": "resources/list"}),
    ];
    for request in &requests {
        client
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
    }

    let (read, write) = tokio::io::split(client);
    let mut lines = BufReader::new(read).lines();
    let mut replies: HashMap<i64, Value> = HashMap::new();
    while replies.len() < 6 {
        let line = lines.next_line().await.unwrap().unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["jsonrpc"], json!("2.0"));
        replies.insert(reply["id"].as_i64().unwrap(), reply);
    }

    assert_eq!(
        replies[&1]["result"],
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "penguin-mail", "version": env!("CARGO_PKG_VERSION")},
        })
    );
    let tools = replies[&2]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], json!("search_mail"));
    assert_eq!(tools[0]["description"], json!("Searches mail."));
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["query"]));
    assert_eq!(
        replies[&3]["result"],
        json!({"content": [{"type": "text", "text": "{\"hits\":2}"}], "isError": false})
    );
    assert_eq!(
        replies[&4]["result"],
        json!({"content": [{"type": "text", "text": "no such label"}], "isError": true})
    );
    assert_eq!(replies[&5]["result"], json!({}));
    assert_eq!(replies[&6]["error"]["code"], json!(-32601));

    assert_eq!(
        host.calls(),
        vec![
            ("search_mail".into(), json!({"query": "invoice"})),
            ("fail".into(), json!({})),
        ]
    );

    // Closing stdin ends the server.
    drop(write);
    drop(lines);
    server.await.unwrap().unwrap();

    let socket = bridge.socket.clone();
    drop(bridge);
    assert!(!socket.exists());
    assert!(!socket.parent().unwrap().exists());
}

#[tokio::test]
async fn tool_calls_fail_cleanly_when_the_app_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("missing.sock");
    let (mut client, server_side) = tokio::io::duplex(4096);
    let (server_read, server_write) = tokio::io::split(server_side);
    let server = tokio::spawn(async move { serve_mcp(&socket, server_read, server_write).await });

    let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "x"}});
    client
        .write_all(format!("{call}\nnot json\n").as_bytes())
        .await
        .unwrap();
    let (read, write) = tokio::io::split(client);
    let mut lines = BufReader::new(read).lines();
    let mut replies = Vec::new();
    for _ in 0..2 {
        let line = lines.next_line().await.unwrap().unwrap();
        replies.push(serde_json::from_str::<Value>(&line).unwrap());
    }
    let call = replies.iter().find(|r| r["id"] == json!(1)).unwrap();
    assert_eq!(call["result"]["isError"], json!(true));
    let parse = replies.iter().find(|r| r["id"].is_null()).unwrap();
    assert_eq!(parse["error"]["code"], json!(-32700));
    drop(write);
    drop(lines);
    server.await.unwrap().unwrap();
}
