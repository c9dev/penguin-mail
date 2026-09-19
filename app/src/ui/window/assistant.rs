//! What the assistant's tools do. Each call arrives from the model through
//! `assistant::Host` and runs here, on the GTK thread, with the window's
//! accounts, store, and actions.

use std::rc::Rc;
use std::sync::Arc;

use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
use mailrs_ai::ToolOutcome;
use mailrs_domain::{Account, Filter, FlagColor, Label, LabelKind, ThreadSummary, Vacation};
use mailrs_store::threads::{self, ThreadFilter};
use mailrs_store::{flags, messages, reminders};
use mailrs_sync::TriageAction;
use serde_json::{Value, json};

use super::{MainWindow, Target};
use crate::compose::{self, Draft, SendWhen};
use crate::core::Sync;
use crate::rules::{RuleForm, describe_action, describe_criteria};
use crate::settings::{Choice, ColorScheme, MarkRead, RemoteImages, Settings, TextSize, UndoSend};
use crate::smart::{Condition, SmartMailbox};
use crate::ui::vacation::missing_scope;
use crate::ui::{Folder, summarize_search};

type ToolResult = Result<Value, String>;

/// Settings the assistant may read and change. The assistant's own
/// settings are left out on purpose.
const SETTING_NAMES: [&str; 10] = [
    "threading",
    "mark_read",
    "remote_images",
    "text_size",
    "color_scheme",
    "notifications",
    "notification_previews",
    "notify_vips_only",
    "undo_send",
    "default_account",
];

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

/// Every choice of a setting, in the shape the settings file uses.
fn choices<T: Choice + serde::Serialize>() -> Vec<Value> {
    T::ALL
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .collect()
}

impl MainWindow {
    /// Runs one tool call from the assistant.
    pub(super) async fn run_tool(self: &Rc<Self>, name: &str, input: Value) -> ToolOutcome {
        let result = match name {
            "get_context" => self.tool_context(),
            "list_mail" => self.tool_list(&input).await,
            "search_mail" => self.tool_search(&input).await,
            "read_conversation" => self.tool_read(&input).await,
            "organize" => self.tool_organize(&input).await,
            "label" => self.tool_label(&input).await,
            "remind_me" => self.tool_remind(&input).await,
            "draft_email" => self.tool_message(&input, false).await,
            "send_email" => self.tool_message(&input, true).await,
            "block_sender" => self.tool_block(&input).await,
            "get_automatic_reply" => self.tool_get_vacation(&input).await,
            "set_automatic_reply" => self.tool_set_vacation(&input).await,
            "list_rules" => self.tool_list_rules(&input).await,
            "create_rule" => self.tool_create_rule(&input).await,
            "delete_rule" => self.tool_delete_rule(&input).await,
            "create_label" => self.tool_create_label(&input).await,
            "get_settings" => Ok(self.tool_settings()),
            "change_setting" => self.tool_change_setting(&input),
            "set_signature" => self.tool_signature(&input),
            "vip" => self.tool_vip(&input),
            "create_smart_mailbox" => self.tool_smart(&input),
            "open_conversation" => self.tool_open(&input),
            "categorize_sender" => self.tool_categorize(&input).await,
            "dismiss_follow_up" => self.tool_dismiss_follow_up(&input).await,
            "list_hidden_addresses" => Ok(self.tool_hidden_list()),
            "create_hidden_address" => self.tool_hidden_create(&input).await,
            "set_hidden_address" => self.tool_hidden_set(&input).await,
            other => Err(format!("There is no tool called {other}.")),
        };
        match result {
            Ok(value) => ToolOutcome::Ok(value),
            Err(message) => ToolOutcome::Err(message),
        }
    }

    // ---- Lookups ---------------------------------------------------------

    fn account_named(&self, email: &str) -> Result<Account, String> {
        self.accounts
            .borrow()
            .iter()
            .find(|a| a.email.eq_ignore_ascii_case(email.trim()))
            .cloned()
            .ok_or_else(|| format!("There is no account {email}."))
    }

    fn sync_for(&self, email: &str) -> Result<(Account, Arc<Sync>), String> {
        let account = self.account_named(email)?;
        let sync = self
            .core
            .account(account.id)
            .ok_or_else(|| format!("{} is not connected.", account.email))?;
        Ok((account, sync))
    }

    fn email_of(&self, account_id: i64) -> String {
        self.accounts
            .borrow()
            .iter()
            .find(|a| a.id == account_id)
            .map(|a| a.email.clone())
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
                    message_id: None,
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

    /// Asks the user when the assistant settings want approval.
    async fn approve(&self, what: &str) -> Result<(), String> {
        if !self.settings().ai.confirm_actions || self.assistant.confirm(what).await {
            Ok(())
        } else {
            Err("The user declined.".into())
        }
    }

    /// Explains a Gmail error, offering to fix a missing permission.
    fn gmail_error(self: &Rc<Self>, account: &Account, err: anyhow::Error) -> String {
        if missing_scope(&err) {
            self.ask_for_settings_access(account.id);
            format!(
                "Penguin Mail needs permission to change Gmail settings for {}. The user was asked to grant it; try again once they have.",
                account.email
            )
        } else {
            err.to_string()
        }
    }

    /// Applies `action` to `targets` now and keeps it for Ctrl+Z.
    async fn triage_now(
        self: &Rc<Self>,
        targets: &[Target],
        action: TriageAction,
    ) -> Result<usize, String> {
        let mut done = 0;
        for target in targets {
            let Some(sync) = self.core.account(target.account_id) else {
                continue;
            };
            let (thread, act) = (target.thread_id.clone(), action.clone());
            self.core
                .call(async move { sync.triage_thread(&thread, &act).await })
                .await
                .map_err(|e| format!("{} failed: {e}", action.describe()))?;
            done += 1;
        }
        *self.undo.borrow_mut() = Some((targets.to_vec(), action.inverse()));
        self.prune_folder(targets);
        self.queue_refresh();
        Ok(done)
    }

    // ---- Reading ---------------------------------------------------------

    fn tool_context(&self) -> ToolResult {
        let settings = self.settings();
        let accounts: Vec<Value> = self
            .accounts
            .borrow()
            .iter()
            .map(|a| {
                let labels: Vec<String> = self
                    .labels
                    .borrow()
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
                    "labels": labels,
                })
            })
            .collect();
        let open = self.conversation.with_open(|o| {
            json!({
                "account": self.email_of(o.account_id),
                "thread_id": o.thread_id,
                "subject": o.subject,
            })
        });
        let selected: Vec<Value> = self
            .list
            .selected_rows()
            .iter()
            .map(|r| self.row_json(r))
            .collect();
        Ok(json!({
            "now": Local::now().format("%A %Y-%m-%d %H:%M").to_string(),
            "accounts": accounts,
            "default_account": settings.default_account,
            "vips": settings.vips.keys().collect::<Vec<_>>(),
            "mailbox_on_screen": self.mailbox.borrow().title(),
            "open_conversation": open,
            "selected": selected,
        }))
    }

    async fn tool_list(self: &Rc<Self>, input: &Value) -> ToolResult {
        let mailbox = required(input, "mailbox")?;
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
        let folder = match mailbox.as_str() {
            "junk" => Some(Folder::Junk),
            "trash" => Some(Folder::Trash),
            "all_mail" => Some(Folder::AllMail),
            _ => None,
        };
        let mut rows: Vec<ThreadSummary> = if mailbox == "follow_up" {
            let scope_id = scope.as_ref().map(|a| a.id);
            let now = Local::now().timestamp_millis();
            self.core
                .read(move |c| {
                    let mut rows = Vec::new();
                    for item in mailrs_store::follow_ups::waiting(c, now)? {
                        if scope_id.is_some_and(|id| id != item.account_id) {
                            continue;
                        }
                        if let Some(row) = threads::get_thread(c, item.account_id, &item.thread_id)?
                        {
                            rows.push(row);
                        }
                    }
                    Ok(rows)
                })
                .await
                .map_err(|e| e.to_string())?
        } else if let Some(folder) = folder {
            let mut query = folder.query().to_string();
            if unread_only {
                query.push_str(" is:unread");
            }
            self.remote_rows(&query, scope.as_ref(), limit).await?
        } else {
            let mut filters = self.list_filters(&mailbox, text(input, "label"), scope.as_ref())?;
            if let Some(category) = text(input, "category") {
                let (any, none): (&[&str], &[&str]) = match category.as_str() {
                    "primary" => (
                        &[],
                        &[
                            "CATEGORY_UPDATES",
                            "CATEGORY_PROMOTIONS",
                            "CATEGORY_SOCIAL",
                            "CATEGORY_FORUMS",
                        ],
                    ),
                    "updates" => (&["CATEGORY_UPDATES"], &[]),
                    "promotions" => (&["CATEGORY_PROMOTIONS"], &[]),
                    "social" => (&["CATEGORY_SOCIAL", "CATEGORY_FORUMS"], &[]),
                    other => return Err(format!("Unknown category {other}.")),
                };
                filters = filters
                    .into_iter()
                    .map(|f| f.with_labels(any, none))
                    .collect();
            }
            let fetch = limit * if unread_only { 4 } else { 1 };
            let found = self
                .core
                .read(move |c| {
                    let mut out = Vec::new();
                    for filter in &filters {
                        out.extend(threads::list_threads(c, filter, 0, fetch as i64)?);
                    }
                    Ok(out)
                })
                .await
                .map_err(|e| e.to_string())?;
            let mut found = found;
            found.sort_by_key(|r| std::cmp::Reverse(r.last_message_at));
            found
        };
        if unread_only {
            rows.retain(|r| r.unread);
        }
        rows.truncate(limit);
        Ok(json!({
            "count": rows.len(),
            "conversations": rows.iter().map(|r| self.row_json(r)).collect::<Vec<_>>(),
        }))
    }

    /// Store filters for a mailbox name, one per account for labels.
    fn list_filters(
        &self,
        mailbox: &str,
        label: Option<String>,
        scope: Option<&Account>,
    ) -> Result<Vec<ThreadFilter>, String> {
        let at = |label: &str| match scope {
            Some(account) => ThreadFilter::account(account.id, label),
            None => ThreadFilter::unified(label),
        };
        Ok(match mailbox {
            "inbox" => vec![at("INBOX")],
            "flagged" => vec![at("STARRED")],
            "sent" => vec![at("SENT")],
            "drafts" => vec![at("DRAFT")],
            "vips" => vec![at("").from_senders(self.settings().vips.keys().cloned().collect())],
            "label" => {
                let name = label.ok_or("`label` is missing")?;
                let labels = self.labels.borrow();
                let found: Vec<ThreadFilter> = labels
                    .iter()
                    .filter(|(id, _)| scope.is_none_or(|a| a.id == **id))
                    .flat_map(|(id, all)| {
                        all.iter()
                            .filter(|l| l.name.eq_ignore_ascii_case(&name))
                            .map(|l| ThreadFilter::account(*id, l.id.clone()))
                            .collect::<Vec<_>>()
                    })
                    .collect();
                if found.is_empty() {
                    return Err(format!("There is no label called {name}."));
                }
                found
            }
            other => return Err(format!("Unknown mailbox {other}.")),
        })
    }

    /// Runs a Gmail search in one account or all, one row per conversation.
    async fn remote_rows(
        self: &Rc<Self>,
        query: &str,
        scope: Option<&Account>,
        limit: usize,
    ) -> Result<Vec<ThreadSummary>, String> {
        let accounts: Vec<Account> = match scope {
            Some(account) => vec![account.clone()],
            None => self.accounts.borrow().clone(),
        };
        let mut hits = Vec::new();
        for account in accounts {
            let Some(sync) = self.core.account(account.id) else {
                continue;
            };
            let query = query.to_string();
            let found = self
                .core
                .call(async move { sync.search(&query, limit).await })
                .await
                .map_err(|e| format!("Search failed for {}: {e}", account.email))?;
            hits.extend(found);
        }
        let mut rows = summarize_search(hits, true);
        rows.truncate(limit);
        Ok(rows)
    }

    async fn tool_search(self: &Rc<Self>, input: &Value) -> ToolResult {
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
        let rows = self.remote_rows(&query, scope.as_ref(), limit).await?;
        Ok(json!({
            "count": rows.len(),
            "conversations": rows.iter().map(|r| self.row_json(r)).collect::<Vec<_>>(),
        }))
    }

    async fn tool_read(self: &Rc<Self>, input: &Value) -> ToolResult {
        const MAX_CHARS: usize = 8000;
        let (account, sync) = self.sync_for(&required(input, "account")?)?;
        let thread_id = required(input, "thread_id")?;
        let (s, t) = (sync.clone(), thread_id.clone());
        if let Err(err) = self
            .core
            .call(async move { s.ensure_thread(&t).await })
            .await
        {
            tracing::info!(error = %err, "reading the stored copy of the thread");
        }
        let key = thread_id.clone();
        let found = self
            .core
            .read(move |c| messages::thread_messages(c, account.id, &key))
            .await
            .map_err(|e| e.to_string())?;
        if found.is_empty() {
            return Err("That conversation was not found.".into());
        }
        let mut out = Vec::new();
        for meta in found {
            let (s, id) = (sync.clone(), meta.id.clone());
            let body = self.core.call(async move { s.body(&id).await }).await.ok();
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
                "from": meta.from.as_ref().map(|a| format!("{} <{}>", a.display(), a.email)),
                "to": people(&meta.to),
                "cc": people(&meta.cc),
                "date": crate::format::local(meta.date).map(|d| d.format("%Y-%m-%d %H:%M").to_string()),
                "subject": meta.subject,
                "labels": meta.label_ids,
                "text": body_text,
                "attachments": body
                    .map(|b| b.attachments.iter().map(|a| a.filename.clone()).collect::<Vec<_>>())
                    .unwrap_or_default(),
            }));
        }
        Ok(json!({"account": account.email, "thread_id": thread_id, "messages": out}))
    }

    // ---- Organizing ------------------------------------------------------

    async fn tool_organize(self: &Rc<Self>, input: &Value) -> ToolResult {
        let targets = self.parse_targets(input)?;
        let action = required(input, "action")?;
        let color: Option<FlagColor> = text(input, "color").and_then(|c| c.parse().ok());
        let triage = match action.as_str() {
            "archive" => TriageAction::Archive,
            "trash" => TriageAction::Trash,
            "junk" => TriageAction::Junk,
            "not_junk" => TriageAction::NotJunk,
            "move_to_inbox" => TriageAction::Untrash,
            "mark_read" => TriageAction::MarkRead,
            "mark_unread" => TriageAction::MarkUnread,
            "flag" => TriageAction::Star,
            "unflag" => TriageAction::Unstar,
            other => return Err(format!("Unknown action {other}.")),
        };
        if matches!(triage, TriageAction::Trash) && targets.len() > 25 {
            self.approve(&format!(
                "Move {} conversations to the Trash?",
                targets.len()
            ))
            .await?;
        }
        let done = self.triage_now(&targets, triage.clone()).await?;
        if matches!(triage, TriageAction::Star | TriageAction::Unstar) {
            let color =
                matches!(triage, TriageAction::Star).then(|| color.unwrap_or(FlagColor::Red));
            let targets = targets.clone();
            self.core
                .write(move |c| {
                    for t in &targets {
                        flags::set_color(c, t.account_id, &t.thread_id, None, color)?;
                    }
                    Ok(())
                })
                .await
                .map_err(|e| e.to_string())?;
            self.queue_refresh();
        }
        Ok(json!({"done": done, "undo": "The user can press Ctrl+Z to undo this."}))
    }

    /// Label ids for names in one account, creating missing ones when asked.
    async fn label_ids(
        self: &Rc<Self>,
        account_id: i64,
        names: &[String],
        create: bool,
    ) -> Result<Vec<String>, String> {
        let known: Vec<Label> = self
            .labels
            .borrow()
            .get(&account_id)
            .cloned()
            .unwrap_or_default();
        let mut ids = Vec::new();
        for name in names {
            if let Some(label) = known.iter().find(|l| l.name.eq_ignore_ascii_case(name)) {
                ids.push(label.id.clone());
            } else if create {
                let sync = self
                    .core
                    .account(account_id)
                    .ok_or("That account is not connected.")?;
                let wanted = name.clone();
                let label = self
                    .core
                    .call(async move { sync.create_label(&wanted).await })
                    .await
                    .map_err(|e| format!("Could not create the label {name}: {e}"))?;
                ids.push(label.id);
            } else {
                return Err(format!("There is no label called {name}."));
            }
        }
        Ok(ids)
    }

    async fn tool_label(self: &Rc<Self>, input: &Value) -> ToolResult {
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
        let mut accounts: Vec<i64> = targets.iter().map(|t| t.account_id).collect();
        accounts.sort();
        accounts.dedup();
        let mut changed = 0;
        for account_id in accounts {
            let action = TriageAction::Relabel {
                add: self.label_ids(account_id, &add, true).await?,
                remove: self.label_ids(account_id, &remove, false).await?,
            };
            let mine: Vec<Target> = targets
                .iter()
                .filter(|t| t.account_id == account_id)
                .cloned()
                .collect();
            changed += self.triage_now(&mine, action).await?;
        }
        Ok(json!({"done": changed}))
    }

    async fn tool_create_label(self: &Rc<Self>, input: &Value) -> ToolResult {
        let (account, sync) = self.sync_for(&required(input, "account")?)?;
        let name = required(input, "name")?;
        let label = self
            .core
            .call(async move { sync.create_label(&name).await })
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({"account": account.email, "created": label.name}))
    }

    async fn tool_remind(self: &Rc<Self>, input: &Value) -> ToolResult {
        let targets = self.parse_targets(input)?;
        let at = required(input, "at")?;
        let naive = NaiveDateTime::parse_from_str(&at, "%Y-%m-%dT%H:%M")
            .or_else(|_| NaiveDateTime::parse_from_str(&at, "%Y-%m-%dT%H:%M:%S"))
            .map_err(|_| format!("Could not read the time {at}; use YYYY-MM-DDTHH:MM."))?;
        let when = Local
            .from_local_datetime(&naive)
            .earliest()
            .ok_or("That time does not exist here.")?
            .timestamp_millis();
        if when <= Local::now().timestamp_millis() {
            return Err("That time is in the past.".into());
        }
        let items = targets.clone();
        self.core
            .write(move |c| {
                for t in &items {
                    let subject = threads::get_thread(c, t.account_id, &t.thread_id)?
                        .map(|r| r.subject)
                        .unwrap_or_default();
                    reminders::set(
                        c,
                        &reminders::Reminder {
                            account_id: t.account_id,
                            thread_id: t.thread_id.clone(),
                            subject,
                            remind_at: when,
                        },
                    )?;
                }
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())?;
        self.triage_now(&targets, TriageAction::Archive).await?;
        self.reminders_changed();
        Ok(
            json!({"done": targets.len(), "returns": crate::format::future_date(when, Local::now())}),
        )
    }

    // ---- Writing ---------------------------------------------------------

    /// Opens a draft, or sends after the user approves.
    async fn tool_message(self: &Rc<Self>, input: &Value, send: bool) -> ToolResult {
        let app = self.app.upgrade().ok_or("The app is closing.")?;
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
                    let id = self.default_account().ok_or("Add an account first.")?;
                    self.accounts
                        .borrow()
                        .iter()
                        .find(|a| a.id == id)
                        .cloned()
                        .ok_or("Add an account first.")?
                }
            },
        };
        let mut draft = Draft::new(account.id, app.identity(account.id));
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
                .core
                .read(move |c| messages::thread_messages(c, account.id, &key))
                .await
                .map_err(|e| e.to_string())?;
            let parent = found.iter().rev().find(|m| !m.has_label("DRAFT"));
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
        let draft = app.signed(draft);
        if !send {
            app.compose(draft);
            return Ok(
                json!({"opened": "A composer window shows the draft for the user to review."}),
            );
        }
        if let Some(problem) = draft.problem() {
            return Err(problem);
        }
        let to = compose::format_recipients(&draft.to);
        self.approve(&format!("Send “{}” to {to}?", draft.subject))
            .await?;
        let delay = self.settings().undo_send.seconds();
        app.send(draft, SendWhen::Now);
        Ok(json!({"sent": true, "undo_seconds": delay}))
    }

    // ---- Gmail settings --------------------------------------------------

    async fn tool_block(self: &Rc<Self>, input: &Value) -> ToolResult {
        let (account, sync) = self.sync_for(&required(input, "account")?)?;
        let email = required(input, "email")?;
        self.approve(&format!(
            "Block {email}? Their future mail goes straight to the Trash."
        ))
        .await?;
        let rule = Filter::block(&email);
        match self
            .core
            .call(async move { sync.create_filter(rule).await })
            .await
        {
            Ok(_) => Ok(json!({"blocked": email})),
            Err(err) => Err(self.gmail_error(&account, err)),
        }
    }

    async fn tool_get_vacation(self: &Rc<Self>, input: &Value) -> ToolResult {
        let (account, sync) = self.sync_for(&required(input, "account")?)?;
        match self.core.call(async move { sync.vacation().await }).await {
            Ok(v) => Ok(vacation_json(&v)),
            Err(err) => Err(self.gmail_error(&account, err)),
        }
    }

    async fn tool_set_vacation(self: &Rc<Self>, input: &Value) -> ToolResult {
        let (account, sync) = self.sync_for(&required(input, "account")?)?;
        let s = sync.clone();
        let mut vacation = match self.core.call(async move { s.vacation().await }).await {
            Ok(v) => v,
            Err(err) => return Err(self.gmail_error(&account, err)),
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
        vacation.enabled = flag(input, "enabled").unwrap_or(true);
        if let Some(subject) = text(input, "subject") {
            vacation.subject = subject;
        }
        if let Some(message) = input.get("message").and_then(Value::as_str) {
            vacation.body = message.to_string();
        }
        if let Some(contacts) = flag(input, "contacts_only") {
            vacation.contacts_only = contacts;
        }
        vacation.start = day("first_day")?;
        // Gmail stops at `end`; the last day counts, so end the next midnight.
        vacation.end = day("last_day")?.map(|t| t + 24 * 60 * 60 * 1000);
        if vacation.enabled && vacation.subject.trim().is_empty() {
            vacation.subject = "Out of office".into();
        }
        let summary = if vacation.enabled {
            let day = |t: Option<i64>| {
                t.and_then(crate::format::local)
                    .map(|d| d.format("%a %-d %b").to_string())
            };
            let dates = match (day(vacation.start), day(vacation.end.map(|t| t - 1))) {
                (Some(first), Some(last)) => format!(" from {first} to {last}"),
                (None, Some(last)) => format!(" until {last}"),
                (Some(first), None) => format!(" from {first}"),
                (None, None) => String::new(),
            };
            let preview: String = vacation.body.chars().take(160).collect();
            format!(
                "Turn on the automatic reply for {}{dates}?\n\n“{}”\n{}",
                account.email, vacation.subject, preview
            )
        } else {
            format!("Turn off the automatic reply for {}?", account.email)
        };
        self.approve(&summary).await?;
        let saved = vacation.clone();
        match self
            .core
            .call(async move { sync.set_vacation(saved).await })
            .await
        {
            Ok(()) => Ok(vacation_json(&vacation)),
            Err(err) => Err(self.gmail_error(&account, err)),
        }
    }

    async fn tool_list_rules(self: &Rc<Self>, input: &Value) -> ToolResult {
        let (account, sync) = self.sync_for(&required(input, "account")?)?;
        let labels = self
            .labels
            .borrow()
            .get(&account.id)
            .cloned()
            .unwrap_or_default();
        match self.core.call(async move { sync.filters().await }).await {
            Ok(filters) => Ok(json!({
                "rules": filters.iter().map(|f| json!({
                    "id": f.id,
                    "when": describe_criteria(&f.criteria),
                    "then": describe_action(&f.action, |id| {
                        labels.iter().find(|l| l.id == id).map(|l| l.name.clone())
                    }),
                })).collect::<Vec<_>>(),
            })),
            Err(err) => Err(self.gmail_error(&account, err)),
        }
    }

    async fn tool_create_rule(self: &Rc<Self>, input: &Value) -> ToolResult {
        let (account, sync) = self.sync_for(&required(input, "account")?)?;
        let label = match text(input, "label") {
            Some(name) => self
                .label_ids(account.id, &[name], true)
                .await?
                .into_iter()
                .next(),
            None => None,
        };
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
            label,
            never_spam: flag(input, "never_spam").unwrap_or(false),
            trash: flag(input, "delete").unwrap_or(false),
        };
        let filter = form.filter().map_err(str::to_string)?;
        let labels = self
            .labels
            .borrow()
            .get(&account.id)
            .cloned()
            .unwrap_or_default();
        let name = |id: &str| labels.iter().find(|l| l.id == id).map(|l| l.name.clone());
        let summary = format!(
            "Create a Gmail rule for {}: {} → {}?",
            account.email,
            describe_criteria(&filter.criteria),
            describe_action(&filter.action, name).to_lowercase()
        );
        self.approve(&summary).await?;
        match self
            .core
            .call(async move { sync.create_filter(filter).await })
            .await
        {
            Ok(created) => Ok(json!({"created": created.id})),
            Err(err) => Err(self.gmail_error(&account, err)),
        }
    }

    async fn tool_delete_rule(self: &Rc<Self>, input: &Value) -> ToolResult {
        let (account, sync) = self.sync_for(&required(input, "account")?)?;
        let id = required(input, "id")?;
        self.approve(&format!("Delete a Gmail rule from {}?", account.email))
            .await?;
        match self
            .core
            .call(async move { sync.delete_filter(&id).await })
            .await
        {
            Ok(()) => Ok(json!({"deleted": true})),
            Err(err) => Err(self.gmail_error(&account, err)),
        }
    }

    // ---- App settings ----------------------------------------------------

    fn tool_settings(&self) -> Value {
        let settings = serde_json::to_value(self.settings()).unwrap_or_default();
        let current: serde_json::Map<String, Value> = SETTING_NAMES
            .iter()
            .map(|name| {
                (
                    name.to_string(),
                    settings.get(*name).cloned().unwrap_or(Value::Null),
                )
            })
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

    fn tool_change_setting(&self, input: &Value) -> ToolResult {
        let name = required(input, "name")?;
        if !SETTING_NAMES.contains(&name.as_str()) {
            return Err(format!("{name} is not a setting the assistant can change."));
        }
        let value = input.get("value").cloned().unwrap_or(Value::Null);
        let mut raw = serde_json::to_value(self.settings()).map_err(|e| e.to_string())?;
        raw[&name] = value.clone();
        let changed: Settings = serde_json::from_value(raw)
            .map_err(|e| format!("{value} is not a valid value for {name}: {e}"))?;
        let app = self.app.upgrade().ok_or("The app is closing.")?;
        app.update_settings(move |s| *s = changed);
        Ok(json!({"changed": name, "value": value}))
    }

    fn tool_signature(&self, input: &Value) -> ToolResult {
        let account = self.account_named(&required(input, "account")?)?;
        let text = input
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let app = self.app.upgrade().ok_or("The app is closing.")?;
        let email = account.email.clone();
        app.update_settings(move |s| s.set_signature(&email, &text));
        Ok(json!({"signature_set_for": account.email}))
    }

    fn tool_vip(&self, input: &Value) -> ToolResult {
        let email = required(input, "email")?.to_lowercase();
        let add = flag(input, "add").unwrap_or(true);
        let name = text(input, "name").unwrap_or_else(|| email.clone());
        let app = self.app.upgrade().ok_or("The app is closing.")?;
        let key = email.clone();
        app.update_settings(move |s| {
            if add {
                s.vips.insert(key, name);
            } else {
                s.vips.remove(&key);
            }
        });
        Ok(json!({"email": email, "vip": add}))
    }

    fn tool_smart(&self, input: &Value) -> ToolResult {
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
        let app = self.app.upgrade().ok_or("The app is closing.")?;
        let name = mailbox.name.clone();
        app.update_settings(move |s| s.smart_mailboxes.push(mailbox));
        Ok(json!({"created": name, "gmail_query": query}))
    }

    fn tool_open(self: &Rc<Self>, input: &Value) -> ToolResult {
        let account = self.account_named(&required(input, "account")?)?;
        let thread_id = required(input, "thread_id")?;
        self.open_thread(ThreadSummary {
            account_id: account.id,
            id: thread_id,
            message_count: 1,
            ..ThreadSummary::default()
        });
        Ok(json!({"opened": true}))
    }
}

impl MainWindow {
    async fn tool_categorize(self: &Rc<Self>, input: &Value) -> ToolResult {
        let account = self.account_named(&required(input, "account")?)?;
        let email = required(input, "email")?;
        let key = required(input, "category")?;
        let category = super::categories::Category::from_key(&key)
            .filter(|c| *c != super::categories::Category::All)
            .ok_or_else(|| format!("Unknown category {key}."))?;
        let who = text(input, "name").unwrap_or_else(|| email.clone());
        self.approve(&format!(
            "Move mail from {who} to {} in {}, and add a Gmail rule for their future mail?",
            category.name(),
            account.email
        ))
        .await?;
        self.categorize_sender(account.id, email.clone(), who, None, category);
        Ok(json!({"sender": email, "category": key}))
    }

    async fn tool_dismiss_follow_up(self: &Rc<Self>, input: &Value) -> ToolResult {
        let account = self.account_named(&required(input, "account")?)?;
        let thread_id = required(input, "thread_id")?;
        let now = Local::now().timestamp_millis();
        let key = thread_id.clone();
        self.core
            .write(move |c| mailrs_store::follow_ups::dismiss(c, account.id, &key, now))
            .await
            .map_err(|e| e.to_string())?;
        self.refresh_counts();
        self.reload_list();
        Ok(json!({"dismissed": thread_id}))
    }

    fn tool_hidden_list(&self) -> Value {
        json!({
            "addresses": self.hidden_addresses().iter().map(|h| json!({
                "address": h.address,
                "account": h.account,
                "note": h.note,
                "active": h.active,
            })).collect::<Vec<_>>(),
        })
    }

    async fn tool_hidden_create(self: &Rc<Self>, input: &Value) -> ToolResult {
        let account = self.account_named(&required(input, "account")?)?;
        let note = text(input, "note").unwrap_or_default();
        match self.create_hidden_address(account.id, &note).await {
            Ok(hidden) => {
                gtk::prelude::WidgetExt::clipboard(&self.window).set_text(&hidden.address);
                Ok(json!({"address": hidden.address, "copied": true}))
            }
            Err(err) => Err(self.gmail_error(&account, err)),
        }
    }

    async fn tool_hidden_set(self: &Rc<Self>, input: &Value) -> ToolResult {
        let address = required(input, "address")?;
        let active = flag(input, "active").ok_or("`active` is missing")?;
        let hidden = self
            .hidden_addresses()
            .into_iter()
            .find(|h| h.address.eq_ignore_ascii_case(&address))
            .ok_or_else(|| format!("{address} is not a Hide My Email address."))?;
        let account = self.account_named(&hidden.account)?;
        match self.set_hidden_address_active(&address, active).await {
            Ok(()) => Ok(json!({"address": address, "active": active})),
            Err(err) => Err(self.gmail_error(&account, err)),
        }
    }
}

fn vacation_json(v: &Vacation) -> Value {
    let day = |t: Option<i64>| {
        t.and_then(crate::format::local)
            .map(|d| d.format("%Y-%m-%d").to_string())
    };
    json!({
        "enabled": v.enabled,
        "subject": v.subject,
        "message": v.body,
        "contacts_only": v.contacts_only,
        "first_day": day(v.start),
        // Gmail's end is the midnight after the last day.
        "last_day": day(v.end.map(|t| t - 1)),
    })
}
