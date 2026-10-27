//! What `Core` knows about the desktop's Online Accounts: the list it
//! last read, linking an account to it, and the watch that restarts an
//! account when Online Accounts changes.

use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use futures::StreamExt;
use mailrs_domain::Account;
use mailrs_domain::translate::gettext;
use mailrs_store::{Db, accounts};
use mailrs_sync::goa::{self, GoaAccount, GoaChange, GoaError};
use mailrs_sync::now_millis;
use mailrs_sync::sign_in::goa_link;

use super::{Core, Reach, start_again};
use crate::goa_notice::{self, Notice};

/// How long the watch waits before it opens Online Accounts' change stream
/// again, doubling up to the longest.
const RETRY_FIRST: Duration = Duration::from_secs(5);
const RETRY_LONGEST: Duration = Duration::from_secs(300);

/// What the app last read from Online Accounts.
#[derive(Default)]
pub(super) struct GoaState {
    /// The last list, or why there was none.
    list: Mutex<Option<Result<Vec<GoaAccount>, GoaError>>>,
    /// Reads that failed in a row.
    failures: AtomicU32,
}

impl GoaState {
    fn keep(&self, list: &Result<Vec<GoaAccount>, GoaError>) {
        match list {
            Ok(_) => self.failures.store(0, Ordering::SeqCst),
            Err(_) => {
                self.failures.fetch_add(1, Ordering::SeqCst);
            }
        }
        let copy = match list {
            Ok(list) => Ok(list.clone()),
            Err(GoaError::Unreachable(text)) => Err(GoaError::Unreachable(text.clone())),
            Err(other) => Err(GoaError::Other(other.to_string())),
        };
        *self.list.lock().unwrap_or_else(PoisonError::into_inner) = Some(copy);
    }

    /// The accounts the last good read listed, or `None`.
    fn last_accounts(&self) -> Option<Vec<GoaAccount>> {
        match self
            .list
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()?
        {
            Ok(list) => Some(list.clone()),
            Err(_) => None,
        }
    }
}

/// Every account that reads through Online Accounts, with the id it saved.
async fn linked_accounts(
    db: &Db,
) -> Result<Vec<(Account, Option<String>)>, mailrs_store::StoreError> {
    db.read(|c| {
        let mut linked = Vec::new();
        for account in accounts::list_accounts(c)? {
            let sign_in = accounts::gmail_sign_in(c, account.id)?;
            if let Some(id) = sign_in.goa_id() {
                let id = Some(id.to_string());
                linked.push((account, id));
            }
        }
        Ok(linked)
    })
    .await
}

impl Core {
    /// The Google accounts Online Accounts holds, read now. Answers
    /// `Unreachable` after the daemon fails to answer in time.
    pub async fn goa_accounts(&self) -> Result<Vec<GoaAccount>, GoaError> {
        let (goa, state) = (self.goa.clone(), self.goa_state.clone());
        let read = self
            .runtime
            .spawn(async move {
                let list = goa.google_accounts().await;
                state.keep(&list);
                list
            })
            .await;
        read.unwrap_or_else(|err| Err(GoaError::Other(err.to_string())))
    }

    /// Whether the list last read holds `email`; `None` when it could not
    /// be read or has not been yet.
    pub fn goa_holds(&self, email: &str) -> Option<bool> {
        let list = self
            .goa_state
            .list
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match list.as_ref()? {
            Ok(list) => Some(goa::find(list, None, email).is_some()),
            Err(_) => None,
        }
    }

    /// Links `goa_account` to Penguin Mail: a new address becomes an
    /// account and starts. An address already held is refused.
    pub async fn link_goa(&self, goa_account: GoaAccount) -> Result<Account> {
        if self.demo {
            bail!(gettext("The demo cannot sign in through Online Accounts."));
        }
        let connector = self.connector();
        let (reach, engine, db) = (
            Reach {
                connector: self.connector(),
                demo: self.demo_mail.clone(),
            },
            self.engine
                .current()
                .ok_or_else(|| anyhow!("sync is not running"))?,
            self.db.clone(),
        );
        let account = self
            .call(async move {
                let account = goa_link::link(&connector, &goa_account, now_millis()).await?;
                if let Err(err) = start_again(&reach, &engine, &db, &account, false).await {
                    tracing::warn!(account = account.id, error = %err, "could not start the linked account");
                }
                Ok::<_, anyhow::Error>(account)
            })
            .await?;
        self.finish_google_setup();
        Ok(account)
    }

    /// What the window should say about the accounts that read through
    /// Online Accounts, from the last list.
    pub async fn goa_notices(&self) -> Vec<Notice> {
        let db = self.db.clone();
        let Ok(linked) = self.call(async move { linked_accounts(&db).await }).await else {
            return Vec::new();
        };
        let state = &self.goa_state;
        let list = state.list.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(list) = list.as_ref() else {
            return Vec::new();
        };
        let failures = state.failures.load(Ordering::SeqCst);
        goa_notice::notices(&linked, list, failures)
    }

    /// Records whether an account reads through Online Accounts, and
    /// starts or stops the watch to match.
    pub fn set_goa_linked(self: &Rc<Self>, linked: bool, on_change: impl Fn() + 'static) {
        self.goa_linked.set(linked);
        self.match_goa_watch(on_change);
    }

    /// Follows Online Accounts only on Linux, and only while an account
    /// reads through it, so a person who never uses Online Accounts never
    /// wakes its daemon. When it changes under a linked account, that
    /// account stops and starts again on what it holds now. `on_change`
    /// runs on the main loop after each change and each failure.
    fn match_goa_watch(self: &Rc<Self>, on_change: impl Fn() + 'static) {
        let wanted = cfg!(target_os = "linux") && !self.demo && self.goa_linked.get();
        let mut watch = self.goa_watch.borrow_mut();
        match (wanted, watch.is_some()) {
            (true, false) => *watch = Some(self.watch_goa(on_change)),
            (false, true) => {
                if let Some(handle) = watch.take() {
                    handle.abort();
                }
            }
            _ => {}
        }
    }

    fn watch_goa(self: &Rc<Self>, on_change: impl Fn() + 'static) -> tokio::task::JoinHandle<()> {
        let (tx, rx) = async_channel::unbounded::<()>();
        let (goa, state, db) = (self.goa.clone(), self.goa_state.clone(), self.db.clone());
        let reach = Reach {
            connector: self.connector(),
            demo: None,
        };
        let engine = std::sync::Arc::clone(&self.engine);
        let handle = self.runtime.spawn(async move {
            let restarts = Restarts {
                reach: &reach,
                engine: &engine,
                db: &db,
            };
            let mut wait = RETRY_FIRST;
            // Whether the last read failed. A linked account that started
            // then has no mail, so it starts again once a read answers.
            let mut failed = false;
            loop {
                let list = goa.google_accounts().await;
                state.keep(&list);
                if let Ok(linked) = linked_accounts(&db).await
                    && failed
                    && list.is_ok()
                {
                    restarts.all(&linked).await;
                }
                failed = list.is_err();
                let _ = tx.send(()).await;
                match goa.changes().await {
                    Ok(mut changes) => {
                        wait = RETRY_FIRST;
                        while let Some(change) = changes.next().await {
                            let before = state.last_accounts();
                            let list = goa.google_accounts().await;
                            state.keep(&list);
                            if let Ok(linked) = linked_accounts(&db).await {
                                if failed && list.is_ok() {
                                    restarts.all(&linked).await;
                                } else {
                                    restarts
                                        .for_change(&change, before.as_deref(), &linked)
                                        .await;
                                }
                            }
                            failed = list.is_err();
                            let _ = tx.send(()).await;
                        }
                    }
                    Err(err) => {
                        tracing::debug!(error = %err, "could not follow Online Accounts");
                        state.keep(&Err(err));
                        failed = true;
                        let _ = tx.send(()).await;
                    }
                }
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(RETRY_LONGEST);
            }
        });
        gtk::glib::spawn_future_local(async move {
            while rx.recv().await.is_ok() {
                on_change();
            }
        });
        handle
    }
}

/// What restarting a linked account needs.
struct Restarts<'a> {
    reach: &'a Reach,
    engine: &'a super::RunningEngine,
    db: &'a Db,
}

impl Restarts<'_> {
    /// Stops and starts again every linked account, once Online Accounts
    /// answers after it did not.
    async fn all(&self, linked: &[(Account, Option<String>)]) {
        let Some(engine) = self.engine.current() else {
            return;
        };
        for (account, _) in linked {
            engine.stop_account(account.id);
            if let Err(err) = start_again(self.reach, &engine, self.db, account, false).await {
                tracing::warn!(account = account.id, error = %err, "could not start the account again once Online Accounts answered");
            }
        }
    }

    /// Stops and starts again each linked account `change` is about, when
    /// something that decides how it reads mail changed. `before` is the
    /// list read before the change.
    async fn for_change(
        &self,
        change: &GoaChange,
        before: Option<&[GoaAccount]>,
        linked: &[(Account, Option<String>)],
    ) {
        let Some(engine) = self.engine.current() else {
            return;
        };
        for (account, saved) in linked {
            if !concerns(change, before, account, saved.as_deref()) {
                continue;
            }
            engine.stop_account(account.id);
            if let Err(err) = start_again(self.reach, &engine, self.db, account, false).await {
                tracing::warn!(account = account.id, error = %err, "could not start the account again after Online Accounts changed");
            }
        }
    }
}

/// Whether `change` should restart `account`, linked to `saved`. A change
/// to a property the mail does not depend on, such as a token refresh,
/// does not.
fn concerns(
    change: &GoaChange,
    before: Option<&[GoaAccount]>,
    account: &Account,
    saved: Option<&str>,
) -> bool {
    match change {
        GoaChange::Added(a) => goa::find(std::slice::from_ref(a), saved, &account.email).is_some(),
        GoaChange::Changed(a) => {
            goa::find(std::slice::from_ref(a), saved, &account.email).is_some()
                && !before
                    .and_then(|list| list.iter().find(|old| old.id == a.id))
                    .is_some_and(|old| same_for_mail(old, a))
        }
        GoaChange::Removed { id } => saved == Some(id.as_str()),
    }
}

/// Whether `old` and `new` read mail the same way.
fn same_for_mail(old: &GoaAccount, new: &GoaAccount) -> bool {
    old.identity == new.identity
        && old.mail_disabled == new.mail_disabled
        && old.attention_needed == new.attention_needed
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailrs_domain::{AccountState, Provider};

    fn dana() -> (Account, Option<String>) {
        let account = Account {
            id: 4,
            email: "dana@example.com".into(),
            state: AccountState::Ok,
            provider: Provider::Gmail,
            provider_name: None,
        };
        (account, Some("account_1_0".into()))
    }

    fn asking(attention: bool) -> Vec<GoaAccount> {
        vec![GoaAccount {
            id: "account_1_0".into(),
            identity: "dana@example.com".into(),
            presentation: "dana@example.com".into(),
            mail_disabled: false,
            attention_needed: attention,
        }]
    }

    #[test]
    fn a_token_refresh_restarts_nothing_and_a_mail_switch_does() {
        let (account, saved) = dana();
        let before = asking(false);
        let same = GoaChange::Changed(before[0].clone());
        assert!(!concerns(&same, Some(&before), &account, saved.as_deref()));
        let off = GoaChange::Changed(GoaAccount {
            mail_disabled: true,
            ..before[0].clone()
        });
        assert!(concerns(&off, Some(&before), &account, saved.as_deref()));
        assert!(concerns(&same, None, &account, saved.as_deref()));
    }

    #[test]
    fn failures_count_in_a_row_and_a_good_read_resets_them() {
        let state = GoaState::default();
        let down = || Err(GoaError::Unreachable("x".into()));
        state.keep(&down());
        state.keep(&down());
        assert_eq!(state.failures.load(Ordering::SeqCst), 2);
        state.keep(&Ok(asking(false)));
        assert_eq!(state.failures.load(Ordering::SeqCst), 0);
    }
}
