//! The tools for what waits and what can be taken back: messages in Send
//! Later and the Outbox, conversations set aside with Remind Me, muting
//! undone, and the undo stack Ctrl+Z reads. Each goes through the sync
//! module the window's buttons use, `Outbox` for the queue and
//! `MailActions` for the rest, so the assistant and the window cannot
//! disagree about what a queued message or a reminder is.

use mailrs_store::outbox::Queued;
use mailrs_store::reminders::{self, Reminder};
use mailrs_sync::{Outbox, Posted};

use super::*;

/// The subject a question quotes, or a stand-in for a message with none.
fn subject_of(subject: &str) -> String {
    match subject.trim() {
        "" => gettext("(no subject)"),
        subject => subject.to_string(),
    }
}

/// A question about `items`: `one` quotes the subject when there is one
/// item, and `many` counts them otherwise.
fn about<T>(
    items: &[T],
    subject: impl Fn(&T) -> &str,
    one: impl FnOnce(&str) -> String,
    many: impl FnOnce(usize, &str) -> String,
) -> String {
    match items {
        [only] => one(&subject_of(subject(only))),
        _ => many(items.len(), &items.len().to_string()),
    }
}

impl<A: Accounts> Tools<A> {
    /// The outbox over the same accounts and store as every other tool.
    /// It keeps no state of its own, so a fresh one sees what the window's
    /// sees.
    pub(super) fn outbox(&self) -> Arc<Outbox<A>> {
        Arc::new(Outbox::new(
            Arc::clone(&self.modules.accounts),
            self.modules.db.clone(),
        ))
    }

    /// A row of Send Later, the Outbox or Remind Me, as `list_mail` gives
    /// it back: what waits, why, and when it goes or returns. The account,
    /// `thread_id` and `message_id` name it to the tools that change it.
    pub(super) fn waiting_json(&self, mailbox: &str, row: &ThreadSummary) -> Value {
        let mut json = json!({
            "account": self.email_of(row.account_id),
            "thread_id": row.id,
            "message_id": row.message_id,
            "subject": row.subject,
            "waiting": row.snippet,
        });
        match mailbox {
            "reminders" => {
                json["from"] = json!(row.from);
                json["unread"] = json!(row.unread);
                json["returns_at"] = json!(local_text(row.last_message_at));
            }
            _ => {
                json["to"] = json!(row.from);
                json["sends_at"] = json!(local_text(row.last_message_at));
            }
        }
        json
    }

    /// The saved smart mailbox called `name`.
    pub(super) fn smart_named(&self, name: &str) -> Result<Mailbox, String> {
        let saved = self.desk.settings().smart_mailboxes;
        if let Some(found) = saved
            .iter()
            .find(|m| m.name.trim().eq_ignore_ascii_case(name))
        {
            return Ok(Mailbox::Smart(found.clone()));
        }
        let names: Vec<&str> = saved.iter().map(|m| m.name.as_str()).collect();
        Err(match names.is_empty() {
            true => "There are no smart mailboxes.".into(),
            false => format!(
                "There is no smart mailbox called {name}. There are: {}.",
                names.join(", ")
            ),
        })
    }

    // ---- Send Later and the Outbox ---------------------------------------

    /// The waiting messages the call's targets name, in either mailbox.
    async fn queued(&self, input: &Value) -> Result<(Vec<Target>, Vec<Queued>), String> {
        let targets = self.parse_targets(input)?;
        let outbox = self.outbox();
        let given = targets.clone();
        let named = self.call(async move { outbox.named(&given).await }).await?;
        if named.is_empty() {
            return Err("Nothing in Send Later or the Outbox matches those rows. List them again with list_mail.".into());
        }
        Ok((targets, named))
    }

    pub(super) async fn send_now<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let (_, waiting) = self.queued(input).await?;
        let question = about(
            &waiting,
            |m| &m.subject,
            |subject| fill(&gettext("Send “{subject}” now?"), &[("subject", subject)]),
            |n, count| {
                fill_plural(
                    "Send {count} waiting message now?",
                    "Send {count} waiting messages now?",
                    n,
                    &[("count", count)],
                )
            },
        );
        Ok(Plan::ask(question, async move {
            let (mut sent, mut still, mut refused) = (0, Vec::new(), Vec::new());
            for message in waiting {
                let (outbox, id) = (self.outbox(), message.id);
                match self.call(async move { outbox.send_one(id).await }).await {
                    Ok(Posted::Sent(_)) => sent += 1,
                    Ok(Posted::Waiting(_)) => still.push(message.subject),
                    Ok(Posted::Refused(error)) | Err(error) => {
                        refused.push(json!({"subject": message.subject, "error": error}))
                    }
                }
            }
            self.effects.queue_changed();
            if sent == 0 && still.is_empty() {
                let error = refused[0]["error"].as_str().unwrap_or_default();
                return Err(format!("Nothing was sent: {error}"));
            }
            let mut result = json!({"sent": sent});
            if !still.is_empty() {
                result["still_waiting"] = json!(still);
                result["note"] = json!(
                    "Gmail could not be reached for those, so they wait in the Outbox and go on their own."
                );
            }
            if !refused.is_empty() {
                result["refused"] = json!(refused);
            }
            Ok(result)
        }))
    }

    /// Cancels Send Later. What Gmail holds goes back to Drafts; a message
    /// Gmail never had and cannot take now opens in a composer, as Cancel
    /// Send in the window does, since this computer holds its only copy.
    pub(super) async fn cancel_send<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let (targets, named) = self.queued(input).await?;
        let scheduled: Vec<Queued> = named.into_iter().filter(|m| m.problem.is_none()).collect();
        if scheduled.is_empty() {
            return Err("Those messages are in the Outbox, not Send Later. Use send_now or delete_queued for them.".into());
        }
        let question = about(
            &scheduled,
            |m| &m.subject,
            |subject| {
                fill(
                    &gettext("Cancel sending “{subject}”? It goes back to Drafts."),
                    &[("subject", subject)],
                )
            },
            |n, count| {
                fill_plural(
                    "Cancel {count} scheduled message? It goes back to Drafts.",
                    "Cancel {count} scheduled messages? They go back to Drafts.",
                    n,
                    &[("count", count)],
                )
            },
        );
        Ok(Plan::ask(question, async move {
            let outbox = self.outbox();
            let cancelled = self
                .call(async move { outbox.cancel_scheduled(&targets).await })
                .await?;
            let mut reopened = 0;
            for message in &cancelled.unsaved {
                if self.reopen(message).await {
                    reopened += 1;
                }
            }
            self.effects.queue_changed();
            let kept = cancelled.unsaved.len() - reopened;
            let mut result = json!({"in_drafts": cancelled.in_drafts});
            if reopened > 0 {
                result["opened_in_composer"] = json!(reopened);
                result["note"] = json!(
                    "Gmail was out of reach, so those messages are open in a composer for the user to save."
                );
            }
            if kept > 0 {
                result["still_scheduled"] = json!(kept);
                result["note"] = json!(
                    "Gmail is out of reach and some messages could not be reopened, so they stay in Send Later."
                );
            }
            Ok(result)
        }))
    }

    /// Opens a cancelled message in a composer and then drops its row,
    /// so its only copy is never gone before the writer has it. False
    /// leaves it waiting.
    async fn reopen(&self, message: &Queued) -> bool {
        let Ok(draft) = serde_json::from_str::<Draft>(&message.composer) else {
            return false;
        };
        if self.effects.reopen_unsent(draft).is_err() {
            return false;
        }
        let (outbox, id) = (self.outbox(), message.id);
        if let Err(err) = self.call(async move { outbox.drop_one(id).await }).await {
            tracing::warn!(error = %err, "could not drop a cancelled message after reopening it");
        }
        true
    }

    /// Drops messages from the Outbox. A Send Later message has a draft in
    /// Gmail that deleting here would leave behind, so it goes through
    /// `cancel_send` instead.
    pub(super) async fn delete_queued<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let (_, named) = self.queued(input).await?;
        let stuck: Vec<Queued> = named.into_iter().filter(|m| m.problem.is_some()).collect();
        if stuck.is_empty() {
            return Err(
                "Those messages are in Send Later, not the Outbox. Use cancel_send to stop them."
                    .into(),
            );
        }
        let question = about(
            &stuck,
            |m| &m.subject,
            |subject| {
                fill(
                    &gettext("Delete “{subject}” from the Outbox? It will not be sent."),
                    &[("subject", subject)],
                )
            },
            |n, count| {
                fill_plural(
                    "Delete {count} message from the Outbox? It will not be sent.",
                    "Delete {count} messages from the Outbox? They will not be sent.",
                    n,
                    &[("count", count)],
                )
            },
        );
        Ok(Plan::ask(question, async move {
            let mut deleted = 0;
            for message in stuck {
                let (outbox, id) = (self.outbox(), message.id);
                self.call(async move { outbox.drop_one(id).await }).await?;
                deleted += 1;
            }
            self.effects.queue_changed();
            Ok(json!({"deleted": deleted, "undo": "Deleted messages cannot be brought back."}))
        }))
    }

    pub(super) async fn reschedule<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let at = future_instant(&required(input, "at")?)?;
        let (_, named) = self.queued(input).await?;
        let scheduled: Vec<Queued> = named.into_iter().filter(|m| m.problem.is_none()).collect();
        if scheduled.is_empty() {
            return Err("Those messages are in the Outbox, which tries them again on its own. Use send_now to try one now.".into());
        }
        let when = crate::format::future_date(at, Local::now());
        let question = about(
            &scheduled,
            |m| &m.subject,
            |subject| {
                fill(
                    &gettext("Send “{subject}” {when} instead?"),
                    &[("when", &when), ("subject", subject)],
                )
            },
            |n, count| {
                fill_plural(
                    "Send {count} scheduled message {when} instead?",
                    "Send {count} scheduled messages {when} instead?",
                    n,
                    &[("when", &when), ("count", count)],
                )
            },
        );
        Ok(Plan::ask(question, async move {
            let mut moved = 0;
            for message in scheduled {
                let (outbox, id) = (self.outbox(), message.id);
                let done = self
                    .call(async move { outbox.reschedule(id, at).await })
                    .await?;
                moved += usize::from(done.is_some());
            }
            self.effects.queue_changed();
            if moved == 0 {
                return Err("Those messages are no longer in Send Later.".into());
            }
            Ok(json!({"rescheduled": moved, "sends_at": local_text(at)}))
        }))
    }

    // ---- Remind Me -------------------------------------------------------

    pub(super) async fn list_reminders(&self) -> ToolResult {
        let waiting = self.read(reminders::list).await?;
        Ok(json!({
            "reminders": waiting.iter().map(|r| json!({
                "account": self.email_of(r.account_id),
                "thread_id": r.thread_id,
                "subject": r.subject,
                "returns_at": local_text(r.remind_at),
                "returns": crate::format::future_date(r.remind_at, Local::now()),
            })).collect::<Vec<_>>(),
        }))
    }

    /// The reminders the call's targets name. Every target must have one,
    /// so a question never names a conversation the change would skip.
    async fn reminders_named(&self, input: &Value) -> Result<Vec<(Target, Reminder)>, String> {
        let targets = self.parse_targets(input)?;
        let asked = targets.clone();
        let found = self
            .read(move |c| {
                asked
                    .iter()
                    .map(|t| reminders::get(c, t.account_id, &t.thread_id))
                    .collect::<mailrs_store::Result<Vec<_>>>()
            })
            .await?;
        targets
            .into_iter()
            .zip(found)
            .map(|(target, reminder)| match reminder {
                Some(reminder) => Ok((target, reminder)),
                None => Err(format!(
                    "The conversation {} has no reminder. list_reminders shows the ones there are.",
                    target.thread_id
                )),
            })
            .collect()
    }

    /// Drops reminders and puts the conversations back in the inbox now.
    /// The window keeps this off the undo stack, and so does the tool, so
    /// Ctrl+Z still reverses the action before it.
    pub(super) async fn cancel_reminder<'a>(
        &'a self,
        input: &'a Value,
    ) -> Result<Plan<'a>, String> {
        let named = self.reminders_named(input).await?;
        let question = about(
            &named,
            |(_, r)| &r.subject,
            |subject| {
                fill(
                    &gettext("Cancel the reminder on “{subject}” and put it back in the Inbox?"),
                    &[("subject", subject)],
                )
            },
            |n, count| {
                fill_plural(
                    "Cancel {count} reminder and put its conversation back in the Inbox?",
                    "Cancel {count} reminders and put their conversations back in the Inbox?",
                    n,
                    &[("count", count)],
                )
            },
        );
        let targets: Vec<Target> = named.into_iter().map(|(t, _)| t).collect();
        Ok(Plan::ask(question, async move {
            let action = MailAction::CancelReminder;
            let mail = Arc::clone(&self.modules.mail);
            let (given, asked) = (targets.clone(), action.clone());
            let outcome = self
                .away(async move { mail.run(&given, asked, History::Skip).await })
                .await?;
            self.effects.mail_changed(&action, &outcome);
            let mut result = report(&outcome)?;
            result["undo"] = json!("The conversations are back in the inbox.");
            Ok(result)
        }))
    }

    pub(super) async fn change_reminder<'a>(
        &'a self,
        input: &'a Value,
    ) -> Result<Plan<'a>, String> {
        let at = future_instant(&required(input, "at")?)?;
        let named = self.reminders_named(input).await?;
        let when = crate::format::future_date(at, Local::now());
        let question = about(
            &named,
            |(_, r)| &r.subject,
            |subject| {
                fill(
                    &gettext("Bring “{subject}” back {when} instead?"),
                    &[("when", &when), ("subject", subject)],
                )
            },
            |n, count| {
                fill_plural(
                    "Bring {count} conversation back {when} instead?",
                    "Bring {count} conversations back {when} instead?",
                    n,
                    &[("when", &when), ("count", count)],
                )
            },
        );
        let targets: Vec<Target> = named.into_iter().map(|(t, _)| t).collect();
        Ok(Plan::ask(question, async move {
            let mut result = report(&self.act(targets, MailAction::Remind { at }).await)?;
            result["returns"] = json!(when);
            Ok(result)
        }))
    }

    // ---- Taking back -----------------------------------------------------

    pub(super) async fn unmute(&self, input: &Value) -> ToolResult {
        let targets = self.parse_targets(input)?;
        report(&self.act(targets, MailAction::Mute { muted: false }).await)
    }

    /// Takes back the newest action on the undo stack, as Ctrl+Z does,
    /// whether the window or the assistant took it.
    pub(super) async fn undo<'a>(&'a self, _input: &'a Value) -> Result<Plan<'a>, String> {
        let newest = self
            .modules
            .mail
            .newest()
            .ok_or("There is nothing to undo.")?;
        let question = fill(
            &gettext("Undo “{action}”?"),
            &[("action", &newest.describe())],
        );
        Ok(Plan::ask(question, async move {
            let mail = Arc::clone(&self.modules.mail);
            let undone = self
                .away(async move { mail.undo().await })
                .await?
                .ok_or("There is nothing to undo.")?;
            self.effects.undone(&undone.outcome);
            let outcome = &undone.outcome;
            if let (true, Some(error)) = (outcome.done.is_empty(), outcome.first_error()) {
                return Err(error.to_string());
            }
            let mut result = json!({
                "undone": undone.action.describe(),
                "done": outcome.done.len(),
            });
            if !outcome.failed.is_empty() {
                result["failed"] = outcome
                    .failed
                    .iter()
                    .map(|f| json!({"thread_id": f.target.thread_id, "error": f.error}))
                    .collect();
            }
            Ok(result)
        }))
    }
}
