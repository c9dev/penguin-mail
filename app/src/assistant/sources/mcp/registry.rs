//! Where MCP servers live while the app runs.
//!
//! A turn builds its sources from the settings and throws them away when it
//! ends, but a server is a process or a session worth keeping between
//! turns. So the servers belong to one registry for the whole process, the
//! way the keyring's keys belong to one cache: the pane reaches it through
//! `sources::for_settings`, the AI page reads each server's status from it,
//! and quitting empties it. Every call that hands it the settings also
//! stops the servers those settings no longer hold as they were.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use mailrs_ai::ToolOutcome;
use mailrs_ai::mcp::{McpClient, McpTool, Transport};
use serde_json::Value;

use super::{McpServer, McpSource, McpTransport};

/// How long a server that failed to start is left alone before a turn
/// tries it again. A server that hangs costs a turn its whole start
/// timeout, and paying that on every question is worse than waiting.
const RETRY_AFTER: Duration = Duration::from_secs(300);

/// How a server stands, for the AI page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Not started yet: it starts when the assistant first needs it.
    Waiting,
    Starting,
    Connected {
        tools: usize,
    },
    Failed(String),
}

static REGISTRY: LazyLock<Registry> = LazyLock::new(Registry::default);

/// The servers of this process.
pub fn registry() -> &'static Registry {
    &REGISTRY
}

#[derive(Default)]
pub struct Registry {
    servers: Mutex<BTreeMap<String, Arc<Running>>>,
    /// What the last Test said, for a server that is not running.
    tested: Mutex<BTreeMap<String, Status>>,
}

impl Registry {
    fn servers(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Arc<Running>>> {
        self.servers
            .lock()
            .expect("the server registry lock is never poisoned")
    }

    /// Stops every server the settings no longer hold as they were, and
    /// returns the enabled ones as sources.
    pub fn sources(&self, servers: &[McpServer]) -> Vec<Arc<McpSource>> {
        self.reconcile(servers);
        let mut running = self.servers();
        servers
            .iter()
            .filter(|server| server.enabled)
            .map(|server| {
                let entry = running
                    .entry(server.name.clone())
                    .or_insert_with(|| Arc::new(Running::new(server.clone())));
                Arc::new(McpSource {
                    server: Arc::clone(entry),
                })
            })
            .collect()
    }

    /// Stops the servers that were turned off, removed or changed.
    pub fn reconcile(&self, servers: &[McpServer]) {
        let stale: Vec<Arc<Running>> = {
            let mut running = self.servers();
            let gone: Vec<String> = running
                .iter()
                .filter(|(name, entry)| {
                    !servers
                        .iter()
                        .any(|s| s.enabled && &s.name == *name && s == &entry.config)
                })
                .map(|(name, _)| name.clone())
                .collect();
            gone.iter()
                .filter_map(|name| running.remove(name))
                .collect()
        };
        // Dropped outside the lock: stopping a server closes its pipes.
        for entry in stale {
            entry.stop();
        }
        self.tested
            .lock()
            .expect("the test results lock is never poisoned")
            .retain(|name, _| servers.iter().any(|s| &s.name == name));
    }

    /// Stops one server so that the next turn starts it afresh, as after
    /// its token changed.
    pub fn restart(&self, name: &str) {
        let entry = self.servers().remove(name);
        if let Some(entry) = entry {
            entry.stop();
        }
    }

    /// How a server stands now.
    pub fn status(&self, name: &str) -> Status {
        if let Some(entry) = self.servers().get(name) {
            let status = entry.status();
            if status != Status::Waiting {
                return status;
            }
        }
        self.tested
            .lock()
            .expect("the test results lock is never poisoned")
            .get(name)
            .cloned()
            .unwrap_or(Status::Waiting)
    }

    /// Connects to a server as the dialog describes it, lists its tools and
    /// disconnects. The answer is also what the AI page shows for it until
    /// the assistant starts it.
    pub async fn test(&self, server: McpServer, token: Option<String>) -> Status {
        let status = match McpClient::connect(&server.name, transport(&server, token)).await {
            Ok(client) => Status::Connected {
                tools: client.tools().len(),
            },
            Err(err) => Status::Failed(err.to_string()),
        };
        self.tested
            .lock()
            .expect("the test results lock is never poisoned")
            .insert(server.name, status.clone());
        status
    }

    /// Stops every server. The app calls this as it quits, since a static
    /// is never dropped and a stdio server would otherwise outlive it.
    pub fn stop_all(&self) {
        let all = std::mem::take(&mut *self.servers());
        for entry in all.into_values() {
            entry.stop();
        }
    }
}

/// The client's transport for a saved server, with its token.
fn transport(server: &McpServer, token: Option<String>) -> Transport {
    match &server.transport {
        McpTransport::Stdio { command, args, env } => Transport::Stdio {
            command: expand_home(command),
            args: args.clone(),
            env: env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        },
        McpTransport::Http { url } => Transport::Http {
            url: url.trim().to_string(),
            token,
        },
    }
}

/// `~/bin/server` as the shell would read it, since the command line is
/// typed the way a shell would take it.
fn expand_home(command: &str) -> String {
    match (command.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => std::path::Path::new(&home).join(rest).display().to_string(),
        _ => command.to_string(),
    }
}

/// One server the registry holds, started or not.
pub struct Running {
    pub(super) config: McpServer,
    client: Mutex<Option<Arc<McpClient>>>,
    status: Mutex<Status>,
    failed_at: Mutex<Option<Instant>>,
    /// Held while starting, so two turns at once start one process.
    starting: tokio::sync::Mutex<()>,
}

impl Running {
    fn new(config: McpServer) -> Running {
        Running {
            config,
            client: Mutex::new(None),
            status: Mutex::new(Status::Waiting),
            failed_at: Mutex::new(None),
            starting: tokio::sync::Mutex::new(()),
        }
    }

    fn client(&self) -> Option<Arc<McpClient>> {
        self.client
            .lock()
            .expect("the client lock is never poisoned")
            .clone()
    }

    fn set_status(&self, status: Status) {
        *self
            .status
            .lock()
            .expect("the status lock is never poisoned") = status;
    }

    fn status(&self) -> Status {
        match self.client() {
            Some(client) if client.is_alive() => Status::Connected {
                tools: client.tools().len(),
            },
            Some(_) => Status::Failed("The server stopped.".into()),
            None => self
                .status
                .lock()
                .expect("the status lock is never poisoned")
                .clone(),
        }
    }

    /// The tools the server offers now; none until it has started.
    pub(super) fn tools(&self) -> Vec<McpTool> {
        self.client()
            .filter(|client| client.is_alive())
            .map(|client| client.tools())
            .unwrap_or_default()
    }

    /// Starts the server unless it runs already, or failed a short while
    /// ago. A server that failed offers no tools, and its error waits on
    /// the AI page.
    pub(super) async fn start(&self) {
        let _one = self.starting.lock().await;
        if self.client().is_some_and(|client| client.is_alive()) {
            return;
        }
        let recent = self
            .failed_at
            .lock()
            .expect("the failure time lock is never poisoned")
            .is_some_and(|at| at.elapsed() < RETRY_AFTER);
        if recent {
            return;
        }
        self.set_status(Status::Starting);
        let key = self.config.token_key();
        let token = match self.config.transport {
            McpTransport::Http { .. } => {
                tokio::task::spawn_blocking(move || crate::assistant::read_key(&key))
                    .await
                    .ok()
                    .flatten()
            }
            McpTransport::Stdio { .. } => None,
        };
        let transport = transport(&self.config, token);
        match McpClient::connect(&self.config.name, transport).await {
            Ok(client) => {
                *self
                    .client
                    .lock()
                    .expect("the client lock is never poisoned") = Some(Arc::new(client));
                *self
                    .failed_at
                    .lock()
                    .expect("the failure time lock is never poisoned") = None;
            }
            Err(err) => {
                tracing::warn!(server = %self.config.name, error = %err, "could not start an MCP server");
                *self
                    .client
                    .lock()
                    .expect("the client lock is never poisoned") = None;
                *self
                    .failed_at
                    .lock()
                    .expect("the failure time lock is never poisoned") = Some(Instant::now());
                self.set_status(Status::Failed(err.to_string()));
            }
        }
    }

    pub(super) async fn call(&self, tool: &str, input: Value) -> ToolOutcome {
        match self.client() {
            Some(client) => client.call(tool, input).await,
            None => ToolOutcome::Err(format!(
                "The MCP server {} is not running.",
                self.config.name
            )),
        }
    }

    fn stop(&self) {
        self.client
            .lock()
            .expect("the client lock is never poisoned")
            .take();
        self.set_status(Status::Waiting);
    }
}
