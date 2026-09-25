//! What the Add Account dialog decides, without widgets: the picker's
//! choices, whether an address can be looked up, where discovery's
//! answer leads, which of its candidates gets tried and when a failed
//! one hands off to the next, the line about app passwords, the words
//! for a failed sign-in, the Server Settings form, and which answers
//! still count. `ui::add_account` draws what these say.

use std::cell::Cell;

use mailrs_discover::{
    Candidate, Found, PasswordKind, ProviderInfo, Security, Server, Source, Unreachable, UserName,
    Verdict,
};
use mailrs_domain::Account;
use mailrs_domain::translate::{fill, gettext};
use mailrs_imap::{CheckError, ImapError};
use mailrs_store::servers::{Saved, Servers};

/// One row of the Add Account picker. Microsoft joins them once Penguin
/// Mail can sign in to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Google,
    Other,
}

impl Choice {
    pub const ALL: [Choice; 2] = [Choice::Google, Choice::Other];

    pub fn title(self) -> String {
        match self {
            // A brand, so it is not translated.
            Choice::Google => "Google".to_string(),
            Choice::Other => gettext("Another Provider"),
        }
    }

    pub fn subtitle(self) -> String {
        match self {
            Choice::Google => gettext("Gmail and Google Workspace"),
            Choice::Other => gettext("Fastmail, iCloud, Yahoo and any server with IMAP"),
        }
    }
}

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
    /// The user name typed in Server Settings, sent as typed to both
    /// servers. `None` logs in with the address, the way each server's
    /// user name rule says.
    pub user: Option<String>,
    /// The person must say yes to these host names before the password
    /// goes out: discovery guessed them, or found them outside the domain.
    pub confirm: bool,
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
    /// A Google Workspace domain: the Google sign-in serves it.
    Google,
    /// Nothing found: Server Settings, filled with a guess, under a line.
    Manual { proposal: Proposal, line: String },
}

/// Where discovery's answer for `address` leads. The best candidate comes
/// first in `found`, so it is the one step 2 offers; the rest wait in its
/// `Proposal::remaining` for a connection or a TLS failure to reach for.
pub fn after_discovery(found: Found, address: &Address) -> Next {
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
        } => Next::Say(fill(
            &gettext("{provider} has no IMAP, so other mail apps cannot reach it."),
            &[("provider", &provider)],
        )),
        Verdict::Unreachable {
            provider,
            reason: Unreachable::NotYet,
        } => Next::Say(fill(
            &gettext("Penguin Mail cannot reach {provider} yet."),
            &[("provider", &provider)],
        )),
        Verdict::Google => Next::Google,
        Verdict::Microsoft => Next::Say(gettext(
            "Microsoft accounts come in a later version of Penguin Mail.",
        )),
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
        user: None,
        confirm: candidate.confirm,
        remaining,
    }
}

/// Server Settings' first guess for a domain nobody lists: the host names
/// most servers use, on the ports with TLS from the first byte.
pub fn guess(address: &Address) -> Proposal {
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
        user: None,
        confirm: false,
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

/// Whether Sign In can go: there is a password, and the person said yes
/// to servers Penguin Mail guessed.
pub fn can_sign_in(password: &str, proposal: &Proposal, confirmed: bool) -> bool {
    !password.is_empty() && (confirmed || !proposal.confirm)
}

/// One server as the confirmation shows it.
pub fn server_line(server: &Server) -> String {
    fill(
        &gettext("{host} on port {port}"),
        &[("host", &server.host), ("port", &server.port.to_string())],
    )
}

/// One try at signing in, as the core takes it. It has no `Debug`, so
/// the password cannot reach a log line.
pub struct Attempt {
    pub address: String,
    pub provider_name: String,
    pub imap: Server,
    pub smtp: Server,
    /// What `mailrs_imap::check` signs in with: the user name typed in
    /// Server Settings, else the address. Each server's `user_name` rule
    /// turns it into the names to try, and a typed name comes with the
    /// `Address` rule on both, so it goes as typed.
    pub login_as: String,
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
        login_as: proposal.user.clone().unwrap_or_else(|| address.full()),
        password: password.to_string(),
    }
}

/// What a failed attempt does next: try discovery's next candidate, or
/// show the failure at step 2 and wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Try this candidate next, with the same password. It carries its
    /// own `confirm`, so a candidate the person must still say yes to is
    /// never tried silently.
    TryNext(Proposal),
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
        Some((candidate, rest)) => {
            Outcome::TryNext(proposal_from(candidate.clone(), rest.to_vec(), address))
        }
        None => Outcome::Failed(failure(err, tried)),
    }
}

/// What step 2 says after a sign-in failed: a line, and the pages that
/// can help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub line: String,
    pub links: Vec<Link>,
}

/// The words for `err`. A login the servers turned down has its own
/// words; anything else, such as the keyring refusing or an address a
/// Google account holds, already reads as a sentence.
pub fn failure(err: &anyhow::Error, proposal: &Proposal) -> Failure {
    let Some(check) = err.downcast_ref::<CheckError>() else {
        return Failure {
            line: err.to_string(),
            links: Vec::new(),
        };
    };
    // `check` says which server failed; a network error names that host,
    // since the other one may have answered.
    let (host, err) = match check {
        CheckError::Imap(err) => (&proposal.imap.host, err),
        CheckError::Smtp(err) => (&proposal.smtp.host, err),
    };
    let only = |line: String| Failure {
        line,
        links: Vec::new(),
    };
    let could_not_sign_in = |reason: &str| {
        only(fill(
            &gettext("Could not sign in: {reason}"),
            &[("reason", reason)],
        ))
    };
    match err {
        ImapError::Auth { text } => {
            refused(gettext("The server refused the password."), text, proposal)
        }
        ImapError::ImapDisabled { text } => {
            refused(gettext("IMAP is off for this account."), text, proposal)
        }
        // The spec's one line for a TLS failure. The detail, such as a
        // server without TLS 1.2, goes to the log with the error.
        ImapError::Tls { host, .. } => only(fill(
            &gettext("The server's certificate does not match {host}."),
            &[("host", host)],
        )),
        ImapError::Network(reason) | ImapError::TooManyConnections { text: reason } => only(fill(
            &gettext("Could not reach {host}: {reason}"),
            &[("host", host), ("reason", reason)],
        )),
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

/// A turned-down login: `line`, the server's own words, and the pages
/// for the usual causes, IMAP left off and an account password where an
/// app password is due.
fn refused(mut line: String, said: &str, proposal: &Proposal) -> Failure {
    if !said.trim().is_empty() {
        line.push(' ');
        line.push_str(&fill(
            &gettext("The server said: {reason}"),
            &[("reason", said.trim())],
        ));
    }
    let mut links = Vec::new();
    if let Some(info) = &proposal.info {
        let provider = [("provider", info.name.as_str())];
        if let Some(url) = &info.enable_imap_url {
            links.push(Link {
                label: fill(
                    &gettext("Turn on IMAP in {provider}'s settings."),
                    &provider,
                ),
                url: url.clone(),
            });
        }
        if info.password != PasswordKind::AccountPassword
            && let Some(url) = &info.app_password_url
        {
            links.push(Link {
                label: fill(
                    &gettext("Make an app password in {provider}'s settings."),
                    &provider,
                ),
                url: url.clone(),
            });
        }
        if let Some(url) = &info.documentation_url {
            links.push(Link {
                label: fill(&gettext("{provider}'s help for mail apps"), &provider),
                url: url.clone(),
            });
        }
    }
    Failure { line, links }
}

/// What Server Settings holds for one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Typed {
    pub host: String,
    pub port: u16,
    pub security: Security,
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

/// Server Settings as a proposal, or the line saying what to fix. A
/// blank user name leaves the servers' own rule for it, and servers the
/// person typed need no second yes or fallback candidate.
pub fn typed_servers(
    imap: &Typed,
    smtp: &Typed,
    user: &str,
    before: &Proposal,
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
    let user = user.trim();
    let server = |host: String, typed: &Typed, rule: UserName| Server {
        host,
        port: typed.port,
        security: typed.security,
        user_name: if user.is_empty() {
            rule
        } else {
            UserName::Address
        },
    };
    Ok(Proposal {
        provider_name: before.provider_name.clone(),
        info: before.info.clone(),
        imap: server(imap_host, imap, before.imap.user_name),
        smtp: server(smtp_host, smtp, before.smtp.user_name),
        user: (!user.is_empty()).then(|| user.to_string()),
        confirm: false,
        remaining: Vec::new(),
    })
}

/// Step 2 for an account signing in again, from the servers it kept and
/// the user name each took last time. `mailrs_imap::check` signs in with
/// one name, so each kept name becomes the rule that gives it back from
/// the address: the address itself, or its part before @ (iCloud's IMAP
/// server takes that while its SMTP server takes the address). A name
/// neither rule gives, typed in Server Settings, goes to both servers as
/// typed, as the form sent it. There is no discovery to fall back to, so
/// nothing waits in `remaining`.
pub fn saved_proposal(account: &Account, saved: &Servers) -> Proposal {
    let local = account.email.rsplit_once('@').map(|(local, _)| local);
    let rule = |kept: &Saved| {
        if kept.user_name == account.email {
            Some(UserName::Address)
        } else if Some(kept.user_name.as_str()) == local {
            Some(UserName::LocalPartFirst)
        } else {
            None
        }
    };
    let mut imap = mailrs_sync::server_of(&saved.imap);
    let mut smtp = mailrs_sync::server_of(&saved.smtp);
    let user = match (rule(&saved.imap), rule(&saved.smtp)) {
        (Some(imap_rule), Some(smtp_rule)) => {
            imap.user_name = imap_rule;
            smtp.user_name = smtp_rule;
            None
        }
        // `server_of` already gives both servers the `Address` rule, so
        // the typed name goes as it is.
        _ => Some(saved.imap.user_name.clone()),
    };
    Proposal {
        provider_name: account.provider_name().to_string(),
        info: mailrs_discover::provider_named(account.provider_name()),
        imap,
        smtp,
        user,
        confirm: false,
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
        match after_discovery(found(Some(fastmail_info()), Source::Table, false), &dana()) {
            Next::Password(proposal) => proposal,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_picker_offers_google_then_another_provider_and_no_microsoft() {
        assert_eq!(Choice::ALL, [Choice::Google, Choice::Other]);
        assert_eq!(Choice::Google.title(), "Google");
        assert_eq!(Choice::Other.title(), "Another Provider");
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
        let Next::Password(proposal) = after_discovery(found(None, Source::Srv, true), &address)
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
        let Next::Password(proposal) = after_discovery(found, &address) else {
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
            after_discovery(tuta, &dana()),
            Next::Say("Tuta has no IMAP, so other mail apps cannot reach it.".into())
        );
    }

    #[test]
    fn proton_mail_answers_that_it_cannot_be_reached_yet() {
        let proton = verdict(Verdict::Unreachable {
            provider: "Proton Mail".into(),
            reason: Unreachable::NotYet,
        });
        assert_eq!(
            after_discovery(proton, &dana()),
            Next::Say("Penguin Mail cannot reach Proton Mail yet.".into())
        );
    }

    #[test]
    fn a_google_domain_goes_to_the_google_sign_in_and_microsoft_waits() {
        assert_eq!(
            after_discovery(verdict(Verdict::Google), &dana()),
            Next::Google
        );
        assert_eq!(
            after_discovery(verdict(Verdict::Microsoft), &dana()),
            Next::Say("Microsoft accounts come in a later version of Penguin Mail.".into())
        );
    }

    #[test]
    fn nothing_found_opens_server_settings_with_a_guess() {
        let address = Address::parse("me@example.org").unwrap();
        let Next::Manual { proposal, line } =
            after_discovery(verdict(Verdict::NothingFound), &address)
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
        assert!(!can_sign_in("", &fastmail(), false));
        assert!(can_sign_in("pw", &fastmail(), false));
        assert!(!can_sign_in("pw", &guessed, false));
        assert!(can_sign_in("pw", &guessed, true));
    }

    #[test]
    fn the_password_goes_as_typed_and_the_address_logs_in() {
        let tried = attempt(&dana(), &fastmail(), " abcd efgh ");
        assert_eq!(tried.password, " abcd efgh ");
        assert_eq!(tried.login_as, "Dana@fastmail.com");
        assert_eq!(tried.provider_name, "Fastmail");
    }

    #[test]
    fn a_user_name_typed_in_server_settings_is_who_logs_in() {
        let typed = Proposal {
            user: Some("dana".into()),
            ..fastmail()
        };
        assert_eq!(attempt(&dana(), &typed, "pw").login_as, "dana");
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
                label: "Turn on IMAP in GMX's settings.".into(),
                url: "https://gmx.example/imap".into(),
            }]
        );
        let at_fastmail = failure(&refused, &fastmail());
        assert_eq!(
            at_fastmail.links,
            [Link {
                label: "Make an app password in Fastmail's settings.".into(),
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
                label: "Turn on IMAP in GMX's settings.".into(),
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

    #[test]
    fn server_settings_want_a_server_name_on_both_sides() {
        let ok = Typed {
            host: " IMAP.Example.org ".into(),
            port: 993,
            security: Security::Tls,
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
            typed_servers(&empty, &ok, "", &fastmail()),
            Err("Type the incoming server's name.".into())
        );
        assert_eq!(
            typed_servers(&ok, &empty, "", &fastmail()),
            Err("Type the outgoing server's name.".into())
        );
        assert_eq!(
            typed_servers(&url, &ok, "", &fastmail()),
            Err("https://mail.example.org is not a server name.".into())
        );
        let proposal = typed_servers(&ok, &ok, "  ", &fastmail()).unwrap();
        assert_eq!(proposal.imap.host, "imap.example.org");
        assert_eq!(proposal.user, None);
        assert!(!proposal.confirm);
        assert_eq!(proposal.provider_name, "Fastmail");
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
        assert_eq!(proposal.user, None);
        assert!(!proposal.confirm);
        assert_eq!(
            again_line(&account),
            "Penguin Mail needs the password for dana@icloud.com again."
        );
    }

    #[test]
    fn a_user_name_typed_in_server_settings_is_kept_for_signing_in_again() {
        let proposal = saved_proposal(&icloud_account(), &kept("d.santos", "d.santos"));
        assert_eq!(proposal.user.as_deref(), Some("d.santos"));
        assert_eq!(proposal.imap.user_name, UserName::Address);
        assert_eq!(proposal.smtp.user_name, UserName::Address);
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

    #[test]
    fn the_confirmation_names_each_host_and_its_port() {
        assert_eq!(
            server_line(&server("imap.example.org", 993)),
            "imap.example.org on port 993"
        );
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
        let Next::Password(first) = after_discovery(two_candidates(false), &address) else {
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
        let Next::Password(first) = after_discovery(two_candidates(false), &address) else {
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
        let Next::Password(first) = after_discovery(two_candidates(true), &address) else {
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
        assert!(!can_sign_in("pw", &second, false));
        assert!(can_sign_in("pw", &second, true));
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
}
