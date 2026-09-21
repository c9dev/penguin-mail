use std::collections::BTreeMap;

use mailrs_ai::ToolOutcome;
use mailrs_ai::mcp::McpTool;
use serde_json::json;

use super::registry::Registry;
use super::*;
use crate::settings::{Change, Settings};

fn tool(name: &str) -> McpTool {
    McpTool::new(name, "", json!({"type": "object"}))
}

fn names(server: &str, tools: &[&str]) -> Vec<String> {
    let tools: Vec<McpTool> = tools.iter().map(|t| tool(t)).collect();
    tool_names(server, &tools)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

#[test]
fn tools_are_named_after_their_server_in_characters_a_model_takes() {
    assert_eq!(
        names("github", &["list_issues", "get-file"]),
        ["github__list_issues", "github__get-file"]
    );
    assert_eq!(
        names("files", &["read.file", "read file", "ler ficheiro ç"]),
        [
            "files__read_file",
            "files__read_file_2",
            "files__ler_ficheiro__"
        ]
    );
    let long = names("server", &[&"x".repeat(100), &"x".repeat(90)]);
    assert!(long.iter().all(|n| n.len() <= 64), "{long:?}");
    assert_ne!(long[0], long[1]);
}

#[test]
fn a_name_maps_back_to_the_tool_it_stands_for() {
    let tools = [tool("read.file"), tool("read file")];
    let named = tool_names("files", &tools);
    assert_eq!(named[0].1.name, "read.file");
    assert_eq!(named[1].1.name, "read file");
}

#[test]
fn a_server_name_holds_what_a_tool_name_can() {
    assert!(valid_name("github"));
    assert!(valid_name("my-server_2"));
    assert!(!valid_name(""));
    assert!(!valid_name("my server"));
    assert!(!valid_name("a/b"));
}

#[test]
fn a_command_line_splits_the_way_a_shell_reads_it() {
    let line = split_command_line(
        r#"GITHUB_TOKEN=abc npx -y "@modelcontextprotocol/server-filesystem" '/home/ana/My Files' it\'s"#,
    )
    .unwrap();
    assert_eq!(line.command, "npx");
    assert_eq!(
        line.args,
        [
            "-y",
            "@modelcontextprotocol/server-filesystem",
            "/home/ana/My Files",
            "it's"
        ]
    );
    assert_eq!(
        line.env,
        BTreeMap::from([("GITHUB_TOKEN".to_string(), "abc".to_string())])
    );
    assert!(split_command_line("  ").is_err());
    assert!(split_command_line("server 'open").is_err());
    assert_eq!(split_command_line("run ''").unwrap().args, [""]);
}

#[test]
fn a_joined_command_line_reads_back_the_same() {
    let env = BTreeMap::from([("KEY".to_string(), "two words".to_string())]);
    let args = ["--dir".to_string(), "/it's here".to_string(), String::new()];
    let line = join_command_line(&env, "~/bin/server", &args);
    assert_eq!(
        line,
        r"KEY='two words' ~/bin/server --dir '/it'\''s here' ''"
    );
    let back = split_command_line(&line).unwrap();
    assert_eq!(
        (back.env, back.command, back.args),
        (env, "~/bin/server".to_string(), args.to_vec())
    );
}

#[test]
fn the_question_shows_what_the_call_sends() {
    assert_eq!(input_summary(&json!({})), "");
    assert_eq!(
        input_summary(&json!({"path": "/tmp"})),
        r#"{"path":"/tmp"}"#
    );
    let long = input_summary(&json!({"text": "word ".repeat(100)}));
    assert!(long.ends_with('…') && long.chars().count() == SUMMARY_CHARS + 1);
}

fn stdio(name: &str, command: &str, args: &[&str]) -> McpServer {
    McpServer {
        name: name.into(),
        enabled: true,
        transport: McpTransport::Stdio {
            command: command.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            env: BTreeMap::new(),
        },
    }
}

#[test]
fn servers_survive_the_settings_file() {
    let settings = Settings {
        mcp_servers: vec![
            stdio("files", "npx", &["-y", "server"]),
            McpServer {
                name: "linear".into(),
                enabled: false,
                transport: McpTransport::Http {
                    url: "https://mcp.linear.app/mcp".into(),
                },
            },
        ],
        ..Settings::default()
    };
    let written = toml::to_string(&settings).expect("settings serialise");
    let read: Settings = toml::from_str(&written).expect("and come back");
    assert_eq!(read.mcp_servers, settings.mcp_servers);
}

#[test]
fn saving_renaming_and_removing_a_server_keeps_the_allowed_tools_in_step() {
    let mut settings = Settings::default();
    Change::SaveMcpServer {
        was: None,
        server: stdio("files", "server", &[]),
    }
    .apply(&mut settings);
    settings.assistant_allowed_tools = vec![
        "mcp:files/files__read".into(),
        "mcp:filesystem/filesystem__read".into(),
    ];
    // Saving under the same name edits the server in place.
    Change::SaveMcpServer {
        was: Some("files".into()),
        server: stdio("files", "other-server", &[]),
    }
    .apply(&mut settings);
    assert_eq!(settings.mcp_servers.len(), 1);
    assert_eq!(settings.assistant_allowed_tools.len(), 2);

    Change::EnableMcpServer {
        name: "files".into(),
        on: false,
    }
    .apply(&mut settings);
    assert!(!settings.mcp_servers[0].enabled);

    Change::SaveMcpServer {
        was: Some("files".into()),
        server: stdio("docs", "other-server", &[]),
    }
    .apply(&mut settings);
    assert_eq!(settings.mcp_servers[0].name, "docs");
    assert_eq!(
        settings.assistant_allowed_tools,
        ["mcp:filesystem/filesystem__read"]
    );

    Change::RemoveMcpServer("docs".into()).apply(&mut settings);
    assert!(settings.mcp_servers.is_empty());
    // Changing servers leaves the window as it is.
    assert!(
        Change::SaveMcpServer {
            was: None,
            server: stdio("x", "y", &[]),
        }
        .apply(&mut settings)
        .is_empty()
    );
}

/// A stdio server in a few lines of shell, answering the client's first
/// three requests in the order it sends them: the discovery probe, the
/// tool list, and one call.
const CANNED: &str = r#"
read -r probe
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"supportedVersions":["2026-07-28"],"capabilities":{"tools":{}}}}'
read -r list
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"read file","description":"Reads a file.","inputSchema":{"type":"object"}}]}}'
read -r call
case "$call" in
  *'"name":"read file"'*) printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"hello"}]}}' ;;
  *) printf '%s\n' '{"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"wrong tool"}}' ;;
esac
read -r rest
"#;

#[tokio::test]
async fn a_server_starts_on_first_use_and_offers_its_tools_under_its_name() {
    let registry = Registry::default();
    let server = stdio("files", "sh", &["-c", CANNED]);
    let sources = registry.sources(std::slice::from_ref(&server));
    let source = &sources[0];
    assert_eq!(source.id(), "mcp:files");
    assert!(source.specs().is_empty(), "nothing runs before a turn");
    assert_eq!(registry.status("files"), Status::Waiting);

    source.prepare().await;
    let specs = source.specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].name, "files__read_file");
    assert_eq!(specs[0].description, "Reads a file.");
    assert_eq!(registry.status("files"), Status::Connected { tools: 1 });

    let question = source
        .ask("files__read_file", &json!({"path": "/tmp/a"}))
        .expect("every call asks");
    assert!(question.contains("read file") && question.contains("files"));
    assert!(question.contains(r#"{"path":"/tmp/a"}"#), "{question}");
    // The call reaches the server under the server's own name for the tool.
    assert_eq!(
        source.call("files__read_file".into(), json!({})).await,
        ToolOutcome::Ok(json!("hello"))
    );

    // A second turn shares the running server.
    let again = registry.sources(std::slice::from_ref(&server));
    assert_eq!(again[0].specs().len(), 1);

    // Turning it off stops it.
    let off = McpServer {
        enabled: false,
        ..server
    };
    assert!(registry.sources(&[off]).is_empty());
    assert_eq!(registry.status("files"), Status::Waiting);
}

#[tokio::test]
async fn a_server_that_fails_to_start_offers_nothing_and_says_why() {
    let registry = Registry::default();
    let server = stdio("broken", "/nonexistent/penguin-mcp-server", &[]);
    let sources = registry.sources(&[server]);
    sources[0].prepare().await;
    assert!(sources[0].specs().is_empty());
    let Status::Failed(why) = registry.status("broken") else {
        panic!("the failure is kept for the AI page");
    };
    assert!(why.contains("could not start"), "{why}");
    // The next turn leaves it alone rather than waiting on it again.
    sources[0].prepare().await;
    assert!(matches!(registry.status("broken"), Status::Failed(_)));
}

#[tokio::test]
async fn a_test_reports_the_tool_count_or_the_error() {
    let registry = Registry::default();
    let good = stdio("good", "sh", &["-c", CANNED]);
    assert_eq!(
        registry.test(good, None).await,
        Status::Connected { tools: 1 }
    );
    assert_eq!(registry.status("good"), Status::Connected { tools: 1 });
    let bad = stdio("bad", "/nonexistent/penguin-mcp-server", &[]);
    assert!(matches!(registry.test(bad, None).await, Status::Failed(_)));
}
