//! The network discovery reads: DNS, HTTPS and TCP, behind one trait so
//! tests answer from memory.

use std::future::Future;

use crate::Security;

/// DNS, HTTPS and TCP, so tests answer from fakes.
pub trait Net: Send + Sync {
    /// The domain's mail exchangers, lowest preference first, as lower
    /// case names without the root dot. Empty when there are none.
    fn mx(&self, domain: &str) -> impl Future<Output = Vec<String>> + Send;
    /// The SRV records at `name`, as the server gave them, targets as
    /// they came (a target of `.` means the service is not offered).
    fn srv(&self, name: &str) -> impl Future<Output = Vec<SrvRecord>> + Send;
    /// The body of an HTTPS page, or `None` for anything but a success.
    fn get(&self, url: &str) -> impl Future<Output = Option<String>> + Send;
    /// Whether a TLS (or STARTTLS) connection to host:port succeeds with a valid certificate.
    fn reaches(
        &self,
        host: &str,
        port: u16,
        security: Security,
    ) -> impl Future<Output = bool> + Send;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SrvRecord {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    pub target: String,
}
