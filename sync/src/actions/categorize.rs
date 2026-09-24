//! Categorize Sender: moves the mail a sender has already sent into an
//! inbox category, and adds a rule that sorts their future mail there.
//! The window and the assistant both end here, so which conversations
//! count as the sender's and which old rules the new one replaces are
//! decided once.

use std::sync::Arc;

use mailrs_domain::{
    AccountId, Category, Filter, FilterAction, FilterCriteria, MailSet, Target, category,
};
use mailrs_store::threads::{self, ThreadFilter};

use super::{History, MailAction, MailActions, Outcome};
use crate::{AccountSettings, Accounts, Permitted, SyncError, TriageAction};

/// The most conversations from one sender a move reaches. A sender with
/// more than this in the store is a mailing list that has run for years,
/// and the rule sorts the rest as it arrives.
const MOST_MOVED: i64 = 10_000;

/// What Categorize Sender did.
#[derive(Debug)]
pub struct Categorized {
    /// Moving the sender's stored conversations. No undo records it:
    /// putting the old categories back would need each thread's own.
    pub moved: Outcome,
    /// Adding the rule for their future mail. `NeedsPermission` when the
    /// account has not granted the settings permission, which leaves the
    /// moved mail moved.
    pub sorted: Result<Permitted<()>, SyncError>,
}

impl<A: Accounts> MailActions<A> {
    /// Moves every stored conversation from `email` in the account into
    /// `category`, plus the thread `also` when given, and replaces any
    /// category rule for `email` with one that sorts their future mail
    /// there.
    pub async fn categorize_sender(
        &self,
        account_id: AccountId,
        email: &str,
        also: Option<&str>,
        category: Category,
    ) -> Categorized {
        let key = email.to_string();
        let mut ids: Vec<String> = self
            .db
            .read(move |c| {
                let theirs = ThreadFilter::everything()
                    .in_account(account_id)
                    .from_senders(vec![key]);
                Ok(threads::list_threads(c, &theirs, 0, MOST_MOVED)?
                    .into_iter()
                    .map(|t| t.id)
                    .collect())
            })
            .await
            .unwrap_or_else(|err| {
                tracing::warn!(error = %err, "could not list the sender's conversations");
                Vec::new()
            });
        if let Some(thread) = also
            && !ids.iter().any(|id| id == thread)
        {
            ids.push(thread.to_string());
        }
        let targets: Vec<Target> = ids
            .into_iter()
            .map(|id| Target::thread(account_id, id))
            .collect();
        let label = category.id();
        let relabel = TriageAction::Relabel {
            add: vec![MailSet::Category(label.into())],
            remove: category::IDS
                .iter()
                .filter(|c| **c != label)
                .map(|c| MailSet::Category(c.to_string()))
                .collect(),
        };
        let moved = self
            .run(&targets, MailAction::Triage(relabel), History::Skip)
            .await;
        let sorted = self.sort_future_mail(account_id, email, label).await;
        Categorized { moved, sorted }
    }

    /// Replaces any rule that puts `email`'s mail in a category with one
    /// that adds `category`. Account settings keep no state of their own,
    /// so this borrows the same accounts and store rather than holding a
    /// second handle.
    async fn sort_future_mail(
        &self,
        account_id: AccountId,
        email: &str,
        category: &str,
    ) -> Result<Permitted<()>, SyncError> {
        let settings = AccountSettings::new(Arc::clone(&self.accounts), self.db.clone());
        let Permitted::Done(rules) = settings.rules(account_id).await? else {
            return Ok(Permitted::NeedsPermission);
        };
        for old in rules {
            let sorts_sender = old
                .criteria
                .from
                .as_deref()
                .is_some_and(|f| f.eq_ignore_ascii_case(email))
                && !old.action.add.is_empty()
                && old
                    .action
                    .add
                    .iter()
                    .all(|s| matches!(s, MailSet::Category(_)));
            if let (true, Some(id)) = (sorts_sender, old.id.as_deref())
                && settings.delete_rule(account_id, id).await? == Permitted::NeedsPermission
            {
                return Ok(Permitted::NeedsPermission);
            }
        }
        let rule = Filter {
            id: None,
            criteria: FilterCriteria {
                from: Some(email.to_string()),
                ..FilterCriteria::default()
            },
            action: FilterAction {
                add: vec![MailSet::Category(category.into())],
                ..FilterAction::default()
            },
        };
        Ok(match settings.add_rule(account_id, rule).await? {
            Permitted::Done(_) => Permitted::Done(()),
            Permitted::NeedsPermission => Permitted::NeedsPermission,
        })
    }
}
