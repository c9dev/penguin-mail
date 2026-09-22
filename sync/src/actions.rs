//! Mail actions: the changes a person or the assistant makes to mail, such as
//! archiving, flagging in a colour, or setting a reminder. The window and the
//! assistant both call this module, so the two cannot drift apart. It keeps
//! the undo stack and reports what happened instead of showing anything.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{AccountId, EpochMillis, FlagColor, Folder, Target, system_label};
use mailrs_gmail::GmailError;
use mailrs_store::reminders::{self, Reminder};
use mailrs_store::{Db, flags, follow_ups, labels, messages, threads};

use crate::{AccountSync, GmailApi, Permitted, SyncEngine, SyncError, TriageAction};

mod categorize;
mod labelling;
mod returning;

pub use categorize::Categorized;
pub use labelling::NewLabels;
pub use returning::Returned;

/// The folders Undo places mail in, most specific first. Archive is left
/// out: All Mail holds archived mail too, and Undo only needs to know
/// whether mail went to the Junk or the Trash since.
const PLACES: [Folder; 3] = [Folder::Junk, Folder::Trash, Folder::AllMail];

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

/// The id of the label called `name` in `account_id`, ignoring case. With
/// `create`, makes the label when the account has none by that name.
pub(crate) async fn label_id<A: Accounts>(
    accounts: &A,
    db: &Db,
    account_id: AccountId,
    name: &str,
    create: bool,
) -> Result<String, SyncError> {
    let known = db.read(move |c| labels::list_labels(c, account_id)).await?;
    if let Some(label) = known.iter().find(|l| l.name.eq_ignore_ascii_case(name)) {
        return Ok(label.id.clone());
    }
    if !create {
        return Err(SyncError::NoLabel(name.to_string()));
    }
    let sync = accounts
        .account(account_id)
        .ok_or(SyncError::UnknownAccount(account_id))?;
    Ok(sync.create_label(name).await?.id)
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
    /// Mutes the targets, so Gmail archives the replies that follow, or
    /// with `false` unmutes them and puts them back in the inbox.
    Mute { muted: bool },
    /// Adds and removes labels by name. Adding a name the account lacks
    /// creates that label when `create` holds and skips the name when it
    /// does not; removing one it lacks fails. Ask with [`NewLabels`] before
    /// setting `create`, since a new label shows up in Gmail everywhere.
    Label {
        add: Vec<String>,
        remove: Vec<String>,
        create: bool,
    },
    /// Takes the targets out of Follow Up until the person writes in them
    /// again. Only this computer knows about Follow Up, so Gmail hears
    /// nothing of it.
    DismissFollowUp,
}

impl MailAction {
    /// The action in words, for a toast the person reads.
    pub fn describe(&self) -> String {
        match self {
            MailAction::Triage(triage) => triage.describe(),
            MailAction::Flag(Some(_)) => gettext("Flag"),
            MailAction::Flag(None) => gettext("Unflag"),
            MailAction::Remind { .. } => gettext("Remind Me"),
            MailAction::CancelReminder => gettext("Cancel Reminder"),
            MailAction::Mute { muted: true } => TriageAction::Mute.describe(),
            MailAction::Mute { muted: false } => TriageAction::Unmute.describe(),
            MailAction::Label { .. } => gettext("Change labels"),
            MailAction::DismissFollowUp => gettext("Dismiss Follow-Up"),
        }
    }
}

/// Whether Undo should reverse this action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum History {
    /// Puts the action on the undo stack, above the ones before it.
    Record,
    /// Leaves the stack as it was, so Undo still reverses the action
    /// before this one.
    Skip,
}

/// How many actions the undo stack holds. One pass down a screenful of
/// the list is about this many, and an action further back has had long
/// enough for Gmail's own filters, another device, or the person
/// themselves to move the mail again. Recording past the depth drops the
/// oldest, so a long session cannot grow the stack.
pub(crate) const DEPTH: usize = 20;

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

/// How to reverse one recorded action.
struct Undo {
    /// What ran, so an undo can say what it took back.
    action: MailAction,
    /// Each changed target with the label change that reverses it.
    relabel: Vec<Reversal>,
    /// Flag colours per message before the action: account, message, colour.
    colors: Vec<(AccountId, String, Option<FlagColor>)>,
    /// Reminders per thread before the action.
    reminders: Vec<(Target, Option<Reminder>)>,
    /// Threads the action took out of Follow Up.
    follow_ups: Vec<Target>,
}

/// One target the action changed, and how to take that change back.
struct Reversal {
    target: Target,
    inverse: TriageAction,
    /// The folder the action left the target in, which `record` reads
    /// once the action has run. A target somewhere else by the time Undo
    /// comes round is one the world moved under, and Undo leaves it
    /// where it is. `None` when the store held no message of it to place,
    /// and Undo then reverses it as it always did.
    folder: Option<Folder>,
}

impl Undo {
    /// Drops whatever would reverse a change to `target`'s thread. Flag
    /// colours name a message without its thread, and restoring one on a
    /// message the store has dropped does nothing, so they stay.
    fn forget(&mut self, target: &Target) {
        self.relabel.retain(|r| !same_thread(&r.target, target));
        self.reminders.retain(|(t, _)| !same_thread(t, target));
        self.follow_ups.retain(|t| !same_thread(t, target));
    }

    /// Drops whatever would reverse a change in `account_id`.
    fn forget_account(&mut self, account_id: AccountId) {
        self.relabel.retain(|r| r.target.account_id != account_id);
        self.reminders.retain(|(t, _)| t.account_id != account_id);
        self.colors.retain(|(id, _, _)| *id != account_id);
        self.follow_ups.retain(|t| t.account_id != account_id);
    }

    /// Whether anything is left to reverse.
    fn is_empty(&self) -> bool {
        self.relabel.is_empty()
            && self.reminders.is_empty()
            && self.colors.is_empty()
            && self.follow_ups.is_empty()
    }
}

/// What an undo took back: the action it reversed, and what reversing it
/// did to each target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undone {
    pub action: MailAction,
    pub outcome: Outcome,
}

pub struct MailActions<A: Accounts> {
    pub(crate) accounts: Arc<A>,
    pub(crate) db: Db,
    /// The recorded actions, oldest first. Undo takes from the end.
    stack: Mutex<VecDeque<Undo>>,
}

impl<A: Accounts> MailActions<A> {
    pub fn new(accounts: Arc<A>, db: Db) -> Self {
        MailActions {
            accounts,
            db,
            stack: Mutex::new(VecDeque::new()),
        }
    }

    /// Runs `action` on each target, carrying on past failures. With
    /// `History::Record` and at least one change, it goes on top of the
    /// undo stack, and the next undo is the one that reverses it.
    pub async fn run(&self, targets: &[Target], action: MailAction, history: History) -> Outcome {
        let mut outcome = Outcome::default();
        let mut undo = Undo {
            action: action.clone(),
            relabel: Vec::new(),
            colors: Vec::new(),
            reminders: Vec::new(),
            follow_ups: Vec::new(),
        };
        let resolved = self.resolve(targets, &action).await;
        // A new colour on a flagged thread keeps its star on undo. Read
        // that before the star lands, not after.
        let mut recolor: Vec<Result<bool, SyncError>> = targets.iter().map(|_| Ok(false)).collect();
        if matches!(action, MailAction::Flag(Some(_))) {
            for (target, flagged) in targets.iter().zip(&mut recolor) {
                *flagged = self.starred(target).await;
            }
        }
        // The label change goes first: it fetches threads the store lacks,
        // which gives a reminder its subject.
        let triaged = self.triage_grouped(targets, &resolved).await;

        for (index, target) in targets.iter().enumerate() {
            let step = resolved[index].clone();
            let done = self
                .after_triage(
                    target,
                    &action,
                    step,
                    &triaged[index],
                    &recolor[index],
                    &mut undo,
                )
                .await;
            match done {
                Ok(()) => outcome.done.push(target.clone()),
                Err(error) => outcome.failed.push(Failure {
                    target: target.clone(),
                    error,
                }),
            }
        }
        if history == History::Record && !outcome.done.is_empty() {
            self.record(undo).await;
        }
        outcome
    }

    /// Erases the targets from Gmail and from the store. Gmail cannot bring
    /// erased mail back, so no undo records this and the caller asks the
    /// user first. `Permitted::NeedsPermission` means the account has not
    /// granted the delete permission and nothing changed.
    pub async fn erase(&self, targets: &[Target]) -> Result<Permitted<Outcome>, SyncError> {
        let mut outcome = Outcome::default();
        for target in targets {
            let erased = self
                .sync(target.account_id)?
                .erase(&target.thread_id, target.message_id.as_deref())
                .await;
            match erased {
                Ok(()) => outcome.done.push(target.clone()),
                // Gmail refuses before it erases anything, so a refusal on
                // the first target leaves every target as it was.
                Err(SyncError::Gmail(GmailError::MissingScope)) if outcome.done.is_empty() => {
                    return Ok(Permitted::NeedsPermission);
                }
                Err(err) => outcome.failed.push(Failure {
                    target: target.clone(),
                    error: fill(
                        &gettext("Delete Forever failed: {reason}"),
                        &[("reason", &err.to_string())],
                    ),
                }),
            }
        }
        // Undo cannot bring erased mail back, so the stack lets go of the
        // erased threads, and of any entry left with nothing to reverse.
        let mut stack = self.lock();
        for target in &outcome.done {
            for undo in stack.iter_mut() {
                undo.forget(target);
            }
        }
        stack.retain(|undo| !undo.is_empty());
        drop(stack);
        Ok(Permitted::Done(outcome))
    }

    /// Applies each target's label change, one Gmail call per account
    /// rather than one per target, and gives the results back in the order
    /// the targets came in. Targets an account refuses fail together; the
    /// other accounts still go through.
    async fn triage_grouped(
        &self,
        targets: &[Target],
        resolved: &[Result<Option<TriageAction>, String>],
    ) -> Vec<Result<(), String>> {
        let mut results: Vec<Result<(), String>> = vec![Ok(()); targets.len()];
        for (triage, members) in group_by_account(targets, resolved) {
            let batch: Vec<Target> = members.iter().map(|i| targets[*i].clone()).collect();
            let done = match self.sync(batch[0].account_id) {
                Ok(sync) => sync.triage_all(&batch, &triage).await,
                Err(err) => Err(err),
            };
            if let Err(err) = done {
                let message = fill(
                    &gettext("{action} failed: {reason}"),
                    &[("action", &triage.describe()), ("reason", &err.to_string())],
                );
                for index in members {
                    results[index] = Err(message.clone());
                }
            }
        }
        results
    }

    /// The per-target work that follows the label change: the reminder or
    /// the flag colour, and the note Undo needs.
    async fn after_triage(
        &self,
        target: &Target,
        action: &MailAction,
        step: Result<Option<TriageAction>, String>,
        triaged: &Result<(), String>,
        recolor: &Result<bool, SyncError>,
        undo: &mut Undo,
    ) -> Result<(), String> {
        let triage = step?;
        let described = triage
            .as_ref()
            .map_or_else(|| action.describe(), TriageAction::describe);
        let failed = |err: &SyncError| {
            fill(
                &gettext("{action} failed: {reason}"),
                &[("action", &described), ("reason", &err.to_string())],
            )
        };
        let recolor = *recolor.as_ref().map_err(failed)?;
        triaged.clone()?;
        if let (Some(triage), false) = (triage, recolor) {
            undo.relabel.push(Reversal {
                target: target.clone(),
                inverse: triage.inverse(),
                folder: None,
            });
        }
        match action {
            MailAction::Remind { at } => {
                let earlier = self
                    .set_reminder(target, Some(*at))
                    .await
                    .map_err(|e| failed(&e))?;
                undo.reminders.push((target.clone(), earlier));
            }
            MailAction::CancelReminder => {
                let earlier = self
                    .set_reminder(target, None)
                    .await
                    .map_err(|e| failed(&e))?;
                undo.reminders.push((target.clone(), earlier));
            }
            MailAction::Flag(color) => {
                let before = self.color(target, *color).await.map_err(|e| failed(&e))?;
                let account_id = target.account_id;
                undo.colors
                    .extend(before.into_iter().map(|(id, c)| (account_id, id, c)));
            }
            MailAction::DismissFollowUp => {
                self.dismiss_follow_up(target)
                    .await
                    .map_err(|e| failed(&e))?;
                undo.follow_ups.push(target.clone());
            }
            MailAction::Triage(_) | MailAction::Label { .. } | MailAction::Mute { .. } => {}
        }
        Ok(())
    }

    /// Reverses the action on top of the stack and takes it off, leaving
    /// the one before it for the next undo. `None` when the stack is
    /// empty. A target that has left the folder the action put it in is
    /// left there, with a word about it among the failures, so undoing an
    /// archive cannot pull a conversation back out of the trash.
    pub async fn undo(&self) -> Option<Undone> {
        let undo = self.lock().pop_back()?;
        let mut outcome = Outcome::default();
        let mut targets = Vec::new();
        let mut inverses = Vec::new();
        let mut left_alone = Vec::new();
        for (reversal, moved) in undo.relabel.iter().zip(self.moved(&undo.relabel).await) {
            if moved {
                outcome.failed.push(Failure {
                    target: reversal.target.clone(),
                    error: gettext("This mail has moved since, so Undo left it where it is."),
                });
                left_alone.push(reversal.target.clone());
                continue;
            }
            targets.push(reversal.target.clone());
            inverses.push(Ok(Some(reversal.inverse.clone())));
        }
        // Reversing a bulk action goes back in as few calls as it went out.
        for (target, done) in targets
            .iter()
            .zip(self.triage_grouped(&targets, &inverses).await)
        {
            match done {
                Ok(()) => outcome.done.push(target.clone()),
                Err(error) => outcome.failed.push(Failure {
                    target: target.clone(),
                    error,
                }),
            }
        }
        // A flag colour is a mark on this computer rather than a place, so
        // it goes back even on mail that moved. A reminder would put the
        // thread back in the inbox, so it follows the label change.
        let colors = undo.colors;
        let earlier: Vec<(Target, Option<Reminder>)> = undo
            .reminders
            .into_iter()
            .filter(|(target, _)| !left_alone.contains(target))
            .collect();
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
        // Follow Up lives on this computer alone, so taking a dismissal
        // back is the whole undo for these targets, and the person hears
        // whether it worked.
        if !undo.follow_ups.is_empty() {
            let back = undo.follow_ups.clone();
            let restored = self
                .db
                .write(move |c| {
                    for target in &back {
                        follow_ups::restore(c, target.account_id, &target.thread_id)?;
                    }
                    Ok(())
                })
                .await;
            match restored {
                Ok(()) => outcome.done.extend(undo.follow_ups),
                Err(err) => {
                    let error = fill(
                        &gettext("Could not undo: {reason}"),
                        &[("reason", &err.to_string())],
                    );
                    outcome
                        .failed
                        .extend(undo.follow_ups.into_iter().map(|target| Failure {
                            target,
                            error: error.clone(),
                        }));
                }
            }
        }
        Some(Undone {
            action: undo.action,
            outcome,
        })
    }

    /// Drops everything the stack holds about `account_id`, and the
    /// entries left with nothing. An account that has gone has no sync
    /// handle to reverse anything through, and one signed in again comes
    /// back with whatever Gmail made of the mail meanwhile.
    pub fn forget_account(&self, account_id: AccountId) {
        let mut stack = self.lock();
        for undo in stack.iter_mut() {
            undo.forget_account(account_id);
        }
        stack.retain(|undo| !undo.is_empty());
    }

    /// Puts `undo` on the stack, noting where the action left each target
    /// so a later undo can tell whether the world moved under it. The
    /// oldest entry goes when the stack is full.
    async fn record(&self, mut undo: Undo) {
        let targets: Vec<Target> = undo.relabel.iter().map(|r| r.target.clone()).collect();
        let folders = self.folders_of(&targets).await.unwrap_or_else(|err| {
            tracing::warn!(error = %err, "could not read where the action left the mail");
            vec![None; targets.len()]
        });
        for (reversal, folder) in undo.relabel.iter_mut().zip(folders) {
            reversal.folder = folder;
        }
        let mut stack = self.lock();
        while stack.len() >= DEPTH {
            stack.pop_front();
        }
        stack.push_back(undo);
    }

    /// Whether each reversal's target has left the folder the action put
    /// it in, as the store sees it. A target the store could not place
    /// when it was recorded counts as still there.
    async fn moved(&self, relabel: &[Reversal]) -> Vec<bool> {
        let mut moved = vec![false; relabel.len()];
        for folder in PLACES {
            let members: Vec<usize> = relabel
                .iter()
                .enumerate()
                .filter(|(_, r)| r.folder == Some(folder))
                .map(|(index, _)| index)
                .collect();
            if members.is_empty() {
                continue;
            }
            let targets: Vec<Target> = members.iter().map(|i| relabel[*i].target.clone()).collect();
            match self.gone_from(folder, &targets).await {
                Ok(gone) => {
                    for (index, target) in members.iter().zip(&targets) {
                        moved[*index] = gone.contains(target);
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, "could not check where the mail stands now");
                }
            }
        }
        moved
    }

    /// The folder each target stands in, as the store sees it. `None` for
    /// a target the store holds no message of.
    async fn folders_of(&self, targets: &[Target]) -> Result<Vec<Option<Folder>>, SyncError> {
        let targets = targets.to_vec();
        Ok(self
            .db
            .read(move |c| {
                let mut folders = Vec::with_capacity(targets.len());
                for target in &targets {
                    let held = messages::thread_messages(c, target.account_id, &target.thread_id)?;
                    let mine: Vec<_> = held
                        .iter()
                        .filter(|m| target.message_id.as_ref().is_none_or(|id| &m.id == id))
                        .collect();
                    folders.push(
                        PLACES
                            .into_iter()
                            .find(|f| mine.iter().any(|m| f.holds(&m.label_ids))),
                    );
                }
                Ok(folders)
            })
            .await?)
    }

    /// The id of the label called `name` in the account, ignoring case.
    /// With `create`, makes the label when the account has none by that name.
    pub async fn label_id(
        &self,
        account_id: AccountId,
        name: &str,
        create: bool,
    ) -> Result<String, SyncError> {
        label_id(self.accounts.as_ref(), &self.db, account_id, name, create).await
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

    /// The label change each target gets, or `None` for an action that
    /// changes nothing at Gmail. Only `Label` differs per target, since
    /// label ids differ per account.
    async fn resolve(
        &self,
        targets: &[Target],
        action: &MailAction,
    ) -> Vec<Result<Option<TriageAction>, String>> {
        let every = |triage: TriageAction| vec![Ok(Some(triage)); targets.len()];
        let (add, remove, create) = match action {
            MailAction::Triage(triage) => return every(triage.clone()),
            MailAction::Flag(Some(_)) => return every(TriageAction::Star),
            MailAction::Flag(None) => return every(TriageAction::Unstar),
            MailAction::Remind { .. } => return every(TriageAction::Archive),
            MailAction::Mute { muted: true } => return every(TriageAction::Mute),
            MailAction::Mute { muted: false } => return every(TriageAction::Unmute),
            MailAction::CancelReminder => {
                return every(TriageAction::Relabel {
                    add: vec![system_label::INBOX.into()],
                    remove: vec![],
                });
            }
            MailAction::DismissFollowUp => return vec![Ok(None); targets.len()],
            MailAction::Label {
                add,
                remove,
                create,
            } => (add, remove, *create),
        };
        let mut per_account: BTreeMap<AccountId, Result<Option<TriageAction>, String>> =
            BTreeMap::new();
        for target in targets {
            if per_account.contains_key(&target.account_id) {
                continue;
            }
            let relabel = self.relabel(target.account_id, add, remove, create).await;
            per_account.insert(target.account_id, relabel.map(Some));
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
        create: bool,
    ) -> Result<TriageAction, String> {
        let mut ids = (Vec::new(), Vec::new());
        let mut skipped = None;
        for name in add {
            match self.label_id(account_id, name, create).await {
                Ok(id) => ids.0.push(id),
                Err(SyncError::NoLabel(_)) => skipped = Some(name),
                Err(e) => return Err(format!("Could not create the label {name}: {e}")),
            }
        }
        for name in remove {
            let id = self
                .label_id(account_id, name, false)
                .await
                .map_err(|e| e.to_string())?;
            ids.1.push(id);
        }
        // Without new labels, an account that holds none of the names has
        // nothing to change, and its mail did not get what was asked.
        if let (Some(name), true, true) = (skipped, ids.0.is_empty(), ids.1.is_empty()) {
            return Err(format!("This account has no label called {name}."));
        }
        Ok(TriageAction::Relabel {
            add: ids.0,
            remove: ids.1,
        })
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

    /// Stops Follow Up suggesting the target's thread until the person
    /// sends something newer in it.
    async fn dismiss_follow_up(&self, target: &Target) -> Result<(), SyncError> {
        let (account_id, thread) = (target.account_id, target.thread_id.clone());
        let now = crate::now_millis();
        self.db
            .write(move |c| follow_ups::dismiss(c, account_id, &thread, now))
            .await?;
        Ok(())
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

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Undo>> {
        self.stack.lock().expect("undo lock poisoned")
    }
}

/// Whether two targets name the same thread, whichever messages of it
/// they point at.
fn same_thread(one: &Target, other: &Target) -> bool {
    one.account_id == other.account_id && one.thread_id == other.thread_id
}

/// The targets that want the same label change in the same account, as
/// indexes into `targets`. One Gmail call serves each group.
fn group_by_account(
    targets: &[Target],
    resolved: &[Result<Option<TriageAction>, String>],
) -> Vec<(TriageAction, Vec<usize>)> {
    let mut groups: BTreeMap<(AccountId, TriageAction), Vec<usize>> = BTreeMap::new();
    for (index, (target, step)) in targets.iter().zip(resolved).enumerate() {
        let Ok(Some(triage)) = step else { continue };
        groups
            .entry((target.account_id, triage.clone()))
            .or_default()
            .push(index);
    }
    groups
        .into_iter()
        .map(|((_, triage), members)| (triage, members))
        .collect()
}
