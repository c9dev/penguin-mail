//! From an email address to the servers that hold its mail. Discovery
//! asks the built-in provider table, the domain's MX records, autoconfig
//! files, SRV records and a probe, and sends nothing but the address's
//! domain to anyone. It knows nothing of the store or the window.

mod autoconfig;
mod find;
mod name;
mod net;
mod probe;
mod srv;
mod table;

/// Answers DNS, HTTPS and TCP from memory. Discovery's own tests always
/// have it; anyone else asks for the `fake` feature.
#[cfg(any(test, feature = "fake"))]
pub mod fake;

pub use find::{STEP_LIMIT, find};
pub use net::{Net, RealNet, RealNetError, SrvRecord};
pub use table::provider_named;

use serde::Deserialize;

/// What discovery found for one address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub verdict: Verdict,
    /// Best first. Empty unless the verdict is `Servers`.
    pub candidates: Vec<Candidate>,
}

impl Found {
    pub(crate) fn nothing() -> Found {
        Found {
            verdict: Verdict::NothingFound,
            candidates: Vec::new(),
        }
    }

    /// `Servers` with `candidates`, or `None` when there are none, so a
    /// step that found no usable server counts as a step that found
    /// nothing.
    pub(crate) fn servers(candidates: Vec<Candidate>) -> Option<Found> {
        (!candidates.is_empty()).then_some(Found {
            verdict: Verdict::Servers,
            candidates,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Servers,
    /// The provider has no IMAP (Tuta, HEY) or the app cannot reach it yet (Proton Mail).
    Unreachable {
        provider: String,
        reason: Unreachable,
    },
    /// A Google Workspace domain, found by MX: the Google sign-in serves it.
    Google,
    /// A Microsoft 365 or Outlook domain: part 4 serves it.
    Microsoft,
    NothingFound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unreachable {
    NoImap,
    NotYet,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub source: Source,
    pub provider: Option<ProviderInfo>,
    pub imap: Server,
    pub smtp: Server,
    /// The person must confirm these host names before the password goes out.
    pub confirm: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Table,
    Mx,
    Autoconfig,
    Ispdb,
    MxAutoconfig,
    Srv,
    Probe,
    Manual,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub user_name: UserName,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Security {
    Tls,
    StartTls,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserName {
    Address,
    /// The local part first, then the full address if that fails (RFC 6186 section 4).
    LocalPartFirst,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderInfo {
    pub name: String,
    pub password: PasswordKind,
    pub app_password_url: Option<String>,
    pub enable_imap_url: Option<String>,
    pub documentation_url: Option<String>,
    pub files_sent_mail: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasswordKind {
    AppPassword,
    AccountPassword,
    AppPasswordWithTwoStep,
}

/// One candidate for each IMAP server with each SMTP server, in the order
/// given: the first IMAP server with every SMTP server, then the next.
pub(crate) fn pairs(
    source: Source,
    provider: Option<&ProviderInfo>,
    imap: &[Server],
    smtp: &[Server],
    confirm: bool,
) -> Vec<Candidate> {
    imap.iter()
        .flat_map(|imap| {
            smtp.iter().map(move |smtp| Candidate {
                source,
                provider: provider.cloned(),
                imap: imap.clone(),
                smtp: smtp.clone(),
                confirm,
            })
        })
        .collect()
}
