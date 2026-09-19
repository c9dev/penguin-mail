//! Mail actions: the changes a person or the assistant makes to mail, such as
//! archiving, flagging in a colour, or setting a reminder. The window and the
//! assistant both call this module, so the two cannot drift apart. It keeps
//! the one-level undo and reports what happened instead of showing anything.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use mailrs_domain::{AccountId, EpochMillis, FlagColor, Folder, Target, system_label};
use mailrs_store::reminders::{self, Reminder};
use mailrs_store::{Db, flags, labels, messages, threads};

use crate::{AccountSync, GmailApi, SyncEngine, SyncError, TriageAction};

/// Finds the sync handle of a connected account.
pub trait Accounts: Send + Sync + 'static {
    type Api: GmailApi;

    /// `None` when the account is not syncing.
    fn account(&self, account_id: AccountId) -> Option<Arc<AccountSync<Self::Api>>>;
}

impl<G: GmailApi> Accounts for SyncEngine<G> {
    type Api = G;

    fn account(&self, account_id: AccountId) -> Option<Arc<AccountSync<G>>> {
        SyncEngine::account(self, account_id).ok()
    }
}

/// A change to mail. Each one runs on every target on its own, so one
/// failure leaves the other targets changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailAction {
    /// A label change: archive, trash, junk, read, a label by id, and so on.
    Triage(TriageAction),
    /// Stars the targets and colours the flag, or with `None` takes both off.
    Flag(Option<FlagColor>),
    /// Archives the targets and brings them back to the inbox at `at`.
    Remind { at: EpochMillis },
    /// Drops the targets' reminders and puts them back in the inbox now.
    CancelReminder,
    /// Adds and removes labels by name. Adding a name the account lacks
    /// creates that label; removing one it lacks fails.
    Label {
        add: Vec<String>,
        remove: Vec<String>,
    },
}

/// Whether Undo should reverse this action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum History {
    Record,
    /// Leaves the last recorded action as the one Undo reverses.
    Skip,
}

/// What an action or an undo did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Targets that changed, in the order given.
    pub done: Vec<Target>,
    pub failed: Vec<Failure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub target: Target,
    pub error: String,
}

impl Outcome {
    /// The first failure's error, for a one-line message.
    pub fn first_error(&self) -> Option<&str> {
        self.failed.first().map(|f| f.error.as_str())
    }
}

/// How to reverse the last recorded action.
#[derive(Default)]
struct Undo {
    /// Each changed target with the label change that reverses it.
    relabel: Vec<(Target, TriageAction)>,
    /// Flag colours per message before the action: account, message, colour.
    colors: Vec<(AccountId, String, Option<FlagColor>)>,
    /// Reminders per thread before the action.
    reminders: Vec<(Target, Option<Reminder>)>,
}

pub struct MailActions<A: Accounts> {
    accounts: Arc<A>,
    db: Db,
    last: Mutex<Option<Undo>>,
}

impl<A: Accounts> MailActions<A> {
    pub fn new(accounts: Arc<A>, db: Db) -> Self {
        MailActions {
            accounts,
            db,
            last: Mutex::new(None),
        }
    }

    /// Runs `action` on each target, carrying on past failures. With
    /// `History::Record` and at least one change, Undo reverses it next.
    pub async fn run(&self, targets: &[Target], action: MailAction, history: History) -> Outcome {
        let mut outcome = Outcome::default();
        let mut undo = Undo::default();
        let resolved = self.resolve(targets, &action).await;
        for (target, step) in targets.iter().zip(resolved) {
            match self.run_one(target, &action, step, &mut undo).await {
                Ok(()) => outcome.done.push(target.clone()),
                Err(error) => outcome.failed.push(Failure {
                    target: target.clone(),
                    error,
                }),
            }
        }
        if history == History::Record && !outcome.done.is_empty() {
            *self.lock() = Some(undo);
        }
        outcome
    }

    /// Reverses the last recorded action, once. `None` when there is none.
    pub async fn undo(&self) -> Option<Outcome> {
        let undo = self.lock().take()?;
        let mut outcome = Outcome::default();
        for (target, inverse) in &undo.relabel {
            match self.triage(target, inverse).await {
                Ok(()) => outcome.done.push(target.clone()),
                Err(err) => outcome.failed.push(Failure {
                    target: target.clone(),
                    error: format!("{} failed: {err}", inverse.describe()),
                }),
            }
        }
        let (colors, earlier) = (undo.colors, undo.reminders);
        let restored = self
            .db
            .write(move |c| {
                for (account_id, message_id, color) in &colors {
                    let thread = messages::thread_id_of(c, *account_id, message_id)?;
                    if let Some(thread) = thread {
                        flags::set_color(c, *account_id, &thread, Some(message_id), *color)?;
                    }
                }
                for (target, reminder) in &earlier {
                    reminders::restore(c, target.account_id, &target.thread_id, reminder.as_ref())?;
                }
                Ok(())
            })
            .await;
        if let Err(err) = restored {
            tracing::warn!(error = %err, "could not restore flag colours or reminders");
        }
        Some(outcome)
    }

    /// The id of the label called `name` in the account, ignoring case.
    /// With `create`, makes the label when the account has none by that name.
    pub async fn label_id(
        &self,
        account_id: AccountId,
        name: &str,
        create: bool,
    ) -> Result<String, SyncError> {
        let known = self
            .db
            .read(move |c| labels::list_labels(c, account_id))
            .await?;
        if let Some(label) = known.iter().find(|l| l.name.eq_ignore_ascii_case(name)) {
            return Ok(label.id.clone());
        }
        if !create {
            return Err(SyncError::NoLabel(name.to_string()));
        }
        let sync = self.sync(account_id)?;
        Ok(sync.create_label(name).await?.id)
    }

    /// The targets that `folder` no longer holds, judged by the labels in
    /// the store. A list of a Gmail folder drops these rows.
    pub async fn gone_from(
        &self,
        folder: Folder,
        targets: &[Target],
    ) -> Result<Vec<Target>, SyncError> {
        let targets = targets.to_vec();
        Ok(self
            .db
            .read(move |c| {
                let mut gone = Vec::new();
                for target in targets {
                    let held = messages::thread_messages(c, target.account_id, &target.thread_id)?
                        .iter()
                        .filter(|m| target.message_id.as_ref().is_none_or(|id| &m.id == id))
                        .any(|m| folder.holds(&m.label_ids));
                    if !held {
                        gone.push(target);
                    }
                }
                Ok(gone)
            })
            .await?)
    }

    /// The label change each target gets. Only `Label` differs per target,
    /// since label ids differ per account.
    async fn resolve(
        &self,
        targets: &[Target],
        action: &MailAction,
    ) -> Vec<Result<TriageAction, String>> {
        let (add, remove) = match action {
            MailAction::Triage(triage) => return vec![Ok(triage.clone()); targets.len()],
            MailAction::Flag(Some(_)) => return vec![Ok(TriageAction::Star); targets.len()],
            MailAction::Flag(None) => return vec![Ok(TriageAction::Unstar); targets.len()],
            MailAction::Remind { .. } => return vec![Ok(TriageAction::Archive); targets.len()],
            MailAction::CancelReminder => {
                let inbox = TriageAction::Relabel {
                    add: vec![system_label::INBOX.into()],
                    remove: vec![],
                };
                return vec![Ok(inbox); targets.len()];
            }
            MailAction::Label { add, remove } => (add, remove),
        };
        let mut per_account: BTreeMap<AccountId, Result<TriageAction, String>> = BTreeMap::new();
        for target in targets {
            if per_account.contains_key(&target.account_id) {
                continue;
            }
            let relabel = self.relabel(target.account_id, add, remove).await;
            per_account.insert(target.account_id, relabel);
        }
        targets
            .iter()
            .map(|t| per_account[&t.account_id].clone())
            .collect()
    }

    async fn relabel(
        &self,
        account_id: AccountId,
        add: &[String],
        remove: &[String],
    ) -> Result<TriageAction, String> {
        let mut ids = (Vec::new(), Vec::new());
        for name in add {
            let id = self
                .label_id(account_id, name, true)
                .await
                .map_err(|e| format!("Could not create the label {name}: {e}"))?;
            ids.0.push(id);
        }
        for name in remove {
            let id = self
                .label_id(account_id, name, false)
                .await
                .map_err(|e| e.to_string())?;
            ids.1.push(id);
        }
        Ok(TriageAction::Relabel {
            add: ids.0,
            remove: ids.1,
        })
    }

    async fn run_one(
        &self,
        target: &Target,
        action: &MailAction,
        step: Result<TriageAction, String>,
        undo: &mut Undo,
    ) -> Result<(), String> {
        let triage = step?;
        let failed = |err: SyncError| format!("{} failed: {err}", triage.describe());
        // A new colour on a flagged thread keeps its star on undo.
        let recolor = matches!(action, MailAction::Flag(Some(_)))
            && self.starred(target).await.map_err(failed)?;
        // The label change goes first: it fetches a thread the store lacks,
        // which gives a reminder its subject.
        self.triage(target, &triage).await.map_err(failed)?;
        if !recolor {
            undo.relabel.push((target.clone(), triage.inverse()));
        }
        match action {
            MailAction::Remind { at } => {
                let earlier = self.set_reminder(target, Some(*at)).await.map_err(failed)?;
                undo.reminders.push((target.clone(), earlier));
            }
            MailAction::CancelReminder => {
                let earlier = self.set_reminder(target, None).await.map_err(failed)?;
                undo.reminders.push((target.clone(), earlier));
            }
            MailAction::Flag(color) => {
                let before = self.color(target, *color).await.map_err(failed)?;
                let account_id = target.account_id;
                undo.colors
                    .extend(before.into_iter().map(|(id, c)| (account_id, id, c)));
            }
            MailAction::Triage(_) | MailAction::Label { .. } => {}
        }
        Ok(())
    }

    async fn triage(&self, target: &Target, action: &TriageAction) -> Result<(), SyncError> {
        let sync = self.sync(target.account_id)?;
        match &target.message_id {
            Some(id) => sync.triage_message(&target.thread_id, id, action).await,
            None => sync.triage_thread(&target.thread_id, action).await,
        }
    }

    /// Whether every message the target names is starred. False when the
    /// store holds none of them.
    async fn starred(&self, target: &Target) -> Result<bool, SyncError> {
        let target = target.clone();
        Ok(self
            .db
            .read(move |c| {
                let held: Vec<_> =
                    messages::thread_messages(c, target.account_id, &target.thread_id)?
                        .into_iter()
                        .filter(|m| target.message_id.as_ref().is_none_or(|id| &m.id == id))
                        .collect();
                Ok(!held.is_empty() && held.iter().all(|m| m.has_label(system_label::STARRED)))
            })
            .await?)
    }

    /// Sets the target's reminder to `at`, or removes it with `None`, and
    /// returns the reminder it had. The subject comes from the store.
    async fn set_reminder(
        &self,
        target: &Target,
        at: Option<EpochMillis>,
    ) -> Result<Option<Reminder>, SyncError> {
        let target = target.clone();
        Ok(self
            .db
            .write(move |c| {
                let earlier = reminders::get(c, target.account_id, &target.thread_id)?;
                match at {
                    Some(at) => {
                        let subject = threads::get_thread(c, target.account_id, &target.thread_id)?
                            .map(|t| t.subject)
                            .unwrap_or_default();
                        reminders::set(
                            c,
                            &Reminder {
                                account_id: target.account_id,
                                thread_id: target.thread_id.clone(),
                                subject,
                                remind_at: at,
                            },
                        )?;
                    }
                    None => reminders::remove(c, target.account_id, &target.thread_id)?,
                }
                Ok(earlier)
            })
            .await?)
    }

    /// Colours the target's flag and returns each message's earlier colour.
    async fn color(
        &self,
        target: &Target,
        color: Option<FlagColor>,
    ) -> Result<Vec<(String, Option<FlagColor>)>, SyncError> {
        let target = target.clone();
        Ok(self
            .db
            .write(move |c| {
                let (account_id, thread) = (target.account_id, &target.thread_id);
                let only = target.message_id.as_deref();
                let before = flags::colors(c, account_id, thread, only)?;
                flags::set_color(c, account_id, thread, only, color)?;
                Ok(before)
            })
            .await?)
    }

    fn sync(&self, account_id: AccountId) -> Result<Arc<AccountSync<A::Api>>, SyncError> {
        self.accounts
            .account(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Undo>> {
        self.last.lock().expect("undo lock poisoned")
    }
}
