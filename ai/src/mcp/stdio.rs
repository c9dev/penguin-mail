//! The stdio transport: the server is a child process, and each message is
//! one line of JSON on its stdin or stdout.

use std::collections::{HashMap, VecDeque};
use std::process::Stdio as Piped;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::Failure;

/// How many of the server's last stderr lines an error quotes.
const STDERR_KEPT: usize = 5;

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>;
type Writer = Arc<tokio::sync::Mutex<Option<ChildStdin>>>;

pub(super) struct Stdio {
    child: Mutex<Option<Child>>,
    stdin: Writer,
    pending: Pending,
    next_id: AtomicI64,
    closed: Arc<AtomicBool>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    tasks: Vec<JoinHandle<()>>,
}

impl Stdio {
    /// Starts the server. Notifications it sends go to `notifications`.
    pub(super) fn spawn(
        label: &str,
        command: &str,
        args: &[String],
        env: &[(String, String)],
        notifications: mpsc::UnboundedSender<Value>,
    ) -> Result<Stdio, Failure> {
        let mut child = Command::new(command)
            .args(args)
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Piped::piped())
            .stdout(Piped::piped())
            .stderr(Piped::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Failure::Other(format!("could not start {command}: {e}")))?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr_pipe = child.stderr.take().expect("stderr is piped");
        let stdin: Writer = Arc::new(tokio::sync::Mutex::new(child.stdin.take()));
        let pending: Pending = Arc::default();
        let closed = Arc::new(AtomicBool::new(false));
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let (logged, stderr_done) = oneshot::channel();

        let reader = tokio::spawn(read_stdout(
            label.to_string(),
            BufReader::new(stdout),
            Output {
                pending: Arc::clone(&pending),
                stdin: Arc::clone(&stdin),
                closed: Arc::clone(&closed),
                notifications,
                stderr_done,
            },
        ));
        let logger = tokio::spawn(read_stderr(
            label.to_string(),
            BufReader::new(stderr_pipe),
            Arc::clone(&stderr),
            logged,
        ));
        Ok(Stdio {
            child: Mutex::new(Some(child)),
            stdin,
            pending,
            next_id: AtomicI64::new(1),
            closed,
            stderr,
            tasks: vec![reader, logger],
        })
    }

    pub(super) fn alive(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }

    /// Sends a request and hands back where its response will arrive.
    pub(super) async fn start(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(i64, oneshot::Receiver<Value>), Failure> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (reply, answer) = oneshot::channel();
        self.pending
            .lock()
            .expect("the pending map lock is never poisoned")
            .insert(id, reply);
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(failure) = self.write(&message).await {
            self.forget(id);
            return Err(failure);
        }
        Ok((id, answer))
    }

    pub(super) async fn request(
        &self,
        method: &str,
        params: Value,
        limit: Duration,
    ) -> Result<Value, Failure> {
        let (id, answer) = self.start(method, params).await?;
        match tokio::time::timeout(limit, answer).await {
            Ok(Ok(message)) => super::response(message),
            Ok(Err(_)) => Err(self.exited()),
            Err(_) => {
                self.forget(id);
                // The server should stop working on it, since nobody will
                // read the answer.
                let _ = self
                    .notify(
                        "notifications/cancelled",
                        json!({"requestId": id, "reason": "timed out"}),
                    )
                    .await;
                Err(Failure::Timeout)
            }
        }
    }

    pub(super) async fn notify(&self, method: &str, params: Value) -> Result<(), Failure> {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await
    }

    async fn write(&self, message: &Value) -> Result<(), Failure> {
        let mut line = message.to_string();
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        let Some(pipe) = stdin.as_mut() else {
            return Err(self.exited());
        };
        let written = match pipe.write_all(line.as_bytes()).await {
            Ok(()) => pipe.flush().await,
            Err(e) => Err(e),
        };
        written.map_err(|_| self.exited())
    }

    fn forget(&self, id: i64) {
        self.pending
            .lock()
            .expect("the pending map lock is never poisoned")
            .remove(&id);
    }

    /// Why the server stopped answering, with the last thing it logged.
    fn exited(&self) -> Failure {
        let tail: Vec<String> = self
            .stderr
            .lock()
            .expect("the stderr lock is never poisoned")
            .iter()
            .cloned()
            .collect();
        Failure::Closed(if tail.is_empty() {
            "the server exited".to_string()
        } else {
            format!("the server exited: {}", tail.join(" "))
        })
    }

    /// Closes stdin, which tells a well-behaved server to exit, and kills
    /// the process in case it does not. Closing the pipe also reaches a
    /// grandchild, such as the node process `npx` starts, which killing
    /// `npx` alone would leave running.
    pub(super) fn stop(&self) {
        if let Ok(mut stdin) = self.stdin.try_lock() {
            stdin.take();
        }
        if let Some(mut child) = self
            .child
            .lock()
            .expect("the child lock is never poisoned")
            .take()
        {
            let _ = child.start_kill();
        }
        self.closed.store(true, Ordering::SeqCst);
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Drop for Stdio {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What the stdout reader shares with the rest of the transport.
struct Output {
    pending: Pending,
    stdin: Writer,
    closed: Arc<AtomicBool>,
    notifications: mpsc::UnboundedSender<Value>,
    /// Fires once stderr has closed, so an error can quote its last lines.
    stderr_done: oneshot::Receiver<()>,
}

async fn read_stdout(label: String, stdout: BufReader<tokio::process::ChildStdout>, out: Output) {
    let Output {
        pending,
        stdin,
        closed,
        notifications,
        stderr_done,
    } = out;
    let mut lines = stdout.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            // The spec keeps stdout for protocol messages, but a server
            // that prints a banner there should still work.
            tracing::debug!(server = %label, "skipping a line of MCP output that is not JSON");
            continue;
        };
        let id = message.get("id").filter(|id| !id.is_null()).cloned();
        let method = message.get("method").and_then(Value::as_str);
        match (id, method) {
            // A response to one of ours.
            (Some(id), None) => {
                let waiting = id.as_i64().and_then(|id| {
                    pending
                        .lock()
                        .expect("the pending map lock is never poisoned")
                        .remove(&id)
                });
                if let Some(reply) = waiting {
                    let _ = reply.send(message);
                }
            }
            // A request from the server, which only servers of the
            // handshake era send. The client declares no capabilities, so
            // only a ping can be answered.
            (Some(id), Some(method)) => {
                let reply = super::answer_server_request(id, method);
                let mut out = reply.to_string();
                out.push('\n');
                if let Some(pipe) = stdin.lock().await.as_mut() {
                    let _ = pipe.write_all(out.as_bytes()).await;
                    let _ = pipe.flush().await;
                }
            }
            (None, Some(_)) => {
                let _ = notifications.send(message);
            }
            (None, None) => {}
        }
    }
    // A server that exits says why on stderr, and the callers about to hear
    // it is gone should hear why too.
    let _ = tokio::time::timeout(Duration::from_secs(1), stderr_done).await;
    closed.store(true, Ordering::SeqCst);
    // Dropping every waiting sender tells each caller the server is gone.
    pending
        .lock()
        .expect("the pending map lock is never poisoned")
        .clear();
}

async fn read_stderr(
    label: String,
    stderr: BufReader<tokio::process::ChildStderr>,
    kept: Arc<Mutex<VecDeque<String>>>,
    done: oneshot::Sender<()>,
) {
    let mut lines = stderr.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::info!(server = %label, "{line}");
        let mut kept = kept.lock().expect("the stderr lock is never poisoned");
        if kept.len() == STDERR_KEPT {
            kept.pop_front();
        }
        kept.push_back(line);
    }
    let _ = done.send(());
}
