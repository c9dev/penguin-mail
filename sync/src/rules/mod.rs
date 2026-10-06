//! The local rules engine: new Inbox mail through an account's local
//! rules, while Penguin Mail runs. It looks at the Inbox messages past
//! the account's watermark, oldest first and at most [`PASS`] at a time,
//! runs every rule over each in the order they were made, and turns a
//! match into the same mail action the window would run, so it queues
//! and syncs like any other. It stays off the undo stack the window
//! shares. A message is recorded as seen once
//! its rules ran, whatever they did, so it never runs twice; a restart
//! halfway picks up where the last pass stopped.

pub mod forward;
pub mod matching;

use std::sync::Arc;

use mailrs_domain::{AccountId, Filter, MessageMeta, Role, Target};
use mailrs_store::{Db, local_rules};

use crate::{
    Accounts, AccountSync, BackendError, History, MailAction, MailActions, MailBackend, SyncError,
    TriageAction,
};

/// The most messages one pass looks at. A pass after a week away takes
/// the rest on the next call.
pub const PASS: usize = 200;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ran {
    pub looked_at: usize,
    /// Messages a rule changed.
    pub acted_on: Vec<String>,
    /// Messages a rule failed on, with nothing retried: the next change
    /// on the server brings them back to the person's eye.
    pub failed: Vec<String>,
}

pub struct RulesEngine<A: Accounts> {
    accounts: Arc<A>,
    db: Db,
    actions: Arc<MailActions<A>>,
    /// One pass at a time, so the timer and a new-mail event never run
    /// the same message together.
    running: tokio::sync::Mutex<()>,
}

impl<A: Accounts> RulesEngine<A> {
    pub fn new(accounts: Arc<A>, db: Db, actions: Arc<MailActions<A>>) -> Self {
        RulesEngine { accounts, db, actions, running: tokio::sync::Mutex::new(()) }
    }

    pub fn actions(&self) -> &Arc<MailActions<A>> {
        &self.actions
    }

    pub async fn run_due(&self, account_id: AccountId) -> Result<Ran, SyncError> {
        let _one = self.running.lock().await;
        let (rules, due) = self
            .db
            .read(move |c| Ok((local_rules::list(c, account_id)?, local_rules::candidates(c, account_id, PASS)?)))
            .await?;
        let mut ran = Ran { looked_at: due.len(), ..Ran::default() };
        if rules.is_empty() || due.is_empty() {
            return Ok(ran);
        }
        let sync = self.accounts.account(account_id).ok_or(SyncError::UnknownAccount(account_id))?;
        let mut seen = Vec::with_capacity(due.len());
        for meta in due {
            match self.run_one(&sync, &rules, &meta).await {
                Ok(true) => ran.acted_on.push(meta.id.clone()),
                Ok(false) => {}
                Err(err) => {
                    tracing::warn!(account = account_id, %err, "a local rule failed on a message");
                    ran.failed.push(meta.id.clone());
                }
            }
            seen.push((meta.id, meta.date));
        }
        self.db.write(move |c| local_rules::mark_ran(c, account_id, &seen)).await?;
        Ok(ran)
    }

    /// Runs every rule over one message; whether one changed it.
    async fn run_one(&self, sync: &AccountSync, rules: &[Filter], meta: &MessageMeta) -> Result<bool, SyncError> {
        // The body is read once, and only when some rule reads words.
        let body = match rules.iter().any(|r| matching::needs_body(&r.criteria)) {
            true => sync.body(&meta.id).await.ok().and_then(|b| b.text),
            false => None,
        };
        let target = Target {
            account_id: meta.account_id,
            thread_id: meta.thread_id.clone(),
            message_id: Some(meta.id.clone()),
        };
        let mut acted = false;
        for rule in rules {
            if !matching::matches(&rule.criteria, meta, body.as_deref()) || !self.in_inbox(meta).await? {
                continue;
            }
            let action = &rule.action;
            if !action.add.is_empty() || !action.remove.is_empty() {
                let relabel = MailAction::Triage(TriageAction::Relabel {
                    add: action.add.clone(),
                    remove: action.remove.clone(),
                });
                // The window shares this undo stack, and Undo there takes back
                // what the person did last, not a change they never saw.
                let outcome = self.actions.run(std::slice::from_ref(&target), relabel, History::Skip).await;
                if let Some(error) = outcome.first_error() {
                    return Err(SyncError::Backend(BackendError::Refused(error.to_string())));
                }
                acted = true;
            }
            if let Some(to) = &action.forward {
                let from = self.address(meta.account_id).await?;
                // Mail from the account itself, or a forward that came
                // back, would be forwarded again each time it returned.
                if meta.from.as_ref().is_some_and(|f| f.email.eq_ignore_ascii_case(&from)) {
                    continue;
                }
                let raw = sync.raw_message(&meta.id).await?;
                if forward::is_forward(&raw) {
                    continue;
                }
                let message = forward::forwarded(&raw, &from, to, crate::now_millis())?;
                // No copy in Sent: a server-side rule's forward leaves none.
                sync.services().mail.send(&message, None).await?;
                acted = true;
            }
        }
        Ok(acted)
    }

    /// Whether the message is still in the Inbox: a rule before this one
    /// may have moved it on.
    async fn in_inbox(&self, meta: &MessageMeta) -> Result<bool, SyncError> {
        let (account_id, id) = (meta.account_id, meta.id.clone());
        Ok(self
            .db
            .read(move |c| Ok(mailrs_store::messages::by_ids(c, account_id, &[id])?.pop()))
            .await?
            .is_some_and(|m| m.in_role(Role::Inbox)))
    }

    async fn address(&self, account_id: AccountId) -> Result<String, SyncError> {
        Ok(self
            .db
            .read(move |c| mailrs_store::accounts::account(c, account_id))
            .await?
            .ok_or(SyncError::UnknownAccount(account_id))?
            .email)
    }
}
