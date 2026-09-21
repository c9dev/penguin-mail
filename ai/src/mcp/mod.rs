//! A client for MCP servers the person adds, over stdio or Streamable HTTP.
//!
//! The protocol has two eras. From revision 2026-07-28 every request
//! carries its version, the client's identity and its capabilities in
//! `_meta`, and there is no handshake. Revisions up to 2025-11-25 open with
//! `initialize` and hold the answer for the connection. Most servers people
//! run today are of the second kind, so the client speaks both: it asks
//! `server/discover` first, as the spec says a client of both eras should,
//! and falls back to `initialize` when the server does not understand it.
//!
//! The client asks for no capabilities of its own: no sampling, no
//! elicitation, no roots. A server that needs one says so in an error, and
//! the model reads that error like any other.

mod http;
mod stdio;

#[cfg(test)]
pub(crate) use http::header_value as http_header_value;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::ToolOutcome;

/// The revision this client speaks by preference.
pub const PROTOCOL_VERSION: &str = "2026-07-28";

/// Revisions of the handshake era the client accepts, newest first. The
/// first is what `initialize` asks for.
pub const HANDSHAKE_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// How long a server has to start and list its tools.
pub const START_TIMEOUT: Duration = Duration::from_secs(30);
/// How long one tool call may take.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// How long `server/discover` waits before taking silence for a server of
/// the handshake era, which may ignore a request it does not know.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long before asking again for a change stream the server closed.
const FOLLOW_AGAIN: Duration = Duration::from_secs(30);
/// Pages of tools the client reads before it stops trusting the cursor.
const MAX_PAGES: usize = 50;

/// Error codes a modern server answers with, which tell it from a server
/// of the handshake era.
const HEADER_MISMATCH: i64 = -32020;
const MISSING_CAPABILITY: i64 = -32021;
const UNSUPPORTED_VERSION: i64 = -32022;

/// How to reach a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// A command the client starts, talking over its stdin and stdout.
    Stdio {
        command: String,
        args: Vec<String>,
        /// Added to the app's own environment.
        env: Vec<(String, String)>,
    },
    /// A URL the client POSTs to, with an optional bearer token.
    Http { url: String, token: Option<String> },
}

/// One tool a server offers, under the server's own name for it.
#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    pub name: String,
    /// A name for people, when the server gives one.
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Value,
    /// Argument paths the server wants mirrored into `Mcp-Param-*`
    /// headers, with the header name for each.
    headers: Vec<(Vec<String>, String)>,
}

/// Why a server could not be reached or used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpError {
    #[error("{0}")]
    Unreachable(String),
    #[error("the server did not answer in time")]
    Timeout,
    #[error("the server speaks only protocol versions {0}")]
    Unsupported(String),
    #[error("the server asks for a sign-in: add a token")]
    Unauthorized,
    #[error("the server said: {0}")]
    Refused(String),
}

/// A failure below the protocol, before it is worded for a person.
#[derive(Debug)]
enum Failure {
    Rpc {
        code: i64,
        message: String,
        data: Value,
    },
    Status {
        status: u16,
        body: String,
    },
    Closed(String),
    Timeout,
    Other(String),
}

impl Failure {
    fn rpc(error: &Value) -> Failure {
        Failure::Rpc {
            code: error["code"].as_i64().unwrap_or(0),
            message: error["message"].as_str().unwrap_or("error").to_string(),
            data: error.get("data").cloned().unwrap_or(Value::Null),
        }
    }

    /// Whether this is an error only a server of the 2026-07-28 era sends.
    fn is_modern(&self) -> bool {
        matches!(
            self,
            Failure::Rpc { code, .. }
                if matches!(*code, HEADER_MISMATCH | MISSING_CAPABILITY | UNSUPPORTED_VERSION)
        )
    }
}

impl From<Failure> for McpError {
    fn from(failure: Failure) -> McpError {
        match failure {
            Failure::Rpc { message, .. } => McpError::Refused(message),
            Failure::Status {
                status: 401 | 403, ..
            } => McpError::Unauthorized,
            Failure::Status { status, body } => {
                let body = body.trim();
                McpError::Refused(if body.is_empty() || body.len() > 200 {
                    format!("HTTP {status}")
                } else {
                    format!("HTTP {status}: {body}")
                })
            }
            Failure::Closed(why) | Failure::Other(why) => McpError::Unreachable(why),
            Failure::Timeout => McpError::Timeout,
        }
    }
}

/// The result of a JSON-RPC response message, or its error.
fn response(message: Value) -> Result<Value, Failure> {
    if let Some(error) = message.get("error").filter(|e| !e.is_null()) {
        return Err(Failure::rpc(error));
    }
    Ok(message.get("result").cloned().unwrap_or(Value::Null))
}

/// The reply to a request a server of the handshake era sent the client.
fn answer_server_request(id: Value, method: &str) -> Value {
    match method {
        "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": format!("Penguin Mail does not offer {method}")},
        }),
    }
}

enum Wire {
    Stdio(stdio::Stdio),
    Http(http::Http),
}

impl Wire {
    async fn request(
        &self,
        method: &str,
        params: Value,
        mirrored: http::Mirrored,
        limit: Duration,
    ) -> Result<Value, Failure> {
        match self {
            Wire::Stdio(wire) => wire.request(method, params, limit).await,
            Wire::Http(wire) => wire.request(method, params, mirrored, limit).await,
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), Failure> {
        match self {
            Wire::Stdio(wire) => wire.notify(method, params).await,
            Wire::Http(wire) => wire.notify(method, params).await,
        }
    }
}

/// Which era the server turned out to speak, and the revision agreed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Era {
    Modern(String),
    Handshake(String),
}

struct Inner {
    label: String,
    wire: Wire,
    era: Era,
    tools: Mutex<Vec<McpTool>>,
    changes: watch::Sender<u64>,
}

impl Inner {
    /// The params of a request, with the `_meta` a modern server needs.
    fn params(&self, params: Value) -> Value {
        match &self.era {
            Era::Modern(version) => with_meta(params, version),
            Era::Handshake(_) => params,
        }
    }

    async fn list_tools(&self) -> Result<Vec<McpTool>, Failure> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut params = json!({});
            if let Some(cursor) = &cursor {
                params["cursor"] = json!(cursor);
            }
            let page = self
                .wire
                .request(
                    "tools/list",
                    self.params(params),
                    http::Mirrored::default(),
                    START_TIMEOUT,
                )
                .await?;
            for raw in page["tools"].as_array().into_iter().flatten() {
                match tool(raw, matches!(self.wire, Wire::Http(_))) {
                    Ok(tool) => tools.push(tool),
                    Err(why) => {
                        tracing::warn!(server = %self.label, tool = %raw["name"], "skipping a tool: {why}")
                    }
                }
            }
            cursor = page["nextCursor"]
                .as_str()
                .filter(|c| !c.is_empty())
                .map(String::from);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    async fn refresh(&self) {
        match self.list_tools().await {
            Ok(tools) => {
                tracing::info!(server = %self.label, tools = tools.len(), "the server's tools changed");
                *self.tools.lock().expect("the tools lock is never poisoned") = tools;
                self.changes.send_modify(|n| *n += 1);
            }
            Err(failure) => {
                tracing::warn!(server = %self.label, ?failure, "could not list the server's tools again")
            }
        }
    }
}

/// A running connection to one server. Dropping it stops the server.
pub struct McpClient {
    inner: Arc<Inner>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for McpClient {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        match &self.inner.wire {
            Wire::Stdio(wire) => wire.stop(),
            Wire::Http(wire) => {
                if wire.session().is_some()
                    && let Ok(runtime) = tokio::runtime::Handle::try_current()
                {
                    let inner = Arc::clone(&self.inner);
                    runtime.spawn(async move {
                        if let Wire::Http(wire) = &inner.wire {
                            wire.close().await;
                        }
                    });
                }
            }
        }
    }
}

impl McpClient {
    /// Starts or reaches the server, agrees on a revision, and lists its
    /// tools, all within [`START_TIMEOUT`]. `label` names the server in the
    /// log. Must run inside a tokio runtime.
    pub async fn connect(label: &str, transport: Transport) -> Result<McpClient, McpError> {
        tokio::time::timeout(START_TIMEOUT, Self::open(label, transport))
            .await
            .map_err(|_| McpError::Timeout)?
    }

    async fn open(label: &str, transport: Transport) -> Result<McpClient, McpError> {
        let (sender, notifications) = mpsc::unbounded_channel();
        let wire = match &transport {
            Transport::Stdio { command, args, env } => {
                Wire::Stdio(stdio::Stdio::spawn(label, command, args, env, sender)?)
            }
            Transport::Http { url, token } => {
                Wire::Http(http::Http::new(url, token.clone(), sender)?)
            }
        };
        let (era, capabilities) = agree(&wire).await?;
        tracing::info!(server = %label, ?era, "connected to an MCP server");
        let inner = Arc::new(Inner {
            label: label.to_string(),
            wire,
            era,
            tools: Mutex::new(Vec::new()),
            changes: watch::channel(0).0,
        });
        let tools = inner.list_tools().await?;
        *inner
            .tools
            .lock()
            .expect("the tools lock is never poisoned") = tools;

        let mut tasks = vec![tokio::spawn(watch_notifications(
            Arc::clone(&inner),
            notifications,
        ))];
        if capabilities["tools"]["listChanged"].as_bool() == Some(true)
            && let Some(task) = follow_changes(&inner).await
        {
            tasks.push(task);
        }
        Ok(McpClient { inner, tasks })
    }

    /// The tools the server offers now, kept up to date as it says they
    /// change.
    pub fn tools(&self) -> Vec<McpTool> {
        self.inner
            .tools
            .lock()
            .expect("the tools lock is never poisoned")
            .clone()
    }

    /// Counts each time the tool list was read again, so a caller can wait
    /// for a change.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.inner.changes.subscribe()
    }

    /// The revision the two sides agreed on.
    pub fn protocol_version(&self) -> &str {
        match &self.inner.era {
            Era::Modern(version) | Era::Handshake(version) => version,
        }
    }

    /// False once a stdio server has exited.
    pub fn is_alive(&self) -> bool {
        match &self.inner.wire {
            Wire::Stdio(wire) => wire.alive(),
            Wire::Http(_) => true,
        }
    }

    /// Runs one tool, by the server's name for it, within [`CALL_TIMEOUT`].
    /// Text parts come back joined; other parts are named in a line each,
    /// since the model reads text.
    pub async fn call(&self, name: &str, arguments: Value) -> ToolOutcome {
        let headers = self
            .tools()
            .into_iter()
            .find(|tool| tool.name == name)
            .map(|tool| tool.headers)
            .unwrap_or_default();
        let mirrored = http::Mirrored {
            name: Some(name.to_string()),
            params: mirrored_params(&headers, &arguments),
        };
        let params = self
            .inner
            .params(json!({"name": name, "arguments": arguments}));
        let reply = self
            .inner
            .wire
            .request("tools/call", params, mirrored, CALL_TIMEOUT)
            .await;
        match reply {
            Ok(result) => outcome(&result),
            Err(failure) => ToolOutcome::Err(McpError::from(failure).to_string()),
        }
    }
}

/// Works out which era the server speaks and returns its capabilities.
async fn agree(wire: &Wire) -> Result<(Era, Value), McpError> {
    if let Wire::Http(http) = wire {
        http.set_version(Some(PROTOCOL_VERSION));
    }
    let probe = wire
        .request(
            "server/discover",
            with_meta(json!({}), PROTOCOL_VERSION),
            http::Mirrored::default(),
            PROBE_TIMEOUT,
        )
        .await;
    let offered: Vec<String> = match probe {
        Ok(found) => {
            let offered = strings(&found["supportedVersions"]);
            if offered.iter().any(|v| v == PROTOCOL_VERSION) {
                let capabilities = found["capabilities"].clone();
                return Ok((Era::Modern(PROTOCOL_VERSION.into()), capabilities));
            }
            offered
        }
        Err(Failure::Rpc {
            code: UNSUPPORTED_VERSION,
            data,
            ..
        }) => strings(&data["supported"]),
        Err(failure) if failure.is_modern() => return Err(failure.into()),
        // A server that is gone, or unreachable, will not answer the
        // handshake either, and saying so now is clearer than a timeout.
        Err(failure @ Failure::Closed(_)) => return Err(failure.into()),
        Err(Failure::Status {
            status: 401 | 403, ..
        }) => return Err(McpError::Unauthorized),
        Err(Failure::Other(why)) if matches!(wire, Wire::Http(_)) => {
            return Err(McpError::Unreachable(why));
        }
        // Anything else is how a server of the handshake era answers a
        // request it does not know.
        Err(_) => HANDSHAKE_VERSIONS.iter().map(|v| v.to_string()).collect(),
    };
    let Some(first) = HANDSHAKE_VERSIONS
        .iter()
        .find(|v| offered.iter().any(|o| o == *v))
    else {
        return Err(McpError::Unsupported(offered.join(", ")));
    };
    handshake(wire, first).await
}

/// The `initialize` exchange of revisions up to 2025-11-25.
async fn handshake(wire: &Wire, asked: &str) -> Result<(Era, Value), McpError> {
    if let Wire::Http(http) = wire {
        http.set_version(None);
    }
    let params = json!({
        "protocolVersion": asked,
        "capabilities": {},
        "clientInfo": client_info(),
    });
    let result = wire
        .request(
            "initialize",
            params,
            http::Mirrored::default(),
            START_TIMEOUT,
        )
        .await?;
    // The server answers with the revision it will speak, which may be an
    // older one than asked for.
    let version = result["protocolVersion"].as_str().unwrap_or_default();
    if !HANDSHAKE_VERSIONS.contains(&version) {
        return Err(McpError::Unsupported(version.to_string()));
    }
    if let Wire::Http(http) = wire {
        http.set_version(Some(version));
    }
    wire.notify("notifications/initialized", json!({})).await?;
    Ok((
        Era::Handshake(version.to_string()),
        result["capabilities"].clone(),
    ))
}

/// Reads the server's notifications, and lists the tools again when it
/// says they changed. Several changes in a row cost one listing.
async fn watch_notifications(inner: Arc<Inner>, mut notifications: mpsc::UnboundedReceiver<Value>) {
    while let Some(first) = notifications.recv().await {
        let mut changed = false;
        let mut next = Some(first);
        while let Some(message) = next {
            match message["method"].as_str() {
                Some("notifications/tools/list_changed") => changed = true,
                Some("notifications/message") => {
                    tracing::info!(server = %inner.label, "{}", message["params"]["data"]);
                }
                _ => {}
            }
            next = notifications.try_recv().ok();
        }
        if changed {
            inner.refresh().await;
        }
    }
}

/// Opens the stream a server sends change notifications on. A stdio server
/// of the handshake era sends them on stdout with no asking, so it needs
/// none.
async fn follow_changes(inner: &Arc<Inner>) -> Option<JoinHandle<()>> {
    let listen = json!({"notifications": {"toolsListChanged": true}});
    match (&inner.wire, &inner.era) {
        (Wire::Stdio(wire), Era::Modern(_)) => {
            // The response only comes when the server ends the stream, so
            // nobody waits on it; the notifications arrive on stdout.
            match wire
                .start("subscriptions/listen", inner.params(listen))
                .await
            {
                Ok(_) => None,
                Err(failure) => {
                    tracing::warn!(server = %inner.label, ?failure, "could not ask for tool changes");
                    None
                }
            }
        }
        (Wire::Stdio(_), Era::Handshake(_)) => None,
        (Wire::Http(_), era) => {
            let listen = matches!(era, Era::Modern(_)).then(|| inner.params(listen));
            let inner = Arc::clone(inner);
            Some(tokio::spawn(async move {
                let Wire::Http(wire) = &inner.wire else {
                    return;
                };
                loop {
                    match wire.follow(listen.clone()).await {
                        Ok(()) => {}
                        // The handshake era made the GET stream optional,
                        // and a server without one answers 405.
                        Err(Failure::Status { status: 405, .. }) => return,
                        Err(failure) => {
                            tracing::debug!(server = %inner.label, ?failure, "the change stream failed")
                        }
                    }
                    tokio::time::sleep(FOLLOW_AGAIN).await;
                }
            }))
        }
    }
}

fn client_info() -> Value {
    json!({"name": "penguin-mail", "version": env!("CARGO_PKG_VERSION")})
}

/// Puts the per-request fields of the 2026-07-28 revision into `_meta`.
fn with_meta(mut params: Value, version: &str) -> Value {
    if !params.is_object() {
        params = json!({});
    }
    let meta = params
        .as_object_mut()
        .expect("params is an object")
        .entry("_meta")
        .or_insert_with(|| json!({}));
    if let Some(meta) = meta.as_object_mut() {
        meta.insert(
            "io.modelcontextprotocol/protocolVersion".into(),
            json!(version),
        );
        meta.insert("io.modelcontextprotocol/clientInfo".into(), client_info());
        meta.insert(
            "io.modelcontextprotocol/clientCapabilities".into(),
            json!({}),
        );
    }
    params
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(String::from))
        .collect()
}

/// Reads one tool from `tools/list`. Over HTTP a tool whose `x-mcp-header`
/// marks break the spec's rules is left out, as the spec requires, so one
/// bad tool does not cost the rest.
fn tool(raw: &Value, http: bool) -> Result<McpTool, String> {
    let name = raw["name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .ok_or("it has no name")?;
    let input_schema = match &raw["inputSchema"] {
        Value::Object(_) => raw["inputSchema"].clone(),
        _ => json!({"type": "object"}),
    };
    let headers = if http {
        header_marks(&input_schema)?
    } else {
        Vec::new()
    };
    Ok(McpTool {
        name: name.to_string(),
        title: raw["title"]
            .as_str()
            .or_else(|| raw["annotations"]["title"].as_str())
            .map(String::from),
        description: raw["description"].as_str().unwrap_or_default().to_string(),
        input_schema,
        headers,
    })
}

/// The `x-mcp-header` marks in a schema, each with the chain of property
/// names that leads to it. Only a chain of `properties` keys counts.
pub(crate) fn header_marks(schema: &Value) -> Result<Vec<(Vec<String>, String)>, String> {
    fn walk(
        schema: &Value,
        path: &mut Vec<String>,
        found: &mut Vec<(Vec<String>, String)>,
    ) -> Result<(), String> {
        let Some(properties) = schema["properties"].as_object() else {
            return Ok(());
        };
        for (key, property) in properties {
            path.push(key.clone());
            if let Some(mark) = property.get("x-mcp-header") {
                let name = mark.as_str().unwrap_or_default();
                let token = !name.is_empty()
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
                if !token {
                    return Err(format!("x-mcp-header {mark} is not a header name"));
                }
                if !matches!(
                    property["type"].as_str(),
                    Some("string" | "integer" | "boolean")
                ) {
                    return Err(format!(
                        "x-mcp-header {name} marks a value that is not simple"
                    ));
                }
                if found.iter().any(|(_, n)| n.eq_ignore_ascii_case(name)) {
                    return Err(format!("x-mcp-header {name} appears twice"));
                }
                found.push((path.clone(), name.to_string()));
            }
            walk(property, path, found)?;
            path.pop();
        }
        Ok(())
    }
    let mut found = Vec::new();
    walk(schema, &mut Vec::new(), &mut found)?;
    Ok(found)
}

/// The `Mcp-Param-*` headers one call's arguments fill in.
pub(crate) fn mirrored_params(
    marks: &[(Vec<String>, String)],
    arguments: &Value,
) -> Vec<(String, String)> {
    marks
        .iter()
        .filter_map(|(path, name)| {
            let value = path.iter().try_fold(arguments, |at, key| at.get(key))?;
            let text = match value {
                Value::String(text) => text.clone(),
                Value::Bool(on) => on.to_string(),
                Value::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
                _ => return None,
            };
            Some((name.clone(), text))
        })
        .collect()
}

/// What the model reads of a `tools/call` result.
pub(crate) fn outcome(result: &Value) -> ToolOutcome {
    match result["resultType"].as_str() {
        None | Some("complete") => {}
        Some("input_required") => {
            return ToolOutcome::Err(
                "The server asked for input Penguin Mail cannot give, such as a form or a \
                 model's help."
                    .into(),
            );
        }
        Some(other) => return ToolOutcome::Err(format!("The server sent a {other} result.")),
    }
    let mut parts: Vec<String> = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(content_part)
        .collect();
    if parts.is_empty()
        && let Some(structured) = result.get("structuredContent").filter(|v| !v.is_null())
    {
        parts.push(structured.to_string());
    }
    let text = parts.join("\n\n");
    if result["isError"].as_bool() == Some(true) {
        ToolOutcome::Err(if text.is_empty() {
            "The tool failed and said nothing more.".into()
        } else {
            text
        })
    } else {
        ToolOutcome::Ok(Value::String(text))
    }
}

fn content_part(part: &Value) -> Option<String> {
    let mime = |part: &Value| {
        part["mimeType"]
            .as_str()
            .unwrap_or("unknown type")
            .to_string()
    };
    Some(match part["type"].as_str()? {
        "text" => part["text"].as_str()?.to_string(),
        "image" => format!("[An image, {}, not shown]", mime(part)),
        "audio" => format!("[A sound, {}, not played]", mime(part)),
        "resource_link" => format!(
            "[A link to {}: {}]",
            part["name"].as_str().unwrap_or("a resource"),
            part["uri"].as_str().unwrap_or_default()
        ),
        "resource" => {
            let resource = &part["resource"];
            let uri = resource["uri"].as_str().unwrap_or_default();
            match resource["text"].as_str() {
                Some(text) => format!("{uri}\n{text}"),
                None => format!("[A file at {uri}, {}, not shown]", mime(resource)),
            }
        }
        other => format!("[A part of type {other}, not shown]"),
    })
}

impl McpTool {
    /// A tool made by hand, for tests of code that lists tools.
    pub fn new(name: &str, description: &str, input_schema: Value) -> McpTool {
        McpTool {
            name: name.to_string(),
            title: None,
            description: description.to_string(),
            input_schema,
            headers: Vec::new(),
        }
    }
}
