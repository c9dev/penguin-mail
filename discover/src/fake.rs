//! A network held in memory, which records every request it is asked.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::{Net, Security, SrvRecord};

/// One request discovery made, as it would have gone out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Mx(String),
    Srv(String),
    Get(String),
    Reaches(String, u16, Security),
}

impl Request {
    /// The DNS name, URL or host the request names.
    pub fn name(&self) -> &str {
        match self {
            Request::Mx(name) | Request::Srv(name) | Request::Get(name) => name,
            Request::Reaches(host, _, _) => host,
        }
    }
}

/// Answers from what a test put in and nothing else: no MX, no SRV, no
/// page and no server unless one was added. A delay keyed by a name, URL
/// or host holds that answer back, for tests run with tokio's clock
/// paused.
#[derive(Default)]
pub struct FakeNet {
    mx: HashMap<String, Vec<String>>,
    srv: HashMap<String, Vec<SrvRecord>>,
    pages: HashMap<String, String>,
    servers: HashSet<(String, u16, Security)>,
    delays: HashMap<String, Duration>,
    requests: Mutex<Vec<Request>>,
}

impl FakeNet {
    /// `domain` has these mail exchangers, best first.
    pub fn answer_mx(mut self, domain: &str, hosts: &[&str]) -> FakeNet {
        self.mx
            .insert(domain.into(), hosts.iter().map(|h| h.to_string()).collect());
        self
    }

    pub fn answer_srv(mut self, name: &str, records: Vec<SrvRecord>) -> FakeNet {
        self.srv.insert(name.into(), records);
        self
    }

    /// `url` answers with `body`.
    pub fn serve(mut self, url: &str, body: &str) -> FakeNet {
        self.pages.insert(url.into(), body.into());
        self
    }

    /// `host` takes connections on `port` with a valid certificate.
    pub fn accept(mut self, host: &str, port: u16, security: Security) -> FakeNet {
        self.servers.insert((host.into(), port, security));
        self
    }

    /// Holds back every answer about `name` (a DNS name, a URL or a host)
    /// by `by`.
    pub fn delay(mut self, name: &str, by: Duration) -> FakeNet {
        self.delays.insert(name.into(), by);
        self
    }

    /// Every request so far, in the order they were made.
    pub fn requests(&self) -> Vec<Request> {
        self.log().clone()
    }

    fn log(&self) -> MutexGuard<'_, Vec<Request>> {
        self.requests.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn record(&self, request: Request) {
        let delay = self.delays.get(request.name()).copied();
        self.log().push(request);
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
    }
}

impl Net for FakeNet {
    async fn mx(&self, domain: &str) -> Vec<String> {
        self.record(Request::Mx(domain.into())).await;
        self.mx.get(domain).cloned().unwrap_or_default()
    }

    async fn srv(&self, name: &str) -> Vec<SrvRecord> {
        self.record(Request::Srv(name.into())).await;
        self.srv.get(name).cloned().unwrap_or_default()
    }

    async fn get(&self, url: &str) -> Option<String> {
        self.record(Request::Get(url.into())).await;
        self.pages.get(url).cloned()
    }

    async fn reaches(&self, host: &str, port: u16, security: Security) -> bool {
        self.record(Request::Reaches(host.into(), port, security))
            .await;
        self.servers.contains(&(host.to_string(), port, security))
    }
}
