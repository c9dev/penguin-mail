use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};

use super::{FakeHost, drain};
use crate::providers::ClaudeCodeChat;
use crate::{AgentEvent, AiError};

/// A stand-in `claude` that records its argv (NUL-separated) and stdin for
/// run N, then prints the canned stream-json in `out-N`.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
dir="$(dirname "$0")"
n=$(ls "$dir" | grep -c '^argv-')
printf '%s\0' "$@" > "$dir/argv-$n"
cat > "$dir/stdin-$n"
cat "$dir/out-$n"
"#;

fn write_script(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("claude");
    std::fs::write(&path, FAKE_CLAUDE).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn lines(messages: &[Value]) -> String {
    messages.iter().map(|m| format!("{m}\n")).collect()
}

fn argv(dir: &Path, n: usize) -> Vec<String> {
    let raw = std::fs::read_to_string(dir.join(format!("argv-{n}"))).unwrap();
    raw.split('\0').map(str::to_string).collect::<Vec<_>>()[..raw.matches('\0').count()].to_vec()
}

/// The value after `flag`.
fn flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let at = args.iter().position(|a| a == flag)?;
    args.get(at + 1).map(String::as_str)
}

#[tokio::test]
async fn runs_claude_headless_and_maps_its_stream() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let script = write_script(&bin);
    std::fs::write(
        bin.join("out-0"),
        lines(&[
            json!({"type": "system", "subtype": "init", "session_id": "sess-1",
                "tools": ["mcp__penguin-mail__search_mail"], "mcp_servers": [{"name": "penguin-mail", "status": "connected"}]}),
            // Recorded from `claude -p --output-format stream-json`: a
            // thinking block comes whole, with its signature.
            json!({"type": "assistant", "session_id": "sess-1", "message": {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "The user wants x.", "signature": "EqoBCkgIBxAB"},
            ]}}),
            json!({"type": "assistant", "session_id": "sess-1", "message": {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "", "signature": "EqoBCkgIBxAC"},
                {"type": "text", "text": "Looking."},
                {"type": "tool_use", "id": "tu_1", "name": "mcp__penguin-mail__search_mail", "input": {"query": "x"}},
            ]}}),
            json!({"type": "user", "session_id": "sess-1", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu_1", "content": [{"type": "text", "text": "{\"hits\":2}"}]},
            ]}}),
            json!({"type": "assistant", "session_id": "sess-1", "message": {"role": "assistant", "content": [
                {"type": "text", "text": "Two hits."},
            ]}}),
            json!({"type": "result", "subtype": "success", "is_error": false, "session_id": "sess-1",
                "result": "Two hits."}),
        ]),
    )
    .unwrap();
    std::fs::write(
        bin.join("out-1"),
        lines(&[
            json!({"type": "result", "subtype": "success", "is_error": true,
            "session_id": "sess-1", "result": "Usage limit reached"}),
        ]),
    )
    .unwrap();

    let work = dir.path().join("work");
    let bridge_command = dir.path().join("penguin-mail");
    let mut chat = ClaudeCodeChat::new(script, Some("sonnet".into()), "You sort mail.".into())
        .with_paths(work.clone(), bridge_command.clone());
    let (tx, rx) = async_channel::unbounded();
    let host = Arc::new(FakeHost::default());

    let reply = chat.send("-hello".into(), host.clone(), &tx).await.unwrap();
    assert_eq!(reply, "Two hits.");
    assert_eq!(
        drain(&rx),
        vec![
            AgentEvent::Thinking("The user wants x.".into()),
            AgentEvent::Text("Looking.".into()),
            AgentEvent::ToolStarted {
                id: "tu_1".into(),
                name: "search_mail".into(),
                input: json!({"query": "x"}),
            },
            AgentEvent::ToolFinished {
                id: "tu_1".into(),
                name: "search_mail".into(),
                ok: true,
                preview: "{\"hits\":2}".into(),
                output: "{\"hits\":2}".into(),
            },
            AgentEvent::Text("\n\nTwo hits.".into()),
        ]
    );

    let args = argv(&bin, 0);
    assert_eq!(args[0], "-p");
    assert_eq!(flag(&args, "--output-format"), Some("stream-json"));
    assert!(args.contains(&"--verbose".to_string()));
    assert!(args.contains(&"--strict-mcp-config".to_string()));
    assert_eq!(flag(&args, "--tools"), Some(""));
    assert_eq!(flag(&args, "--allowedTools"), Some("mcp__penguin-mail"));
    assert_eq!(flag(&args, "--permission-mode"), Some("dontAsk"));
    assert_eq!(flag(&args, "--system-prompt"), Some("You sort mail."));
    assert_eq!(flag(&args, "--model"), Some("sonnet"));
    assert_eq!(flag(&args, "--resume"), None);
    assert!(
        !args.contains(&"-hello".to_string()),
        "the prompt goes on stdin"
    );
    assert_eq!(
        std::fs::read_to_string(bin.join("stdin-0")).unwrap(),
        "-hello"
    );

    let config: Value = serde_json::from_str(flag(&args, "--mcp-config").unwrap()).unwrap();
    let server = &config["mcpServers"]["penguin-mail"];
    assert_eq!(server["type"], json!("stdio"));
    assert_eq!(server["command"], json!(bridge_command));
    assert_eq!(server["args"][0], json!("--mcp-bridge"));
    let socket = Path::new(server["args"][1].as_str().unwrap());
    assert!(socket.starts_with(&work));
    assert!(!socket.exists(), "the bridge closes when the turn ends");

    // The second turn resumes the session, and an error result fails the send.
    let err = chat.send("again".into(), host, &tx).await.unwrap_err();
    assert!(
        matches!(err, AiError::Api(ref m) if m == "Usage limit reached"),
        "{err}"
    );
    assert_eq!(flag(&argv(&bin, 1), "--resume"), Some("sess-1"));
}

#[tokio::test]
async fn web_search_turns_on_claude_codes_own_web_tools() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let script = write_script(&bin);
    let result = lines(&[json!({"type": "result", "subtype": "success",
        "is_error": false, "result": "Done.", "session_id": "sess-web"})]);
    std::fs::write(bin.join("out-0"), &result).unwrap();
    std::fs::write(bin.join("out-1"), &result).unwrap();
    let mut chat = ClaudeCodeChat::new(script, None, String::new())
        .with_paths(dir.path().join("work"), dir.path().join("bridge"));
    chat.web = true;
    let host = Arc::new(FakeHost::default());
    let (tx, _rx) = async_channel::unbounded();
    chat.send("news?".into(), host.clone(), &tx).await.unwrap();
    let args = argv(&bin, 0);
    assert_eq!(flag(&args, "--tools"), Some("WebSearch,WebFetch"));
    assert_eq!(
        flag(&args, "--allowedTools"),
        Some("mcp__penguin-mail,WebSearch,WebFetch")
    );
    // Turned off, the next run has no built-in tools again.
    chat.web = false;
    chat.send("again".into(), host, &tx).await.unwrap();
    let args = argv(&bin, 1);
    assert_eq!(flag(&args, "--tools"), Some(""));
    assert_eq!(flag(&args, "--allowedTools"), Some("mcp__penguin-mail"));
}

#[tokio::test]
async fn reports_a_crash_with_its_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("claude");
    std::fs::write(&script, "#!/bin/sh\necho 'not logged in' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut chat = ClaudeCodeChat::new(script, None, String::new())
        .with_paths(dir.path().join("work"), dir.path().join("bridge"));
    let (tx, _rx) = async_channel::unbounded();
    let err = chat
        .send("hi".into(), Arc::new(FakeHost::default()), &tx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not logged in"), "{err}");
}

/// Trimmed from the catalog the installed CLI wrote to
/// `~/.claude/cache/model-catalog/…-cc.json`, keeping the fields the picker
/// reads.
fn recorded_catalog() -> Value {
    json!({
        "version": 2,
        "catalog": {
            "surface": "cc",
            "config": {
                "id": "cc",
                "models": [
                    {"id": "claude-opus-5", "name": "Opus 5", "short_name": "Opus",
                     "description": "For complex tasks", "section": "main"},
                    {"id": "claude-fable-5-1", "name": "Fable 5.1", "short_name": "Fable",
                     "description": "For your toughest challenges", "section": "main",
                     "min_claude_code_version": "2.1.251"},
                    {"id": "claude-sonnet-5", "name": "Sonnet 5", "short_name": "Sonnet",
                     "description": "Most efficient for everyday tasks", "section": "main"},
                    {"id": "claude-haiku-4-5-20251001", "name": "Haiku 4.5",
                     "short_name": "Haiku", "description": "Fastest for quick answers",
                     "section": "main"}
                ]
            }
        }
    })
}

#[test]
fn reads_the_aliases_and_versions_from_the_cached_catalog() {
    let models = crate::providers::catalog_models(&recorded_catalog(), Some("2.1.278"));
    let listed: Vec<(&str, &str, bool)> = models
        .iter()
        .map(|m| (m.id.as_str(), m.name.as_str(), m.alias))
        .collect();
    assert_eq!(
        listed,
        vec![
            ("opus", "Opus, newest version (now Opus 5)", true),
            ("fable", "Fable, newest version (now Fable 5.1)", true),
            ("sonnet", "Sonnet, newest version (now Sonnet 5)", true),
            ("haiku", "Haiku, newest version (now Haiku 4.5)", true),
            ("claude-opus-5", "Opus 5", false),
            ("claude-fable-5-1", "Fable 5.1", false),
            ("claude-sonnet-5", "Sonnet 5", false),
            ("claude-haiku-4-5-20251001", "Haiku 4.5", false),
        ]
    );
}

#[test]
fn leaves_out_a_model_the_installed_cli_is_too_old_for() {
    let models = crate::providers::catalog_models(&recorded_catalog(), Some("2.1.100"));
    assert!(!models.iter().any(|m| m.id.contains("fable")), "{models:?}");
    assert!(models.iter().any(|m| m.id == "claude-opus-5"), "{models:?}");
    // Without a version to compare against, every model stays.
    let all = crate::providers::catalog_models(&recorded_catalog(), None);
    assert_eq!(all.len(), 8);
}

#[test]
fn a_catalog_with_no_models_lists_nothing() {
    assert!(crate::providers::catalog_models(&json!({}), Some("2.1.278")).is_empty());
}
