//! What the Add Account dialog decides, without widgets: whether an
//! address can be looked up, where discovery's answer leads, which of its
//! candidates gets tried and when a failed one hands off to the next, the
//! line about app passwords, the words for a failed sign-in, the Server
//! Settings form, and which answers still count. `post` decides the band
//! and the tiles, `lookup` the lookup page's checks. `ui::add_account`
//! draws what these say.

pub mod lookup;
pub mod post;

use std::cell::Cell;

use mailrs_discover::{
    Candidate, Found, PasswordKind, ProviderInfo, Security, Server, Source, Unreachable, UserName,
    Verdict,
};
use mailrs_domain::Account;
use mailrs_domain::translate::{fill, gettext};
use mailrs_imap::{CheckError, ImapError};
use mailrs_store::servers::{Saved, Servers};

use crate::keyring_plug::Plug;

/// An address the person typed, split at its last `@`. The domain is in
/// lower case, as DNS and the provider table read it; the local part
/// stays as typed, since some servers tell case apart there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub local: String,
    pub domain: String,
}

impl Address {
    /// `typed` as an address, or `None` unless it is a whole one: a local
    /// part without spaces and a domain of at least two labels.
    pub fn parse(typed: &str) -> Option<Address> {
        let (local, domain) = typed.trim().rsplit_once('@')?;
        let domain = domain.trim_end_matches('.').to_lowercase();
        let labels_ok = domain.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_alphanumeric() || c == '-')
        });
        let whole = !local.is_empty()
            && !local.contains(char::is_whitespace)
            && domain.contains('.')
            && labels_ok;
        whole.then(|| Address {
            local: local.to_string(),
            domain,
        })
    }

    pub fn full(&self) -> String {
        format!("{}@{}", self.local, self.domain)
    }
}

/// `address` with its domain corrected, when the domain is one typing
/// slip from one the provider table lists: a letter added, dropped,
/// changed, or two next to each other swapped. A domain the table lists
/// itself is never corrected, since ymail.com is one letter from
/// gmail.com and both are real. Where two listed domains are one slip
/// away, the table's order picks, which puts a provider's main domain
/// first.
pub fn suggestion(address: &Address) -> Option<Address> {
    let typed = address.domain.as_str();
    if mailrs_discover::listed_domains().any(|domain| domain == typed) {
        return None;
    }
    let domain = mailrs_discover::listed_domains().find(|domain| one_slip_apart(typed, domain))?;
    Some(Address {
        local: address.local.clone(),
        domain: domain.to_string(),
    })
}

/// Whether one insertion, deletion, substitution or swap of two
/// neighbours turns `a` into `b`.
fn one_slip_apart(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a == b {
        return false;
    }
    let (short, long) = if a.len() <= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    match long.len() - short.len() {
        0 => {
            let differ: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
            match differ[..] {
                [_] => true,
                [i, j] => j == i + 1 && a[i] == b[j] && a[j] == b[i],
                _ => false,
            }
        }
        1 => {
            let at = (0..short.len())
                .find(|&i| short[i] != long[i])
                .unwrap_or(short.len());
            short[at..] == long[at + 1..]
        }
        _ => false,
    }
}

/// The line step 1 shows above the button that takes the suggestion.
pub fn did_you_mean(suggested: &Address) -> String {
    fill(
        &gettext("Did you mean {address}?"),
        &[("address", &suggested.full())],
    )
}

/// What Continue on step 1 does with what was typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Continue {
    /// Look this address up.
    Look(Address),
    /// Offer this address in place of the one typed, and look nothing up.
    Suggest(Address),
    /// Stay on step 1 and say this.
    Say(String),
}

/// Continue for `typed`. `declined` is the address whose suggestion step
/// 1 already showed: continuing with it unchanged means the person kept
/// it, so it is looked up as typed.
pub fn on_continue(typed: &str, declined: Option<&Address>) -> Continue {
    let Some(address) = Address::parse(typed) else {
        return Continue::Say(not_an_address());
    };
    if declined == Some(&address) {
        return Continue::Look(address);
    }
    match suggestion(&address) {
        Some(better) => Continue::Suggest(better),
        None => Continue::Look(address),
    }
}

/// What step 1 says when the address is not a whole one.
pub fn not_an_address() -> String {
    gettext("Type the whole address, such as dana@example.com.")
}

/// What step 2 signs in to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    /// Who runs the servers: the provider table's name, or the address's
    /// domain for a server the table does not list.
    pub provider_name: String,
    pub info: Option<ProviderInfo>,
    pub imap: Server,
    pub smtp: Server,
    /// The user name typed in Server Settings for the incoming server,
    /// sent as typed. `None` logs in with the address, the way the
    /// server's user name rule says.
    pub imap_user: Option<String>,
    /// The same for the outgoing server.
    pub smtp_user: Option<String>,
    /// The person must say yes to these host names before the password
    /// goes out: discovery guessed them, or found them outside the domain.
    pub confirm: bool,
    /// Which discovery step found these servers, or `None` for servers
    /// kept from before or typed by hand.
    pub source: Option<Source>,
    /// Discovery's other candidates for this address, best first, still
    /// untried. A connection or a TLS failure on this proposal moves on
    /// to the first of these; a refused password does not.
    pub remaining: Vec<Candidate>,
}

/// Where discovery's answer takes the dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Step 2, to sign in to these servers.
    Password(Proposal),
    /// Stay on step 1 and say this.
    Say(String),
    /// Stay on step 1, say this, and take Server Settings away: the
    /// provider lets no other mail app in, so no server would help.
    Closed(String),
    /// A Google Workspace domain: the Google sign-in serves it.
    Google,
    /// An Outlook.com or Microsoft 365 domain: Microsoft's sign-in serves
    /// it.
    Microsoft,
    /// Nothing found: Server Settings, filled with a guess, under a line.
    Manual { proposal: Proposal, line: String },
}

/// What Add Account says for a Microsoft address in a build without
/// Microsoft's client.
pub fn no_microsoft_line() -> String {
    gettext("This copy of Penguin Mail was built without Microsoft sign-in.")
}

/// Where discovery's answer for `address` leads. The best candidate comes
/// first in `found`, so it is the one step 2 offers; the rest wait in its
/// `Proposal::remaining` for a connection or a TLS failure to reach for.
/// `microsoft` is whether the build can sign in to Microsoft.
pub fn after_discovery(found: Found, address: &Address, microsoft: bool) -> Next {
    match found.verdict {
        Verdict::Servers => {
            let mut candidates = found.candidates.into_iter();
            match candidates.next() {
                Some(best) => Next::Password(proposal_from(best, candidates.collect(), address)),
                None => nothing_found(address),
            }
        }
        Verdict::Unreachable {
            provider,
            reason: Unreachable::NoImap,
        } => Next::Closed(fill(
            &gettext("{provider} has no IMAP, so other mail apps cannot reach it."),
            &[("provider", &provider)],
        )),
        Verdict::Unreachable {
            provider,
            reason: Unreachable::NotYet,
        } => Next::Closed(fill(
            &gettext("Penguin Mail cannot reach {provider} yet."),
            &[("provider", &provider)],
        )),
        Verdict::Google => Next::Google,
        Verdict::Microsoft if microsoft => Next::Microsoft,
        Verdict::Microsoft => Next::Closed(no_microsoft_line()),
        Verdict::NothingFound => nothing_found(address),
    }
}

/// `candidate` as a proposal for `address`, with `remaining` kept for a
/// connection failure to reach for. The table and an MX match speak for
/// the provider by name (`Source::Table`, `Source::Mx`); every other
/// source only ever guessed at a domain's own servers, so the domain is
/// the truer name. An autoconfig file, for one, can carry a display name
/// of its own, a hoster's marketing name rather than the provider a
/// person would type, and that name must never stand in for the address's
/// domain in what the dialog calls the account.
fn proposal_from(candidate: Candidate, remaining: Vec<Candidate>, address: &Address) -> Proposal {
    let named_by_table = matches!(candidate.source, Source::Table | Source::Mx);
    let provider_name = named_by_table
        .then(|| candidate.provider.as_ref().map(|info| info.name.clone()))
        .flatten()
        .unwrap_or_else(|| address.domain.clone());
    Proposal {
        provider_name,
        info: candidate.provider,
        imap: candidate.imap,
        smtp: candidate.smtp,
        imap_user: None,
        smtp_user: None,
        confirm: candidate.confirm,
        source: Some(candidate.source),
        remaining,
    }
}

/// Server Settings' first guess: the built-in table's own servers for a
/// domain it lists, with no network at all; failing that, the host names
/// most servers use, on the ports with TLS from the first byte. A person
/// who presses "Set up manually" before a lookup finishes, or when one
/// found nothing, still gets a listed provider's real servers this way.
pub fn guess(address: &Address) -> Proposal {
    if let Some(candidate) = mailrs_discover::table_only(&address.domain)
        .candidates
        .into_iter()
        .next()
    {
        return proposal_from(candidate, Vec::new(), address);
    }
    let server = |host: String, port| Server {
        host,
        port,
        security: Security::Tls,
        user_name: UserName::Address,
    };
    Proposal {
        provider_name: address.domain.clone(),
        info: None,
        imap: server(format!("imap.{}", address.domain), 993),
        smtp: server(format!("smtp.{}", address.domain), 465),
        imap_user: None,
        smtp_user: None,
        confirm: false,
        source: None,
        remaining: Vec::new(),
    }
}

fn nothing_found(address: &Address) -> Next {
    Next::Manual {
        proposal: guess(address),
        line: fill(
            &gettext("Penguin Mail found no mail servers for {domain}. Enter them below."),
            &[("domain", &address.domain)],
        ),
    }
}

/// A page on the provider's site, with the words that open it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub label: String,
    pub url: String,
}

/// The line under the password when the provider wants an app password,
/// with the page that makes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub line: String,
    pub link: Option<Link>,
}

pub fn password_hint(proposal: &Proposal) -> Option<Hint> {
    let info = proposal.info.as_ref()?;
    let provider = [("provider", info.name.as_str())];
    let line = match info.password {
        PasswordKind::AppPassword => fill(&gettext("{provider} needs an app password."), &provider),
        PasswordKind::AppPasswordWithTwoStep => fill(
            &gettext("With two-step sign-in on, {provider} needs an app password."),
            &provider,
        ),
        PasswordKind::AccountPassword => return None,
    };
    let link = info.app_password_url.clone().map(|url| Link {
        label: fill(&gettext("Make one in {provider}'s settings."), &provider),
        url,
    });
    Some(Hint { line, link })
}

/// What the password field is called: App Password where nothing else
/// will do, Password otherwise.
pub fn password_title(proposal: &Proposal) -> String {
    match proposal.info.as_ref().map(|info| info.password) {
        Some(PasswordKind::AppPassword) => gettext("App Password"),
        _ => gettext("Password"),
    }
}

/// Whether Sign In can go: there is a password, the person said yes
/// to servers Penguin Mail guessed, and no sign-in is on its way.
pub fn can_sign_in(password: &str, proposal: &Proposal, confirmed: bool, running: bool) -> bool {
    !running && !password.is_empty() && (confirmed || !proposal.confirm)
}

/// Whether a sign-in is on its way. Sign In and Enter in the password
/// both start one, and a second run beside the first would send the
/// password twice and race it to the keyring.
#[derive(Default)]
pub struct Running(Cell<bool>);

impl Running {
    /// Starts a run, or says no while one is on its way.
    pub fn begin(&self) -> bool {
        !self.0.replace(true)
    }

    pub fn end(&self) {
        self.0.set(false);
    }

    pub fn is_on(&self) -> bool {
        self.0.get()
    }
}

/// One server as its row under the password shows it: host, port and
/// security.
pub fn server_row_line(server: &Server) -> String {
    fill(
        &gettext("{host} · port {port} · {security}"),
        &[
            ("host", &server.host),
            ("port", &server.port.to_string()),
            ("security", security_label(server.security)),
        ],
    )
}

/// Both servers in one line, for the row that stands for them after a
/// refused password: "imap and smtp.fastmail.com · TLS" when the hosts
/// differ only in their first label.
pub fn servers_summary(proposal: &Proposal) -> String {
    let (imap, smtp) = (&proposal.imap, &proposal.smtp);
    let rest = |host: &str| host.split_once('.').map(|(_, rest)| rest.to_string());
    let hosts = match (imap.host.split_once('.'), rest(&smtp.host)) {
        (Some((first, domain)), Some(other)) if domain == other => fill(
            &gettext("{first} and {host}"),
            &[("first", first), ("host", &smtp.host)],
        ),
        _ => fill(
            &gettext("{first} and {host}"),
            &[("first", &imap.host), ("host", &smtp.host)],
        ),
    };
    let security = if imap.security == smtp.security {
        security_label(imap.security).to_string()
    } else {
        fill(
            &gettext("{first} and {second}"),
            &[
                ("first", security_label(imap.security)),
                ("second", security_label(smtp.security)),
            ],
        )
    };
    fill(
        &gettext("{hosts} · {security}"),
        &[("hosts", &hosts), ("security", &security)],
    )
}

/// The line under the password page's heading: where the servers came
/// from, and what that sent anywhere.
pub fn found_line(proposal: &Proposal) -> String {
    match proposal.source {
        Some(Source::Table) => gettext(
            "Penguin Mail's own list knows this provider. Nothing has left this computer.",
        ),
        Some(Source::Mx) => gettext("Found by its mail servers. Nothing else was asked."),
        Some(Source::Autoconfig | Source::MxAutoconfig | Source::WellKnown) => {
            gettext("Found in the domain's own settings.")
        }
        Some(Source::Ispdb) => gettext("Found in Mozilla's provider list."),
        Some(Source::Srv | Source::Probe) => gettext("Found by trying common server names."),
        Some(Source::Manual) | None => gettext("The servers you gave."),
    }
}

/// How long the browser has left, as the waiting page shows it.
pub fn time_left(left: std::time::Duration) -> String {
    // Rounded up, so the last second reads 0:01 rather than 0:00.
    let seconds = left.as_millis().div_ceil(1000);
    fill(
        &gettext("{time} left"),
        &[("time", &format!("{}:{:02}", seconds / 60, seconds % 60))],
    )
}

/// A count from two to nine in words, as a sentence writes it, and
/// anything larger in figures.
pub fn small_number(count: usize) -> String {
    match count {
        2 => gettext("two"),
        3 => gettext("three"),
        4 => gettext("four"),
        5 => gettext("five"),
        6 => gettext("six"),
        7 => gettext("seven"),
        8 => gettext("eight"),
        9 => gettext("nine"),
        _ => count.to_string(),
    }
}

/// How far a folder's first sync has got: waiting, or how many
/// conversations are in.
pub fn folder_line(conversations: usize) -> String {
    if conversations == 0 {
        return gettext("Waiting");
    }
    mailrs_domain::translate::fill_plural(
        "{count} conversation",
        "{count} conversations",
        conversations,
        &[("count", &conversations.to_string())],
    )
}

/// One try at signing in, as the core takes it. It has no `Debug`, so
/// the password cannot reach a log line.
pub struct Attempt {
    pub address: String,
    pub provider_name: String,
    pub imap: Server,
    pub smtp: Server,
    /// What `mailrs_imap::check` signs in to the incoming server with:
    /// the user name typed for it in Server Settings, else the address.
    /// The server's `user_name` rule turns it into the names to try, and a
    /// typed name comes with the `Address` rule, so it goes as typed.
    pub imap_login: String,
    /// The same for the outgoing server.
    pub smtp_login: String,
    pub password: String,
}

/// The try Sign In makes. The password goes as typed: app passwords are
/// shown in groups with spaces, and trimming one changes it.
pub fn attempt(address: &Address, proposal: &Proposal, password: &str) -> Attempt {
    Attempt {
        address: address.full(),
        provider_name: proposal.provider_name.clone(),
        imap: proposal.imap.clone(),
        smtp: proposal.smtp.clone(),
        imap_login: proposal.imap_user.clone().unwrap_or_else(|| address.full()),
        smtp_login: proposal.smtp_user.clone().unwrap_or_else(|| address.full()),
        password: password.to_string(),
    }
}

/// What a failed attempt does next: try discovery's next candidate, or
/// show the failure at step 2 and wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Try this candidate next, with the same password. It carries its
    /// own `confirm`, so a candidate the person must still say yes to is
    /// never tried silently. Boxed, since a proposal is several times the
    /// size of a failure.
    TryNext(Box<Proposal>),
    /// Show this and wait for another try.
    Failed(Failure),
}

/// What `tried`'s failure does next, for `address`. Only a connection or
/// a TLS failure is worth another candidate: the port was blocked or the
/// certificate did not match, which says nothing about the password. A
/// refused password, or anything else a server said, would meet the next
/// candidate the same way it met this one, so it goes back to step 2 at
/// once instead of spending the password on a second server.
pub fn after_failure(err: &anyhow::Error, tried: &Proposal, address: &Address) -> Outcome {
    let worth_another_candidate = matches!(
        err.downcast_ref::<CheckError>(),
        Some(
            CheckError::Imap(ImapError::Network(_) | ImapError::Tls { .. })
                | CheckError::Smtp(ImapError::Network(_) | ImapError::Tls { .. })
        )
    );
    let next = worth_another_candidate
        .then(|| tried.remaining.split_first())
        .flatten();
    match next {
        Some((candidate, rest)) => Outcome::TryNext(Box::new(proposal_from(
            candidate.clone(),
            rest.to_vec(),
            address,
        ))),
        None => Outcome::Failed(failure(err, tried)),
    }
}

/// What step 2 says after a sign-in failed: a line, and the pages that
/// can help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The whole failure in one sentence, for a screen reader.
    pub line: String,
    /// The card's heading.
    pub title: String,
    /// What to do about it.
    pub body: String,
    /// The server's own words, shown as it sent them.
    pub said: Option<String>,
    pub links: Vec<Link>,
    /// Text a Copy button puts on the clipboard, such as a command to run.
    pub copy: Option<String>,
    pub kind: FailureKind,
}

/// Which page a failure puts the dialog on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureKind {
    /// The servers answered and turned the sign-in down.
    Refused,
    /// A server did not answer. `password_sent` says whether the password
    /// reached the other server first: the incoming one is tried first.
    Unreachable {
        host: String,
        port: u16,
        password_sent: bool,
    },
}

/// The words for `err`. A login the servers turned down has its own
/// words; anything else, such as the keyring refusing or an address a
/// Google account holds, already reads as a sentence.
pub fn failure(err: &anyhow::Error, proposal: &Proposal) -> Failure {
    failure_given(err, proposal, crate::keyring_plug::current())
}

/// `failure`, with the keyring plug as `plug` found it.
fn failure_given(err: &anyhow::Error, proposal: &Proposal, plug: Plug) -> Failure {
    if let Some(unplugged) = keyring_unplugged(err, plug) {
        return unplugged;
    }
    let could_not = gettext("Could not sign in");
    let Some(check) = err.downcast_ref::<CheckError>() else {
        return Failure::plain(err.to_string(), could_not.clone(), err.to_string());
    };
    // `check` says which server failed; a network error names that host,
    // since the other one may have answered.
    let (server, err) = match check {
        CheckError::Imap(err) => (&proposal.imap, err),
        CheckError::Smtp(err) => (&proposal.smtp, err),
    };
    let host = &server.host;
    let could_not_sign_in = |reason: &str| {
        let line = fill(
            &gettext("Could not sign in: {reason}"),
            &[("reason", reason)],
        );
        Failure::plain(line, could_not.clone(), reason.to_string())
    };
    match err {
        ImapError::Auth { text } => refused(
            gettext("The server refused the password."),
            text,
            proposal,
            refused_title(proposal),
            refused_body(proposal),
            false,
        ),
        ImapError::ImapDisabled { text } => refused(
            gettext("IMAP is off for this account."),
            text,
            proposal,
            gettext("IMAP is off for this account"),
            match &proposal.info {
                Some(info) => fill(
                    &gettext("Turn on IMAP in {provider}'s settings, then choose Sign In."),
                    &[("provider", &post::short_name(&info.name))],
                ),
                None => gettext("Turn on IMAP in your mail settings, then choose Sign In."),
            },
            true,
        ),
        // The spec's one line for a TLS failure. The detail, such as a
        // server without TLS 1.2, goes to the log with the error.
        ImapError::Tls { host, .. } => {
            let line = fill(
                &gettext("The server's certificate does not match {host}."),
                &[("host", host)],
            );
            Failure::plain(
                line.clone(),
                gettext("The certificate does not match"),
                line,
            )
        }
        ImapError::Network(reason) => Failure {
            line: fill(
                &gettext("Could not reach {host}: {reason}"),
                &[("host", host), ("reason", reason)],
            ),
            title: fill(
                &gettext("No answer on port {port}"),
                &[("port", &server.port.to_string())],
            ),
            body: gettext(
                "The server did not answer. Check the server name and port, or try again when you are online.",
            ),
            said: Some(reason.clone()),
            links: Vec::new(),
            copy: None,
            kind: FailureKind::Unreachable {
                host: host.clone(),
                port: server.port,
                // The incoming server is signed in to first, so an
                // outgoing server out of reach comes after the password
                // went to the incoming one.
                password_sent: matches!(check, CheckError::Smtp(_)),
            },
        },
        // The server answered, so it is reachable and the password may be
        // right; it holds a few connections per account and this one was
        // over the limit.
        ImapError::TooManyConnections { .. } => {
            let line = fill(
                &gettext(
                    "{host} answered but turned down another connection. Try again in a few minutes.",
                ),
                &[("host", host)],
            );
            Failure::plain(line.clone(), gettext("Too many connections"), line)
        }
        ImapError::Protocol(reason) | ImapError::Refused(reason) | ImapError::NoMailbox(reason) => {
            could_not_sign_in(reason)
        }
        ImapError::Unsupported(what) => could_not_sign_in(what),
        // A value the client refused before it went anywhere: the
        // proposal itself could not carry a login, which is not a
        // failure Server Settings or an app password can fix.
        ImapError::Invalid(reason) => could_not_sign_in(reason),
    }
}

/// The failure for a sign-in the keyring would not keep because the snap
/// cannot reach it: the command that connects the plug, with a Copy
/// button, in place of the keyring's own words. `None` for any other
/// failure, and outside a snap.
pub fn keyring_unplugged(err: &anyhow::Error, plug: Plug) -> Option<Failure> {
    if !plug.explains_refusal() || !crate::keyring_plug::is_keyring_refusal(err) {
        return None;
    }
    let command = crate::keyring_plug::COMMAND;
    let body = gettext(
        "Penguin Mail cannot save your sign-in until the snap can reach your keyring. \
         Run this command in a terminal, restart Penguin Mail, then add the account again.",
    );
    Some(Failure {
        line: format!("{body} {command}"),
        title: gettext("Connect Your Keyring"),
        body,
        said: Some(command.to_string()),
        links: Vec::new(),
        copy: Some(command.to_string()),
        kind: FailureKind::Refused,
    })
}

impl Failure {
    /// A failure with nothing from the server and no page to help.
    fn plain(line: String, title: String, body: String) -> Failure {
        Failure {
            line,
            title,
            body,
            said: None,
            links: Vec::new(),
            copy: None,
            kind: FailureKind::Refused,
        }
    }
}

/// The heading over a refused password: the provider by name where the
/// list knows it.
fn refused_title(proposal: &Proposal) -> String {
    match &proposal.info {
        Some(info) => fill(
            &gettext("{provider} refused the password"),
            &[("provider", &post::short_name(&info.name))],
        ),
        None => gettext("The server refused the password"),
    }
}

fn refused_body(proposal: &Proposal) -> String {
    match &proposal.info {
        Some(info) if info.password != PasswordKind::AccountPassword => fill(
            &gettext(
                "{provider} wants an app password here, not your website password. Make one, paste it above, then choose Sign In.",
            ),
            &[("provider", &post::short_name(&info.name))],
        ),
        _ => gettext("Check the password, then choose Sign In."),
    }
}

/// A turned-down login: `line`, the server's own words, and the page for
/// the likelier cause: IMAP left off when `imap_off` says the server said
/// so, else an account password where an app password is due.
fn refused(
    mut line: String,
    said: &str,
    proposal: &Proposal,
    title: String,
    body: String,
    imap_off: bool,
) -> Failure {
    let said = (!said.trim().is_empty()).then(|| {
        fill(
            &gettext("The server said: {reason}"),
            &[("reason", said.trim())],
        )
    });
    if let Some(said) = &said {
        line.push(' ');
        line.push_str(said);
    }
    // One page, as the approved card has: the page that turns IMAP on
    // when the server said it is off, else the app password page where
    // one is due, else the IMAP page.
    let links = proposal
        .info
        .as_ref()
        .and_then(|info| {
            let app_password = info
                .app_password_url
                .as_ref()
                .filter(|_| info.password != PasswordKind::AccountPassword)
                .map(|url| Link {
                    label: gettext("Make an App Password"),
                    url: url.clone(),
                });
            let imap = info.enable_imap_url.as_ref().map(|url| Link {
                label: gettext("Turn On IMAP"),
                url: url.clone(),
            });
            if imap_off {
                imap.or(app_password)
            } else {
                app_password.or(imap)
            }
        })
        .into_iter()
        .collect();
    Failure {
        line,
        title,
        body,
        said,
        links,
        copy: None,
        kind: FailureKind::Refused,
    }
}

/// What Server Settings holds for one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Typed {
    pub host: String,
    pub port: u16,
    pub security: Security,
    /// The user name typed for this server, as typed.
    pub user: String,
}

/// Which of the two servers a Server Settings group edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Incoming,
    Outgoing,
}

/// The port a server of `role` listens on for `security` unless its
/// provider says otherwise: 993 and 143 for IMAP, 465 and 587 for
/// submission (RFC 8314).
fn default_port(role: Role, security: Security) -> u16 {
    match (role, security) {
        (Role::Incoming, Security::Tls) => 993,
        (Role::Incoming, Security::StartTls) => 143,
        (Role::Outgoing, Security::Tls) => 465,
        (Role::Outgoing, Security::StartTls) => 587,
    }
}

/// The port after the Security row switched to `now`. A port that still
/// holds the other choice's default moves to this one's; a port the
/// person typed stays.
pub fn port_after_switch(role: Role, port: u16, now: Security) -> u16 {
    let other = match now {
        Security::Tls => Security::StartTls,
        Security::StartTls => Security::Tls,
    };
    if port == default_port(role, other) {
        default_port(role, now)
    } else {
        port
    }
}

/// The choices the Security row offers. Nothing here reaches a server
/// without TLS, so there is no plain choice.
pub const SECURITIES: [Security; 2] = [Security::Tls, Security::StartTls];

/// A protocol's name, so it is not translated.
pub fn security_label(security: Security) -> &'static str {
    match security {
        Security::Tls => "TLS",
        Security::StartTls => "STARTTLS",
    }
}

pub fn security_index(security: Security) -> u32 {
    match security {
        Security::Tls => 0,
        Security::StartTls => 1,
    }
}

pub fn security_at(index: u32) -> Security {
    match index {
        1 => Security::StartTls,
        _ => Security::Tls,
    }
}

/// Server Settings as a proposal for `address`, or the line saying what
/// to fix. A blank incoming user name leaves the server's own rule for
/// it, and a blank outgoing one takes the incoming one. Servers the
/// person typed need no second yes or fallback candidate. The provider
/// found for the address keeps naming the account only while both hosts
/// are still its own: another host is a server the person chose, so the
/// account takes the address's domain and none of the table's rules,
/// such as its server filing sent mail, carry over to it.
pub fn typed_servers(
    imap: &Typed,
    smtp: &Typed,
    before: &Proposal,
    address: &Address,
) -> Result<Proposal, String> {
    let host = |typed: &Typed, missing: String| -> Result<String, String> {
        let host = typed.host.trim().trim_end_matches('.').to_lowercase();
        if host.is_empty() {
            return Err(missing);
        }
        if host
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '/' | ':' | '@'))
        {
            return Err(fill(
                &gettext("{host} is not a server name."),
                &[("host", typed.host.trim())],
            ));
        }
        Ok(host)
    };
    let imap_host = host(imap, gettext("Type the incoming server's name."))?;
    let smtp_host = host(smtp, gettext("Type the outgoing server's name."))?;
    let typed_user = |typed: &Typed| {
        let user = typed.user.trim();
        (!user.is_empty()).then(|| user.to_string())
    };
    let imap_user = typed_user(imap);
    let smtp_user = typed_user(smtp).or_else(|| imap_user.clone());
    let server = |host: String, typed: &Typed, user: &Option<String>, rule: UserName| Server {
        host,
        port: typed.port,
        security: typed.security,
        user_name: if user.is_some() {
            UserName::Address
        } else {
            rule
        },
    };
    let same_hosts = imap_host == before.imap.host && smtp_host == before.smtp.host;
    let (provider_name, info) = if same_hosts {
        (before.provider_name.clone(), before.info.clone())
    } else {
        (address.domain.clone(), None)
    };
    Ok(Proposal {
        provider_name,
        info,
        imap: server(imap_host, imap, &imap_user, before.imap.user_name),
        smtp: server(smtp_host, smtp, &smtp_user, before.smtp.user_name),
        imap_user,
        smtp_user,
        confirm: false,
        source: None,
        remaining: Vec::new(),
    })
}

/// Step 2 for an account signing in again, from the servers it kept and
/// the user name each took last time. Each kept name becomes the rule
/// that gives it back from the address where one does: the address
/// itself, or its part before @ (iCloud's IMAP server takes that while
/// its SMTP server takes the address). A name no rule gives, typed in
/// Server Settings, goes to that server as typed again. There is no
/// discovery to fall back to, so nothing waits in `remaining`.
pub fn saved_proposal(account: &Account, saved: &Servers) -> Proposal {
    let local = account.email.rsplit_once('@').map(|(local, _)| local);
    // `server_of` gives the server the `Address` rule, which sends a
    // typed name as it is.
    let restore = |kept: &Saved| {
        let mut server = mailrs_sync::server_of(kept);
        let user = if kept.user_name == account.email {
            None
        } else if Some(kept.user_name.as_str()) == local {
            server.user_name = UserName::LocalPartFirst;
            None
        } else {
            Some(kept.user_name.clone())
        };
        (server, user)
    };
    let (imap, imap_user) = restore(&saved.imap);
    let (smtp, smtp_user) = restore(&saved.smtp);
    let provider_name = mailrs_discover::resolved_provider_name(account.provider_name());
    Proposal {
        info: mailrs_discover::provider_named(&provider_name),
        provider_name,
        imap,
        smtp,
        imap_user,
        smtp_user,
        confirm: false,
        source: None,
        remaining: Vec::new(),
    }
}

/// What step 2 says above the password for an account signing in again.
pub fn again_line(account: &Account) -> String {
    fill(
        &gettext("Penguin Mail needs the password for {address} again."),
        &[("address", &account.email)],
    )
}

/// Which question the dialog asked last. Continue and Sign In each take
/// a ticket, and an answer that comes back counts only while its ticket
/// is the last one handed out and the dialog is open. Editing the
/// address or closing the dialog leaves every answer on its way with
/// nowhere to go.
#[derive(Default)]
pub struct Asking {
    last: Cell<u64>,
    closed: Cell<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket(u64);

impl Asking {
    pub fn ask(&self) -> Ticket {
        let next = self.last.get() + 1;
        self.last.set(next);
        Ticket(next)
    }

    /// Makes every answer on its way stale.
    pub fn forget(&self) {
        self.last.set(self.last.get() + 1);
    }

    pub fn close(&self) {
        self.closed.set(true);
    }

    pub fn wants(&self, ticket: Ticket) -> bool {
        !self.closed.get() && self.last.get() == ticket.0
    }
}

#[cfg(test)]
mod tests {
    use mailrs_discover::{
        Candidate, Found, PasswordKind, ProviderInfo, Security, Server, Source, Unreachable,
        UserName, Verdict,
    };
    use mailrs_domain::{Account, AccountState, Provider};
    use mailrs_imap::{CheckError, ImapError};
    use mailrs_store::servers::{Saved, Servers};

    use super::*;

    fn fastmail_info() -> ProviderInfo {
        ProviderInfo {
            name: "Fastmail".into(),
            password: PasswordKind::AppPassword,
            app_password_url: Some("https://fastmail.example/app-passwords".into()),
            enable_imap_url: None,
            documentation_url: None,
            files_sent_mail: true,
        }
    }

    fn gmx_info() -> ProviderInfo {
        ProviderInfo {
            name: "GMX".into(),
            password: PasswordKind::AccountPassword,
            app_password_url: None,
            enable_imap_url: Some("https://gmx.example/imap".into()),
            documentation_url: None,
            files_sent_mail: true,
        }
    }

    fn server(host: &str, port: u16) -> Server {
        Server {
            host: host.into(),
            port,
            security: Security::Tls,
            user_name: UserName::Address,
        }
    }

    fn found(provider: Option<ProviderInfo>, source: Source, confirm: bool) -> Found {
        Found {
            verdict: Verdict::Servers,
            candidates: vec![Candidate {
                source,
                provider,
                imap: server("imap.example.org", 993),
                smtp: server("smtp.example.org", 465),
                confirm,
            }],
        }
    }

    fn verdict(verdict: Verdict) -> Found {
        Found {
            verdict,
            candidates: Vec::new(),
        }
    }

    fn dana() -> Address {
        Address::parse("Dana@fastmail.com").unwrap()
    }

    fn fastmail() -> Proposal {
        match after_discovery(found(Some(fastmail_info()), Source::Table, false), &dana(), true) {
            Next::Password(proposal) => proposal,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_address_is_trimmed_and_its_domain_put_in_lower_case() {
        let address = Address::parse("  Dana@FastMail.COM. ").unwrap();
        assert_eq!(address.local, "Dana");
        assert_eq!(address.domain, "fastmail.com");
        assert_eq!(address.full(), "Dana@fastmail.com");
    }

    #[test]
    fn half_an_address_is_never_looked_up() {
        for typed in [
            "",
            "dana",
            "dana@",
            "@fastmail.com",
            "dana@localhost",
            "dana smith@example.org",
            "dana@fast mail.com",
            "dana@-example.org",
            "dana@example..org",
        ] {
            assert_eq!(Address::parse(typed), None, "{typed}");
        }
    }

    #[test]
    fn a_provider_from_the_table_leads_to_the_password_step_under_its_name() {
        let proposal = fastmail();
        assert_eq!(proposal.provider_name, "Fastmail");
        assert!(!proposal.confirm);
        assert_eq!(proposal.imap.host, "imap.example.org");
    }

    #[test]
    fn a_server_nobody_lists_is_named_after_the_domain_and_asks_first() {
        let address = Address::parse("me@example.org").unwrap();
        let Next::Password(proposal) = after_discovery(found(None, Source::Srv, true), &address, true)
        else {
            panic!("expected the password step");
        };
        assert_eq!(proposal.provider_name, "example.org");
        assert!(proposal.confirm);
    }

    #[test]
    fn an_autoconfig_display_name_never_becomes_the_provider_name() {
        // Only the table and an MX match speak for the provider by name;
        // an autoconfig file's own display name is a hoster's word, not
        // one the person typed, so the domain names the account instead.
        let address = Address::parse("ann@example.org").unwrap();
        let info = ProviderInfo {
            name: "A Hoster's Mail Service".into(),
            ..gmx_info()
        };
        let found = found(Some(info), Source::Autoconfig, false);
        let Next::Password(proposal) = after_discovery(found, &address, true) else {
            panic!("expected the password step");
        };
        assert_eq!(proposal.provider_name, "example.org");
        // The hint under the password still speaks in the provider's own
        // words, since that is about the servers it found, not its name.
        assert_eq!(
            proposal.info.map(|info| info.name),
            Some("A Hoster's Mail Service".into())
        );
    }

    #[test]
    fn tuta_answers_at_the_first_step_in_the_specs_words() {
        let tuta = verdict(Verdict::Unreachable {
            provider: "Tuta".into(),
            reason: Unreachable::NoImap,
        });
        assert_eq!(
            after_discovery(tuta, &dana(), true),
            Next::Closed("Tuta has no IMAP, so other mail apps cannot reach it.".into())
        );
    }

    #[test]
    fn proton_mail_answers_that_it_cannot_be_reached_yet() {
        let proton = verdict(Verdict::Unreachable {
            provider: "Proton Mail".into(),
            reason: Unreachable::NotYet,
        });
        assert_eq!(
            after_discovery(proton, &dana(), true),
            Next::Closed("Penguin Mail cannot reach Proton Mail yet.".into())
        );
    }

    #[test]
    fn a_microsoft_domain_goes_to_microsofts_sign_in() {
        assert_eq!(
            after_discovery(verdict(Verdict::Microsoft), &dana(), true),
            Next::Microsoft
        );
        assert_eq!(
            after_discovery(verdict(Verdict::Microsoft), &dana(), false),
            Next::Closed("This copy of Penguin Mail was built without Microsoft sign-in.".into())
        );
        assert_eq!(
            after_discovery(verdict(Verdict::Google), &dana(), true),
            Next::Google
        );
        assert_eq!(
            no_microsoft_line(),
            "This copy of Penguin Mail was built without Microsoft sign-in."
        );
    }

    #[test]
    fn set_up_manually_before_a_lookup_still_finds_a_listed_provider() {
        let proposal = guess(&dana());
        assert_eq!(proposal.provider_name, "Fastmail");
        assert_eq!(proposal.imap, server("imap.fastmail.com", 993));
        assert_eq!(proposal.smtp, server("smtp.fastmail.com", 465));
        assert!(
            proposal.info.is_some_and(|info| info.password == PasswordKind::AppPassword),
            "Fastmail's own password rule, not the guess's default"
        );
    }

    #[test]
    fn guessing_an_unlisted_domain_still_falls_back_to_host_names() {
        let address = Address::parse("me@example.org").unwrap();
        assert_eq!(
            guess(&address),
            Proposal {
                provider_name: "example.org".into(),
                info: None,
                imap: server("imap.example.org", 993),
                smtp: server("smtp.example.org", 465),
                imap_user: None,
                smtp_user: None,
                confirm: false,
                source: None,
                remaining: Vec::new(),
            }
        );
    }

    #[test]
    fn nothing_found_opens_server_settings_with_a_guess() {
        let address = Address::parse("me@example.org").unwrap();
        let Next::Manual { proposal, line } =
            after_discovery(verdict(Verdict::NothingFound), &address, true)
        else {
            panic!("expected Server Settings");
        };
        assert_eq!(proposal.imap, server("imap.example.org", 993));
        assert_eq!(proposal.smtp, server("smtp.example.org", 465));
        assert_eq!(
            line,
            "Penguin Mail found no mail servers for example.org. Enter them below."
        );
    }

    #[test]
    fn fastmail_asks_for_an_app_password_and_links_the_page_that_makes_one() {
        let hint = password_hint(&fastmail()).expect("Fastmail wants an app password");
        assert_eq!(hint.line, "Fastmail needs an app password.");
        assert_eq!(
            hint.link,
            Some(Link {
                label: "Make one in Fastmail's settings.".into(),
                url: "https://fastmail.example/app-passwords".into(),
            })
        );
        assert_eq!(password_title(&fastmail()), "App Password");
    }

    #[test]
    fn a_provider_that_takes_the_account_password_gets_no_line() {
        let gmx = Proposal {
            info: Some(gmx_info()),
            ..fastmail()
        };
        assert_eq!(password_hint(&gmx), None);
        assert_eq!(password_title(&gmx), "Password");
    }

    #[test]
    fn sign_in_waits_for_a_password_and_a_yes_to_guessed_servers() {
        let guessed = Proposal {
            confirm: true,
            ..fastmail()
        };
        assert!(!can_sign_in("", &fastmail(), false, false));
        assert!(can_sign_in("pw", &fastmail(), false, false));
        assert!(!can_sign_in("pw", &guessed, false, false));
        assert!(can_sign_in("pw", &guessed, true, false));
    }

    #[test]
    fn the_password_goes_as_typed_and_the_address_logs_in() {
        let tried = attempt(&dana(), &fastmail(), " abcd efgh ");
        assert_eq!(tried.password, " abcd efgh ");
        assert_eq!(tried.imap_login, "Dana@fastmail.com");
        assert_eq!(tried.smtp_login, "Dana@fastmail.com");
        assert_eq!(tried.provider_name, "Fastmail");
    }

    #[test]
    fn a_user_name_typed_in_server_settings_is_who_logs_in() {
        let typed = Proposal {
            imap_user: Some("dana".into()),
            smtp_user: Some("dana@fastmail.com".into()),
            ..fastmail()
        };
        let tried = attempt(&dana(), &typed, "pw");
        assert_eq!(tried.imap_login, "dana");
        assert_eq!(tried.smtp_login, "dana@fastmail.com");
    }

    #[test]
    fn a_refused_login_says_so_with_the_servers_words_and_the_pages_that_help() {
        let gmx = Proposal {
            provider_name: "GMX".into(),
            info: Some(gmx_info()),
            ..fastmail()
        };
        let refused = anyhow::Error::new(CheckError::Imap(ImapError::Auth {
            text: "[AUTHENTICATIONFAILED] Invalid credentials".into(),
        }));
        let said = failure(&refused, &gmx);
        assert_eq!(
            said.line,
            "The server refused the password. The server said: [AUTHENTICATIONFAILED] Invalid credentials"
        );
        assert_eq!(
            said.links,
            [Link {
                label: "Turn On IMAP".into(),
                url: "https://gmx.example/imap".into(),
            }]
        );
        let at_fastmail = failure(&refused, &fastmail());
        assert_eq!(
            at_fastmail.links,
            [Link {
                label: "Make an App Password".into(),
                url: "https://fastmail.example/app-passwords".into(),
            }]
        );
    }

    #[test]
    fn imap_turned_off_gives_the_servers_words_and_the_page_that_turns_it_on() {
        let gmx = Proposal {
            provider_name: "GMX".into(),
            info: Some(gmx_info()),
            ..fastmail()
        };
        let off = anyhow::Error::new(CheckError::Imap(ImapError::ImapDisabled {
            text: "IMAP access is disabled".into(),
        }));
        let said = failure(&off, &gmx);
        assert_eq!(
            said.line,
            "IMAP is off for this account. The server said: IMAP access is disabled"
        );
        assert_eq!(
            said.links,
            [Link {
                label: "Turn On IMAP".into(),
                url: "https://gmx.example/imap".into(),
            }]
        );
    }

    #[test]
    fn a_certificate_that_does_not_match_names_the_host_and_offers_nothing() {
        let bad = anyhow::Error::new(CheckError::Imap(ImapError::Tls {
            host: "mail.example.org".into(),
            detail: "invalid peer certificate: NotValidForName".into(),
        }));
        let said = failure(&bad, &fastmail());
        assert_eq!(
            said.line,
            "The server's certificate does not match mail.example.org."
        );
        assert!(said.links.is_empty());
    }

    #[test]
    fn an_outgoing_server_out_of_reach_is_the_one_named() {
        let gone = anyhow::Error::new(CheckError::Smtp(ImapError::Network(
            "connection refused".into(),
        )));
        assert_eq!(
            failure(&gone, &fastmail()).line,
            "Could not reach smtp.example.org: connection refused"
        );
    }

    #[test]
    fn an_error_from_elsewhere_reads_as_itself() {
        let demo = anyhow::anyhow!("Demo mode cannot add real accounts.");
        assert_eq!(
            failure(&demo, &fastmail()).line,
            "Demo mode cannot add real accounts."
        );
    }

    fn keyring_refused() -> anyhow::Error {
        anyhow::Error::new(mailrs_sync::sign_in::ImapSignInError::Password(
            mailrs_sync::passwords::PasswordError::Keyring("no such interface".into()),
        ))
    }

    #[test]
    fn a_keyring_the_snap_cannot_reach_gets_the_command_that_connects_it() {
        let said = failure_given(&keyring_refused(), &fastmail(), Plug::Disconnected);
        assert_eq!(said.said.as_deref(), Some(crate::keyring_plug::COMMAND));
        assert_eq!(said.copy.as_deref(), Some(crate::keyring_plug::COMMAND));
        assert!(said.body.contains("restart"), "{}", said.body);
        let command = crate::keyring_plug::COMMAND;
        assert!(said.line.contains(command), "{}", said.line);
    }

    #[test]
    fn a_keyring_refusal_with_the_plug_connected_reads_as_itself() {
        let said = failure_given(&keyring_refused(), &fastmail(), Plug::Connected);
        assert_eq!(said.line, "The keyring refused: no such interface");
        assert_eq!(said.copy, None);
    }

    #[test]
    fn a_refused_password_in_a_snap_is_not_blamed_on_the_keyring() {
        let refused = anyhow::Error::new(CheckError::Imap(ImapError::Auth {
            text: "AUTHENTICATIONFAILED".into(),
        }));
        let said = failure_given(&refused, &fastmail(), Plug::Disconnected);
        assert_eq!(said.copy, None);
    }

    #[test]
    fn server_settings_want_a_server_name_on_both_sides() {
        let ok = Typed {
            host: " IMAP.Example.org ".into(),
            port: 993,
            security: Security::Tls,
            user: "  ".into(),
        };
        let empty = Typed {
            host: "  ".into(),
            ..ok.clone()
        };
        let url = Typed {
            host: "https://mail.example.org".into(),
            ..ok.clone()
        };
        assert_eq!(
            typed_servers(&empty, &ok, &fastmail(), &dana()),
            Err("Type the incoming server's name.".into())
        );
        assert_eq!(
            typed_servers(&ok, &empty, &fastmail(), &dana()),
            Err("Type the outgoing server's name.".into())
        );
        assert_eq!(
            typed_servers(&url, &ok, &fastmail(), &dana()),
            Err("https://mail.example.org is not a server name.".into())
        );
        let proposal = typed_servers(&ok, &ok, &fastmail(), &dana()).unwrap();
        assert_eq!(proposal.imap.host, "imap.example.org");
        assert_eq!(proposal.imap_user, None);
        assert_eq!(proposal.smtp_user, None);
        assert!(!proposal.confirm);
    }

    #[test]
    fn starttls_is_the_other_choice_and_there_is_no_plain_one() {
        assert_eq!(SECURITIES, [Security::Tls, Security::StartTls]);
        assert_eq!(
            security_at(security_index(Security::StartTls)),
            Security::StartTls
        );
        assert_eq!(security_label(Security::StartTls), "STARTTLS");
    }

    fn kept(imap_user: &str, smtp_user: &str) -> Servers {
        let saved = |host: &str, port, user: &str| Saved {
            host: host.into(),
            port,
            security: mailrs_store::servers::Security::Tls,
            user_name: user.into(),
        };
        Servers {
            imap: saved("imap.mail.me.com", 993, imap_user),
            smtp: saved("smtp.mail.me.com", 587, smtp_user),
        }
    }

    fn icloud_account() -> Account {
        Account {
            id: 4,
            email: "dana@icloud.com".into(),
            state: AccountState::NeedsReauth,
            provider: Provider::Imap,
            provider_name: Some("iCloud Mail".into()),
        }
    }

    #[test]
    fn signing_in_again_starts_from_the_servers_the_account_kept() {
        let account = icloud_account();
        let proposal = saved_proposal(&account, &kept("dana", "dana@icloud.com"));
        assert_eq!(proposal.provider_name, "iCloud Mail");
        assert_eq!(proposal.imap.host, "imap.mail.me.com");
        // Each server gets back the name it took last time from the
        // address: the part before @ for IMAP, the whole address for SMTP.
        assert_eq!(proposal.imap.user_name, UserName::LocalPartFirst);
        assert_eq!(proposal.smtp.user_name, UserName::Address);
        assert_eq!(proposal.imap_user, None);
        assert_eq!(proposal.smtp_user, None);
        assert!(!proposal.confirm);
        assert_eq!(
            again_line(&account),
            "Penguin Mail needs the password for dana@icloud.com again."
        );
    }

    #[test]
    fn a_user_name_typed_in_server_settings_is_kept_for_signing_in_again() {
        let proposal = saved_proposal(&icloud_account(), &kept("d.santos", "d.santos"));
        assert_eq!(proposal.imap_user.as_deref(), Some("d.santos"));
        assert_eq!(proposal.smtp_user.as_deref(), Some("d.santos"));
        assert_eq!(proposal.imap.user_name, UserName::Address);
        assert_eq!(proposal.smtp.user_name, UserName::Address);
    }

    /// An account "Set up manually" saved before it consulted the table
    /// keeps its address's domain as `provider_name`. Signing in again
    /// must still show the real provider and its app-password link.
    #[test]
    fn an_account_saved_under_its_domain_shows_its_real_provider() {
        let account = Account {
            id: 8,
            email: "dana@fastmail.com".into(),
            state: AccountState::NeedsReauth,
            provider: Provider::Imap,
            provider_name: Some("fastmail.com".into()),
        };
        let saved = Servers {
            imap: Saved {
                host: "imap.fastmail.com".into(),
                port: 993,
                security: mailrs_store::servers::Security::Tls,
                user_name: "dana@fastmail.com".into(),
            },
            smtp: Saved {
                host: "smtp.fastmail.com".into(),
                port: 465,
                security: mailrs_store::servers::Security::Tls,
                user_name: "dana@fastmail.com".into(),
            },
        };
        let proposal = saved_proposal(&account, &saved);
        assert_eq!(proposal.provider_name, "Fastmail");
        assert!(proposal.info.is_some_and(|info| info.password == PasswordKind::AppPassword));
    }

    #[test]
    fn a_late_answer_counts_only_for_the_last_question_while_the_dialog_is_open() {
        let asking = Asking::default();
        let first = asking.ask();
        let second = asking.ask();
        assert!(!asking.wants(first));
        assert!(asking.wants(second));
        asking.forget();
        assert!(!asking.wants(second));
        let third = asking.ask();
        asking.close();
        assert!(!asking.wants(third));
    }

    /// The controller's own scenario: two candidates, the first unreachable.
    fn two_candidates(second_confirm: bool) -> Found {
        Found {
            verdict: Verdict::Servers,
            candidates: vec![
                Candidate {
                    source: Source::Autoconfig,
                    provider: None,
                    imap: server("imap1.example.org", 993),
                    smtp: server("smtp1.example.org", 465),
                    confirm: false,
                },
                Candidate {
                    source: Source::Probe,
                    provider: None,
                    imap: server("imap2.example.org", 993),
                    smtp: server("smtp2.example.org", 465),
                    confirm: second_confirm,
                },
            ],
        }
    }

    #[test]
    fn the_first_candidates_port_is_blocked_the_second_signs_in() {
        let address = Address::parse("ann@example.org").unwrap();
        let Next::Password(first) = after_discovery(two_candidates(false), &address, true) else {
            panic!("expected the password step");
        };
        assert_eq!(first.imap.host, "imap1.example.org");
        let blocked = anyhow::Error::new(CheckError::Imap(ImapError::Network(
            "connection refused".into(),
        )));
        let Outcome::TryNext(second) = after_failure(&blocked, &first, &address) else {
            panic!("expected the second candidate");
        };
        assert_eq!(second.imap.host, "imap2.example.org");
        assert!(second.remaining.is_empty());
    }

    #[test]
    fn a_refused_password_never_reaches_for_another_candidate() {
        let address = Address::parse("ann@example.org").unwrap();
        let Next::Password(first) = after_discovery(two_candidates(false), &address, true) else {
            panic!("expected the password step");
        };
        let refused = anyhow::Error::new(CheckError::Imap(ImapError::Auth { text: "no".into() }));
        assert!(matches!(
            after_failure(&refused, &first, &address),
            Outcome::Failed(_)
        ));
    }

    #[test]
    fn a_candidate_needing_confirmation_still_needs_it_after_a_retry() {
        let address = Address::parse("ann@example.org").unwrap();
        let Next::Password(first) = after_discovery(two_candidates(true), &address, true) else {
            panic!("expected the password step");
        };
        let blocked = anyhow::Error::new(CheckError::Imap(ImapError::Tls {
            host: "imap1.example.org".into(),
            detail: "handshake failed".into(),
        }));
        let Outcome::TryNext(second) = after_failure(&blocked, &first, &address) else {
            panic!("expected the second candidate");
        };
        assert!(second.confirm);
        assert!(!can_sign_in("pw", &second, false, false));
        assert!(can_sign_in("pw", &second, true, false));
    }

    #[test]
    fn a_failure_with_no_candidate_left_goes_back_to_step_2() {
        let address = Address::parse("ann@example.org").unwrap();
        let single = fastmail();
        assert!(single.remaining.is_empty());
        let blocked = anyhow::Error::new(CheckError::Imap(ImapError::Network("gone".into())));
        assert!(matches!(
            after_failure(&blocked, &single, &address),
            Outcome::Failed(_)
        ));
    }

    fn typed(host: &str, port: u16, user: &str) -> Typed {
        Typed {
            host: host.into(),
            port,
            security: Security::Tls,
            user: user.into(),
        }
    }

    #[test]
    fn sign_in_waits_while_a_sign_in_runs() {
        assert!(!can_sign_in("pw", &fastmail(), false, true));
    }

    #[test]
    fn two_sign_ins_never_run_at_once() {
        let running = Running::default();
        assert!(running.begin());
        assert!(!running.begin(), "a second run started while one ran");
        assert!(running.is_on());
        running.end();
        assert!(!running.is_on());
        assert!(running.begin());
    }

    #[test]
    fn the_found_provider_stays_while_its_hosts_do() {
        let found = fastmail();
        let same = typed_servers(
            &typed("imap.example.org", 143, ""),
            &typed("SMTP.example.org", 587, ""),
            &found,
            &dana(),
        )
        .unwrap();
        assert_eq!(same.provider_name, "Fastmail");
        assert_eq!(same.info, Some(fastmail_info()));
    }

    #[test]
    fn a_typed_host_names_the_account_after_its_domain_and_drops_the_tables_rules() {
        let found = fastmail();
        let moved = typed_servers(
            &typed("mail.example.net", 993, ""),
            &typed("smtp.example.org", 465, ""),
            &found,
            &dana(),
        )
        .unwrap();
        assert_eq!(moved.provider_name, "fastmail.com");
        assert_eq!(moved.info, None);
    }

    #[test]
    fn each_server_takes_the_user_name_typed_for_it() {
        let proposal = typed_servers(
            &typed("imap.example.org", 993, " d.santos "),
            &typed("smtp.example.org", 465, "dana@example.org"),
            &fastmail(),
            &dana(),
        )
        .unwrap();
        assert_eq!(proposal.imap_user.as_deref(), Some("d.santos"));
        assert_eq!(proposal.smtp_user.as_deref(), Some("dana@example.org"));
        assert_eq!(proposal.imap.user_name, UserName::Address);
        assert_eq!(proposal.smtp.user_name, UserName::Address);
    }

    #[test]
    fn an_empty_outgoing_user_name_is_the_incoming_one() {
        let proposal = typed_servers(
            &typed("imap.example.org", 993, "d.santos"),
            &typed("smtp.example.org", 465, ""),
            &fastmail(),
            &dana(),
        )
        .unwrap();
        assert_eq!(proposal.smtp_user.as_deref(), Some("d.santos"));
        let tried = attempt(&dana(), &proposal, "pw");
        assert_eq!(tried.smtp_login, "d.santos");
    }

    #[test]
    fn a_name_typed_for_one_server_is_kept_for_that_server_alone() {
        let proposal = saved_proposal(&icloud_account(), &kept("d.santos", "dana@icloud.com"));
        assert_eq!(proposal.imap_user.as_deref(), Some("d.santos"));
        assert_eq!(proposal.imap.user_name, UserName::Address);
        assert_eq!(proposal.smtp_user, None);
        assert_eq!(proposal.smtp.user_name, UserName::Address);
    }

    #[test]
    fn switching_security_moves_a_default_port_to_the_other_default() {
        use Role::{Incoming, Outgoing};
        assert_eq!(port_after_switch(Incoming, 993, Security::StartTls), 143);
        assert_eq!(port_after_switch(Incoming, 143, Security::Tls), 993);
        assert_eq!(port_after_switch(Outgoing, 465, Security::StartTls), 587);
        assert_eq!(port_after_switch(Outgoing, 587, Security::Tls), 465);
    }

    #[test]
    fn switching_security_keeps_a_port_the_person_chose() {
        assert_eq!(
            port_after_switch(Role::Incoming, 1143, Security::StartTls),
            1143
        );
        assert_eq!(port_after_switch(Role::Outgoing, 2525, Security::Tls), 2525);
        // Already the new mode's default: nothing to move.
        assert_eq!(port_after_switch(Role::Outgoing, 465, Security::Tls), 465);
    }

    #[test]
    fn too_many_connections_says_the_server_answered_and_to_try_later() {
        let busy = anyhow::Error::new(CheckError::Imap(ImapError::TooManyConnections {
            text: "[LIMIT] Too many simultaneous connections".into(),
        }));
        assert_eq!(
            failure(&busy, &fastmail()).line,
            "imap.example.org answered but turned down another connection. Try again in a few minutes."
        );
    }

    #[test]
    fn a_refused_password_at_an_app_password_provider_says_to_make_one() {
        let refused = anyhow::Error::new(CheckError::Imap(ImapError::Auth {
            text: "[AUTHENTICATIONFAILED]".into(),
        }));
        let said = failure(&refused, &fastmail());
        assert_eq!(said.kind, FailureKind::Refused);
        assert_eq!(said.title, "Fastmail refused the password");
        assert_eq!(
            said.body,
            "Fastmail wants an app password here, not your website password. Make one, paste it above, then choose Sign In."
        );
        assert_eq!(
            said.said.as_deref(),
            Some("The server said: [AUTHENTICATIONFAILED]")
        );
    }

    #[test]
    fn a_refused_password_elsewhere_asks_to_check_it() {
        let gmx = Proposal {
            provider_name: "GMX".into(),
            info: Some(gmx_info()),
            ..fastmail()
        };
        let refused = anyhow::Error::new(CheckError::Imap(ImapError::Auth { text: " ".into() }));
        let said = failure(&refused, &gmx);
        assert_eq!(said.title, "GMX refused the password");
        assert_eq!(said.body, "Check the password, then choose Sign In.");
        assert_eq!(said.said, None);
    }

    #[test]
    fn an_incoming_server_out_of_reach_never_got_the_password() {
        let gone = anyhow::Error::new(CheckError::Imap(ImapError::Network(
            "connection timed out".into(),
        )));
        let said = failure(&gone, &fastmail());
        assert_eq!(
            said.kind,
            FailureKind::Unreachable {
                host: "imap.example.org".into(),
                port: 993,
                password_sent: false,
            }
        );
        assert_eq!(said.title, "No answer on port 993");
        assert_eq!(
            said.body,
            "The server did not answer. Check the server name and port, or try again when you are online."
        );
        assert_eq!(said.said.as_deref(), Some("connection timed out"));
    }

    #[test]
    fn an_outgoing_server_out_of_reach_comes_after_the_password_went_in() {
        let gone = anyhow::Error::new(CheckError::Smtp(ImapError::Network("refused".into())));
        assert_eq!(
            failure(&gone, &fastmail()).kind,
            FailureKind::Unreachable {
                host: "smtp.example.org".into(),
                port: 465,
                password_sent: true,
            }
        );
    }

    #[test]
    fn an_error_from_elsewhere_is_a_card_of_its_own_words() {
        let demo = anyhow::anyhow!("Demo mode cannot add real accounts.");
        let said = failure(&demo, &fastmail());
        assert_eq!(said.title, "Could not sign in");
        assert_eq!(said.body, "Demo mode cannot add real accounts.");
        assert_eq!(said.kind, FailureKind::Refused);
    }

    #[test]
    fn a_server_row_names_host_port_and_security() {
        assert_eq!(
            server_row_line(&server("imap.fastmail.com", 993)),
            "imap.fastmail.com · port 993 · TLS"
        );
    }

    #[test]
    fn two_hosts_on_one_domain_share_a_summary() {
        let both = Proposal {
            imap: server("imap.fastmail.com", 993),
            smtp: server("smtp.fastmail.com", 465),
            ..fastmail()
        };
        assert_eq!(servers_summary(&both), "imap and smtp.fastmail.com · TLS");
        let apart = Proposal {
            imap: server("mail.example.org", 993),
            smtp: Server {
                security: Security::StartTls,
                ..server("smtp.example.net", 587)
            },
            ..fastmail()
        };
        assert_eq!(
            servers_summary(&apart),
            "mail.example.org and smtp.example.net · TLS and STARTTLS"
        );
    }

    #[test]
    fn the_found_line_says_what_asked_and_what_did_not() {
        let by = |source| Proposal {
            source: Some(source),
            ..fastmail()
        };
        assert_eq!(
            found_line(&by(Source::Mx)),
            "Found by its mail servers. Nothing else was asked."
        );
        assert_eq!(
            found_line(&by(Source::Table)),
            "Penguin Mail's own list knows this provider. Nothing has left this computer."
        );
        assert_eq!(
            found_line(&by(Source::Ispdb)),
            "Found in Mozilla's provider list."
        );
        assert_eq!(
            found_line(&by(Source::Probe)),
            "Found by trying common server names."
        );
    }

    #[test]
    fn the_browser_wait_counts_down_in_minutes_and_seconds() {
        use std::time::Duration;
        assert_eq!(time_left(Duration::from_secs(252)), "4:12 left");
        assert_eq!(time_left(Duration::from_millis(59_200)), "1:00 left");
        assert_eq!(time_left(Duration::ZERO), "0:00 left");
    }

    #[test]
    fn a_refused_password_offers_one_page_as_the_board_does() {
        let with_help = Proposal {
            info: Some(ProviderInfo {
                documentation_url: Some("https://fastmail.example/help".into()),
                enable_imap_url: Some("https://fastmail.example/imap".into()),
                ..fastmail_info()
            }),
            ..fastmail()
        };
        let refused = anyhow::Error::new(CheckError::Imap(ImapError::Auth { text: "no".into() }));
        let labels: Vec<String> = failure(&refused, &with_help)
            .links
            .into_iter()
            .map(|link| link.label)
            .collect();
        assert_eq!(labels, ["Make an App Password"]);
    }

    #[test]
    fn a_small_count_is_written_in_words() {
        assert_eq!(small_number(2), "two");
        assert_eq!(small_number(9), "nine");
        assert_eq!(small_number(12), "12");
    }

    #[test]
    fn a_folder_waits_then_counts_its_conversations() {
        assert_eq!(folder_line(0), "Waiting");
        assert_eq!(folder_line(1), "1 conversation");
        assert_eq!(folder_line(312), "312 conversations");
    }

    fn suggested(typed: &str) -> Option<String> {
        suggestion(&Address::parse(typed).unwrap()).map(|address| address.full())
    }

    #[test]
    fn a_domain_one_edit_from_a_listed_one_gets_a_suggestion() {
        // A swap, a missing letter, and a swap in a longer name.
        assert_eq!(
            suggested("dana@gmial.com").as_deref(),
            Some("dana@gmail.com")
        );
        assert_eq!(
            suggested("dana@gmail.co").as_deref(),
            Some("dana@gmail.com")
        );
        assert_eq!(
            suggested("dana@hotmial.com").as_deref(),
            Some("dana@hotmail.com")
        );
        // A letter too many, and one wrong letter.
        assert_eq!(
            suggested("dana@fastmaill.com").as_deref(),
            Some("dana@fastmail.com")
        );
        assert_eq!(
            suggested("dana@yahoo.cim").as_deref(),
            Some("dana@yahoo.com")
        );
    }

    #[test]
    fn a_listed_domain_is_never_corrected() {
        // ymail.com is one letter from gmail.com, and both are real.
        assert_eq!(suggested("dana@ymail.com"), None);
        assert_eq!(suggested("dana@gmail.com"), None);
    }

    #[test]
    fn a_domain_far_from_every_listed_one_gets_no_suggestion() {
        assert_eq!(suggested("dana@example.org"), None);
    }

    #[test]
    fn continue_offers_the_suggestion_once_and_then_looks_the_typo_up() {
        let Continue::Suggest(better) = on_continue("dana@gmial.com", None) else {
            panic!("expected a suggestion");
        };
        assert_eq!(better.full(), "dana@gmail.com");
        assert_eq!(did_you_mean(&better), "Did you mean dana@gmail.com?");
        let declined = Address::parse("dana@gmial.com").unwrap();
        assert_eq!(
            on_continue("dana@gmial.com", Some(&declined)),
            Continue::Look(declined.clone())
        );
        assert_eq!(
            on_continue("dana@example.org", None),
            Continue::Look(Address::parse("dana@example.org").unwrap())
        );
        assert_eq!(
            on_continue("dana", None),
            Continue::Say("Type the whole address, such as dana@example.com.".into())
        );
    }
}
