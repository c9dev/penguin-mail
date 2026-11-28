//! Rules kept on this computer, for an account whose server runs none.
//! The rules service answers the Rules dialog, Block Sender and the
//! assistant from the store; `crate::rules::RulesEngine` runs them.

use mailrs_domain::translate::gettext;
use mailrs_domain::{AccountId, Filter, MailSet, RemoteMailbox};
use mailrs_store::{Db, local_rules, mailboxes};

use super::RulesService;
use crate::BackendError;

#[derive(Clone)]
pub struct LocalRules {
    db: Db,
    account_id: AccountId,
}

impl LocalRules {
    pub fn new(db: Db, account_id: AccountId) -> LocalRules {
        LocalRules { db, account_id }
    }

    /// The address book kept on this computer beside these rules.
    pub fn contacts(&self) -> super::LocalContacts {
        super::LocalContacts::new(self.db.clone(), self.account_id)
    }
}

fn stored(err: mailrs_store::StoreError) -> BackendError {
    BackendError::Refused(err.to_string())
}

fn read_only() -> BackendError {
    BackendError::Refused(gettext(
        "This rule is read-only, so Penguin Mail leaves it as it is.",
    ))
}

fn new_id() -> String {
    format!("local-{:016x}", rand::random::<u64>())
}

/// How a write to one rule ended, decided inside the write so a rule
/// cannot change between the look and the change.
enum Wrote {
    Done,
    Missing,
    ReadOnly,
}

impl RulesService for LocalRules {
    async fn filters(&self) -> Result<Vec<Filter>, BackendError> {
        let account_id = self.account_id;
        self.db
            .read(move |c| local_rules::list(c, account_id))
            .await
            .map_err(stored)
    }

    async fn create_filter(&self, filter: &Filter) -> Result<Filter, BackendError> {
        let account_id = self.account_id;
        let made = Filter {
            id: Some(filter.id.clone().unwrap_or_else(new_id)),
            ..filter.clone()
        };
        let kept = made.clone();
        let now = crate::now_millis();
        self.db
            .write(move |c| {
                local_rules::add(c, account_id, &kept)?;
                // The first rule runs on mail from now on, not on the
                // Inbox the account already holds.
                local_rules::start_running(c, account_id, now)
            })
            .await
            .map_err(stored)?;
        Ok(made)
    }

    async fn delete_filter(&self, id: &str) -> Result<(), BackendError> {
        let (account_id, id) = (self.account_id, id.to_string());
        let wrote = self
            .db
            .write(move |c| {
                let held = local_rules::list(c, account_id)?;
                Ok(
                    match held.iter().find(|f| f.id.as_deref() == Some(id.as_str())) {
                        None => Wrote::Missing,
                        Some(f) if f.read_only => Wrote::ReadOnly,
                        Some(_) => {
                            local_rules::remove(c, account_id, &id)?;
                            Wrote::Done
                        }
                    },
                )
            })
            .await
            .map_err(stored)?;
        match wrote {
            Wrote::Done => Ok(()),
            Wrote::Missing => Err(BackendError::NotFound),
            Wrote::ReadOnly => Err(read_only()),
        }
    }

    /// The edited rule takes the old one's turn, so a rule that files mail
    /// still runs before the one that archives it. Saving turns a rule
    /// that is off back on, unless a folder it names is still missing
    /// from the server.
    async fn replace_filter(&self, old_id: &str, new: &Filter) -> Result<Filter, BackendError> {
        let (account_id, old_id) = (self.account_id, old_id.to_string());
        let mut made = Filter {
            id: Some(new_id()),
            read_only: false,
            off: false,
            ..new.clone()
        };
        let (wrote, made) = self
            .db
            .write(move |c| {
                let held = local_rules::list(c, account_id)?;
                let wrote = match held
                    .iter()
                    .find(|f| f.id.as_deref() == Some(old_id.as_str()))
                {
                    None => Wrote::Missing,
                    Some(f) if f.read_only => Wrote::ReadOnly,
                    Some(_) => {
                        made.off = names_missing_mailbox(&made, &mailboxes::listed(c, account_id)?);
                        local_rules::replace(c, account_id, &old_id, &made)?;
                        Wrote::Done
                    }
                };
                Ok((wrote, made))
            })
            .await
            .map_err(stored)?;
        match wrote {
            Wrote::Done => Ok(made),
            Wrote::Missing => Err(BackendError::NotFound),
            Wrote::ReadOnly => Err(read_only()),
        }
    }
}

/// Whether `filter` files mail into, or out of, a folder the server does
/// not list.
fn names_missing_mailbox(filter: &Filter, listed: &[RemoteMailbox]) -> bool {
    filter
        .action
        .add
        .iter()
        .chain(&filter.action.remove)
        .any(|set| matches!(set, MailSet::Mailbox(id) if !listed.iter().any(|m| &m.id == id)))
}
