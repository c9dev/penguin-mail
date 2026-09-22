//! What the assistant's mail tools do. A call arrives by name with its JSON
//! input, [`Tools::run`] finds the tool in the catalog, and the tool's
//! handler answers from the mail modules plus two ports: [`Desk`] for what
//! the window has on screen, and [`Effects`] for what a tool asks the
//! window to do.
//!
//! Nothing here touches GTK. The window is one adapter behind the ports and
//! the tests are another, so the whole tool loop runs headless.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone};
use mailrs_ai::ToolOutcome;
use mailrs_domain::smart::{Condition, SmartMailbox};
use mailrs_domain::{
    Account, AccountId, Category, EpochMillis, FlagColor, Folder, Label, LabelKind, Target,
    ThreadSummary, system_label,
};
use mailrs_gmail::GmailError;
use mailrs_store::{Db, messages};
use mailrs_sync::{
    AccountSettings, AccountSync, Accounts, AutomaticReply, Calendar, Failure, History,
    Invitations, MailAction, MailActions, Mailbox, Mailboxes, NewLabels, Outcome, Permitted, Scope,
    SyncError, TriageAction, View,
};
use serde_json::{Value, json};

use crate::compose::{self, Draft};
use crate::hide_my_email::HiddenAddress;
use crate::rules::{RuleForm, describe_action, describe_criteria};
use crate::settings::{
    Change, Choice, ColorScheme, MarkRead, RemoteImages, Setting, Settings, TextSize, UndoSend,
};
use crate::unsubscribe::Unsubscribe;
use mailrs_domain::translate::{fill, fill_plural, gettext};

mod calendar;
mod catalog;
#[cfg(test)]
mod fake;
mod mail;
#[cfg(test)]
mod tests;

use catalog::Plan;
pub use catalog::{label, specs};

/// The id a planned rule gives a label the account lacks, until the user
/// approves and the label is made. Gmail ids never hold a space.
const NEW_LABEL: &str = "new label";

type ToolResult = Result<Value, String>;

/// An account a tool named, with the loop that syncs it.
type Syncing<A> = (Account, Arc<AccountSync<<A as Accounts>::Api>>);

pub use crate::wanted::Answer;

/// The conversation the window shows, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenConversation {
    pub account_id: AccountId,
    pub thread_id: String,
    /// Set when the window shows one message of the thread rather than all.
    pub message_id: Option<String>,
    pub subject: String,
}

/// What the window has on screen when a tool call arrives.
#[derive(Debug, Clone, Default)]
pub struct OnScreen {
    /// The mailbox title, as the header shows it.
    pub mailbox: String,
    pub open: Option<OpenConversation>,
    pub selected: Vec<ThreadSummary>,
}

/// What the tools read from the window. Every method gives back plain data,
/// so a test fills it in without a widget.
pub trait Desk {
    fn settings(&self) -> Settings;
    fn accounts(&self) -> Vec<Account>;
    fn labels(&self) -> HashMap<AccountId, Vec<Label>>;
    /// The settings that change what a mailbox lists.
    fn view(&self) -> View;
    fn on_screen(&self) -> OnScreen;
    /// The account a new message comes from: the one set in Preferences,
    /// else the account in view, else the first.
    fn default_account(&self) -> Option<AccountId>;
}

/// A Google permission a tool can find missing. The window asks for the
/// same four, so the two share one type.
pub use crate::permission::Permission;

/// What the tools ask the window to do. A test records the calls instead.
pub trait Effects {
    /// Asks the user to approve an action. `true` when they agree.
    fn confirm(&self, question: String) -> Answer<'_, bool>;
    /// Offers the account a Google permission it still lacks.
    fn ask_permission(&self, account_id: AccountId, permission: Permission);
    /// Tells the user that the Google Cloud project has `service` switched
    /// off, and offers the page at `enable_url` that turns it on.
    fn explain_api_off(&self, service: &str, enable_url: &str);
    /// Sends the draft at `at`, from a Gmail draft, as Send Later does. A
    /// draft with no `draft_id` is new and gets its signature first; one
    /// that has an id already went through a composer that signed it.
    fn send_later(&self, draft: Draft, at: EpochMillis) -> Result<(), String>;
    /// Leaves a mailing list the way `how` says, from the account.
    fn unsubscribe(
        &self,
        account_id: AccountId,
        how: Unsubscribe,
    ) -> Answer<'_, Result<(), String>>;
    fn change_settings(&self, change: Change) -> Result<(), String>;
    /// A blank message from the account, carrying its identity.
    fn new_draft(&self, account_id: AccountId) -> Result<Draft, String>;
    /// Opens a composer on the draft, signed.
    fn compose(&self, draft: Draft) -> Result<(), String>;
    /// Sends the draft, signed, after the undo delay.
    fn send(&self, draft: Draft) -> Result<(), String>;
    fn show_thread(&self, summary: ThreadSummary);
    fn copy(&self, text: &str);
    /// Redraws what a mail action changed and lists the mailbox again.
    fn mail_changed(&self, action: &MailAction, outcome: &Outcome);
    /// Counts and rows again, after a change no mail action covers.
    fn relist(&self);
    /// Moves a sender's mail into a category and rules their future mail
    /// there.
    fn categorize_sender(
        &self,
        account_id: AccountId,
        email: String,
        who: String,
        category: Category,
    );
    /// Makes a Hide My Email address for the account and saves it.
    fn hide_address(
        &self,
        account_id: AccountId,
        note: String,
    ) -> Answer<'_, Result<Permitted<HiddenAddress>, String>>;
    /// Turns a Hide My Email address on or off.
    fn set_address_active(
        &self,
        address: String,
        active: bool,
    ) -> Answer<'_, Result<Permitted<()>, String>>;
}

/// Hands a future to the sync runtime. The GTK thread has no tokio reactor
/// of its own, so every store and Gmail call crosses here.
pub trait Background {
    fn start(&self, task: Pin<Box<dyn Future<Output = ()> + Send>>);
}

/// The modules a tool call works through: mail actions, mailbox listing,
/// Gmail settings, the calendar, the invitations in mail, the accounts
/// that sync, and the store.
pub struct Modules<A: Accounts> {
    pub mail: Arc<MailActions<A>>,
    pub lists: Arc<Mailboxes<A>>,
    pub gmail: Arc<AccountSettings<A>>,
    pub calendar: Arc<Calendar<A>>,
    pub invitations: Arc<Invitations<A>>,
    pub accounts: Arc<A>,
    pub db: Db,
}

/// The assistant's tools, and the one way to run them.
pub struct Tools<A: Accounts> {
    modules: Modules<A>,
    background: Rc<dyn Background>,
    desk: Rc<dyn Desk>,
    effects: Rc<dyn Effects>,
}

fn text(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn flag(input: &Value, key: &str) -> Option<bool> {
    input.get(key).and_then(Value::as_bool)
}

fn required(input: &Value, key: &str) -> Result<String, String> {
    text(input, key).ok_or_else(|| format!("`{key}` is missing"))
}

/// What the model hears after a tool asked the user for `permission`.
fn asked_for(permission: Permission, account: &Account) -> String {
    format!(
        "Penguin Mail needs permission to {} for {}. The user was asked to grant it; try again once they have.",
        permission.purpose(),
        account.email
    )
}

/// The category a tool names. The tools offer no "all", since the whole
/// inbox needs no category.
fn named_category(key: &str) -> Result<Category, String> {
    Category::from_key(key)
        .filter(|c| *c != Category::All)
        .ok_or_else(|| format!("Unknown category {key}."))
}

/// A mailbox `list_mail` names. Its keys make the schema's enum, so the
/// model is offered the names this parser takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MailboxName {
    Inbox,
    Flagged,
    Sent,
    Drafts,
    Vips,
    /// Sent mail that has waited 3 to 30 days for a reply.
    FollowUp,
    /// Received mail taken out of the inbox.
    Archive,
    Junk,
    Trash,
    AllMail,
    /// The label `list_mail` names in its `label` field.
    Label,
}

impl MailboxName {
    const ALL: [MailboxName; 11] = [
        MailboxName::Inbox,
        MailboxName::Flagged,
        MailboxName::Sent,
        MailboxName::Drafts,
        MailboxName::Vips,
        MailboxName::FollowUp,
        MailboxName::Archive,
        MailboxName::Junk,
        MailboxName::Trash,
        MailboxName::AllMail,
        MailboxName::Label,
    ];

    fn key(self) -> &'static str {
        match self {
            MailboxName::Inbox => "inbox",
            MailboxName::Flagged => "flagged",
            MailboxName::Sent => "sent",
            MailboxName::Drafts => "drafts",
            MailboxName::Vips => "vips",
            MailboxName::FollowUp => "follow_up",
            MailboxName::Archive => "archive",
            MailboxName::Junk => "junk",
            MailboxName::Trash => "trash",
            MailboxName::AllMail => "all_mail",
            MailboxName::Label => "label",
        }
    }

    fn named(key: &str) -> Option<MailboxName> {
        MailboxName::ALL.into_iter().find(|m| m.key() == key)
    }
}

/// What `organize` does to conversations. Its keys make the schema's enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Organize {
    Archive,
    Trash,
    Junk,
    NotJunk,
    MoveToInbox,
    MarkRead,
    MarkUnread,
    Flag,
    Unflag,
}

impl Organize {
    const ALL: [Organize; 9] = [
        Organize::Archive,
        Organize::Trash,
        Organize::Junk,
        Organize::NotJunk,
        Organize::MoveToInbox,
        Organize::MarkRead,
        Organize::MarkUnread,
        Organize::Flag,
        Organize::Unflag,
    ];

    fn key(self) -> &'static str {
        match self {
            Organize::Archive => "archive",
            Organize::Trash => "trash",
            Organize::Junk => "junk",
            Organize::NotJunk => "not_junk",
            Organize::MoveToInbox => "move_to_inbox",
            Organize::MarkRead => "mark_read",
            Organize::MarkUnread => "mark_unread",
            Organize::Flag => "flag",
            Organize::Unflag => "unflag",
        }
    }

    fn named(key: &str) -> Option<Organize> {
        Organize::ALL.into_iter().find(|o| o.key() == key)
    }

    /// The mail action, flagging in `color`.
    fn action(self, color: FlagColor) -> MailAction {
        let triage = MailAction::Triage;
        match self {
            Organize::Archive => triage(TriageAction::Archive),
            Organize::Trash => triage(TriageAction::Trash),
            Organize::Junk => triage(TriageAction::Junk),
            Organize::NotJunk => triage(TriageAction::NotJunk),
            Organize::MoveToInbox => triage(TriageAction::Untrash),
            Organize::MarkRead => triage(TriageAction::MarkRead),
            Organize::MarkUnread => triage(TriageAction::MarkUnread),
            Organize::Flag => MailAction::Flag(Some(color)),
            Organize::Unflag => MailAction::Flag(None),
        }
    }
}

/// Every choice of a setting, in the shape the settings file uses.
fn choices<T: Choice + serde::Serialize>() -> Vec<Value> {
    T::ALL
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .collect()
}

impl<A: Accounts> Tools<A> {
    pub fn new(
        modules: Modules<A>,
        background: Rc<dyn Background>,
        desk: Rc<dyn Desk>,
        effects: Rc<dyn Effects>,
    ) -> Tools<A> {
        Tools {
            modules,
            background,
            desk,
            effects,
        }
    }

    /// Runs one tool call from the assistant.
    pub async fn run(&self, name: &str, input: Value) -> ToolOutcome {
        let result = match catalog::find::<A>(name) {
            Some(tool) => tool.run(self, &input).await,
            None => Err(format!("There is no tool called {name}.")),
        };
        match result {
            Ok(value) => ToolOutcome::Ok(value),
            Err(message) => ToolOutcome::Err(message),
        }
    }

    // ---- Crossing to the runtime -----------------------------------------

    /// Runs `task` on the sync runtime and waits for its answer here.
    async fn away<T: Send + 'static>(
        &self,
        task: impl Future<Output = T> + Send + 'static,
    ) -> Result<T, String> {
        let (done, answer) = async_channel::bounded(1);
        self.background.start(Box::pin(async move {
            let _ = done.send(task.await).await;
        }));
        answer
            .recv()
            .await
            .map_err(|_| "The background task failed.".to_string())
    }

    /// As [`Tools::away`], with the task's own error folded into the answer.
    async fn call<T, E>(
        &self,
        task: impl Future<Output = Result<T, E>> + Send + 'static,
    ) -> Result<T, String>
    where
        T: Send + 'static,
        E: std::fmt::Display + Send + 'static,
    {
        match self.away(task).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err(err.to_string()),
            Err(problem) => Err(problem),
        }
    }

    /// Runs a read query on the store's reader pool.
    async fn read<T, F>(&self, query: F) -> Result<T, String>
    where
        F: FnOnce(&rusqlite::Connection) -> mailrs_store::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let db = self.modules.db.clone();
        self.call(async move { db.read(query).await }).await
    }

    // ---- Lookups ---------------------------------------------------------

    fn account_named(&self, email: &str) -> Result<Account, String> {
        self.desk
            .accounts()
            .into_iter()
            .find(|a| a.email.eq_ignore_ascii_case(email.trim()))
            .ok_or_else(|| format!("There is no account {email}."))
    }

    fn sync_for(&self, email: &str) -> Result<Syncing<A>, String> {
        let account = self.account_named(email)?;
        let sync = self
            .modules
            .accounts
            .account(account.id)
            .ok_or_else(|| format!("{} is not connected.", account.email))?;
        Ok((account, sync))
    }

    /// The account a tool names and its Gmail settings.
    fn settings_for(&self, email: &str) -> Result<(Account, Arc<AccountSettings<A>>), String> {
        let account = self.account_named(email)?;
        if self.modules.accounts.account(account.id).is_none() {
            return Err(format!("{} is not connected.", account.email));
        }
        Ok((account, Arc::clone(&self.modules.gmail)))
    }

    fn email_of(&self, account_id: AccountId) -> String {
        self.desk
            .accounts()
            .into_iter()
            .find(|a| a.id == account_id)
            .map(|a| a.email)
            .unwrap_or_default()
    }

    fn parse_targets(&self, input: &Value) -> Result<Vec<Target>, String> {
        let items = input
            .get("targets")
            .and_then(Value::as_array)
            .ok_or("`targets` is missing")?;
        items
            .iter()
            .map(|item| {
                Ok(Target {
                    account_id: self.account_named(&required(item, "account")?)?.id,
                    thread_id: required(item, "thread_id")?,
                    message_id: text(item, "message_id"),
                })
            })
            .collect()
    }

    fn row_json(&self, row: &ThreadSummary) -> Value {
        let date = crate::format::local(row.last_message_at)
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        json!({
            "account": self.email_of(row.account_id),
            "thread_id": row.id,
            "message_id": row.message_id,
            "from": row.from,
            "from_email": row.from_email,
            "subject": row.subject,
            "date": date,
            "unread": row.unread,
            "flagged": row.starred,
            "flag_color": row.flag_color.map(|c| c.as_str()),
            "messages": row.message_count,
            "has_attachments": row.has_attachments,
            "snippet": row.snippet,
        })
    }

    /// Asks the user for the Gmail settings permission, and says so.
    fn needs_permission(&self, account: &Account) -> String {
        self.effects
            .ask_permission(account.id, Permission::Settings);
        asked_for(Permission::Settings, account)
    }

    /// Runs a call that needs a Google permission, and turns the two ways
    /// Google can refuse into what the model should hear. A missing
    /// permission asks the user for it; an API the Cloud project has
    /// switched off shows the user where to turn it on, since no
    /// permission would help.
    async fn permitted<T: Send + 'static>(
        &self,
        account: &Account,
        permission: Permission,
        task: impl Future<Output = Result<Permitted<T>, SyncError>> + Send + 'static,
    ) -> Result<T, String> {
        match self.away(task).await? {
            Ok(Permitted::Done(value)) => Ok(value),
            Ok(Permitted::NeedsPermission) => {
                self.effects.ask_permission(account.id, permission);
                Err(asked_for(permission, account))
            }
            Err(SyncError::Gmail(GmailError::ApiDisabled {
                service,
                enable_url,
            })) => {
                self.effects.explain_api_off(&service, &enable_url);
                Err(format!(
                    "The {service} is switched off in the Google Cloud project Penguin Mail signs in with, so Google refuses the call. The user was shown where to turn it on ({enable_url}); try again once they have."
                ))
            }
            Err(err) => Err(err.to_string()),
        }
    }

    /// The account a tool names, or the default one when it names none.
    fn account_or_default(&self, input: &Value) -> Result<Account, String> {
        match text(input, "account") {
            Some(email) => self.account_named(&email),
            None => {
                let id = self.desk.default_account().ok_or("Add an account first.")?;
                self.desk
                    .accounts()
                    .into_iter()
                    .find(|a| a.id == id)
                    .ok_or_else(|| "Add an account first.".to_string())
            }
        }
    }

    // ---- Reading ---------------------------------------------------------

    fn context(&self) -> ToolResult {
        let settings = self.desk.settings();
        let labels = self.desk.labels();
        let accounts: Vec<Value> = self
            .desk
            .accounts()
            .iter()
            .map(|a| {
                let names: Vec<String> = labels
                    .get(&a.id)
                    .map(|all| {
                        all.iter()
                            .filter(|l| l.kind == LabelKind::User)
                            .map(|l| l.name.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                json!({
                    "email": a.email,
                    "name": settings.account_names.get(&a.email),
                    "labels": names,
                })
            })
            .collect();
        let screen = self.desk.on_screen();
        let open = screen.open.as_ref().map(|o| {
            json!({
                "account": self.email_of(o.account_id),
                "thread_id": o.thread_id,
                "message_id": o.message_id,
                "subject": o.subject,
            })
        });
        let selected: Vec<Value> = screen.selected.iter().map(|r| self.row_json(r)).collect();
        Ok(json!({
            "now": Local::now().format("%A %Y-%m-%d %H:%M").to_string(),
            "accounts": accounts,
            "default_account": settings.default_account,
            "vips": settings.vips.keys().collect::<Vec<_>>(),
            "mailbox_on_screen": screen.mailbox,
            "open_conversation": open,
            "selected": selected,
        }))
    }

    /// The accounts a listing may read, in sidebar order.
    fn scope(&self) -> Scope {
        Scope::over(self.desk.accounts())
    }

    /// One page of each mailbox, through `Mailboxes::list`.
    async fn rows_of(&self, mailbox: Mailbox, view: View) -> Result<Vec<ThreadSummary>, String> {
        let lists = Arc::clone(&self.modules.lists);
        let scope = self.scope();
        let listed = self
            .call(async move { lists.list(&mailbox, &scope, &view, 0).await })
            .await?;
        match listed.notices.first() {
            Some(problem) => Err(problem.clone()),
            None => Ok(listed.rows),
        }
    }

    async fn list(&self, input: &Value) -> ToolResult {
        let name = required(input, "mailbox")?;
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .clamp(1, 200) as usize;
        let unread_only = flag(input, "unread_only").unwrap_or(false);
        let scope = match text(input, "account") {
            Some(email) => Some(self.account_named(&email)?),
            None => None,
        };
        let category = match text(input, "category") {
            Some(key) => Some(named_category(&key)?),
            None => None,
        };
        let mailboxes = self.named_mailboxes(&name, text(input, "label"), scope.as_ref())?;
        // Unread mail is picked out of the rows, so ask for extra.
        let view = View {
            category,
            limit: Some(limit * if unread_only { 4 } else { 1 }),
            ..self.desk.view()
        };
        let mut rows: Vec<ThreadSummary> = Vec::new();
        for mailbox in mailboxes {
            rows.extend(self.rows_of(mailbox, view.clone()).await?);
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.last_message_at));
        if unread_only {
            rows.retain(|r| r.unread);
        }
        rows.truncate(limit);
        Ok(json!({
            "count": rows.len(),
            "conversations": rows.iter().map(|r| self.row_json(r)).collect::<Vec<_>>(),
        }))
    }

    /// The mailboxes a tool's name stands for. A label with no account
    /// named becomes one mailbox per account that has it.
    fn named_mailboxes(
        &self,
        name: &str,
        label: Option<String>,
        scope: Option<&Account>,
    ) -> Result<Vec<Mailbox>, String> {
        let at = |label: &'static str| match scope {
            Some(account) => Mailbox::Label {
                account_id: account.id,
                label_id: label.into(),
                name: crate::ui::account_label_name(label),
            },
            None => Mailbox::Unified(label),
        };
        let folder = |folder| Mailbox::Folder {
            account_id: scope.map(|a| a.id),
            folder,
        };
        let named = MailboxName::named(name).ok_or_else(|| format!("Unknown mailbox {name}."))?;
        Ok(match named {
            MailboxName::Inbox => vec![at(system_label::INBOX)],
            MailboxName::Flagged => vec![at(system_label::STARRED)],
            MailboxName::Sent => vec![at(system_label::SENT)],
            MailboxName::Drafts => vec![at(system_label::DRAFT)],
            MailboxName::FollowUp => vec![Mailbox::FollowUp],
            MailboxName::Archive => vec![folder(Folder::Archive)],
            MailboxName::Junk => vec![folder(Folder::Junk)],
            MailboxName::Trash => vec![folder(Folder::Trash)],
            MailboxName::AllMail => vec![folder(Folder::AllMail)],
            MailboxName::Vips => vec![Mailbox::Vips {
                emails: self.desk.settings().vips.keys().cloned().collect(),
                name: "VIPs".into(),
            }],
            MailboxName::Label => {
                let wanted = label.ok_or("`label` is missing")?;
                let labels = self.desk.labels();
                let found: Vec<Mailbox> = labels
                    .iter()
                    .filter(|(id, _)| scope.is_none_or(|a| a.id == **id))
                    .flat_map(|(id, all)| {
                        all.iter()
                            .filter(|l| l.name.eq_ignore_ascii_case(&wanted))
                            .map(|l| Mailbox::Label {
                                account_id: *id,
                                label_id: l.id.clone(),
                                name: l.name.clone(),
                            })
                            .collect::<Vec<_>>()
                    })
                    .collect();
                if found.is_empty() {
                    return Err(format!("There is no label called {wanted}."));
                }
                found
            }
        })
    }

    async fn search(&self, input: &Value) -> ToolResult {
        let query = required(input, "query")?;
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .clamp(1, 100) as usize;
        let scope = match text(input, "account") {
            Some(email) => Some(self.account_named(&email)?),
            None => None,
        };
        let mailbox = Mailbox::Search {
            query,
            account_id: scope.map(|a| a.id),
        };
        let view = View {
            threading: true,
            limit: Some(limit),
            ..self.desk.view()
        };
        let rows = self.rows_of(mailbox, view).await?;
        Ok(json!({
            "count": rows.len(),
            "conversations": rows.iter().map(|r| self.row_json(r)).collect::<Vec<_>>(),
        }))
    }

    async fn read_thread(&self, input: &Value) -> ToolResult {
        const MAX_CHARS: usize = 8000;
        let (account, sync) = self.sync_for(&required(input, "account")?)?;
        let thread_id = required(input, "thread_id")?;
        let (s, t) = (Arc::clone(&sync), thread_id.clone());
        if let Err(err) = self.call(async move { s.ensure_thread(&t).await }).await {
            tracing::info!(error = %err, "reading the stored copy of the thread");
        }
        let key = thread_id.clone();
        let found = self
            .read(move |c| messages::thread_messages(c, account.id, &key))
            .await?;
        if found.is_empty() {
            return Err("That conversation was not found.".into());
        }
        let mut out = Vec::new();
        for meta in found {
            let (s, id) = (Arc::clone(&sync), meta.id.clone());
            let body = self.call(async move { s.body(&id).await }).await.ok();
            let mut body_text = body
                .as_ref()
                .map(compose::body_text)
                .unwrap_or_else(|| meta.snippet.clone());
            if body_text.chars().count() > MAX_CHARS {
                body_text = body_text.chars().take(MAX_CHARS).collect::<String>() + "\n[cut short]";
            }
            let people = |list: &[mailrs_domain::Address]| {
                list.iter()
                    .map(|a| a.display().to_string() + " <" + &a.email + ">")
                    .collect::<Vec<_>>()
            };
            out.push(json!({
                "message_id": meta.id,
                "from": meta.from.as_ref().map(|a| format!("{} <{}>", a.display(), a.email)),
                "to": people(&meta.to),
                "cc": people(&meta.cc),
                "date": crate::format::local(meta.date).map(|d| d.format("%Y-%m-%d %H:%M").to_string()),
                "subject": meta.subject,
                "labels": meta.label_ids,
                "text": body_text,
                "invitation": body.as_ref().is_some_and(|b| b.calendar.is_some()),
                "unsubscribe": body.as_ref().is_some_and(|b| b.list_unsubscribe.is_some()),
                "attachments": body
                    .map(|b| b.attachments.iter().map(|a| a.filename.clone()).collect::<Vec<_>>())
                    .unwrap_or_default(),
            }));
        }
        Ok(json!({"account": account.email, "thread_id": thread_id, "messages": out}))
    }

    // ---- Organizing ------------------------------------------------------

    /// Asks only before trashing more than 25 conversations. Ctrl+Z undoes
    /// the rest, and a handful in the Trash is easy to see and fetch back.
    async fn organize<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let targets = self.parse_targets(input)?;
        let key = required(input, "action")?;
        let organize = Organize::named(&key).ok_or_else(|| format!("Unknown action {key}."))?;
        let color: Option<FlagColor> = text(input, "color").and_then(|c| c.parse().ok());
        let action = organize.action(color.unwrap_or_else(|| self.desk.settings().flag_color));
        let count = targets.len();
        let change = async move { report(&self.act(targets, action).await) };
        if organize == Organize::Trash && count > 25 {
            return Ok(Plan::ask(
                fill_plural(
                    "Move {count} conversation to the Trash?",
                    "Move {count} conversations to the Trash?",
                    count,
                    &[("count", &count.to_string())],
                ),
                change,
            ));
        }
        Ok(Plan::without_asking(change))
    }

    /// Runs a mail action that Ctrl+Z can undo, then updates the window.
    async fn act(&self, targets: Vec<Target>, action: MailAction) -> Outcome {
        let mail = Arc::clone(&self.modules.mail);
        let (given, asked) = (targets.clone(), action.clone());
        let outcome = self
            .away(async move { mail.run(&given, asked, History::Record).await })
            .await
            .unwrap_or_else(|err| Outcome {
                done: vec![],
                failed: targets
                    .into_iter()
                    .map(|target| Failure {
                        target,
                        error: err.clone(),
                    })
                    .collect(),
            });
        self.effects.mail_changed(&action, &outcome);
        outcome
    }

    /// Labels by name. A label an account lacks waits for the user's word,
    /// asked once for the whole call while Ask Before Acting is on. A "no"
    /// labels only the mail in accounts that hold the names already.
    async fn label(&self, input: &Value) -> ToolResult {
        let targets = self.parse_targets(input)?;
        let names = |key: &str| -> Vec<String> {
            input
                .get(key)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        let (add, remove) = (names("add"), names("remove"));
        let plan = NewLabels::plan(&targets, &add, &remove, |account_id| {
            self.labels_of(account_id)
                .into_iter()
                .map(|l| l.name)
                .collect()
        });
        let kept = plan.kept(&targets);
        let create = plan.is_empty()
            || !self.desk.settings().ai.confirm_actions
            || self
                .effects
                .confirm(self.new_labels_question(&plan, !kept.is_empty()))
                .await;
        let targets = if create { targets } else { kept };
        if targets.is_empty() {
            return Err("The user declined.".into());
        }
        let action = MailAction::Label {
            add,
            remove,
            create,
        };
        let mut result = report(&self.act(targets, action).await)?;
        if !create {
            result["declined"] = json!(
                "The user declined new labels, so mail in accounts without them was left alone."
            );
        }
        Ok(result)
    }

    /// The question before labelling makes new labels. `partly` says a "no"
    /// still labels the mail in accounts that have the names.
    fn new_labels_question(&self, plan: &NewLabels, partly: bool) -> String {
        let mut question = format!(
            "{} {}",
            plan.heading(),
            plan.who(|account_id| self.email_of(account_id))
        );
        if partly {
            question.push(' ');
            question.push_str(&gettext(
                "Don't Allow labels only the mail in accounts that have the label already.",
            ));
        }
        question
    }

    async fn create_label(&self, input: &Value) -> ToolResult {
        let (account, settings) = self.settings_for(&required(input, "account")?)?;
        let (name, account_id) = (required(input, "name")?, account.id);
        let made = self
            .call(async move { settings.create_label(account_id, &name).await })
            .await?;
        match made {
            Permitted::Done(label) => Ok(json!({"account": account.email, "created": label.name})),
            Permitted::NeedsPermission => Err(self.needs_permission(&account)),
        }
    }

    async fn remind(&self, input: &Value) -> ToolResult {
        let targets = self.parse_targets(input)?;
        let when = future_instant(&required(input, "at")?)?;
        let mut result = report(&self.act(targets, MailAction::Remind { at: when }).await)?;
        result["returns"] = json!(crate::format::future_date(when, Local::now()));
        Ok(result)
    }

    // ---- Writing ---------------------------------------------------------

    /// Opens a composer on the message for the user to review.
    async fn draft(&self, input: &Value) -> ToolResult {
        let draft = self.draft_from(input).await?;
        self.effects.compose(draft)?;
        Ok(json!({"opened": "A composer window shows the draft for the user to review."}))
    }

    async fn send<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let draft = self.draft_from(input).await?;
        if let Some(problem) = draft.problem() {
            return Err(problem);
        }
        let to = compose::format_recipients(&draft.to);
        let question = fill(
            &gettext("Send “{subject}” to {recipients}?"),
            &[("subject", &draft.subject), ("recipients", &to)],
        );
        Ok(Plan::ask(question, async move {
            let delay = self.desk.settings().undo_send.seconds();
            self.effects.send(draft)?;
            Ok(json!({"sent": true, "undo_seconds": delay}))
        }))
    }

    /// The message the fields `message_fields` describes, threaded into
    /// the conversation `reply_to` names.
    async fn draft_from(&self, input: &Value) -> Result<Draft, String> {
        let list = |key: &str| -> String {
            input
                .get(key)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default()
        };
        let reply = input.get("reply_to").filter(|v| v.is_object());
        let reply_account = match reply {
            Some(r) => Some(self.account_named(&required(r, "account")?)?),
            None => None,
        };
        let account = match text(input, "account") {
            Some(email) => self.account_named(&email)?,
            None => match &reply_account {
                Some(a) => a.clone(),
                None => {
                    let id = self.desk.default_account().ok_or("Add an account first.")?;
                    self.desk
                        .accounts()
                        .into_iter()
                        .find(|a| a.id == id)
                        .ok_or("Add an account first.")?
                }
            },
        };
        let mut draft = self.effects.new_draft(account.id)?;
        draft.to = compose::parse_recipients(&list("to"));
        draft.cc = compose::parse_recipients(&list("cc"));
        draft.subject = text(input, "subject").unwrap_or_default();
        draft.markdown = input
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let (Some(r), Some(reply_account)) = (reply, &reply_account)
            && reply_account.id == account.id
        {
            let thread_id = required(r, "thread_id")?;
            let key = thread_id.clone();
            let found = self
                .read(move |c| messages::thread_messages(c, account.id, &key))
                .await?;
            let parent = found
                .iter()
                .rev()
                .find(|m| !m.has_label(system_label::DRAFT));
            draft.thread_id = Some(thread_id);
            draft.in_reply_to = parent.and_then(|m| m.rfc822_msgid.clone());
            draft.references = found
                .iter()
                .filter_map(|m| m.rfc822_msgid.clone())
                .collect();
            if draft.subject.is_empty()
                && let Some(parent) = parent
            {
                draft.subject = if parent.subject.to_lowercase().starts_with("re:") {
                    parent.subject.clone()
                } else {
                    format!("Re: {}", parent.subject)
                };
            }
        }
        Ok(draft)
    }

    // ---- Gmail settings --------------------------------------------------

    async fn block<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let (account, settings) = self.settings_for(&required(input, "account")?)?;
        let email = required(input, "email")?;
        let question = fill(
            &gettext("Block {address}? Their future mail goes straight to the Trash."),
            &[("address", &email)],
        );
        Ok(Plan::ask(question, async move {
            let blocked = {
                let (email, account_id) = (email.clone(), account.id);
                self.call(async move { settings.block_sender(account_id, &email).await })
                    .await
            };
            match blocked {
                Ok(Permitted::Done(_)) => Ok(json!({"blocked": email})),
                Ok(Permitted::NeedsPermission) => Err(self.needs_permission(&account)),
                Err(err) => Err(err),
            }
        }))
    }

    async fn get_vacation(&self, input: &Value) -> ToolResult {
        let (account, settings) = self.settings_for(&required(input, "account")?)?;
        let account_id = account.id;
        let loaded = self
            .call(async move { settings.automatic_reply(account_id).await })
            .await;
        match loaded {
            Ok(Permitted::Done(reply)) => Ok(reply_json(&reply)),
            Ok(Permitted::NeedsPermission) => Err(self.needs_permission(&account)),
            Err(err) => Err(err),
        }
    }

    /// Reads the reply Gmail holds first, so the question shows the reply
    /// as it will stand once the call's fields are laid over it.
    async fn set_vacation<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let (account, settings) = self.settings_for(&required(input, "account")?)?;
        let account_id = account.id;
        let loaded = {
            let settings = Arc::clone(&settings);
            self.call(async move { settings.automatic_reply(account_id).await })
                .await
        };
        let mut reply = match loaded {
            Ok(Permitted::Done(reply)) => reply,
            Ok(Permitted::NeedsPermission) => return Err(self.needs_permission(&account)),
            Err(err) => return Err(err),
        };
        let day = |key: &str| -> Result<Option<i64>, String> {
            match text(input, key) {
                None => Ok(None),
                Some(value) => {
                    let date = NaiveDate::parse_from_str(&value, "%Y-%m-%d")
                        .map_err(|_| format!("Could not read the date {value}; use YYYY-MM-DD."))?;
                    Ok(date
                        .and_hms_opt(0, 0, 0)
                        .and_then(|t| Local.from_local_datetime(&t).earliest())
                        .map(|t| t.timestamp_millis()))
                }
            }
        };
        reply.enabled = flag(input, "enabled").unwrap_or(true);
        if let Some(subject) = text(input, "subject") {
            reply.subject = subject;
        }
        if let Some(message) = input.get("message").and_then(Value::as_str) {
            reply.body = message.to_string();
        }
        if let Some(contacts) = flag(input, "contacts_only") {
            reply.contacts_only = contacts;
        }
        reply.first_day = day("first_day")?;
        reply.last_day = day("last_day")?;
        if reply.enabled && reply.subject.trim().is_empty() {
            reply.subject = "Out of office".into();
        }
        let summary = if reply.enabled {
            let day = |t: Option<i64>| {
                t.and_then(crate::format::local)
                    .map(|d| d.format("%a %-d %b").to_string())
            };
            let dates = match (day(reply.first_day), day(reply.last_day)) {
                (Some(first), Some(last)) => fill(
                    &gettext(" from {first} to {last}"),
                    &[("first", &first), ("last", &last)],
                ),
                (None, Some(last)) => fill(&gettext(" until {last}"), &[("last", &last)]),
                (Some(first), None) => fill(&gettext(" from {first}"), &[("first", &first)]),
                (None, None) => String::new(),
            };
            let preview: String = reply.body.chars().take(160).collect();
            fill(
                &gettext(
                    "Turn on the automatic reply for {account}{dates}?\n\n“{subject}”\n{body}",
                ),
                &[
                    ("account", &account.email),
                    ("dates", &dates),
                    ("subject", &reply.subject),
                    ("body", &preview),
                ],
            )
        } else {
            fill(
                &gettext("Turn off the automatic reply for {account}?"),
                &[("account", &account.email)],
            )
        };
        Ok(Plan::ask(summary, async move {
            let saved = reply.clone();
            let stored = self
                .call(async move { settings.set_automatic_reply(account_id, &saved).await })
                .await;
            match stored {
                Ok(Permitted::Done(())) => Ok(reply_json(&reply)),
                Ok(Permitted::NeedsPermission) => Err(self.needs_permission(&account)),
                Err(err) => Err(err),
            }
        }))
    }

    async fn list_rules(&self, input: &Value) -> ToolResult {
        let (account, settings) = self.settings_for(&required(input, "account")?)?;
        let labels = self.labels_of(account.id);
        let account_id = account.id;
        let listed = self
            .call(async move { settings.rules(account_id).await })
            .await;
        match listed {
            Ok(Permitted::Done(filters)) => Ok(json!({
                "rules": filters.iter().map(|f| json!({
                    "id": f.id,
                    "when": describe_criteria(&f.criteria),
                    "then": describe_action(&f.action, |id| {
                        labels.iter().find(|l| l.id == id).map(|l| l.name.clone())
                    }),
                })).collect::<Vec<_>>(),
            })),
            Ok(Permitted::NeedsPermission) => Err(self.needs_permission(&account)),
            Err(err) => Err(err),
        }
    }

    fn labels_of(&self, account_id: AccountId) -> Vec<Label> {
        self.desk
            .labels()
            .get(&account_id)
            .cloned()
            .unwrap_or_default()
    }

    async fn create_rule<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let (account, settings) = self.settings_for(&required(input, "account")?)?;
        let labels = self.labels_of(account.id);
        // A label the account lacks is made only once the user says yes, so
        // a declined rule leaves nothing behind. Until then the rule names
        // it by a stand-in id the question reads as the label's name.
        let wanted = text(input, "label");
        let existing = wanted.as_ref().and_then(|name| {
            labels
                .iter()
                .find(|l| l.name.to_lowercase() == name.to_lowercase())
                .map(|l| l.id.clone())
        });
        let form = RuleForm {
            from: text(input, "from").unwrap_or_default(),
            to: text(input, "to").unwrap_or_default(),
            subject: text(input, "subject").unwrap_or_default(),
            has_words: text(input, "has_words").unwrap_or_default(),
            not_words: text(input, "not_words").unwrap_or_default(),
            has_attachment: flag(input, "has_attachment").unwrap_or(false),
            skip_inbox: flag(input, "skip_inbox").unwrap_or(false),
            mark_read: flag(input, "mark_read").unwrap_or(false),
            star: flag(input, "star").unwrap_or(false),
            label: wanted
                .as_ref()
                .map(|_| existing.clone().unwrap_or_else(|| NEW_LABEL.into())),
            never_spam: flag(input, "never_spam").unwrap_or(false),
            trash: flag(input, "delete").unwrap_or(false),
        };
        let mut filter = form.filter().map_err(str::to_string)?;
        let name = |id: &str| match id {
            NEW_LABEL => wanted.clone(),
            id => labels.iter().find(|l| l.id == id).map(|l| l.name.clone()),
        };
        let summary = fill(
            &gettext("Create a Gmail rule for {account}: {when} → {then}?"),
            &[
                ("account", &account.email),
                ("when", &describe_criteria(&filter.criteria)),
                (
                    "then",
                    &describe_action(&filter.action, name).to_lowercase(),
                ),
            ],
        );
        Ok(Plan::ask(summary, async move {
            let account_id = account.id;
            if let (Some(name), None) = (wanted, existing) {
                let mail = Arc::clone(&self.modules.mail);
                let wanted = name.clone();
                let id = self
                    .call(async move { mail.label_id(account_id, &wanted, true).await })
                    .await
                    .map_err(|e| format!("Could not create the label {name}: {e}"))?;
                for label in &mut filter.action.add_label_ids {
                    if label == NEW_LABEL {
                        *label = id.clone();
                    }
                }
            }
            let added = self
                .call(async move { settings.add_rule(account_id, filter).await })
                .await;
            match added {
                Ok(Permitted::Done(created)) => Ok(json!({"created": created.id})),
                Ok(Permitted::NeedsPermission) => Err(self.needs_permission(&account)),
                Err(err) => Err(err),
            }
        }))
    }

    async fn delete_rule<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let (account, settings) = self.settings_for(&required(input, "account")?)?;
        let id = required(input, "id")?;
        let question = fill(
            &gettext("Delete a Gmail rule from {account}?"),
            &[("account", &account.email)],
        );
        Ok(Plan::ask(question, async move {
            let account_id = account.id;
            let deleted = self
                .call(async move { settings.delete_rule(account_id, &id).await })
                .await;
            match deleted {
                Ok(Permitted::Done(())) => Ok(json!({"deleted": true})),
                Ok(Permitted::NeedsPermission) => Err(self.needs_permission(&account)),
                Err(err) => Err(err),
            }
        }))
    }

    // ---- App settings ----------------------------------------------------

    fn settings_json(&self) -> Value {
        let settings = self.desk.settings();
        let current: serde_json::Map<String, Value> = Setting::ALL
            .iter()
            .map(|s| (s.name().to_string(), s.value(&settings)))
            .collect();
        json!({
            "settings": current,
            "choices": {
                "mark_read": choices::<MarkRead>(),
                "remote_images": choices::<RemoteImages>(),
                "text_size": choices::<TextSize>(),
                "color_scheme": choices::<ColorScheme>(),
                "undo_send": choices::<UndoSend>(),
                "threading": "true groups mail into conversations",
                "default_account": "an account address, or null for the first",
            },
        })
    }

    fn change_setting(&self, input: &Value) -> ToolResult {
        let name = required(input, "name")?;
        let setting = Setting::named(&name)
            .ok_or_else(|| format!("{name} is not a setting the assistant can change."))?;
        let value = input.get("value").cloned().unwrap_or(Value::Null);
        let change = setting
            .change(&value)
            .map_err(|e| format!("{value} is not a valid value for {name}: {e}"))?;
        self.effects.change_settings(change)?;
        Ok(json!({"changed": name, "value": value}))
    }

    fn signature(&self, input: &Value) -> ToolResult {
        let account = self.account_named(&required(input, "account")?)?;
        let text = input
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.effects.change_settings(Change::Signature {
            email: account.email.clone(),
            text,
        })?;
        Ok(json!({"signature_set_for": account.email}))
    }

    fn vip(&self, input: &Value) -> ToolResult {
        let email = required(input, "email")?.to_lowercase();
        let add = flag(input, "add").unwrap_or(true);
        let name = text(input, "name").unwrap_or_default();
        self.effects.change_settings(Change::SetVip {
            email: email.clone(),
            name,
            add,
        })?;
        Ok(json!({"email": email, "vip": add}))
    }

    fn smart(&self, input: &Value) -> ToolResult {
        let conditions: Vec<Condition> =
            serde_json::from_value(input.get("conditions").cloned().unwrap_or(Value::Null))
                .map_err(|e| format!("Could not read the conditions: {e}"))?;
        let account = match text(input, "account") {
            Some(email) => Some(self.account_named(&email)?.email),
            None => None,
        };
        let mailbox = SmartMailbox {
            id: format!("smart-{}", mailrs_gmail::random_token(6)),
            name: required(input, "name")?,
            account,
            match_all: flag(input, "match_all").unwrap_or(true),
            conditions,
        };
        let query = mailbox
            .query()
            .ok_or("Give at least one condition with a value.")?;
        let name = mailbox.name.clone();
        self.effects
            .change_settings(Change::SaveSmartMailbox(Box::new(mailbox)))?;
        Ok(json!({"created": name, "gmail_query": query}))
    }

    fn open(&self, input: &Value) -> ToolResult {
        let account = self.account_named(&required(input, "account")?)?;
        let thread_id = required(input, "thread_id")?;
        self.effects.show_thread(ThreadSummary {
            account_id: account.id,
            id: thread_id,
            message_count: 1,
            ..ThreadSummary::default()
        });
        Ok(json!({"opened": true}))
    }

    // ---- Senders, follow-ups, and hidden addresses ------------------------

    async fn categorize<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let account = self.account_named(&required(input, "account")?)?;
        let email = required(input, "email")?;
        let key = required(input, "category")?;
        let category = named_category(&key)?;
        let who = text(input, "name").unwrap_or_else(|| email.clone());
        let question = fill(
            &gettext(
                "Move mail from {sender} to {category} in {account}, and add a Gmail \
                 rule for their future mail?",
            ),
            &[
                ("sender", &who),
                ("category", &category.name()),
                ("account", &account.email),
            ],
        );
        Ok(Plan::ask(question, async move {
            self.effects
                .categorize_sender(account.id, email.clone(), who, category);
            Ok(json!({"sender": email, "category": key}))
        }))
    }

    async fn dismiss_follow_up(&self, input: &Value) -> ToolResult {
        let account = self.account_named(&required(input, "account")?)?;
        let thread_id = required(input, "thread_id")?;
        let target = Target::thread(account.id, &thread_id);
        let outcome = self.act(vec![target], MailAction::DismissFollowUp).await;
        if let Some(error) = outcome.first_error() {
            return Err(error.to_string());
        }
        Ok(json!({"dismissed": thread_id}))
    }

    fn hidden_list(&self) -> Value {
        json!({
            "addresses": self.desk.settings().hidden_addresses.iter().map(|h| json!({
                "address": h.address,
                "account": h.account,
                "note": h.note,
                "active": h.active,
            })).collect::<Vec<_>>(),
        })
    }

    async fn hidden_create(&self, input: &Value) -> ToolResult {
        let account = self.account_named(&required(input, "account")?)?;
        let note = text(input, "note").unwrap_or_default();
        match self.effects.hide_address(account.id, note).await {
            Ok(Permitted::Done(hidden)) => {
                self.effects.copy(&hidden.address);
                Ok(json!({"address": hidden.address, "copied": true}))
            }
            Ok(Permitted::NeedsPermission) => Err(self.needs_permission(&account)),
            Err(err) => Err(err),
        }
    }

    async fn hidden_set(&self, input: &Value) -> ToolResult {
        let address = required(input, "address")?;
        let active = flag(input, "active").ok_or("`active` is missing")?;
        let hidden = self
            .desk
            .settings()
            .hidden_addresses
            .into_iter()
            .find(|h| h.address.eq_ignore_ascii_case(&address))
            .ok_or_else(|| format!("{address} is not a Hide My Email address."))?;
        let account = self.account_named(&hidden.account)?;
        match self
            .effects
            .set_address_active(address.clone(), active)
            .await
        {
            Ok(Permitted::Done(())) => Ok(json!({"address": address, "active": active})),
            Ok(Permitted::NeedsPermission) => Err(self.needs_permission(&account)),
            Err(err) => Err(err),
        }
    }
}

/// A moment a tool was given, in the forms the tool specs promise: local
/// time as `YYYY-MM-DDTHH:MM`, with seconds or without, or RFC 3339 with an
/// offset of its own.
fn instant(text: &str) -> Result<EpochMillis, String> {
    let text = text.trim();
    if let Ok(at) = DateTime::parse_from_rfc3339(text) {
        return Ok(at.timestamp_millis());
    }
    let naive = ["%Y-%m-%dT%H:%M", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|shape| NaiveDateTime::parse_from_str(text, shape).ok())
        .ok_or_else(|| format!("Could not read the time {text}; use YYYY-MM-DDTHH:MM."))?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|at| at.timestamp_millis())
        .ok_or_else(|| "That time does not exist here.".to_string())
}

/// As [`instant`], for a time something is to happen at.
fn future_instant(text: &str) -> Result<EpochMillis, String> {
    let at = instant(text)?;
    if at <= Local::now().timestamp_millis() {
        return Err("That time is in the past.".into());
    }
    Ok(at)
}

/// A moment as the tools write one back: local time, `YYYY-MM-DDTHH:MM`,
/// the shape they take it in.
fn local_text(at: EpochMillis) -> String {
    crate::format::local(at)
        .map(|at| at.format("%Y-%m-%dT%H:%M").to_string())
        .unwrap_or_default()
}

fn reply_json(reply: &AutomaticReply) -> Value {
    let day = |t: Option<i64>| {
        t.and_then(crate::format::local)
            .map(|d| d.format("%Y-%m-%d").to_string())
    };
    json!({
        "enabled": reply.enabled,
        "subject": reply.subject,
        "message": reply.body,
        "contacts_only": reply.contacts_only,
        "first_day": day(reply.first_day),
        "last_day": day(reply.last_day),
    })
}

/// What the model hears about a mail action: how many targets changed, and
/// which failed and why. An error when nothing changed.
fn report(outcome: &Outcome) -> ToolResult {
    if let (true, Some(error)) = (outcome.done.is_empty(), outcome.first_error()) {
        return Err(error.to_string());
    }
    let mut result = json!({
        "done": outcome.done.len(),
        "undo": "The user can press Ctrl+Z to undo this.",
    });
    if !outcome.failed.is_empty() {
        result["failed"] = outcome
            .failed
            .iter()
            .map(|f| {
                json!({
                    "thread_id": f.target.thread_id,
                    "message_id": f.target.message_id,
                    "error": f.error,
                })
            })
            .collect();
    }
    Ok(result)
}
