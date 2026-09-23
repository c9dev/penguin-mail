//! Command-line front end to the Penguin Mail core: add Gmail accounts, sync them,
//! and inspect the local store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use mailrs_domain::{Account, AccountId, ChangeEvent, EpochMillis, Provider, system_label};
use mailrs_gmail::{
    GMAIL_API_BASE, KeyringTokenStore, OAuthClient, TokenStore, authorize, built_in_client,
};
use mailrs_store::threads::{self, ThreadFilter};
use mailrs_store::{Db, accounts, messages};
use mailrs_sync::{
    AccountServices, AccountSync, SyncEngine, TriageAction, connect_account, export, now_millis,
};

use mailrs_sync::config::{Config, config_path, data_dir, migrate_old_dirs, secure_dirs};
use mailrs_sync::lock::{LockError, SyncLock};
use mailrs_sync::sign_in::{account_client, signed_in};

/// How long `account add` waits for the browser.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Parser)]
#[command(
    name = "penguin-mail-cli",
    version,
    about = "Sync Gmail accounts into the Penguin Mail store and inspect it"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Add, list, or remove Gmail accounts.
    #[command(subcommand)]
    Account(AccountCommand),
    /// Sync every account until interrupted, printing what changes.
    Sync,
    /// List threads from the local store.
    Threads {
        /// Only this account. Leave it out for the unified view.
        #[arg(long)]
        account: Option<String>,
        #[arg(long, default_value = system_label::INBOX)]
        label: String,
        #[arg(long, default_value_t = 25)]
        limit: i64,
    },
    /// Fetch a whole thread from Gmail and print it as text.
    Show { account: String, thread_id: String },
    /// Save a thread as an mbox file, or one of its messages as an .eml.
    Export {
        account: String,
        thread_id: String,
        /// Save this message alone, as the bytes Gmail holds.
        #[arg(long)]
        message: Option<String>,
        /// Write here rather than to a name built from the subject and the date.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Report why a message may look blank, in counts alone. It prints no
    /// mail, so its output is safe to share.
    Diagnose { account: String, thread_id: String },
    /// Apply archive, read, unread, star, unstar, trash, label:ID, or unlabel:ID to a thread.
    Triage {
        account: String,
        thread_id: String,
        action: TriageAction,
    },
}

#[derive(Subcommand)]
enum AccountCommand {
    /// Authorize an account in the browser and keep its refresh token in the keyring.
    Add,
    /// Show accounts and their sync state.
    List,
    /// Delete an account's local mail and its keyring entry.
    Remove { email: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,mailrs_sync=info")),
        )
        .init();
    let cli = Cli::parse();
    migrate_old_dirs();
    secure_dirs();
    let dir = data_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    let db = Db::open(&dir.join("mailrs.db"))?;
    match cli.command {
        Command::Account(AccountCommand::Add) => add_account(&db).await,
        Command::Account(AccountCommand::List) => list_accounts(&db).await,
        Command::Account(AccountCommand::Remove { email }) => remove_account(&db, &email).await,
        Command::Sync => run_sync(&db, &dir, &load_config()?).await,
        Command::Threads {
            account,
            label,
            limit,
        } => list_threads(&db, account.as_deref(), label, limit).await,
        Command::Show { account, thread_id } => {
            show_thread(&db, &load_config()?, &account, &thread_id).await
        }
        Command::Export {
            account,
            thread_id,
            message,
            out,
        } => {
            export_mail(
                &db,
                &load_config()?,
                &account,
                &thread_id,
                message.as_deref(),
                out,
            )
            .await
        }
        Command::Diagnose { account, thread_id } => {
            diagnose(&db, &load_config()?, &account, &thread_id).await
        }
        Command::Triage {
            account,
            thread_id,
            action,
        } => triage(&db, &load_config()?, &account, &thread_id, action).await,
    }
}

/// `config.toml`, or the defaults when there is none. The file holds the
/// sync settings and, for accounts added through the old setup page, their
/// own Google client; a copy that never had one needs no file.
fn load_config() -> Result<Config> {
    let path = config_path()?;
    match Config::load(&path) {
        Ok(config) => Ok(config),
        Err(err) if err.is_missing() => Ok(Config::default()),
        Err(err) => Err(err.into()),
    }
}

/// The client `account` signs in with, or an error that says what to do.
/// An account left with none is marked as needing a new sign-in.
async fn oauth_for(db: &Db, config: &Config, account: &Account) -> Result<OAuthClient> {
    account_client(db, config, built_in_client(), account)
        .await?
        .ok_or_else(|| {
            anyhow!(
                "{} needs to sign in again: run `penguin-mail-cli account add`",
                account.email
            )
        })
}

fn token_store() -> Arc<dyn TokenStore> {
    Arc::new(KeyringTokenStore::new())
}

async fn add_account(db: &Db) -> Result<()> {
    // Every sign-in, first or again, goes through the build's client.
    let oauth = built_in_client()
        .ok_or_else(|| anyhow!("This copy of Penguin Mail was built without Google sign-in."))?;
    let flow = authorize(&oauth, GMAIL_API_BASE, &[], |url| {
        println!(
            "Opening your browser for Google's consent screen. If it does not open, visit:\n\n{url}\n"
        );
        let _ = Process::new("xdg-open")
            .arg(url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    });
    let authorized = tokio::time::timeout(CONSENT_TIMEOUT, flow)
        .await
        .context("gave up waiting for the browser after five minutes")??;
    let tokens = token_store();
    let (email, refresh) = (authorized.email.clone(), authorized.refresh_token.clone());
    tokio::task::spawn_blocking(move || tokens.save(&email, &refresh)).await??;
    let account = signed_in(db, &authorized.email, now_millis()).await?;
    println!(
        "Added {} as account {}. Run `penguin-mail-cli sync` to download mail.",
        account.email, account.id
    );
    Ok(())
}

async fn list_accounts(db: &Db) -> Result<()> {
    let all = db.read(accounts::list_accounts).await?;
    if all.is_empty() {
        println!("No accounts. Run `penguin-mail-cli account add`.");
        return Ok(());
    }
    for account in all {
        println!(
            "{:>3}  {:<40} {}",
            account.id,
            account.email,
            account.state.as_str()
        );
    }
    Ok(())
}

async fn remove_account(db: &Db, email: &str) -> Result<()> {
    let account = find_account(db, email).await?;
    db.write(move |c| accounts::delete_account(c, account.id))
        .await?;
    let (tokens, owned) = (token_store(), email.to_string());
    tokio::task::spawn_blocking(move || tokens.delete(&owned)).await??;
    println!(
        "Removed {email}. Revoke Google's side at https://myaccount.google.com/permissions if you want."
    );
    Ok(())
}

async fn run_sync(db: &Db, dir: &Path, config: &Config) -> Result<()> {
    // Held until the command ends, so the app cannot sync the same store
    // underneath this one.
    let _lock = match SyncLock::take(dir) {
        Ok(lock) => lock,
        Err(LockError::Held) => bail!(
            "Penguin Mail is already syncing the mail in {}; quit the app or the other \
             `penguin-mail-cli sync` first",
            dir.display()
        ),
        Err(err) => return Err(err.into()),
    };
    let all = db.read(accounts::list_accounts).await?;
    if all.is_empty() {
        bail!("no accounts; run `penguin-mail-cli account add` first");
    }
    let (engine, events) = SyncEngine::new(db.clone(), config.engine_config());
    let tokens = token_store();
    for account in &all {
        let oauth = match oauth_for(db, config, account).await {
            Ok(oauth) => oauth,
            Err(err) => {
                eprintln!("{err}");
                continue;
            }
        };
        let connected = match account.provider {
            Provider::Gmail => connect_account(oauth, Arc::clone(&tokens), account)
                .await
                .map(AccountServices::google),
        };
        match connected {
            Ok(services) => engine.start_account(account.id, services),
            Err(err) => eprintln!("{}: {err}", account.email),
        }
    }
    let emails: HashMap<AccountId, String> = all.iter().map(|a| (a.id, a.email.clone())).collect();
    println!("Syncing {} account(s). Press Ctrl-C to stop.", all.len());
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            event = events.recv() => match event {
                Ok(event) => print_event(&emails, &event),
                Err(_) => break,
            },
        }
    }
    engine.shutdown();
    Ok(())
}

fn print_event(emails: &HashMap<AccountId, String>, event: &ChangeEvent) {
    match event {
        ChangeEvent::AccountStateChanged { account_id, state } => {
            println!("{}: {}", who(emails, *account_id), state.as_str());
        }
        ChangeEvent::LabelsChanged { account_id } => {
            println!("{}: labels updated", who(emails, *account_id))
        }
        ChangeEvent::ThreadsChanged {
            account_id,
            thread_ids,
        } => {
            println!(
                "{}: {} thread(s) changed",
                who(emails, *account_id),
                thread_ids.len()
            );
        }
        ChangeEvent::NewMail {
            account_id,
            message_ids,
        } => {
            println!(
                "{}: {} new message(s)",
                who(emails, *account_id),
                message_ids.len()
            );
        }
        ChangeEvent::WriteFailed {
            account_id,
            message,
        }
        | ChangeEvent::WaitingOnGmail {
            account_id,
            message,
        } => println!("{}: {message}", who(emails, *account_id)),
    }
}

fn who(emails: &HashMap<AccountId, String>, account_id: AccountId) -> &str {
    emails
        .get(&account_id)
        .map(String::as_str)
        .unwrap_or("unknown account")
}

async fn list_threads(db: &Db, account: Option<&str>, label: String, limit: i64) -> Result<()> {
    let account_id = match account {
        Some(email) => Some(find_account(db, email).await?.id),
        None => None,
    };
    let filter = match account_id {
        Some(account_id) => ThreadFilter::account(account_id, label),
        None => ThreadFilter::unified(label),
    };
    let rows = db
        .read(move |c| threads::list_threads(c, &filter, 0, limit))
        .await?;
    for t in rows {
        println!(
            "{} {:>2} {:<18} {:<22} {:<48} {}",
            if t.unread { '*' } else { ' ' },
            t.account_id,
            t.id,
            truncate(&t.from, 22),
            truncate(&t.subject, 48),
            format_date(t.last_message_at)
        );
    }
    Ok(())
}

async fn show_thread(db: &Db, config: &Config, email: &str, thread_id: &str) -> Result<()> {
    let sync = account_sync(db, config, email).await?;
    sync.ensure_thread(thread_id).await?;
    let (account_id, thread) = (sync.account_id(), thread_id.to_string());
    let messages = db
        .read(move |c| messages::thread_messages(c, account_id, &thread))
        .await?;
    if messages.is_empty() {
        bail!("Gmail has no thread {thread_id} in {email}");
    }
    for message in messages {
        println!(
            "From:    {}",
            message
                .from
                .as_ref()
                .map(|a| a.display())
                .unwrap_or("(unknown)")
        );
        println!("Date:    {}", format_date(message.date));
        println!("Subject: {}\n", message.subject);
        let body = sync.body(&message.id).await?;
        println!("{}", body.text.or(body.html).unwrap_or_default().trim_end());
        println!("{}", "-".repeat(72));
    }
    Ok(())
}

/// Writes mail to a file other mail programs read: the whole thread as an
/// mbox, or one message as the RFC 822 bytes it arrived in. Without `--out`
/// the name comes from the subject and the date, in the working directory.
async fn export_mail(
    db: &Db,
    config: &Config,
    email: &str,
    thread_id: &str,
    message_id: Option<&str>,
    out: Option<PathBuf>,
) -> Result<()> {
    let sync = account_sync(db, config, email).await?;
    let extension = if message_id.is_some() { "eml" } else { "mbox" };
    let path = match out {
        Some(path) => path,
        None => {
            sync.ensure_thread(thread_id).await?;
            let (account_id, thread) = (sync.account_id(), thread_id.to_string());
            let stored = db
                .read(move |c| messages::thread_messages(c, account_id, &thread))
                .await?;
            let named = match message_id {
                Some(id) => stored.iter().find(|m| m.id == id),
                None => stored.last(),
            };
            let Some(named) = named else {
                bail!("Gmail has no thread {thread_id} in {email}");
            };
            PathBuf::from(export::file_name(&named.subject, named.date, extension))
        }
    };
    let mail = match message_id {
        Some(id) => sync.raw_message(id).await?,
        None => sync.export_mbox(thread_id, None).await?,
    };
    std::fs::write(&path, &mail).with_context(|| format!("could not write {}", path.display()))?;
    println!("Wrote {} bytes to {}", mail.len(), path.display());
    Ok(())
}

/// Counts what a message holds, so a blank one can be explained without
/// anybody reading the mail. Every line is a number or a yes.
async fn diagnose(db: &Db, config: &Config, email: &str, thread_id: &str) -> Result<()> {
    let sync = account_sync(db, config, email).await?;
    sync.ensure_thread(thread_id).await?;
    let (account_id, thread) = (sync.account_id(), thread_id.to_string());
    let messages = db
        .read(move |c| messages::thread_messages(c, account_id, &thread))
        .await?;
    if messages.is_empty() {
        bail!("Gmail has no thread {thread_id} in {email}");
    }
    for (index, message) in messages.iter().enumerate() {
        let body = sync.body(&message.id).await?;
        let html = body.html.unwrap_or_default();
        let text = body.text.unwrap_or_default();
        let lower = html.to_ascii_lowercase();
        let images = body
            .attachments
            .iter()
            .filter(|a| a.mime_type.starts_with("image/"))
            .count();
        let inline = body
            .attachments
            .iter()
            .filter(|a| a.content_id.is_some())
            .count();
        println!("message {}", index + 1);
        println!("  html bytes            {}", html.len());
        println!("  text bytes            {}", text.len());
        println!("  <img> tags            {}", lower.matches("<img").count());
        println!(
            "  remote image sources  {}",
            lower.matches("src=\"http").count() + lower.matches("src='http").count()
        );
        println!("  cid: image sources    {}", lower.matches("cid:").count());
        println!("  attached images       {images}, of them inline {inline}");
        println!(
            "  background-image      {}",
            lower.matches("background-image").count()
        );
        println!(
            "  <style> blocks        {}",
            lower.matches("<style").count()
        );
        println!(
            "  display:none rules    {}",
            lower.replace(' ', "").matches("display:none").count()
        );
        println!(
            "  dark mode rules       {}",
            lower.matches("prefers-color-scheme").count()
        );
        let squashed = lower.replace(' ', "");
        println!(
            "  near-white text rules {}",
            squashed.matches("color:#fff").count()
                + squashed.matches("color:#ffffff").count()
                + squashed.matches("color:white").count()
                + squashed.matches("color:rgb(255,255,255)").count()
        );
        println!(
            "  remote backgrounds    {}",
            squashed.matches("background-image:url(http").count()
                + squashed.matches("background:url(http").count()
        );
        println!("  words outside tags    {}", visible_words(&html));
    }
    Ok(())
}

/// How many words a reader would see: text outside tags, with script and
/// style contents left out.
fn visible_words(html: &str) -> usize {
    let mut words = 0;
    let mut inside_tag = false;
    let mut skipping: Option<&str> = None;
    let mut word = false;
    let lower = html.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    for (index, ch) in lower.char_indices() {
        if let Some(tag) = skipping {
            if lower[index..].starts_with(tag) {
                skipping = None;
            }
            continue;
        }
        match ch {
            '<' => {
                inside_tag = true;
                word = false;
                if lower[index..].starts_with("<script") {
                    skipping = Some("</script");
                } else if lower[index..].starts_with("<style") {
                    skipping = Some("</style");
                }
            }
            '>' => inside_tag = false,
            _ if inside_tag => {}
            c if c.is_whitespace() => word = false,
            _ => {
                if !word {
                    words += 1;
                    word = true;
                }
            }
        }
        let _ = bytes;
    }
    words
}

async fn triage(
    db: &Db,
    config: &Config,
    email: &str,
    thread_id: &str,
    action: TriageAction,
) -> Result<()> {
    let sync = account_sync(db, config, email).await?;
    sync.triage_thread(thread_id, &action).await?;
    println!("{}: done.", action.describe());
    Ok(())
}

/// A one-off sync handle for commands that do not run the engine.
async fn account_sync(db: &Db, config: &Config, email: &str) -> Result<AccountSync> {
    let account = find_account(db, email).await?;
    let oauth = oauth_for(db, config, &account).await?;
    let services = match account.provider {
        Provider::Gmail => {
            AccountServices::google(connect_account(oauth, token_store(), &account).await?)
        }
    };
    let (events, _) = async_channel::unbounded();
    let engine = config.engine_config();
    Ok(
        AccountSync::new(account.id, services, db.clone(), events)
            .with_limits(engine.window_days, engine.body_cache_bytes),
    )
}

async fn find_account(db: &Db, email: &str) -> Result<Account> {
    let owned = email.to_string();
    db.read(move |c| accounts::account_by_email(c, &owned))
        .await?
        .with_context(|| format!("no account {email}; `penguin-mail-cli account list` shows them"))
}

fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn format_date(millis: EpochMillis) -> String {
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}
