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
use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_domain::{Account, Provider, RemoveSetting};
use mailrs_imap::{CheckError, ImapError};
use mailrs_store::servers::{Pop3Servers, Saved, Servers};

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
    /// The address these servers were found or typed for. Server Settings
    /// takes nothing from a proposal made for another address.
    pub address: String,
    /// Who runs the servers: the provider table's name, or the address's
    /// domain for a server the table does not list.
    pub provider_name: String,
    pub info: Option<ProviderInfo>,
    /// What the incoming server speaks.
    pub protocol: Protocol,
    /// The incoming server, IMAP or POP3 as `protocol` says.
    pub incoming: Server,
    pub smtp: Server,
    /// The user name typed in Server Settings for the incoming server,
    /// sent as typed. `None` logs in with the address, the way the
    /// server's user name rule says.
    pub incoming_user: Option<String>,
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
    /// The POP3 server discovery found beside the IMAP one, which step 2
    /// offers on a row of its own.
    pub pop3_offer: Option<Server>,
    /// What a POP3 account does with mail on the server; `None` for IMAP.
    pub pop3_remove: Option<RemoveSetting>,
    /// Server Settings holds a password for the outgoing server apart from
    /// the incoming one. The password itself stays in the dialog, out of
    /// this `Debug` type.
    pub own_smtp_password: bool,
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
    /// A JMAP session to sign in to, offered before IMAP when both are
    /// found.
    Jmap(JmapProposal),
}

/// What an account's incoming server speaks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Protocol {
    #[default]
    Imap,
    Pop3,
}

pub fn protocol_index(protocol: Protocol) -> u32 {
    match protocol {
        Protocol::Imap => 0,
        Protocol::Pop3 => 1,
    }
}

pub fn protocol_at(index: u32) -> Protocol {
    match index {
        1 => Protocol::Pop3,
        _ => Protocol::Imap,
    }
}

/// What a POP3 account does with mail on the server, as Server Settings
/// lists the choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    Leave,
    AfterDownloading,
    AfterDays,
}

pub const REMOVALS: [Removal; 3] = [
    Removal::Leave,
    Removal::AfterDownloading,
    Removal::AfterDays,
];

/// The day count Remove After {n} Days starts at.
pub const DEFAULT_DAYS: u32 = 14;

/// A removal row's title; the third names `days`.
pub fn removal_title(removal: Removal, days: u32) -> String {
    match removal {
        Removal::Leave => gettext("Leave on Server"),
        Removal::AfterDownloading => gettext("Remove After Downloading"),
        Removal::AfterDays => fill_plural(
            "Remove After {count} Day",
            "Remove After {count} Days",
            days as usize,
            &[("count", &days.to_string())],
        ),
    }
}

pub fn removal_setting(removal: Removal, days: u32) -> RemoveSetting {
    match removal {
        Removal::Leave => RemoveSetting::Never,
        Removal::AfterDownloading => RemoveSetting::Downloaded,
        Removal::AfterDays => RemoveSetting::Days(days.max(1)),
    }
}

/// The row and day count that show `setting`.
pub fn removal_of(setting: RemoveSetting) -> (Removal, u32) {
    match setting {
        RemoveSetting::Never => (Removal::Leave, DEFAULT_DAYS),
        RemoveSetting::Downloaded => (Removal::AfterDownloading, DEFAULT_DAYS),
        RemoveSetting::Days(days) => (Removal::AfterDays, days),
    }
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
///
/// `jmap` is the JMAP session discovery found for the address, if any. It
/// goes first where IMAP would, with IMAP one step away; a provider that
/// signs in through the browser or lets no mail app in keeps that answer.
pub fn after_discovery(
    found: Found,
    jmap: Option<mailrs_discover::JmapFound>,
    address: &Address,
    microsoft: bool,
) -> Next {
    match found.verdict {
        Verdict::Servers => {
            let mut candidates = found.candidates.into_iter();
            let imap = candidates
                .next()
                .map(|best| proposal_from(best, candidates.collect(), address));
            match (jmap, imap) {
                (Some(jmap), imap) => Next::Jmap(jmap_proposal(jmap, imap, address)),
                (None, Some(imap)) => Next::Password(imap),
                (None, None) => nothing_found(address),
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
        Verdict::NothingFound => match jmap {
            Some(jmap) => Next::Jmap(jmap_proposal(jmap, None, address)),
            None => nothing_found(address),
        },
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
    // A provider that offers POP3 and no IMAP signs in over POP3, leaving
    // mail on the server until the person picks otherwise.
    let (protocol, incoming, pop3_offer, pop3_remove) = match (candidate.imap, candidate.pop3) {
        (Some(imap), pop3) => (Protocol::Imap, imap, pop3, None),
        (None, Some(pop3)) => (Protocol::Pop3, pop3, None, Some(RemoveSetting::Never)),
        // Discovery never makes a candidate with neither; the usual host
        // names stand in.
        (None, None) => return usual_servers(address),
    };
    Proposal {
        address: address.full(),
        provider_name,
        info: candidate.provider,
        protocol,
        incoming,
        smtp: candidate.smtp,
        incoming_user: None,
        smtp_user: None,
        confirm: candidate.confirm,
        source: Some(candidate.source),
        remaining,
        pop3_offer,
        pop3_remove,
        own_smtp_password: false,
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
    usual_servers(address)
}

/// The host names most servers use, on the ports with TLS from the first
/// byte.
fn usual_servers(address: &Address) -> Proposal {
    let server = |host: String, port| Server {
        host,
        port,
        security: Security::Tls,
        user_name: UserName::Address,
    };
    Proposal {
        address: address.full(),
        provider_name: address.domain.clone(),
        info: None,
        protocol: Protocol::Imap,
        incoming: server(format!("imap.{}", address.domain), 993),
        smtp: server(format!("smtp.{}", address.domain), 465),
        incoming_user: None,
        smtp_user: None,
        confirm: false,
        source: None,
        remaining: Vec::new(),
        pop3_offer: None,
        pop3_remove: None,
        own_smtp_password: false,
    }
}

/// `proposal` with the outgoing password Server Settings holds, `typed`:
/// an empty one means the outgoing server takes the incoming password.
pub fn with_smtp_password(proposal: Proposal, typed: &str) -> Proposal {
    Proposal {
        own_smtp_password: !typed.is_empty(),
        ..proposal
    }
}

/// `proposal` speaking `protocol`, with `remove` kept only for POP3, so
/// switching back to IMAP leaves no POP3 setting behind.
pub fn with_protocol(proposal: Proposal, protocol: Protocol, remove: RemoveSetting) -> Proposal {
    Proposal {
        protocol,
        pop3_remove: (protocol == Protocol::Pop3).then_some(remove),
        pop3_offer: None,
        ..proposal
    }
}

/// Server Settings for the POP3 server discovery found beside IMAP,
/// leaving mail on the server.
pub fn as_pop3(proposal: &Proposal) -> Option<Proposal> {
    let pop3 = proposal.pop3_offer.clone()?;
    Some(Proposal {
        incoming: pop3,
        remaining: Vec::new(),
        ..with_protocol(proposal.clone(), Protocol::Pop3, RemoveSetting::Never)
    })
}

/// The line step 2 shows when the provider offers POP3 beside the IMAP
/// it picked.
pub fn pop3_line(proposal: &Proposal) -> Option<String> {
    (proposal.protocol == Protocol::Imap && proposal.pop3_offer.is_some()).then(|| {
        fill(
            &gettext("{provider} also offers POP3"),
            &[("provider", &post::short_name(&proposal.provider_name))],
        )
    })
}

/// The title of step 2's incoming server row.
pub fn incoming_title(proposal: &Proposal) -> String {
    match proposal.protocol {
        Protocol::Imap => gettext("Incoming"),
        Protocol::Pop3 => fill(
            &gettext("Incoming over {protocol}"),
            &[("protocol", "POP3")],
        ),
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
    let (imap, smtp) = (&proposal.incoming, &proposal.smtp);
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
        Some(Source::Table) => {
            gettext("Penguin Mail's own list knows this provider. Nothing has left this computer.")
        }
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
/// conversations are in. A folder still empty when the download ends has
/// nothing to wait for, so it says so.
pub fn folder_line(conversations: usize, done: bool) -> String {
    if conversations == 0 {
        return match done {
            false => gettext("Waiting"),
            true => gettext("Empty"),
        };
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
    pub protocol: Protocol,
    pub incoming: Server,
    pub smtp: Server,
    /// What the incoming server is signed in to with:
    /// the user name typed for it in Server Settings, else the address.
    /// The server's `user_name` rule turns it into the names to try, and a
    /// typed name comes with the `Address` rule, so it goes as typed.
    pub incoming_login: String,
    /// The same for the outgoing server.
    pub smtp_login: String,
    pub password: String,
    /// The outgoing server's own password, when Server Settings holds one.
    pub smtp_password: Option<String>,
    /// What a POP3 account does with mail on the server.
    pub pop3_remove: RemoveSetting,
}

/// The try Sign In makes. The password goes as typed: app passwords are
/// shown in groups with spaces, and trimming one changes it. The same
/// holds for `smtp_password`, which goes only when the proposal says
/// Server Settings holds one.
pub fn attempt(
    address: &Address,
    proposal: &Proposal,
    password: &str,
    smtp_password: &str,
) -> Attempt {
    Attempt {
        address: address.full(),
        provider_name: proposal.provider_name.clone(),
        protocol: proposal.protocol,
        incoming: proposal.incoming.clone(),
        smtp: proposal.smtp.clone(),
        incoming_login: proposal
            .incoming_user
            .clone()
            .unwrap_or_else(|| address.full()),
        smtp_login: proposal.smtp_user.clone().unwrap_or_else(|| address.full()),
        password: password.to_string(),
        smtp_password: (proposal.own_smtp_password && !smtp_password.is_empty())
            .then(|| smtp_password.to_string()),
        pop3_remove: proposal.pop3_remove.unwrap_or_default(),
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
        CheckError::Imap(err) => (&proposal.incoming, err),
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
        // With two passwords, the person needs to know which one to fix.
        ImapError::Auth { text } => match check {
            CheckError::Smtp(_) if proposal.own_smtp_password => refused(
                gettext("The outgoing server refused the outgoing password."),
                text,
                proposal,
                gettext("The outgoing server refused its password"),
                gettext("Check the outgoing password in Server Settings, then choose Sign In."),
                false,
            ),
            CheckError::Smtp(_) => refused(
                gettext("The outgoing server refused the password."),
                text,
                proposal,
                refused_title(proposal),
                refused_body(proposal),
                false,
            ),
            CheckError::Imap(_) if proposal.own_smtp_password => refused(
                gettext("The incoming server refused the password."),
                text,
                proposal,
                refused_title(proposal),
                refused_body(proposal),
                false,
            ),
            CheckError::Imap(_) => refused(
                gettext("The server refused the password."),
                text,
                proposal,
                refused_title(proposal),
                refused_body(proposal),
                false,
            ),
        },
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
        // A TLS failure can be an untrusted certificate, a wrong name, or
        // an unsupported protocol. Keep the detail in the log.
        ImapError::Tls { host, .. } => {
            let line = fill(
                &gettext("Could not verify the secure connection to {host}."),
                &[("host", host)],
            );
            let mut failure =
                Failure::plain(line.clone(), gettext("Secure connection failed"), line);
            if host == "127.0.0.1" {
                failure.body = gettext(
                    "If you use Proton Mail Bridge, export its TLS certificate and follow the setup guide, then try signing in again.",
                );
                failure.line = format!("{} {}", failure.line, failure.body);
                failure.links.push(Link {
                    label: gettext("Set Up Proton Mail Bridge"),
                    url: "https://github.com/c9dev/penguin-mail/blob/main/docs/setup.md#proton-mail-bridge".into(),
                });
            }
            failure
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
        // The crate writes these two in English for the log, so the
        // dialog words them itself and leaves the detail to the log.
        ImapError::Protocol(_) => {
            let line = fill(
                &gettext("{host} answered in a way this app cannot work with."),
                &[("host", host)],
            );
            Failure::plain(line.clone(), could_not.clone(), line)
        }
        ImapError::Refused(reason) | ImapError::NoMailbox(reason) => could_not_sign_in(reason),
        ImapError::BandwidthLimit { text } => could_not_sign_in(text),
        ImapError::Unsupported(what) => could_not_sign_in(what),
        // A value the client refused before it went anywhere: the
        // proposal itself could not carry a login, which is not a
        // failure Server Settings or an app password can fix.
        ImapError::Invalid(_) => {
            let line = gettext("This app cannot send these sign-in details to the server.");
            Failure::plain(line.clone(), could_not.clone(), line)
        }
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
/// provider says otherwise: 993 and 143 for IMAP, 995 and 110 for POP3,
/// 465 and 587 for submission (RFC 8314).
fn default_port(role: Role, protocol: Protocol, security: Security) -> u16 {
    match (role, protocol, security) {
        (Role::Incoming, Protocol::Imap, Security::Tls) => 993,
        (Role::Incoming, Protocol::Imap, Security::StartTls) => 143,
        (Role::Incoming, Protocol::Pop3, Security::Tls) => 995,
        (Role::Incoming, Protocol::Pop3, Security::StartTls) => 110,
        (Role::Outgoing, _, Security::Tls) => 465,
        (Role::Outgoing, _, Security::StartTls) => 587,
    }
}

/// The port after the Security row switched to `now`. A port that still
/// holds the other choice's default moves to this one's; a port the
/// person typed stays.
pub fn port_after_switch(role: Role, protocol: Protocol, port: u16, now: Security) -> u16 {
    let other = match now {
        Security::Tls => Security::StartTls,
        Security::StartTls => Security::Tls,
    };
    if port == default_port(role, protocol, other) {
        default_port(role, protocol, now)
    } else {
        port
    }
}

/// The incoming port after the Protocol row switched to `now`, by the
/// same rule.
pub fn port_after_protocol(port: u16, security: Security, now: Protocol) -> u16 {
    let other = match now {
        Protocol::Imap => Protocol::Pop3,
        Protocol::Pop3 => Protocol::Imap,
    };
    if port == default_port(Role::Incoming, other, security) {
        default_port(Role::Incoming, now, security)
    } else {
        port
    }
}

/// The incoming server Server Settings moves to when the Protocol row
/// switches to `now`, for a provider discovery found both IMAP and POP3
/// at: the other protocol's server while `host` still names the one just
/// left. A host the person typed stays, with `None`.
pub fn server_after_protocol(proposal: &Proposal, host: &str, now: Protocol) -> Option<Server> {
    let pop3 = proposal.pop3_offer.as_ref()?;
    let imap = &proposal.incoming;
    match now {
        Protocol::Pop3 if host == imap.host => Some(pop3.clone()),
        Protocol::Imap if host == pop3.host => Some(imap.clone()),
        _ => None,
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
/// are still its own, its POP3 server among them: another host is a server the person chose, so the
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
    let incoming_user = typed_user(imap);
    let smtp_user = typed_user(smtp).or_else(|| incoming_user.clone());
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
    // The POP3 server found beside IMAP is the provider's own too.
    let found = std::iter::once(&before.incoming)
        .chain(&before.pop3_offer)
        .find(|server| server.host == imap_host);
    // What discovery knew counts only for the address it was found for.
    let same_address = before.address.eq_ignore_ascii_case(&address.full());
    let same_hosts = same_address && found.is_some() && smtp_host == before.smtp.host;
    let (provider_name, info) = if same_hosts {
        (before.provider_name.clone(), before.info.clone())
    } else {
        (address.domain.clone(), None)
    };
    Ok(Proposal {
        address: address.full(),
        provider_name,
        info,
        protocol: before.protocol,
        incoming: server(
            imap_host,
            imap,
            &incoming_user,
            found.unwrap_or(&before.incoming).user_name,
        ),
        smtp: server(smtp_host, smtp, &smtp_user, before.smtp.user_name),
        incoming_user,
        smtp_user,
        // Hosts discovery guessed still wait for the yes when the form
        // comes back with them unchanged; hosts the person typed do not.
        confirm: same_hosts && before.confirm,
        source: None,
        remaining: Vec::new(),
        pop3_offer: None,
        pop3_remove: before.pop3_remove,
        own_smtp_password: before.own_smtp_password,
    })
}

/// Step 2 for an account signing in again, from the servers it kept and
/// the user name each took last time. Each kept name becomes the rule
/// that gives it back from the address where one does: the address
/// itself, or its part before @ (iCloud's IMAP server takes that while
/// its SMTP server takes the address). A name no rule gives, typed in
/// Server Settings, goes to that server as typed again. There is no
/// discovery to fall back to, so nothing waits in `remaining`.
pub fn saved_proposal(account: &Account, kept: &Kept) -> Proposal {
    let (protocol, incoming, smtp, pop3_remove) = match kept {
        Kept::Imap(servers) => (Protocol::Imap, &servers.imap, &servers.smtp, None),
        Kept::Pop3(servers, remove) => {
            (Protocol::Pop3, &servers.pop3, &servers.smtp, Some(*remove))
        }
    };
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
    let (incoming, incoming_user) = restore(incoming);
    let (smtp, smtp_user) = restore(smtp);
    let provider_name = mailrs_discover::resolved_provider_name(account.provider_name());
    Proposal {
        address: account.email.clone(),
        info: mailrs_discover::provider_named(&provider_name),
        provider_name,
        protocol,
        incoming,
        smtp,
        incoming_user,
        smtp_user,
        confirm: false,
        source: None,
        remaining: Vec::new(),
        pop3_offer: None,
        pop3_remove,
        own_smtp_password: false,
    }
}

/// The servers an account kept, as its protocol keeps them.
pub enum Kept {
    Imap(Servers),
    Pop3(Pop3Servers, RemoveSetting),
}

/// Whether the added page lists `role` for a new account of `provider`:
/// the folders its server fills. Gmail has no Archive of its own, a POP3
/// server holds an inbox alone, and a calendar account has no mail.
pub fn shows_folder(provider: Provider, role: mailrs_domain::Role) -> bool {
    match provider {
        Provider::Gmail => role != mailrs_domain::Role::Archive,
        Provider::Pop3 => role == mailrs_domain::Role::Inbox,
        Provider::Imap | Provider::Microsoft | Provider::Jmap => true,
        Provider::CalDav => false,
    }
}

/// Whether the calendar page has what a sign-in needs: where the server
/// is, who signs in, and a password. The password is taken as typed,
/// since a space may be part of one.
pub fn calendar_ready(server: &str, user_name: &str, password: &str) -> bool {
    !server.trim().is_empty() && !user_name.trim().is_empty() && !password.is_empty()
}

/// Whether the sign-in that ended as `now` is the one `asked` was opened
/// for. The calendar page opened for Sign In Again takes a password for
/// one account; a sign-in that ends as another account added that one.
pub fn signed_in_again(asked: Option<&Account>, now: &Account) -> bool {
    asked.is_some_and(|asked| asked.id == now.id)
}

/// What the calendar page says when its sign-in fails, signing `again`
/// in again or adding an account. What the sign-in already put in words,
/// an address that is not HTTPS or a name another account holds, stands
/// as it is.
pub fn calendar_problem(err: &anyhow::Error, again: Option<&Account>) -> String {
    use mailrs_sync::sign_in::CalDavSignInError;
    use mailrs_sync::{BackendError, SyncError};

    let unknown = |reason: &dyn std::fmt::Display| {
        let said = match again {
            Some(_) => gettext("Could not sign in: {reason}"),
            None => gettext("Could not add the calendar: {reason}"),
        };
        mailrs_domain::translate::with_reason(&said, &reason, &[])
    };
    match err.downcast_ref::<CalDavSignInError>() {
        // An IMAP or POP3 account that holds the name can take the
        // server's calendars itself, which is the way on from here.
        Some(
            taken @ CalDavSignInError::Taken {
                holder: Provider::Imap | Provider::Pop3,
                ..
            },
        ) => format!(
            "{taken} {}",
            gettext(
                "To see this server's calendars there, open Preferences, then Accounts, open that account, \
                 and type this server's address under Calendar, Contacts and Rules Servers."
            )
        ),
        Some(
            said @ (CalDavSignInError::Taken { .. }
            | CalDavSignInError::Elsewhere { .. }
            | CalDavSignInError::OtherUser { .. }
            | CalDavSignInError::Moves { .. }
            | CalDavSignInError::NotHttps),
        ) => said.to_string(),
        Some(CalDavSignInError::Server(SyncError::Backend(backend))) => match backend {
            BackendError::NeedsReauth => gettext(
                "The server did not take that user name and password. Where it uses two-step \
                 sign-in, it needs an app password.",
            ),
            BackendError::Offline(_) => gettext(
                "Penguin Mail could not reach that server. Check the address and your connection.",
            ),
            // A web page at the address, or no CalDAV there, is refused in
            // the protocol's own words, which are not for the page.
            BackendError::Unsupported | BackendError::Refused(_) => {
                gettext("No calendar was found at that address.")
            }
            other => unknown(other),
        },
        _ => unknown(err),
    }
}

/// What the added page says under its title and under the folder list,
/// and whether the new account's first download is finished, from the
/// store's values.
pub struct AddedPage {
    pub lede: String,
    pub note: String,
    pub done: bool,
}

/// The line under "{account} is ready": the mail is on its way until the
/// first download ends, then it is here.
fn ready_lede(done: bool) -> String {
    match done {
        false => gettext("Mail is downloading. You can close this window."),
        true => gettext("Your mail is here."),
    }
}

/// The lede of the page that offers Grant Access: how the first download
/// stands, then which boxes the person left unticked on Google's page, or
/// for Microsoft, which permissions the organization refused: Microsoft's
/// page has no boxes to untick. It follows the download as the ready
/// page's lede does.
pub fn grant_lede(provider: Provider, missing: usize, done: bool) -> String {
    let spoken = small_number(missing);
    let count = [("count", spoken.as_str())];
    let left = if provider == Provider::Microsoft {
        fill_plural(
            "Your organization did not allow one of the permissions Penguin Mail asked for, so this feature stays off:",
            "Your organization did not allow {count} of the permissions Penguin Mail asked for, so these features stay off:",
            missing,
            &count,
        )
    } else {
        fill_plural(
            "You left one box unticked on Google's page, so this feature stays off:",
            "You left {count} boxes unticked on Google's page, so these features stay off:",
            missing,
            &count,
        )
    };
    let download = match done {
        false => gettext("Mail is downloading."),
        true => gettext("Your mail is here."),
    };
    format!("{download} {left}")
}

/// Decides the added page's words and progress for a new account.
/// Gmail, IMAP and Microsoft accounts are done when the backfill has
/// finished. A POP3 account never sets that marker; its first check does.
pub fn added_page(
    provider: Provider,
    remove: RemoveSetting,
    backfill_done: bool,
    pop3_first_check_done: bool,
) -> AddedPage {
    if provider != Provider::Pop3 {
        // Once the backfill has finished no older mail is on its way, only
        // new mail, which the periodic check brings.
        let note = match backfill_done {
            false => gettext(
                "Newest mail comes first. Older mail keeps coming in the background, even with the window closed.",
            ),
            true => {
                gettext("Penguin Mail keeps checking for new mail, even with the window closed.")
            }
        };
        return AddedPage {
            lede: ready_lede(backfill_done),
            note,
            done: backfill_done,
        };
    }
    let note = match remove {
        RemoveSetting::Never => gettext(
            "Penguin Mail downloads the server's mail into Inbox and keeps checking in the background, even with the window closed. Mail stays on the server.",
        ),
        RemoveSetting::Downloaded => gettext(
            "Penguin Mail downloads the server's mail into Inbox and keeps checking in the background, even with the window closed. It removes each message from the server after downloading it.",
        ),
        RemoveSetting::Days(days) => fill_plural(
            "Penguin Mail downloads the server's mail into Inbox and keeps checking in the background, even with the window closed. It removes each message from the server {days} day after downloading it.",
            "Penguin Mail downloads the server's mail into Inbox and keeps checking in the background, even with the window closed. It removes each message from the server {days} days after downloading it.",
            days as usize,
            &[("days", &days.to_string())],
        ),
    };
    AddedPage {
        lede: ready_lede(pop3_first_check_done),
        note,
        done: pop3_first_check_done,
    }
}

/// What step 2 says above the password for an account signing in again.
pub fn again_line(account: &Account) -> String {
    fill(
        &gettext("Penguin Mail needs the password for {address} again."),
        &[("address", &account.email)],
    )
}

/// What step 2 says above the fields for a JMAP account that signed in
/// through the browser and needs to again. It has no password to give.
pub fn again_browser_line(account: &Account) -> String {
    fill(
        &gettext("Choose Sign In With Browser to sign in to {address} again."),
        &[("address", &account.email)],
    )
}

/// The banner over the mail list for an account that needs to sign in
/// again. One that signed in through the browser does so there again.
pub fn sign_in_again_banner(address: &str, through_browser: bool) -> String {
    let template = match through_browser {
        true => gettext("Sign in again through the browser to keep {account} syncing"),
        false => gettext("Sign in again to keep {account} syncing"),
    };
    fill(&template, &[("account", address)])
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

/// Whether the password field keeps what it holds when the password page
/// opens for `address`: only when the person typed it for that same
/// address. Otherwise one press would send it to another provider.
pub fn keeps_password(typed_for: Option<&str>, address: &str) -> bool {
    typed_for.is_some_and(|typed_for| typed_for.eq_ignore_ascii_case(address))
}

// ---- JMAP --------------------------------------------------------------------

/// What the JMAP sign-in signs in to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JmapProposal {
    /// The address this session was found or typed for. Server Settings
    /// takes nothing from a session held for another address.
    pub address: String,
    /// The provider table's name, or the session's host.
    pub provider_name: String,
    pub session_url: String,
    /// The person must say yes to the session's host before the password
    /// or the token goes there.
    pub confirm: bool,
    pub source: Option<Source>,
    /// What the table knows of the provider's sign-in.
    pub provider: Option<mailrs_discover::JmapProvider>,
    /// IMAP, when discovery found it too, one step away.
    pub imap: Option<Box<Proposal>>,
    /// The user name Basic sends, when the person typed one.
    pub user: Option<String>,
    /// Whether the page opens on a token rather than a password: the
    /// provider takes tokens alone, or the account kept one before.
    pub token: bool,
}

/// Server Settings' Protocol row: the two mail protocols, then JMAP. A
/// protocol's name, so it is not translated.
pub const PROTOCOL_CHOICES: [&str; 3] = ["IMAP", "POP3", "JMAP"];

/// Where JMAP sits on the Protocol row.
pub const JMAP_INDEX: u32 = 2;

/// Whether the Protocol row's `index` is JMAP.
pub fn jmap_at(index: u32) -> bool {
    index == JMAP_INDEX
}

pub(crate) fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| url.to_string())
}

pub fn jmap_proposal(
    found: mailrs_discover::JmapFound,
    imap: Option<Proposal>,
    address: &Address,
) -> JmapProposal {
    let provider_name = found
        .provider
        .as_ref()
        .map_or_else(|| host_of(&found.session_url), |p| p.name.clone());
    let token = found
        .provider
        .as_ref()
        .is_some_and(|p| p.token_url.is_some());
    JmapProposal {
        address: address.full(),
        provider_name,
        session_url: found.session_url,
        confirm: found.confirm,
        source: Some(found.source),
        provider: found.provider,
        imap: imap.map(Box::new),
        user: None,
        token,
    }
}

/// Whether the provider signs JMAP in with a token alone: its table entry
/// names a token page.
pub fn token_only(proposal: &JmapProposal) -> bool {
    proposal
        .provider
        .as_ref()
        .is_some_and(|p| p.token_url.is_some())
}

/// The secret field's title.
pub fn jmap_secret_title(token: bool) -> String {
    match token {
        true => gettext("API Token"),
        false => gettext("Password"),
    }
}

/// The card under the field: for a token, what to make and where.
pub fn jmap_hint(proposal: &JmapProposal, token: bool) -> Option<(String, String, Option<Link>)> {
    if !token {
        return None;
    }
    let provider = post::short_name(&proposal.provider_name);
    let named = [("provider", provider.as_str())];
    let info = proposal.provider.as_ref();
    let access = info
        .map(|p| p.token_access.join(", "))
        .filter(|a| !a.is_empty());
    let mut body = match access {
        Some(access) => fill(
            &gettext(
                "Make an API token in {provider}'s settings with this access: {access}. Paste it here.",
            ),
            &[("provider", provider.as_str()), ("access", access.as_str())],
        ),
        None => fill(
            &gettext("Make an API token in {provider}'s settings and paste it here."),
            &named,
        ),
    };
    if let Some(plan) = info.and_then(|p| p.token_plan_above.as_deref()) {
        body.push(' ');
        body.push_str(&fill(
            &gettext("Tokens need a plan above {plan}."),
            &[("plan", plan)],
        ));
    }
    let link = token_link(proposal);
    Some((
        fill(&gettext("{provider} signs in with an API token"), &named),
        body,
        link,
    ))
}

/// The provider's page on making a token, when the table knows one.
fn token_link(proposal: &JmapProposal) -> Option<Link> {
    let url = proposal.provider.as_ref()?.token_url.clone()?;
    Some(Link {
        label: gettext("How to Make a Token"),
        url,
    })
}

/// The JMAP sign-in's heading: the provider the list knows, or else the
/// address's domain, as the IMAP page names a server it does not know.
pub fn jmap_title(proposal: &JmapProposal, address: &Address) -> String {
    match &proposal.provider {
        Some(_) => fill(
            &gettext("Sign in to {provider}"),
            &[("provider", &post::short_name(&proposal.provider_name))],
        ),
        None => fill(
            &gettext("Sign in to {domain}"),
            &[("domain", &address.domain)],
        ),
    }
}

/// The lede on the JMAP sign-in: where the session came from.
pub fn jmap_found_line(proposal: &JmapProposal, address: &Address) -> String {
    if proposal.source == Some(Source::Manual) {
        return gettext("The JMAP server you gave.");
    }
    match &proposal.provider {
        Some(_) => fill(
            &gettext("Penguin Mail found {provider}'s JMAP server."),
            &[("provider", &post::short_name(&proposal.provider_name))],
        ),
        None => fill(
            &gettext("Penguin Mail found a JMAP server for {domain}."),
            &[("domain", &address.domain)],
        ),
    }
}

/// The session row's line: the host the secret goes to.
pub fn session_line(proposal: &JmapProposal) -> String {
    host_of(&proposal.session_url)
}

/// The question a session outside the address's domain asks before the
/// secret goes there: its title and the line under it, both naming the
/// host.
pub fn jmap_agree(proposal: &JmapProposal, address: &Address, token: bool) -> (String, String) {
    let host = session_line(proposal);
    let named = [("host", host.as_str()), ("domain", address.domain.as_str())];
    let body = match token {
        true => gettext(
            "{host} is outside {domain}. Your token goes there only after you check the box.",
        ),
        false => gettext(
            "{host} is outside {domain}. Your password goes there only after you check the box.",
        ),
    };
    (fill(&gettext("Use {host}"), &named), fill(&body, &named))
}

/// The line under Use IMAP Instead: an app password where the provider
/// wants one.
pub fn imap_instead_line(proposal: &JmapProposal) -> String {
    let info = proposal.imap.as_ref().and_then(|imap| imap.info.as_ref());
    match info.map(|info| info.password) {
        Some(PasswordKind::AppPassword | PasswordKind::AppPasswordWithTwoStep) => {
            gettext("Sign in with an app password over IMAP and SMTP")
        }
        _ => gettext("Sign in with your password over IMAP and SMTP"),
    }
}

/// Whether Sign In can go: there is a secret, the person said yes to a
/// session outside the domain, and no sign-in is on its way. Nothing
/// reaches the session before this says yes.
pub fn can_sign_in_jmap(
    secret: &str,
    proposal: &JmapProposal,
    confirmed: bool,
    running: bool,
) -> bool {
    !running && !secret.is_empty() && (confirmed || !proposal.confirm)
}

/// A session URL the person typed in Server Settings: a bare host means
/// its well-known URL; plain HTTP only on this computer. A URL on the host
/// of `before`, the session the page held, keeps what was known of it,
/// its question among them: Server Settings left unchanged must not
/// take away a yes the person still owes.
pub fn typed_jmap(
    url: &str,
    user: &str,
    address: &Address,
    before: Option<&JmapProposal>,
) -> Result<JmapProposal, String> {
    let url = url.trim();
    let unusable = || gettext("Enter the server's address or its JMAP session URL.");
    if url.is_empty() {
        return Err(unusable());
    }
    let full = match url.contains("://") {
        true => url.to_string(),
        false => format!("https://{}/.well-known/jmap", url.trim_end_matches('/')),
    };
    let parsed = url::Url::parse(&full).map_err(|_| unusable())?;
    let local = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && local) {
        return Err(gettext(
            "Penguin Mail reaches JMAP servers over HTTPS only.",
        ));
    }
    let user = user.trim();
    let user = (!user.is_empty()).then(|| user.to_string());
    let held = before.filter(|b| {
        b.address.eq_ignore_ascii_case(&address.full()) && host_of(&b.session_url) == host_of(&full)
    });
    if let Some(before) = held {
        return Ok(JmapProposal {
            session_url: full,
            user,
            ..before.clone()
        });
    }
    Ok(JmapProposal {
        address: address.full(),
        provider_name: parsed
            .host_str()
            .map_or_else(|| address.domain.clone(), str::to_string),
        session_url: full,
        confirm: false,
        source: Some(Source::Manual),
        provider: None,
        imap: None,
        user,
        token: false,
    })
}

/// A typed bare host once its well-known URL was followed without
/// credentials: the session lives at `target`. A target on another host,
/// outside the address's domain, waits for the person's yes, as one
/// discovery found would. `None` (nothing answered) keeps the well-known
/// URL for the sign-in to try.
pub fn located_jmap(
    proposal: JmapProposal,
    target: Option<String>,
    address: &Address,
) -> JmapProposal {
    let Some(target) = target else {
        return proposal;
    };
    let typed_host = host_of(&proposal.session_url);
    let host = host_of(&target);
    let domain = &address.domain;
    let within = host == *domain || host.ends_with(&format!(".{domain}"));
    let away = host != typed_host && !within;
    JmapProposal {
        session_url: target,
        confirm: proposal.confirm || away,
        ..proposal
    }
}

/// The JMAP sign-in for an account signing in again, from its saved row.
pub fn saved_jmap(account: &Account, saved: &mailrs_store::jmap::JmapSaved) -> JmapProposal {
    JmapProposal {
        address: account.email.clone(),
        provider_name: account.provider_name().to_string(),
        session_url: saved.session_url.clone(),
        confirm: false,
        source: None,
        provider: mailrs_discover::jmap_named(account.provider_name()),
        imap: None,
        user: saved.user_name.clone(),
        token: saved.auth == mailrs_store::jmap::JmapAuth::Bearer,
    }
}

/// The try the JMAP Sign In makes. The secret goes as typed.
#[derive(Clone)]
pub struct JmapAttempt {
    /// The address the account takes: the one the person typed. A server
    /// that names no address of its own (Cyrus) signs in the same way.
    pub address: String,
    pub provider_name: String,
    pub session_url: String,
    pub token: bool,
    /// The user name Basic sends.
    pub user: String,
    pub secret: String,
    /// The app password for the calendar, when the person typed one.
    pub dav_password: Option<String>,
}

pub fn jmap_attempt(
    address: &Address,
    proposal: &JmapProposal,
    secret: &str,
    dav_password: &str,
    token: bool,
) -> JmapAttempt {
    JmapAttempt {
        address: address.full(),
        provider_name: proposal.provider_name.clone(),
        session_url: proposal.session_url.clone(),
        token,
        user: proposal.user.clone().unwrap_or_else(|| address.full()),
        secret: secret.to_string(),
        dav_password: (!dav_password.is_empty()).then(|| dav_password.to_string()),
    }
}

/// Whether the JMAP sign-in offers an app password for the calendar: a
/// token signs in to JMAP alone, and the provider's table entry names a
/// CalDAV server.
pub fn asks_dav_password(proposal: &JmapProposal, token: bool) -> bool {
    token
        && proposal
            .provider
            .as_ref()
            .and_then(|p| p.imap_host.as_deref())
            .is_some_and(|host| mailrs_discover::services_of(host).caldav.is_some())
}

/// Whether the password page asks for the calendar's app password when
/// an account signs in again: it signed in through the browser, and its
/// calendar or contacts server, in `refused`, refused that sign-in. A
/// token account asks by [`asks_dav_password`].
pub fn asks_dav_password_again(
    saved: &mailrs_store::jmap::JmapSaved,
    refused: &[mailrs_store::services::ServiceKind],
) -> bool {
    use mailrs_store::services::ServiceKind;

    saved.auth == mailrs_store::jmap::JmapAuth::OAuth
        && refused
            .iter()
            .any(|kind| matches!(kind, ServiceKind::CalDav | ServiceKind::CardDav))
}

/// The session a browser sign-in for `proposal` goes to.
pub fn jmap_browser(proposal: &JmapProposal) -> mailrs_sync::sign_in::JmapBrowser {
    mailrs_sync::sign_in::JmapBrowser {
        session_url: proposal.session_url.clone(),
        provider: proposal.provider.clone(),
    }
}

/// The line under Sign In With Browser. Where the table's token card
/// lists Masked Email, it says that only a token gives it: Fastmail grants
/// no Masked Email scope to an app that signs in through the browser.
pub fn browser_line(proposal: &JmapProposal) -> String {
    let line = fill(
        &gettext("Sign in on {provider}'s own page instead of making a token."),
        &[("provider", &post::short_name(&proposal.provider_name))],
    );
    let masked = proposal
        .provider
        .as_ref()
        .is_some_and(|p| p.token_access.iter().any(|access| access == "Masked Email"));
    match masked {
        true => format!("{line} {}", gettext("Masked Email needs an API token.")),
        false => line,
    }
}

/// Whether Sign In With Browser can start: nothing else is signing in, and
/// the person said yes to a session outside their domain, since the
/// account's sign-in goes there as it would with a token.
pub fn can_sign_in_jmap_browser(proposal: &JmapProposal, confirmed: bool, running: bool) -> bool {
    !running && (confirmed || !proposal.confirm)
}

/// The numbered steps the waiting page lists for a JMAP server's own
/// sign-in page, which may name its buttons in any way.
pub fn jmap_browser_steps(provider: &str) -> Vec<String> {
    vec![
        fill(
            &gettext("Sign in to {provider} if it asks."),
            &[("provider", provider)],
        ),
        gettext("Read what Penguin Mail asks for and allow it."),
    ]
}

/// What the password page says after Sign In With Browser failed. The
/// server's errors get the dialog's own words; the rest say their own
/// sentence, as the Google and Microsoft sign-ins do.
pub fn jmap_browser_failure(err: &anyhow::Error, proposal: &JmapProposal) -> Failure {
    use mailrs_jmap::JmapError;
    use mailrs_sync::sign_in::BrowserSignInError;
    let Some(BrowserSignInError::Jmap(jmap)) = err.downcast_ref::<BrowserSignInError>() else {
        return Failure::plain(
            err.to_string(),
            gettext("Could not sign in"),
            err.to_string(),
        );
    };
    let provider = post::short_name(&proposal.provider_name);
    let named = [("provider", provider.as_str())];
    let mut said = None;
    let (title, body) = match jmap {
        JmapError::Unauthorized
        | JmapError::BadCredential
        | JmapError::UnknownClient
        | JmapError::Forbidden(_) => (
            fill(&gettext("{provider} refused the sign-in"), &named),
            gettext("Choose Sign In With Browser to try again."),
        ),
        JmapError::Declined => (
            gettext("The sign-in was declined"),
            fill(
                &gettext("Choose Sign In With Browser and allow access on {provider}'s page."),
                &named,
            ),
        ),
        JmapError::Network(_) => (
            fill(&gettext("{provider} does not answer"), &named),
            gettext("Check your connection, then choose Sign In With Browser."),
        ),
        JmapError::RateLimited { .. } | JmapError::SignInLocked => (
            mailrs_sync::SyncError::asked_to_wait(&provider),
            gettext("Wait a minute, then choose Sign In With Browser."),
        ),
        other => {
            said = Some(other.to_string());
            (
                gettext("Could not sign in"),
                fill(
                    &gettext("{provider} gave an answer Penguin Mail cannot use. Try again later."),
                    &named,
                ),
            )
        }
    };
    Failure {
        line: format!("{title}. {body}"),
        title,
        body,
        said,
        links: Vec::new(),
        copy: None,
        kind: FailureKind::Refused,
    }
}

/// What the JMAP sign-in says after a failure, in the dialog's own words.
/// An error that is not the server's, such as an address another account
/// holds, says its own sentence.
pub fn jmap_failure(err: &anyhow::Error, proposal: &JmapProposal, token: bool) -> Failure {
    use mailrs_jmap::JmapError;
    let provider = post::short_name(&proposal.provider_name);
    let named = [("provider", provider.as_str())];
    let Some(jmap) = err.downcast_ref::<JmapError>() else {
        return Failure::plain(
            err.to_string(),
            gettext("Could not sign in"),
            err.to_string(),
        );
    };
    let mut said = None;
    let imap = proposal.imap.is_some();
    let (title, body) = match jmap {
        JmapError::Unauthorized => match token {
            true => (
                fill(&gettext("{provider} refused the token"), &named),
                gettext("Check the token and the access it was made with, then choose Sign In."),
            ),
            false => (
                fill(&gettext("{provider} refused the password"), &named),
                gettext("Check the password, then choose Sign In."),
            ),
        },
        JmapError::BadCredential => (
            match token {
                true => gettext("The token holds a character that cannot be sent"),
                false => gettext("The password holds a character that cannot be sent"),
            },
            gettext("Paste it again without the line break, then choose Sign In."),
        ),
        JmapError::NotFound => (
            fill(
                &gettext("No JMAP server at {host}"),
                &[("host", &session_line(proposal))],
            ),
            match imap {
                true => gettext("Check the session URL in Server Settings, or use IMAP instead."),
                false => gettext("Check the session URL in Server Settings."),
            },
        ),
        JmapError::UnknownCapability(_) => (
            fill(&gettext("{provider} offers no mail over JMAP"), &named),
            match imap {
                true => gettext("Use IMAP for this account instead."),
                false => gettext("Choose IMAP in Server Settings to add this account."),
            },
        ),
        JmapError::OffHost(other) => {
            let other = [("other", other.as_str())];
            (
                fill(
                    &gettext("{host} sends the sign-in elsewhere"),
                    &[("host", &session_line(proposal))],
                ),
                fill(
                    &gettext(
                        "It tried to pass your sign-in on to {other}, and Penguin Mail refused. If you trust {other}, enter its session URL in Server Settings.",
                    ),
                    &other,
                ),
            )
        }
        JmapError::Network(_) => (
            fill(&gettext("{provider} does not answer"), &named),
            gettext("Check your connection, then choose Sign In."),
        ),
        JmapError::RateLimited { .. } => (
            mailrs_sync::SyncError::asked_to_wait(&provider),
            gettext("Wait a minute, then choose Sign In."),
        ),
        other => {
            said = Some(other.to_string());
            (
                gettext("Could not sign in"),
                fill(
                    &match imap {
                        true => gettext(
                            "{provider} gave an answer Penguin Mail cannot use. Try again later, or use IMAP instead.",
                        ),
                        false => gettext(
                            "{provider} gave an answer Penguin Mail cannot use. Try again later.",
                        ),
                    },
                    &named,
                ),
            )
        }
    };
    let refused = matches!(jmap, JmapError::Unauthorized | JmapError::BadCredential);
    let links = token_link(proposal)
        .filter(|_| token && refused)
        .into_iter()
        .collect();
    Failure {
        line: format!("{title}. {body}"),
        title,
        body,
        said,
        links,
        copy: None,
        kind: FailureKind::Refused,
    }
}

/// What kind of failure a sign-in met, for a log line. Never the error's
/// own text: some name the address, or carry the server's words.
pub fn failure_kind(err: &anyhow::Error) -> &'static str {
    use mailrs_jmap::JmapError;
    use mailrs_sync::sign_in::{BrowserSignInError, ImapSignInError, JmapSignInError};
    if let Some(jmap) = err.downcast_ref::<JmapError>() {
        return match jmap {
            JmapError::Unauthorized => "unauthorized",
            JmapError::Network(_) => "network",
            JmapError::RateLimited { .. } => "rate limited",
            JmapError::RequestTooLarge => "request too large",
            JmapError::AnswerTooLarge { .. } => "answer too large",
            JmapError::OffHost(_) => "off host",
            JmapError::BadUrl(_) => "bad url",
            JmapError::BadCredential => "bad credential",
            JmapError::CannotCalculateChanges => "cannot calculate changes",
            JmapError::StateMismatch => "state mismatch",
            JmapError::Forbidden(_) => "forbidden",
            JmapError::NotFound => "not found",
            JmapError::UnknownCapability(_) => "unknown capability",
            JmapError::Method { .. } => "method",
            JmapError::Problem { .. } => "problem",
            JmapError::Decode(_) => "decode",
            JmapError::HoldUnknown => "hold unknown",
            JmapError::SubmitUnknown => "submit unknown",
            JmapError::LocalFile(_) => "local file",
            JmapError::SignInLocked => "sign-in locked",
            JmapError::Declined => "declined",
            JmapError::NotHttps => "not https",
            JmapError::IssuerMismatch => "issuer mismatch",
            JmapError::BadRedirect => "bad redirect",
            JmapError::NotBearer => "not bearer",
            JmapError::UnknownClient => "unknown client",
        };
    }
    if let Some(browser) = err.downcast_ref::<BrowserSignInError>() {
        return match browser {
            BrowserSignInError::NoClient(_) => "no client",
            BrowserSignInError::NeedsRoute => "needs route",
            BrowserSignInError::AlreadyHeld => "already held",
            BrowserSignInError::OtherAddress { .. } => "other address",
            BrowserSignInError::Google(_) => "google",
            BrowserSignInError::Microsoft(_) => "microsoft",
            BrowserSignInError::KeptJmap(JmapSignInError::DifferentAccount { .. }) => {
                "different account"
            }
            BrowserSignInError::Kept(_)
            | BrowserSignInError::KeptMicrosoft(_)
            | BrowserSignInError::KeptJmap(_) => "keeping the account",
            BrowserSignInError::Connect(_) => "connect",
            BrowserSignInError::OwnClient(_) => "own client",
            BrowserSignInError::OwnClientMissing => "own client missing",
            BrowserSignInError::Jmap(_) => "jmap",
            BrowserSignInError::NotOffered => "not offered",
            BrowserSignInError::NoRefreshToken => "no refresh token",
            BrowserSignInError::GaveUp => "gave up",
            BrowserSignInError::Canceled => "canceled",
        };
    }
    if let Some(check) = err.downcast_ref::<CheckError>() {
        return match check {
            CheckError::Imap(_) => "incoming server",
            CheckError::Smtp(_) => "outgoing server",
        };
    }
    match (
        err.downcast_ref::<JmapSignInError>(),
        err.downcast_ref::<ImapSignInError>(),
    ) {
        (Some(JmapSignInError::Taken { .. }), _) | (_, Some(ImapSignInError::Taken { .. })) => {
            "address taken"
        }
        (Some(JmapSignInError::DifferentAccount { .. }), _) => "different account",
        (Some(JmapSignInError::Store(_)), _) | (_, Some(ImapSignInError::Store(_))) => "store",
        (Some(JmapSignInError::Password(_)), _) | (_, Some(ImapSignInError::Password(_))) => {
            "keyring"
        }
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use mailrs_discover::{
        Candidate, Found, PasswordKind, ProviderInfo, Security, Server, Source, Unreachable,
        UserName, Verdict,
    };
    use mailrs_domain::RemoveSetting;
    use mailrs_domain::{Account, AccountState, Provider};
    use mailrs_imap::{CheckError, ImapError};
    use mailrs_store::servers::{Pop3Servers, Saved, Servers};

    use super::*;

    use mailrs_sync::sign_in::CalDavSignInError;
    use mailrs_sync::{BackendError, SyncError};

    #[test]
    fn each_way_a_calendar_sign_in_fails_has_its_own_words() {
        let no_calendar = "No calendar was found";
        let failures = [
            (BackendError::NeedsReauth, "app password"),
            (BackendError::Offline("no route".into()), "could not reach"),
            (BackendError::Unsupported, no_calendar),
            // A web page at the address, or a server that is no CalDAV
            // server, answers the login try in the protocol's own words.
            (
                BackendError::Refused("HTTP 404: Not Found".into()),
                no_calendar,
            ),
            (
                BackendError::Refused("HTTP 405: Method Not Allowed".into()),
                no_calendar,
            ),
            (
                BackendError::Refused("the server named no principal".into()),
                no_calendar,
            ),
        ];
        for (failure, words) in failures {
            let shown = calendar_problem(
                &CalDavSignInError::Server(SyncError::Backend(failure)).into(),
                None,
            );
            assert!(shown.contains(words) && !shown.contains("HTTP "), "{shown}");
        }
        let not_https = calendar_problem(&CalDavSignInError::NotHttps.into(), None);
        assert!(
            not_https.contains("HTTPS only"),
            "words the sign-in chose stand: {not_https}"
        );
        let other_user = CalDavSignInError::OtherUser {
            name: "robin@cloud.example.org".into(),
            user: "Robin".into(),
        };
        let shown = calendar_problem(&other_user.into(), None);
        assert!(
            shown.contains("signed in to this server as Robin. Type Robin"),
            "{shown}"
        );
    }

    #[test]
    fn a_taken_name_says_whose_it_is() {
        let taken = |holder| -> anyhow::Error {
            CalDavSignInError::Taken {
                name: "robin@example.org".into(),
                provider: "Fastmail".into(),
                holder,
            }
            .into()
        };
        let by_mail = calendar_problem(&taken(Provider::Imap), None);
        assert!(
            by_mail.contains("robin@example.org") && by_mail.contains("Fastmail"),
            "{by_mail}"
        );
        assert!(
            by_mail.contains("Preferences"),
            "an IMAP account can take the server itself: {by_mail}"
        );
        // The group the account's servers sit in on the Accounts page,
        // in contacts_prefs.rs, word for word.
        assert!(
            by_mail.contains("under Calendar, Contacts and Rules Servers"),
            "{by_mail}"
        );
        assert!(calendar_problem(&taken(Provider::Pop3), None).contains("Preferences"));
        let by_google = calendar_problem(&taken(Provider::Gmail), None);
        assert!(
            by_google.contains("robin@example.org") && !by_google.contains("Preferences"),
            "{by_google}"
        );
    }

    #[test]
    fn anything_else_keeps_its_reason() {
        let full = || anyhow::anyhow!("the disk is full");
        assert_eq!(
            calendar_problem(&full(), None),
            "Could not add the calendar: the disk is full"
        );
        let again = icloud_account();
        assert_eq!(
            calendar_problem(&full(), Some(&again)),
            "Could not sign in: the disk is full",
            "Sign In Again adds nothing"
        );
    }

    /// Sign In Again opens the calendar page for one account. A sign-in
    /// that ends as another account, because the address or the user
    /// name changed, added one and left the first as it was.
    #[test]
    fn only_the_account_asked_to_sign_in_again_has() {
        let asked = icloud_account();
        let another = Account {
            id: asked.id + 1,
            ..asked.clone()
        };
        assert!(signed_in_again(Some(&asked), &asked));
        assert!(!signed_in_again(Some(&asked), &another));
        assert!(
            !signed_in_again(None, &asked),
            "a first sign-in is not a second"
        );
    }

    #[test]
    fn the_calendar_page_waits_for_an_address_a_user_and_a_password() {
        assert!(calendar_ready("cloud.example.org", "robin", "secret"));
        assert!(!calendar_ready("  ", "robin", "secret"));
        assert!(!calendar_ready("cloud.example.org", " ", "secret"));
        assert!(!calendar_ready("cloud.example.org", "robin", ""));
        assert!(
            calendar_ready("cloud.example.org", "robin", " "),
            "a password may be a space"
        );
    }

    #[test]
    fn a_pop3_account_is_done_when_its_first_check_is() {
        let page = added_page(Provider::Pop3, RemoveSetting::Never, false, true);
        assert!(page.done);
    }

    #[test]
    fn a_pop3_account_is_not_done_before_its_first_check() {
        let page = added_page(Provider::Pop3, RemoveSetting::Never, false, false);
        assert!(!page.done);
    }

    #[test]
    fn a_pop3_account_ignores_the_backfill_marker() {
        let page = added_page(Provider::Pop3, RemoveSetting::Never, true, false);
        assert!(!page.done);
    }

    #[test]
    fn other_accounts_are_done_with_their_backfill_and_ignore_the_pop3_marker() {
        assert!(added_page(Provider::Imap, RemoveSetting::Never, true, false).done);
        assert!(!added_page(Provider::Imap, RemoveSetting::Never, false, true).done);
        assert!(added_page(Provider::Gmail, RemoveSetting::Never, true, false).done);
        assert!(!added_page(Provider::Microsoft, RemoveSetting::Never, false, true).done);
    }

    #[test]
    fn the_ready_page_says_mail_is_downloading_until_the_first_download_ends() {
        for provider in [
            Provider::Gmail,
            Provider::Imap,
            Provider::Microsoft,
            Provider::Pop3,
        ] {
            let page = added_page(provider, RemoveSetting::Never, false, false);
            assert_eq!(
                page.lede, "Mail is downloading. You can close this window.",
                "{provider:?}"
            );
        }
    }

    #[test]
    fn the_ready_page_says_the_mail_is_here_once_the_first_download_ends() {
        for (provider, backfill, first_check) in [
            (Provider::Gmail, true, false),
            (Provider::Imap, true, false),
            (Provider::Microsoft, true, false),
            (Provider::Pop3, false, true),
        ] {
            let page = added_page(provider, RemoveSetting::Never, backfill, first_check);
            assert_eq!(page.lede, "Your mail is here.", "{provider:?}");
        }
    }

    #[test]
    fn only_non_pop3_accounts_are_told_newest_mail_comes_first() {
        for provider in [Provider::Gmail, Provider::Imap, Provider::Microsoft] {
            let page = added_page(provider, RemoveSetting::Never, false, false);
            assert!(page.note.starts_with("Newest mail comes first"));
        }
        let page = added_page(Provider::Pop3, RemoveSetting::Never, false, false);
        assert!(page.note.contains("downloads the server's mail into Inbox"));
        assert!(!page.note.contains("Newest"));
    }

    #[test]
    fn the_pop3_note_says_what_happens_to_mail_on_the_server() {
        let note = |remove| added_page(Provider::Pop3, remove, false, false).note;
        assert!(note(RemoveSetting::Never).contains("Mail stays on the server."));
        assert!(
            note(RemoveSetting::Downloaded).contains("removes each message from the server after")
        );
        assert!(note(RemoveSetting::Days(30)).contains("30 days after"));
        assert!(note(RemoveSetting::Days(1)).contains("1 day after"));
        assert!(!note(RemoveSetting::Days(1)).contains("1 days"));
    }

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
                imap: Some(server("imap.example.org", 993)),
                smtp: server("smtp.example.org", 465),
                pop3: None,
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
        match after_discovery(
            found(Some(fastmail_info()), Source::Table, false),
            None,
            &dana(),
            true,
        ) {
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
        assert_eq!(proposal.incoming.host, "imap.example.org");
    }

    #[test]
    fn a_server_nobody_lists_is_named_after_the_domain_and_asks_first() {
        let address = Address::parse("me@example.org").unwrap();
        let Next::Password(proposal) =
            after_discovery(found(None, Source::Srv, true), None, &address, true)
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
        let Next::Password(proposal) = after_discovery(found, None, &address, true) else {
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
            after_discovery(tuta, None, &dana(), true),
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
            after_discovery(proton, None, &dana(), true),
            Next::Closed("Penguin Mail cannot reach Proton Mail yet.".into())
        );
    }

    #[test]
    fn a_microsoft_domain_goes_to_microsofts_sign_in() {
        assert_eq!(
            after_discovery(verdict(Verdict::Microsoft), None, &dana(), true),
            Next::Microsoft
        );
        assert_eq!(
            after_discovery(verdict(Verdict::Microsoft), None, &dana(), false),
            Next::Closed("This copy of Penguin Mail was built without Microsoft sign-in.".into())
        );
        assert_eq!(
            after_discovery(verdict(Verdict::Google), None, &dana(), true),
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
        assert_eq!(proposal.incoming, server("imap.fastmail.com", 993));
        assert_eq!(proposal.smtp, server("smtp.fastmail.com", 465));
        assert!(
            proposal
                .info
                .is_some_and(|info| info.password == PasswordKind::AppPassword),
            "Fastmail's own password rule, not the guess's default"
        );
    }

    #[test]
    fn guessing_an_unlisted_domain_still_falls_back_to_host_names() {
        let address = Address::parse("me@example.org").unwrap();
        assert_eq!(
            guess(&address),
            Proposal {
                address: "me@example.org".into(),
                provider_name: "example.org".into(),
                info: None,
                protocol: Protocol::Imap,
                incoming: server("imap.example.org", 993),
                smtp: server("smtp.example.org", 465),
                incoming_user: None,
                smtp_user: None,
                confirm: false,
                source: None,
                remaining: Vec::new(),
                pop3_offer: None,
                pop3_remove: None,
                own_smtp_password: false,
            }
        );
    }

    #[test]
    fn nothing_found_opens_server_settings_with_a_guess() {
        let address = Address::parse("me@example.org").unwrap();
        let Next::Manual { proposal, line } =
            after_discovery(verdict(Verdict::NothingFound), None, &address, true)
        else {
            panic!("expected Server Settings");
        };
        assert_eq!(proposal.incoming, server("imap.example.org", 993));
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
        let tried = attempt(&dana(), &fastmail(), " abcd efgh ", "");
        assert_eq!(tried.password, " abcd efgh ");
        assert_eq!(tried.incoming_login, "Dana@fastmail.com");
        assert_eq!(tried.smtp_login, "Dana@fastmail.com");
        assert_eq!(tried.provider_name, "Fastmail");
    }

    #[test]
    fn a_user_name_typed_in_server_settings_is_who_logs_in() {
        let typed = Proposal {
            incoming_user: Some("dana".into()),
            smtp_user: Some("dana@fastmail.com".into()),
            ..fastmail()
        };
        let tried = attempt(&dana(), &typed, "pw", "");
        assert_eq!(tried.incoming_login, "dana");
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
            "Could not verify the secure connection to mail.example.org."
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
        assert_eq!(proposal.incoming.host, "imap.example.org");
        assert_eq!(proposal.incoming_user, None);
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

    fn kept(imap_user: &str, smtp_user: &str) -> Kept {
        let saved = |host: &str, port, user: &str| Saved {
            host: host.into(),
            port,
            security: mailrs_store::servers::Security::Tls,
            user_name: user.into(),
        };
        Kept::Imap(Servers {
            imap: saved("imap.mail.me.com", 993, imap_user),
            smtp: saved("smtp.mail.me.com", 587, smtp_user),
        })
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
    fn an_account_signed_in_through_the_browser_is_asked_to_sign_in_there_again() {
        let account = Account {
            email: "dana@fastmail.com".into(),
            provider: Provider::Jmap,
            provider_name: Some("Fastmail".into()),
            ..icloud_account()
        };
        assert_eq!(
            again_browser_line(&account),
            "Choose Sign In With Browser to sign in to dana@fastmail.com again."
        );
        assert_eq!(
            sign_in_again_banner("dana@fastmail.com", true),
            "Sign in again through the browser to keep dana@fastmail.com syncing"
        );
        assert_eq!(
            sign_in_again_banner("dana@fastmail.com", false),
            "Sign in again to keep dana@fastmail.com syncing"
        );
    }

    #[test]
    fn signing_in_again_starts_from_the_servers_the_account_kept() {
        let account = icloud_account();
        let proposal = saved_proposal(&account, &kept("dana", "dana@icloud.com"));
        assert_eq!(proposal.provider_name, "iCloud Mail");
        assert_eq!(proposal.incoming.host, "imap.mail.me.com");
        // Each server gets back the name it took last time from the
        // address: the part before @ for IMAP, the whole address for SMTP.
        assert_eq!(proposal.incoming.user_name, UserName::LocalPartFirst);
        assert_eq!(proposal.smtp.user_name, UserName::Address);
        assert_eq!(proposal.incoming_user, None);
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
        assert_eq!(proposal.incoming_user.as_deref(), Some("d.santos"));
        assert_eq!(proposal.smtp_user.as_deref(), Some("d.santos"));
        assert_eq!(proposal.incoming.user_name, UserName::Address);
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
        let proposal = saved_proposal(&account, &Kept::Imap(saved));
        assert_eq!(proposal.provider_name, "Fastmail");
        assert!(
            proposal
                .info
                .is_some_and(|info| info.password == PasswordKind::AppPassword)
        );
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
                    imap: Some(server("imap1.example.org", 993)),
                    smtp: server("smtp1.example.org", 465),
                    pop3: None,
                    confirm: false,
                },
                Candidate {
                    source: Source::Probe,
                    provider: None,
                    imap: Some(server("imap2.example.org", 993)),
                    smtp: server("smtp2.example.org", 465),
                    pop3: None,
                    confirm: second_confirm,
                },
            ],
        }
    }

    /// A provider that offers POP3 and no IMAP: the password step signs in
    /// to its POP3 server, leaving mail on the server.
    #[test]
    fn a_pop3_only_provider_goes_to_the_password_step_on_pop3() {
        let found = Found {
            verdict: Verdict::Servers,
            candidates: vec![Candidate {
                source: Source::Srv,
                provider: None,
                imap: None,
                smtp: server("smtp.example.org", 465),
                pop3: Some(server("pop.example.org", 995)),
                confirm: false,
            }],
        };
        let address = Address::parse("ann@example.org").unwrap();
        let Next::Password(proposal) = after_discovery(found, None, &address, true) else {
            panic!("expected the password step");
        };
        assert_eq!(proposal.protocol, Protocol::Pop3);
        assert_eq!(proposal.incoming, server("pop.example.org", 995));
        assert_eq!(proposal.pop3_remove, Some(RemoveSetting::Never));
        assert_eq!(proposal.pop3_offer, None, "nothing to offer beside it");
        assert_eq!(incoming_title(&proposal), "Incoming over POP3");
    }

    #[test]
    fn the_first_candidates_port_is_blocked_the_second_signs_in() {
        let address = Address::parse("ann@example.org").unwrap();
        let Next::Password(first) = after_discovery(two_candidates(false), None, &address, true)
        else {
            panic!("expected the password step");
        };
        assert_eq!(first.incoming.host, "imap1.example.org");
        let blocked = anyhow::Error::new(CheckError::Imap(ImapError::Network(
            "connection refused".into(),
        )));
        let Outcome::TryNext(second) = after_failure(&blocked, &first, &address) else {
            panic!("expected the second candidate");
        };
        assert_eq!(second.incoming.host, "imap2.example.org");
        assert!(second.remaining.is_empty());
    }

    #[test]
    fn a_refused_password_never_reaches_for_another_candidate() {
        let address = Address::parse("ann@example.org").unwrap();
        let Next::Password(first) = after_discovery(two_candidates(false), None, &address, true)
        else {
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
        let Next::Password(first) = after_discovery(two_candidates(true), None, &address, true)
        else {
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
        assert_eq!(proposal.incoming_user.as_deref(), Some("d.santos"));
        assert_eq!(proposal.smtp_user.as_deref(), Some("dana@example.org"));
        assert_eq!(proposal.incoming.user_name, UserName::Address);
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
        let tried = attempt(&dana(), &proposal, "pw", "");
        assert_eq!(tried.smtp_login, "d.santos");
    }

    #[test]
    fn a_name_typed_for_one_server_is_kept_for_that_server_alone() {
        let proposal = saved_proposal(&icloud_account(), &kept("d.santos", "dana@icloud.com"));
        assert_eq!(proposal.incoming_user.as_deref(), Some("d.santos"));
        assert_eq!(proposal.incoming.user_name, UserName::Address);
        assert_eq!(proposal.smtp_user, None);
        assert_eq!(proposal.smtp.user_name, UserName::Address);
    }

    #[test]
    fn switching_security_moves_a_default_port_to_the_other_default() {
        use Role::{Incoming, Outgoing};
        assert_eq!(
            port_after_switch(Incoming, Protocol::Imap, 993, Security::StartTls),
            143
        );
        assert_eq!(
            port_after_switch(Incoming, Protocol::Imap, 143, Security::Tls),
            993
        );
        assert_eq!(
            port_after_switch(Outgoing, Protocol::Imap, 465, Security::StartTls),
            587
        );
        assert_eq!(
            port_after_switch(Outgoing, Protocol::Imap, 587, Security::Tls),
            465
        );
    }

    #[test]
    fn switching_security_keeps_a_port_the_person_chose() {
        assert_eq!(
            port_after_switch(Role::Incoming, Protocol::Imap, 1143, Security::StartTls),
            1143
        );
        assert_eq!(
            port_after_switch(Role::Outgoing, Protocol::Imap, 2525, Security::Tls),
            2525
        );
        // Already the new mode's default: nothing to move.
        assert_eq!(
            port_after_switch(Role::Outgoing, Protocol::Imap, 465, Security::Tls),
            465
        );
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
    fn an_outgoing_password_goes_only_when_server_settings_has_one() {
        let shared = attempt(&dana(), &fastmail(), "pw", "");
        assert!(shared.smtp_password.is_none());
        let own = with_smtp_password(fastmail(), "out");
        assert!(own.own_smtp_password);
        assert_eq!(
            attempt(&dana(), &own, "pw", "out").smtp_password.as_deref(),
            Some("out")
        );
        let cleared = with_smtp_password(own, "");
        assert!(!cleared.own_smtp_password);
        assert!(attempt(&dana(), &cleared, "pw", "").smtp_password.is_none());
    }

    #[test]
    fn a_refused_outgoing_password_of_its_own_is_named() {
        let refused = anyhow::Error::new(CheckError::Smtp(ImapError::Auth {
            text: "535 Bad credentials".into(),
        }));
        let said = failure(&refused, &with_smtp_password(fastmail(), "out"));
        assert_eq!(said.kind, FailureKind::Refused);
        assert_eq!(
            said.line,
            "The outgoing server refused the outgoing password. The server said: 535 Bad credentials"
        );
        assert_eq!(said.title, "The outgoing server refused its password");
        assert_eq!(
            said.body,
            "Check the outgoing password in Server Settings, then choose Sign In."
        );
    }

    #[test]
    fn a_shared_password_the_outgoing_server_refused_says_which_server() {
        let refused = anyhow::Error::new(CheckError::Smtp(ImapError::Auth {
            text: String::new(),
        }));
        let said = failure(&refused, &fastmail());
        assert_eq!(said.line, "The outgoing server refused the password.");
        assert_eq!(said.title, "Fastmail refused the password");
    }

    #[test]
    fn with_an_outgoing_password_apart_an_incoming_refusal_names_the_incoming_server() {
        let refused = anyhow::Error::new(CheckError::Imap(ImapError::Auth {
            text: String::new(),
        }));
        let said = failure(&refused, &with_smtp_password(fastmail(), "out"));
        assert_eq!(said.line, "The incoming server refused the password.");
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
    fn loopback_tls_failure_points_to_bridge_certificate_setup() {
        let mut proposal = fastmail();
        proposal.incoming.host = "127.0.0.1".into();
        proposal.smtp.host = "127.0.0.1".into();
        for error in [
            CheckError::Imap(ImapError::Tls {
                host: "127.0.0.1".into(),
                detail: "unknown issuer".into(),
            }),
            CheckError::Smtp(ImapError::Tls {
                host: "127.0.0.1".into(),
                detail: "unknown issuer".into(),
            }),
        ] {
            let failure = failure(&anyhow::Error::new(error), &proposal);
            assert!(failure.body.contains("Proton Mail Bridge"));
            assert!(failure.body.contains("certificate"));
            assert_eq!(failure.links.len(), 1);
            assert!(
                failure.links[0]
                    .url
                    .ends_with("docs/setup.md#proton-mail-bridge")
            );
        }
    }

    #[test]
    fn the_crates_english_for_the_log_never_reaches_the_card() {
        let server = fastmail();
        let host = server.incoming.host.clone();
        for err in [
            ImapError::Protocol("the server takes neither LOGIN nor AUTHENTICATE PLAIN".into()),
            ImapError::Invalid("\"bad flag\" is not a flag".into()),
        ] {
            let said = failure(&anyhow::Error::new(CheckError::Imap(err)), &server);
            for text in [&said.line, &said.body] {
                assert!(!text.contains("LOGIN") && !text.contains("flag"), "{text}");
            }
        }
        let said = failure(
            &anyhow::Error::new(CheckError::Imap(ImapError::Protocol("garbled".into()))),
            &server,
        );
        assert_eq!(
            said.body,
            format!("{host} answered in a way this app cannot work with.")
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
            incoming: server("imap.fastmail.com", 993),
            smtp: server("smtp.fastmail.com", 465),
            ..fastmail()
        };
        assert_eq!(servers_summary(&both), "imap and smtp.fastmail.com · TLS");
        let apart = Proposal {
            incoming: server("mail.example.org", 993),
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
        assert_eq!(folder_line(0, false), "Waiting");
        assert_eq!(folder_line(1, false), "1 conversation");
        assert_eq!(folder_line(312, false), "312 conversations");
    }

    #[test]
    fn a_folder_with_nothing_in_it_is_empty_once_the_download_ends() {
        assert_eq!(folder_line(0, true), "Empty");
        assert_eq!(folder_line(1, true), "1 conversation");
        assert_eq!(folder_line(312, true), "312 conversations");
    }

    #[test]
    fn the_note_stops_promising_older_mail_once_the_download_ends() {
        for provider in [Provider::Gmail, Provider::Imap, Provider::Microsoft] {
            let running = added_page(provider, RemoveSetting::Never, false, false).note;
            assert!(running.contains("Older mail keeps coming"), "{provider:?}");
            let done = added_page(provider, RemoveSetting::Never, true, false).note;
            assert_eq!(
                done, "Penguin Mail keeps checking for new mail, even with the window closed.",
                "{provider:?}"
            );
        }
    }

    #[test]
    fn the_pop3_note_is_the_same_before_and_after_the_first_check() {
        let before = added_page(Provider::Pop3, RemoveSetting::Never, false, false).note;
        let after = added_page(Provider::Pop3, RemoveSetting::Never, false, true).note;
        assert_eq!(before, after);
    }

    #[test]
    fn the_grant_lede_follows_the_first_download_as_the_ready_lede_does() {
        assert_eq!(
            grant_lede(Provider::Gmail, 2, false),
            "Mail is downloading. You left two boxes unticked on Google's page, so these features stay off:"
        );
        assert_eq!(
            grant_lede(Provider::Gmail, 2, true),
            "Your mail is here. You left two boxes unticked on Google's page, so these features stay off:"
        );
        assert_eq!(
            grant_lede(Provider::Gmail, 1, true),
            "Your mail is here. You left one box unticked on Google's page, so this feature stays off:"
        );
    }

    #[test]
    fn a_password_stays_only_for_the_address_it_was_typed_for() {
        // Back, a new address at another provider, Continue: the first
        // account's password must not be one Enter away from the second
        // provider's servers.
        assert!(!keeps_password(
            Some("a@fastmail.com"),
            "b@otherdomain.example"
        ));
        assert!(!keeps_password(None, "a@fastmail.com"));
        assert!(keeps_password(Some("a@fastmail.com"), "A@Fastmail.com"));
    }

    #[test]
    fn the_microsoft_grant_lede_names_the_organization_not_a_box() {
        // Microsoft's consent page has no boxes to untick. A permission a
        // Microsoft account lacks is one its organization refused.
        assert_eq!(
            grant_lede(Provider::Microsoft, 3, true),
            "Your mail is here. Your organization did not allow three of the permissions Penguin Mail asked for, so these features stay off:"
        );
        assert_eq!(
            grant_lede(Provider::Microsoft, 1, false),
            "Mail is downloading. Your organization did not allow one of the permissions Penguin Mail asked for, so this feature stays off:"
        );
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

    #[test]
    fn switching_protocol_from_pop3_back_to_imap_drops_the_removal_choice() {
        let typed = typed_servers(
            &typed("mail.example.org", 995, ""),
            &typed("smtp.example.org", 465, ""),
            &fastmail(),
            &dana(),
        )
        .unwrap();
        let pop3 = with_protocol(typed, Protocol::Pop3, RemoveSetting::Downloaded);
        assert_eq!(
            (pop3.protocol, pop3.pop3_remove),
            (Protocol::Pop3, Some(RemoveSetting::Downloaded))
        );
        assert_eq!(
            attempt(&dana(), &pop3, "pw", "").pop3_remove,
            RemoveSetting::Downloaded
        );
        let back = with_protocol(pop3, Protocol::Imap, RemoveSetting::Downloaded);
        assert_eq!((back.protocol, back.pop3_remove), (Protocol::Imap, None));
        let tried = attempt(&dana(), &back, "pw", "");
        assert_eq!(
            (tried.protocol, tried.pop3_remove),
            (Protocol::Imap, RemoveSetting::Never)
        );
    }

    #[test]
    fn a_provider_that_also_offers_pop3_says_so_and_keeps_imap_first() {
        let mut found = found(Some(fastmail_info()), Source::Table, false);
        found.candidates[0].pop3 = Some(server("pop.fastmail.com", 995));
        let Next::Password(proposal) = after_discovery(found, None, &dana(), true) else {
            panic!("step 2")
        };
        assert_eq!(proposal.protocol, Protocol::Imap);
        assert_eq!(
            pop3_line(&proposal).as_deref(),
            Some("Fastmail also offers POP3")
        );
        let pop3 = as_pop3(&proposal).expect("a POP3 proposal");
        assert_eq!(
            (pop3.protocol, pop3.incoming.host.as_str()),
            (Protocol::Pop3, "pop.fastmail.com")
        );
        assert_eq!(
            pop3.pop3_remove,
            Some(RemoveSetting::Never),
            "Leave on Server is the default"
        );
        assert_eq!(pop3_line(&pop3), None);
        assert_eq!(incoming_title(&pop3), "Incoming over POP3");
        assert_eq!(incoming_title(&proposal), "Incoming");
    }

    #[test]
    fn the_incoming_port_follows_the_protocol_unless_typed() {
        assert_eq!(port_after_protocol(993, Security::Tls, Protocol::Pop3), 995);
        assert_eq!(
            port_after_protocol(143, Security::StartTls, Protocol::Pop3),
            110
        );
        assert_eq!(port_after_protocol(995, Security::Tls, Protocol::Imap), 993);
        assert_eq!(
            port_after_protocol(1995, Security::Tls, Protocol::Imap),
            1995
        );
        assert_eq!(
            port_after_switch(Role::Incoming, Protocol::Pop3, 995, Security::StartTls),
            110
        );
        assert_eq!(
            port_after_switch(Role::Outgoing, Protocol::Pop3, 465, Security::StartTls),
            587
        );
    }

    #[test]
    fn the_removal_rows_name_their_setting() {
        assert_eq!(removal_title(Removal::AfterDays, 1), "Remove After 1 Day");
        assert_eq!(
            removal_title(Removal::AfterDays, 14),
            "Remove After 14 Days"
        );
        assert_eq!(
            removal_setting(Removal::AfterDays, 30),
            RemoveSetting::Days(30)
        );
        assert_eq!(
            removal_of(RemoveSetting::Days(30)),
            (Removal::AfterDays, 30)
        );
        assert_eq!(
            removal_of(RemoveSetting::Never),
            (Removal::Leave, DEFAULT_DAYS)
        );
    }

    #[test]
    fn a_pop3_account_signing_in_again_starts_as_pop3_with_its_setting() {
        let account = Account {
            provider: Provider::Pop3,
            provider_name: Some("example.org".into()),
            ..icloud_account()
        };
        let saved = |host: &str, port| Saved {
            host: host.into(),
            port,
            security: mailrs_store::servers::Security::Tls,
            user_name: "dana@icloud.com".into(),
        };
        let kept = Kept::Pop3(
            Pop3Servers {
                pop3: saved("pop.example.org", 995),
                smtp: saved("smtp.example.org", 465),
            },
            RemoveSetting::Days(7),
        );
        let proposal = saved_proposal(&account, &kept);
        assert_eq!(proposal.protocol, Protocol::Pop3);
        assert_eq!(proposal.incoming.host, "pop.example.org");
        assert_eq!(proposal.pop3_remove, Some(RemoveSetting::Days(7)));
    }

    fn fastmail_with_pop3() -> Proposal {
        let mut found = found(Some(fastmail_info()), Source::Table, false);
        found.candidates[0].pop3 = Some(server("pop.example.org", 995));
        match after_discovery(found, None, &dana(), true) {
            Next::Password(proposal) => proposal,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn picking_pop3_takes_the_pop3_server_discovery_found_while_the_host_is_untouched() {
        let found = fastmail_with_pop3();
        let pop3 = server_after_protocol(&found, "imap.example.org", Protocol::Pop3);
        assert_eq!(pop3, Some(server("pop.example.org", 995)));
        let back = server_after_protocol(&found, "pop.example.org", Protocol::Imap);
        assert_eq!(back, Some(server("imap.example.org", 993)));
        assert_eq!(
            server_after_protocol(&found, "mail.example.org", Protocol::Pop3),
            None
        );
        assert_eq!(
            server_after_protocol(&fastmail(), "imap.example.org", Protocol::Pop3),
            None
        );
    }

    #[test]
    fn the_pop3_server_discovery_found_keeps_the_provider() {
        let found = fastmail_with_pop3();
        let typed = typed_servers(
            &typed("pop.example.org", 995, ""),
            &typed("smtp.example.org", 465, ""),
            &found,
            &dana(),
        )
        .unwrap();
        assert_eq!(typed.provider_name, "Fastmail");
        assert!(typed.info.is_some());
    }

    #[test]
    fn the_added_page_lists_only_the_folders_the_server_fills() {
        use mailrs_domain::Role as MailRole;
        assert!(shows_folder(Provider::Imap, MailRole::Archive));
        assert!(shows_folder(Provider::Jmap, MailRole::Archive));
        assert!(!shows_folder(Provider::Gmail, MailRole::Archive));
        assert!(shows_folder(Provider::Pop3, MailRole::Inbox));
        assert!(!shows_folder(Provider::Pop3, MailRole::Sent));
        assert!(!shows_folder(Provider::Pop3, MailRole::Archive));
        assert!(!shows_folder(Provider::CalDav, MailRole::Inbox));
    }

    fn fastmail_jmap() -> mailrs_discover::JmapFound {
        mailrs_discover::JmapFound {
            session_url: "https://api.fastmail.com/jmap/session".into(),
            source: Source::Table,
            confirm: false,
            provider: mailrs_discover::jmap_named("Fastmail"),
        }
    }

    fn hoster_jmap() -> mailrs_discover::JmapFound {
        mailrs_discover::JmapFound {
            session_url: "https://jmap.hoster.net/jmap/session".into(),
            source: Source::Srv,
            confirm: true,
            provider: None,
        }
    }

    #[test]
    fn fastmail_s_token_sign_in_offers_the_calendar_password() {
        let address = Address::parse("me@fastmail.com").unwrap();
        let proposal = jmap_proposal(fastmail_jmap(), None, &address);
        assert!(asks_dav_password(&proposal, true));
        assert!(
            !asks_dav_password(&proposal, false),
            "a password signs in to the calendar as it is"
        );
    }

    #[test]
    fn the_browser_button_names_the_provider() {
        let address = Address::parse("me@fastmail.com").unwrap();
        let proposal = jmap_proposal(fastmail_jmap(), None, &address);
        assert!(
            browser_line(&proposal)
                .starts_with("Sign in on Fastmail's own page instead of making a token.")
        );
    }

    #[test]
    fn the_browser_line_says_masked_email_needs_a_token() {
        let address = Address::parse("me@fastmail.com").unwrap();
        let proposal = jmap_proposal(fastmail_jmap(), None, &address);
        assert_eq!(
            browser_line(&proposal),
            "Sign in on Fastmail's own page instead of making a token. Masked Email needs an API token."
        );
        let elsewhere = jmap_proposal(
            hoster_jmap(),
            None,
            &Address::parse("me@example.org").unwrap(),
        );
        assert!(
            !browser_line(&elsewhere).contains("Masked Email"),
            "only a provider whose tokens give Masked Email says so"
        );
    }

    #[test]
    fn the_browser_sign_in_goes_to_the_proposal_s_session() {
        let address = Address::parse("me@fastmail.com").unwrap();
        let proposal = jmap_proposal(fastmail_jmap(), None, &address);
        let offer = jmap_browser(&proposal);
        assert_eq!(offer.session_url, "https://api.fastmail.com/jmap/session");
        assert_eq!(offer.provider.map(|p| p.name), Some("Fastmail".to_string()));
    }

    #[test]
    fn the_browser_sign_in_waits_for_a_yes_to_a_host_outside_the_domain() {
        let address = Address::parse("me@example.org").unwrap();
        let proposal = jmap_proposal(hoster_jmap(), None, &address);
        assert!(!can_sign_in_jmap_browser(&proposal, false, false));
        assert!(can_sign_in_jmap_browser(&proposal, true, false));
        assert!(
            !can_sign_in_jmap_browser(&proposal, true, true),
            "a sign-in already runs"
        );
    }

    #[test]
    fn a_grant_the_server_refuses_asks_for_the_browser_again() {
        use mailrs_sync::sign_in::BrowserSignInError;
        let address = Address::parse("me@fastmail.com").unwrap();
        let proposal = jmap_proposal(fastmail_jmap(), None, &address);
        let err = anyhow::Error::new(BrowserSignInError::Jmap(
            mailrs_jmap::JmapError::Unauthorized,
        ));
        let failure = jmap_browser_failure(&err, &proposal);
        assert_eq!(
            (failure.title.as_str(), failure.body.as_str()),
            (
                "Fastmail refused the sign-in",
                "Choose Sign In With Browser to try again."
            )
        );
    }

    #[test]
    fn a_browser_sign_in_that_cannot_reach_the_server_says_so() {
        use mailrs_sync::sign_in::BrowserSignInError;
        let address = Address::parse("me@fastmail.com").unwrap();
        let proposal = jmap_proposal(fastmail_jmap(), None, &address);
        let err = anyhow::Error::new(BrowserSignInError::Jmap(mailrs_jmap::JmapError::Network(
            "reset".into(),
        )));
        let failure = jmap_browser_failure(&err, &proposal);
        assert_eq!(
            (failure.title.as_str(), failure.body.as_str()),
            (
                "Fastmail does not answer",
                "Check your connection, then choose Sign In With Browser."
            )
        );
    }

    #[test]
    fn a_browser_sign_in_nobody_finished_says_how_long_it_waited() {
        use mailrs_sync::sign_in::BrowserSignInError;
        let address = Address::parse("me@fastmail.com").unwrap();
        let proposal = jmap_proposal(fastmail_jmap(), None, &address);
        let failure =
            jmap_browser_failure(&anyhow::Error::new(BrowserSignInError::GaveUp), &proposal);
        assert_eq!(
            failure.body,
            "Gave up waiting for the browser after ten minutes."
        );
    }

    #[test]
    fn the_waiting_page_lists_the_jmap_server_s_steps() {
        assert_eq!(
            jmap_browser_steps("Fastmail"),
            [
                "Sign in to Fastmail if it asks.",
                "Read what Penguin Mail asks for and allow it."
            ]
        );
    }

    #[test]
    fn a_server_the_table_does_not_know_asks_for_no_calendar_password() {
        let address = Address::parse("me@example.org").unwrap();
        let proposal = jmap_proposal(hoster_jmap(), None, &address);
        assert!(!asks_dav_password(&proposal, true));
    }

    #[test]
    fn an_empty_calendar_password_is_left_out_of_the_attempt() {
        let address = Address::parse("me@fastmail.com").unwrap();
        let proposal = jmap_proposal(fastmail_jmap(), None, &address);
        assert_eq!(
            jmap_attempt(&address, &proposal, "token", "", true).dav_password,
            None
        );
        assert_eq!(
            jmap_attempt(&address, &proposal, "token", "app", true)
                .dav_password
                .as_deref(),
            Some("app")
        );
    }

    #[test]
    fn jmap_is_offered_before_imap_when_both_are_found() {
        let address = Address::parse("me@fastmail.com").unwrap();
        let found = mailrs_discover::table_only("fastmail.com");
        match after_discovery(found, Some(fastmail_jmap()), &address, true) {
            Next::Jmap(proposal) => {
                assert_eq!(proposal.provider_name, "Fastmail");
                assert!(proposal.imap.is_some(), "IMAP stays one step away");
                assert!(token_only(&proposal));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn jmap_alone_is_offered_when_nothing_else_is_found() {
        let address = Address::parse("me@example.org").unwrap();
        match after_discovery(
            verdict(Verdict::NothingFound),
            Some(hoster_jmap()),
            &address,
            true,
        ) {
            Next::Jmap(proposal) => assert!(proposal.imap.is_none()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_google_domain_still_goes_to_google_whatever_jmap_says() {
        let address = Address::parse("me@example.org").unwrap();
        let found = Found {
            verdict: Verdict::Google,
            candidates: Vec::new(),
        };
        assert_eq!(
            after_discovery(found, Some(fastmail_jmap()), &address, true),
            Next::Google
        );
    }

    #[test]
    fn a_server_found_by_srv_takes_a_password_or_a_token_and_names_its_host() {
        let address = Address::parse("me@example.org").unwrap();
        let found = mailrs_discover::JmapFound {
            session_url: "https://mail.example.org/jmap/session".into(),
            source: Source::Srv,
            confirm: false,
            provider: None,
        };
        let proposal = jmap_proposal(found, None, &address);
        assert_eq!(proposal.provider_name, "mail.example.org");
        assert!(!token_only(&proposal));
        assert_eq!(jmap_secret_title(false), "Password");
        assert_eq!(jmap_secret_title(true), "API Token");
    }

    #[test]
    fn a_session_outside_the_domain_waits_for_a_yes_before_the_secret_goes() {
        let address = Address::parse("me@example.org").unwrap();
        let proposal = jmap_proposal(hoster_jmap(), None, &address);
        assert!(!can_sign_in_jmap("secret", &proposal, false, false));
        assert!(can_sign_in_jmap("secret", &proposal, true, false));
        assert!(!can_sign_in_jmap("", &proposal, true, false));
        assert!(!can_sign_in_jmap("secret", &proposal, true, true));
    }

    #[test]
    fn the_question_names_the_host_the_secret_would_go_to() {
        let address = Address::parse("me@example.org").unwrap();
        let proposal = jmap_proposal(hoster_jmap(), None, &address);
        let (title, body) = jmap_agree(&proposal, &address, false);
        assert_eq!(title, "Use jmap.hoster.net");
        assert!(
            body.contains("jmap.hoster.net") && body.contains("example.org"),
            "{body}"
        );
        assert!(body.contains("password"), "{body}");
        assert!(jmap_agree(&proposal, &address, true).1.contains("token"));
    }

    #[test]
    fn the_page_names_the_provider_or_else_the_address_s_domain() {
        let fastmail = Address::parse("me@fastmail.com").unwrap();
        let listed = jmap_proposal(fastmail_jmap(), None, &fastmail);
        assert_eq!(jmap_title(&listed, &fastmail), "Sign in to Fastmail");
        assert_eq!(
            jmap_found_line(&listed, &fastmail),
            "Penguin Mail found Fastmail's JMAP server."
        );
        let address = Address::parse("me@example.org").unwrap();
        let unlisted = jmap_proposal(hoster_jmap(), None, &address);
        assert_eq!(jmap_title(&unlisted, &address), "Sign in to example.org");
        assert_eq!(
            jmap_found_line(&unlisted, &address),
            "Penguin Mail found a JMAP server for example.org."
        );
        let typed = typed_jmap("mail.example.org", "", &address, None).unwrap();
        assert_eq!(
            jmap_found_line(&typed, &address),
            "The JMAP server you gave."
        );
    }

    #[test]
    fn fastmail_s_hint_names_the_plan_and_the_access_to_tick() {
        let address = Address::parse("me@fastmail.com").unwrap();
        let proposal = jmap_proposal(fastmail_jmap(), None, &address);
        let (title, body, link) = jmap_hint(&proposal, true).unwrap();
        assert!(title.contains("Fastmail"), "{title}");
        assert!(
            body.contains("Basic") && body.contains("Masked Email"),
            "{body}"
        );
        assert!(link.unwrap().url.contains("fastmail.help"));
        assert_eq!(jmap_hint(&proposal, false), None);
    }

    #[test]
    fn a_typed_session_url_takes_https_and_a_bare_host() {
        let address = Address::parse("me@example.org").unwrap();
        let typed = typed_jmap("mail.example.org", "", &address, None).unwrap();
        assert_eq!(
            typed.session_url,
            "https://mail.example.org/.well-known/jmap"
        );
        let full = typed_jmap(
            "https://mail.example.org/jmap/session",
            "me",
            &address,
            None,
        )
        .unwrap();
        assert_eq!(full.session_url, "https://mail.example.org/jmap/session");
        assert_eq!(full.user.as_deref(), Some("me"));
        assert!(typed_jmap("http://mail.example.org/jmap", "", &address, None).is_err());
        assert!(typed_jmap("http://localhost:8080/jmap/session", "", &address, None).is_ok());
        assert!(typed_jmap("", "", &address, None).is_err());
    }

    #[test]
    fn the_attempt_logs_in_with_the_address_unless_a_user_name_was_typed() {
        let address = Address::parse("me@example.org").unwrap();
        let bare = typed_jmap("mail.example.org", "", &address, None).unwrap();
        assert_eq!(
            jmap_attempt(&address, &bare, "pw", "", false).user,
            "me@example.org"
        );
        let named = typed_jmap("mail.example.org", "me", &address, None).unwrap();
        assert_eq!(jmap_attempt(&address, &named, "pw", "", false).user, "me");
    }

    #[test]
    fn a_refused_token_says_so_and_links_to_the_token_page() {
        let address = Address::parse("me@fastmail.com").unwrap();
        let proposal = jmap_proposal(fastmail_jmap(), None, &address);
        let err = anyhow::Error::new(mailrs_jmap::JmapError::Unauthorized);
        let failure = jmap_failure(&err, &proposal, true);
        assert_eq!(failure.title, "Fastmail refused the token");
        assert!(
            failure
                .links
                .iter()
                .any(|l| l.url.contains("fastmail.help"))
        );
    }

    #[test]
    fn a_refused_password_says_so() {
        let address = Address::parse("me@example.org").unwrap();
        let proposal = typed_jmap("mail.example.org", "", &address, None).unwrap();
        let err = anyhow::Error::new(mailrs_jmap::JmapError::Unauthorized);
        let failure = jmap_failure(&err, &proposal, false);
        assert_eq!(failure.title, "mail.example.org refused the password");
        assert!(failure.links.is_empty());
    }

    #[test]
    fn a_session_url_with_nothing_there_says_no_jmap_server_answered() {
        let address = Address::parse("me@example.org").unwrap();
        let proposal = typed_jmap("mail.example.org", "", &address, None).unwrap();
        let err = anyhow::Error::new(mailrs_jmap::JmapError::NotFound);
        let failure = jmap_failure(&err, &proposal, false);
        assert_eq!(failure.title, "No JMAP server at mail.example.org");
        assert!(failure.body.contains("Server Settings"), "{}", failure.body);
    }

    #[test]
    fn a_session_without_mail_says_the_server_offers_none() {
        let address = Address::parse("me@example.org").unwrap();
        let proposal = typed_jmap("mail.example.org", "", &address, None).unwrap();
        let err = anyhow::Error::new(mailrs_jmap::JmapError::UnknownCapability(
            mailrs_jmap::MAIL.into(),
        ));
        assert_eq!(
            jmap_failure(&err, &proposal, false).title,
            "mail.example.org offers no mail over JMAP"
        );
    }

    #[test]
    fn a_network_failure_asks_for_the_connection() {
        let address = Address::parse("me@example.org").unwrap();
        let proposal = typed_jmap("mail.example.org", "", &address, None).unwrap();
        let err = anyhow::Error::new(mailrs_jmap::JmapError::Network("timed out".into()));
        let failure = jmap_failure(&err, &proposal, false);
        assert_eq!(failure.title, "mail.example.org does not answer");
        assert!(
            !failure.line.contains("timed out"),
            "the dialog speaks in its own words"
        );
    }

    #[test]
    fn a_secret_with_a_line_break_is_never_sent() {
        let address = Address::parse("me@example.org").unwrap();
        let proposal = typed_jmap("mail.example.org", "", &address, None).unwrap();
        let err = anyhow::Error::new(mailrs_jmap::JmapError::BadCredential);
        let failure = jmap_failure(&err, &proposal, true);
        assert!(!failure.title.contains("refused"), "{}", failure.title);
    }

    #[test]
    fn imap_instead_names_the_app_password_only_where_the_provider_wants_one() {
        let fastmail = jmap_proposal(fastmail_jmap(), Some(fastmail()), &dana());
        assert_eq!(
            imap_instead_line(&fastmail),
            "Sign in with an app password over IMAP and SMTP"
        );
        let mut own = fastmail.clone();
        own.imap = Some(Box::new(guess(&Address::parse("me@example.org").unwrap())));
        assert_eq!(
            imap_instead_line(&own),
            "Sign in with your password over IMAP and SMTP"
        );
    }

    #[test]
    fn signing_in_again_reads_the_saved_session() {
        let account = Account {
            id: 4,
            email: "me@fastmail.com".into(),
            state: AccountState::NeedsReauth,
            provider: Provider::Jmap,
            provider_name: Some("Fastmail".into()),
        };
        let saved = mailrs_store::jmap::JmapSaved {
            session_url: "https://api.fastmail.com/jmap/session".into(),
            auth: mailrs_store::jmap::JmapAuth::Bearer,
            user_name: None,
            labels: true,
            capabilities: Vec::new(),
            oauth: None,
            owner: None,
        };
        let proposal = saved_jmap(&account, &saved);
        assert_eq!(proposal.session_url, saved.session_url);
        assert!(token_only(&proposal));
        assert!(!proposal.confirm);
    }

    #[test]
    fn signing_in_again_starts_on_the_kind_of_secret_the_account_kept() {
        let account = Account {
            id: 5,
            email: "me@example.org".into(),
            state: AccountState::NeedsReauth,
            provider: Provider::Jmap,
            provider_name: Some("mail.example.org".into()),
        };
        let saved = mailrs_store::jmap::JmapSaved {
            session_url: "https://mail.example.org/jmap/session".into(),
            auth: mailrs_store::jmap::JmapAuth::Bearer,
            user_name: None,
            labels: true,
            capabilities: Vec::new(),
            oauth: None,
            owner: None,
        };
        assert!(saved_jmap(&account, &saved).token);
        let basic = mailrs_store::jmap::JmapSaved {
            auth: mailrs_store::jmap::JmapAuth::Basic,
            ..saved
        };
        assert!(!saved_jmap(&account, &basic).token);
    }

    #[test]
    fn a_browser_account_whose_calendar_refused_the_sign_in_asks_for_its_app_password() {
        use mailrs_store::jmap::{JmapAuth, JmapSaved, SavedOAuth};
        use mailrs_store::services::ServiceKind;

        let saved = JmapSaved {
            session_url: "https://api.fastmail.com/jmap/session".into(),
            auth: JmapAuth::OAuth,
            user_name: None,
            labels: true,
            capabilities: Vec::new(),
            oauth: Some(SavedOAuth {
                client_id: "c1".into(),
                token_endpoint: "https://api.fastmail.com/oauth/refresh".into(),
            }),
            owner: None,
        };
        assert!(asks_dav_password_again(&saved, &[ServiceKind::CalDav]));
        assert!(asks_dav_password_again(&saved, &[ServiceKind::CardDav]));
        assert!(
            !asks_dav_password_again(&saved, &[]),
            "a calendar that takes the token needs nothing more"
        );
        let token = JmapSaved {
            auth: JmapAuth::Bearer,
            oauth: None,
            ..saved
        };
        assert!(
            !asks_dav_password_again(&token, &[ServiceKind::CalDav]),
            "a token account asks through its own rule"
        );
    }

    #[test]
    fn the_protocol_row_has_jmap_third() {
        assert_eq!(PROTOCOL_CHOICES, ["IMAP", "POP3", "JMAP"]);
        assert!(jmap_at(2) && !jmap_at(1));
        assert_eq!(
            protocol_at(2),
            Protocol::Imap,
            "the mail protocols ignore the JMAP index"
        );
    }

    #[test]
    fn an_unchanged_round_trip_through_server_settings_keeps_the_question() {
        let address = Address::parse("me@example.org").unwrap();
        let found = jmap_proposal(hoster_jmap(), None, &address);
        let back = typed_jmap(&found.session_url, "", &address, Some(&found)).unwrap();
        assert!(
            back.confirm,
            "the host is still the one discovery found off the domain"
        );
        assert!(!can_sign_in_jmap("secret", &back, false, false));
        assert!(can_sign_in_jmap("secret", &back, true, false));
        let own = typed_jmap(
            "https://mail.example.org/jmap/session",
            "",
            &address,
            Some(&found),
        )
        .unwrap();
        assert!(!own.confirm, "a host the person typed needs no yes");
    }

    #[test]
    fn an_unchanged_round_trip_keeps_what_the_list_knows() {
        let address = Address::parse("me@fastmail.com").unwrap();
        let found = jmap_proposal(fastmail_jmap(), None, &address);
        let back = typed_jmap(&found.session_url, "", &address, Some(&found)).unwrap();
        assert!(token_only(&back));
        assert_eq!(back.provider_name, "Fastmail");
    }

    #[test]
    fn an_unchanged_imap_round_trip_keeps_the_question_for_guessed_servers() {
        let address = Address::parse("me@example.org").unwrap();
        let Next::Password(found) =
            after_discovery(found(None, Source::Srv, true), None, &address, true)
        else {
            panic!("expected the password step");
        };
        let back = typed_servers(
            &typed("imap.example.org", 993, ""),
            &typed("smtp.example.org", 465, ""),
            &found,
            &address,
        )
        .unwrap();
        assert!(back.confirm);
        assert!(!can_sign_in("secret", &back, false, false));
        let own = typed_servers(
            &typed("mail.example.org", 993, ""),
            &typed("smtp.example.org", 465, ""),
            &found,
            &address,
        )
        .unwrap();
        assert!(!own.confirm, "a host the person typed needs no yes");
    }

    #[test]
    fn a_bare_host_that_leads_off_the_domain_asks_first() {
        let address = Address::parse("me@example.org").unwrap();
        let typed = typed_jmap("mail.example.org", "", &address, None).unwrap();
        let away = located_jmap(
            typed.clone(),
            Some("https://jmap.hoster.net/session".into()),
            &address,
        );
        assert_eq!(away.session_url, "https://jmap.hoster.net/session");
        assert!(away.confirm);
        let home = located_jmap(
            typed.clone(),
            Some("https://mail.example.org/jmap/session".into()),
            &address,
        );
        assert_eq!(home.session_url, "https://mail.example.org/jmap/session");
        assert!(!home.confirm);
        assert_eq!(located_jmap(typed.clone(), None, &address), typed);
    }

    #[test]
    fn a_bare_host_off_the_domain_that_stays_on_itself_needs_no_yes() {
        let address = Address::parse("me@example.org").unwrap();
        let typed = typed_jmap("jmap.hoster.net", "", &address, None).unwrap();
        let located = located_jmap(
            typed,
            Some("https://jmap.hoster.net/jmap/session".into()),
            &address,
        );
        assert!(!located.confirm, "the person typed that host");
    }

    #[test]
    fn a_session_that_sends_elsewhere_names_both_hosts() {
        let address = Address::parse("me@example.org").unwrap();
        let proposal = typed_jmap("mail.example.org", "", &address, None).unwrap();
        let err = anyhow::Error::new(mailrs_jmap::JmapError::OffHost("api.other.net".into()));
        let failure = jmap_failure(&err, &proposal, false);
        assert!(
            failure.title.contains("mail.example.org"),
            "{}",
            failure.title
        );
        assert!(failure.body.contains("api.other.net"), "{}", failure.body);
    }

    #[test]
    fn imap_is_offered_in_a_failure_only_where_discovery_found_it() {
        let address = Address::parse("me@example.org").unwrap();
        let alone = typed_jmap("mail.example.org", "", &address, None).unwrap();
        let err = anyhow::Error::new(mailrs_jmap::JmapError::NotFound);
        assert!(!jmap_failure(&err, &alone, false).body.contains("IMAP"));
        let beside = JmapProposal {
            imap: Some(Box::new(guess(&address))),
            ..alone
        };
        assert!(
            jmap_failure(&err, &beside, false)
                .body
                .contains("use IMAP instead")
        );
        let odd = anyhow::Error::new(mailrs_jmap::JmapError::Decode("x".into()));
        assert!(
            jmap_failure(&odd, &beside, false)
                .body
                .contains("use IMAP instead")
        );
        let bare = JmapProposal {
            imap: None,
            ..beside
        };
        assert!(!jmap_failure(&odd, &bare, false).body.contains("IMAP"));
    }

    #[test]
    fn a_failure_logs_its_kind_and_never_the_address() {
        let taken = anyhow::Error::new(mailrs_sync::sign_in::JmapSignInError::Taken {
            address: "me@example.org".into(),
            provider: "Gmail".into(),
        });
        assert!(!failure_kind(&taken).contains('@'));
        let refused = anyhow::Error::new(mailrs_jmap::JmapError::Unauthorized);
        assert_eq!(failure_kind(&refused), "unauthorized");
    }

    #[test]
    fn a_sign_in_that_reached_another_account_says_so_and_logs_no_address() {
        let address = Address::parse("me@fastmail.com").unwrap();
        let proposal = typed_jmap("api.fastmail.com", "", &address, None).unwrap();
        let other = || {
            anyhow::Error::new(mailrs_sync::sign_in::JmapSignInError::DifferentAccount {
                address: "me@fastmail.com".into(),
            })
        };
        let said = "You signed in to a different account than me@fastmail.com, so nothing changed.";
        assert_eq!(jmap_failure(&other(), &proposal, true).body, said);
        let browser = anyhow::Error::new(mailrs_sync::sign_in::BrowserSignInError::KeptJmap(
            mailrs_sync::sign_in::JmapSignInError::DifferentAccount {
                address: "me@fastmail.com".into(),
            },
        ));
        assert_eq!(jmap_browser_failure(&browser, &proposal).body, said);
        assert_eq!(failure_kind(&other()), "different account");
    }

    #[test]
    fn a_session_found_for_another_address_is_never_carried_over() {
        let first = Address::parse("me@fastmail.com").unwrap();
        let held = jmap_proposal(fastmail_jmap(), None, &first);
        assert_eq!(held.address, "me@fastmail.com");
        let now = Address::parse("me@example.net").unwrap();
        let typed = typed_jmap(&held.session_url, "", &now, Some(&held)).unwrap();
        assert_eq!(typed.address, "me@example.net");
        assert!(
            typed.provider.is_none(),
            "the list's facts were for the other address"
        );
        assert!(!typed.token);
        assert_eq!(typed.source, Some(Source::Manual));
    }

    #[test]
    fn servers_found_for_another_address_are_never_carried_over() {
        let first = Address::parse("me@example.org").unwrap();
        let Next::Password(held) = after_discovery(
            found(Some(fastmail_info()), Source::Srv, true),
            None,
            &first,
            true,
        ) else {
            panic!("expected the password step");
        };
        assert_eq!(held.address, "me@example.org");
        let now = Address::parse("me@example.net").unwrap();
        let typed = typed_servers(
            &typed("imap.example.org", 993, ""),
            &typed("smtp.example.org", 465, ""),
            &held,
            &now,
        )
        .unwrap();
        assert_eq!(typed.address, "me@example.net");
        assert_eq!(typed.provider_name, "example.net");
        assert!(typed.info.is_none());
        assert!(!typed.confirm);
    }

    #[test]
    fn a_browser_failure_logs_its_kind_and_never_the_address() {
        let other = anyhow::Error::new(mailrs_sync::sign_in::BrowserSignInError::OtherAddress {
            signed_in: "me@example.org".into(),
            wanted: "you@example.org".into(),
        });
        assert_eq!(failure_kind(&other), "other address");
    }
}
