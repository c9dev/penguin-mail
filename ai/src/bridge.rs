//! Lets Claude Code call the app's tools. The app serves its [`ToolHost`]
//! on a private Unix socket, or a named pipe on Windows; Claude Code
//! starts the app binary as an MCP server over stdio
//! (`--mcp-bridge <socket>`), which relays each call.
//!
//! The socket speaks newline-delimited JSON. A request is
//! `{"id":n,"method":"specs"}` or
//! `{"id":n,"method":"call","name":"...","input":{...}}`; the reply is
//! `{"id":n,"result":...}` or `{"id":n,"error":"..."}`.

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::task::{JoinHandle, JoinSet};

/// The transport the bridge listens on. On Unix it is a socket file made
/// private to this user; on Windows a named pipe, which is local to the
/// machine and denies remote clients.
#[cfg(unix)]
mod transport {
    use std::path::Path;

    use tokio::net::{UnixListener, UnixStream};

    pub type Listener = UnixListener;

    pub fn bind(path: &Path) -> std::io::Result<Listener> {
        UnixListener::bind(path)
    }

    pub async fn accept(listener: &mut Listener) -> std::io::Result<UnixStream> {
        listener.accept().await.map(|(stream, _)| stream)
    }

    pub async fn connect(path: &Path) -> std::io::Result<UnixStream> {
        UnixStream::connect(path).await
    }
}

/// One named-pipe instance serves one connection, so accepting hands out
/// the connected instance and queues a new one to wait on.
#[cfg(windows)]
mod transport {
    use std::mem;
    use std::path::{Path, PathBuf};

    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    pub struct Listener {
        name: PathBuf,
        next: NamedPipeServer,
    }

    pub fn bind(path: &Path) -> std::io::Result<Listener> {
        let next = options().first_pipe_instance(true).create(path)?;
        Ok(Listener {
            name: path.to_path_buf(),
            next,
        })
    }

    pub async fn accept(listener: &mut Listener) -> std::io::Result<NamedPipeServer> {
        listener.next.connect().await?;
        let next = options().create(&listener.name)?;
        Ok(mem::replace(&mut listener.next, next))
    }

    /// The pipe reports busy in the instant between one instance being
    /// handed out and the next being queued, so a connect retries briefly
    /// instead of failing on the gap.
    pub async fn connect(path: &Path) -> std::io::Result<NamedPipeClient> {
        const PIPE_BUSY: i32 = 231;
        for _ in 0..50 {
            match ClientOptions::new().open(path) {
                Err(err) if err.raw_os_error() == Some(PIPE_BUSY) => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                result => return result,
            }
        }
        ClientOptions::new().open(path)
    }

    fn options() -> ServerOptions {
        let mut options = ServerOptions::new();
        options.reject_remote_clients(true);
        options
    }
}

use crate::{ToolHost, ToolOutcome, ToolSpec};

/// The MCP server name Claude Code sees. Its tools appear to the model as
/// `mcp__penguin-mail__<tool>`.
pub const SERVER_NAME: &str = "penguin-mail";

/// A running socket server. Dropping it stops serving and removes the socket.
pub struct Bridge {
    pub socket: PathBuf,
    dir: PathBuf,
    task: JoinHandle<()>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.task.abort();
        // The Windows pipe lives outside the filesystem; aborting the
        // accept task drops its last instance and closes it.
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// Serves `host` on a new socket under `dir`, readable only by this user.
pub async fn serve(dir: &Path, host: Arc<dyn ToolHost>) -> std::io::Result<Bridge> {
    let private = make_private_dir(dir)?;
    let socket = socket_path(&private);
    let listener = match transport::bind(&socket) {
        Ok(listener) => listener,
        Err(e) => {
            let _ = std::fs::remove_dir(&private);
            return Err(e);
        }
    };
    #[cfg(unix)]
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    tracing::debug!(socket = %socket.display(), "tool bridge listening");
    let task = tokio::spawn(accept_loop(listener, host));
    Ok(Bridge {
        socket,
        dir: private,
        task,
    })
}

/// Where the bridge listens. A named pipe has no filesystem path, so on
/// Windows it takes the private directory's unique name.
#[cfg(unix)]
fn socket_path(dir: &Path) -> PathBuf {
    dir.join("tools.sock")
}

#[cfg(windows)]
fn socket_path(dir: &Path) -> PathBuf {
    let name = dir.file_name().unwrap_or_default().to_string_lossy();
    PathBuf::from(format!(r"\\.\pipe\penguin-mail-{name}"))
}

/// Creates a fresh 0700 directory under `parent`.
fn make_private_dir(parent: &Path) -> std::io::Result<PathBuf> {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    std::fs::create_dir_all(parent)?;
    loop {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = parent.join(format!("bridge-{}-{n}", std::process::id()));
        let builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

async fn accept_loop(mut listener: transport::Listener, host: Arc<dyn ToolHost>) {
    // Dropping the set when the task is aborted ends every open connection.
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = transport::accept(&mut listener) => match accepted {
                Ok(stream) => {
                    connections.spawn(serve_connection(stream, host.clone()));
                }
                Err(e) => {
                    tracing::warn!("tool bridge stopped accepting: {e}");
                    return;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

async fn serve_connection<S>(stream: S, host: Arc<dyn ToolHost>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let reply = answer(&line, &host).await;
        let mut out = reply.to_string();
        out.push('\n');
        if write.write_all(out.as_bytes()).await.is_err() {
            return;
        }
    }
}

async fn answer(line: &str, host: &Arc<dyn ToolHost>) -> Value {
    let request: Value = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(e) => return json!({"id": null, "error": format!("bad request: {e}")}),
    };
    let id = request["id"].clone();
    match request["method"].as_str() {
        Some("specs") => json!({"id": id, "result": host.specs()}),
        Some("call") => {
            let Some(name) = request["name"].as_str() else {
                return json!({"id": id, "error": "call needs a tool name"});
            };
            let input = match &request["input"] {
                Value::Null => json!({}),
                input => input.clone(),
            };
            match host.call(name.to_string(), input).await {
                ToolOutcome::Ok(result) => json!({"id": id, "result": result}),
                ToolOutcome::Err(error) => json!({"id": id, "error": error}),
            }
        }
        _ => json!({"id": id, "error": "unknown method"}),
    }
}

/// Sends one request over a new connection and waits for its reply.
async fn request(socket: &Path, body: Value) -> Result<Value, String> {
    let unreachable = |e: std::io::Error| format!("the mail app's tool bridge is unreachable: {e}");
    let stream = transport::connect(socket).await.map_err(unreachable)?;
    let (read, mut write) = tokio::io::split(stream);
    let mut line = body.to_string();
    line.push('\n');
    write
        .write_all(line.as_bytes())
        .await
        .map_err(unreachable)?;
    let reply = BufReader::new(read)
        .lines()
        .next_line()
        .await
        .map_err(unreachable)?
        .ok_or_else(|| "the mail app closed the tool bridge".to_string())?;
    let reply: Value =
        serde_json::from_str(&reply).map_err(|e| format!("bad reply from the mail app: {e}"))?;
    match reply.get("error") {
        Some(Value::String(error)) => Err(error.clone()),
        Some(error) if !error.is_null() => Err(error.to_string()),
        _ => Ok(reply["result"].clone()),
    }
}

/// The MCP stdio server that Claude Code launches. Runs until stdin closes.
pub async fn run_mcp_stdio(socket: &Path) -> std::io::Result<()> {
    serve_mcp(socket, tokio::io::stdin(), tokio::io::stdout()).await
}

/// A minimal MCP server: JSON-RPC 2.0, one message per line, relaying tool
/// calls to the socket. Handles requests concurrently, since a call can wait
/// on the user.
pub(crate) async fn serve_mcp<R, W>(socket: &Path, input: R, mut output: W) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = BufReader::new(input).lines();
    let mut pending = FuturesUnordered::new();
    let mut open = true;
    while open || !pending.is_empty() {
        tokio::select! {
            line = lines.next_line(), if open => match line? {
                Some(line) => {
                    if let Some(reply) = mcp_reply(socket.to_path_buf(), line) {
                        pending.push(reply);
                    }
                }
                None => open = false,
            },
            Some(reply) = pending.next(), if !pending.is_empty() => {
                write_message(&mut output, &reply).await?;
            }
        }
    }
    Ok(())
}

async fn write_message<W: AsyncWrite + Unpin>(
    output: &mut W,
    message: &Value,
) -> std::io::Result<()> {
    let mut line = message.to_string();
    line.push('\n');
    output.write_all(line.as_bytes()).await?;
    output.flush().await
}

type Reply = std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send>>;

/// The reply to one incoming message, or `None` for notifications and
/// anything else that needs no answer.
fn mcp_reply(socket: PathBuf, line: String) -> Option<Reply> {
    if line.trim().is_empty() {
        return None;
    }
    let message: Value = match serde_json::from_str(&line) {
        Ok(message) => message,
        Err(e) => {
            let reply = rpc_error(Value::Null, -32700, &format!("parse error: {e}"));
            return Some(Box::pin(async move { reply }));
        }
    };
    // Notifications carry no id. A message with no method is a response,
    // and this server sends no requests, so it skips those too.
    let id = message.get("id").filter(|id| !id.is_null())?.clone();
    let method = message["method"].as_str()?.to_string();
    let params = message["params"].clone();
    Some(Box::pin(async move {
        match method.as_str() {
            "initialize" => rpc_result(id, initialize(&params)),
            "ping" => rpc_result(id, json!({})),
            "tools/list" => match request(&socket, json!({"id": 1, "method": "specs"})).await {
                Ok(specs) => rpc_result(id, json!({"tools": mcp_tools(specs)})),
                Err(e) => rpc_error(id, -32603, &e),
            },
            "tools/call" => rpc_result(id, call_tool(&socket, &params).await),
            _ => rpc_error(id, -32601, &format!("method not found: {method}")),
        }
    }))
}

fn initialize(params: &Value) -> Value {
    let version = params["protocolVersion"]
        .as_str()
        .unwrap_or("2025-06-18")
        .to_string();
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
    })
}

fn mcp_tools(specs: Value) -> Vec<Value> {
    let specs: Vec<ToolSpec> = serde_json::from_value(specs).unwrap_or_default();
    specs
        .into_iter()
        .map(|spec| {
            json!({
                "name": spec.name,
                "description": spec.description,
                "inputSchema": spec.input_schema,
            })
        })
        .collect()
}

async fn call_tool(socket: &Path, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or_default();
    let input = match &params["arguments"] {
        Value::Null => json!({}),
        arguments => arguments.clone(),
    };
    let call = json!({"id": 1, "method": "call", "name": name, "input": input});
    let (text, is_error) = match request(socket, call).await {
        Ok(Value::String(text)) => (text, false),
        Ok(result) => (result.to_string(), false),
        Err(error) => (error, true),
    };
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": is_error,
    })
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}
