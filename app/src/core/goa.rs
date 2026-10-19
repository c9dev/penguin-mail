//! What `Core` knows about the desktop's Online Accounts: the list it
//! last read, linking an account to it, moving an account onto it, and
//! the watch that restarts an account when Online Accounts changes.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use futures::StreamExt;
use mailrs_domain::translate::gettext;
use mailrs_domain::{Account, AccountId, SignInClient};
use mailrs_store::{Db, accounts};
use mailrs_sync::goa::{self, GoaAccount, GoaChange, GoaError};
use mailrs_sync::now_millis;
use mailrs_sync::route_move;
use mailrs_sync::sign_in::goa_link;

use super::{Core, MoveLeases, MovePort, Reach, start_again};
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
    /// The accounts Online Accounts has asked to sign in again, and those
    /// it asked twice, since the app started.
    asked: Mutex<(HashSet<AccountId>, HashSet<AccountId>)>,
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

    /// Counts a sign-in request for each linked account that Online
    /// Accounts shows as needing one, and forgets the ones it no longer
    /// does.
    fn count_attention(&self, linked: &[(Account, Option<String>)], list: &[GoaAccount]) {
        let mut asked = self.asked.lock().unwrap_or_else(PoisonError::into_inner);
        for (account, saved) in linked {
            let needs = goa::find(list, saved.as_deref(), &account.email)
                .is_some_and(|found| found.attention_needed);
            if !needs {
                asked.0.remove(&account.id);
                asked.1.remove(&account.id);
            } else if !asked.0.insert(account.id) {
                asked.1.insert(account.id);
            }
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
            if accounts::sign_in_client(c, account.id)? == SignInClient::Goa {
                let id = accounts::goa_id(c, account.id)?;
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

    /// Links `goa_account` to Penguin Mail. A new address becomes an
    /// account and starts; an address already held moves to Online
    /// Accounts in place.
    pub async fn link_goa(&self, goa_account: GoaAccount) -> Result<Account> {
        if self.demo {
            bail!(gettext("The demo cannot sign in through Online Accounts."));
        }
        let address = goa_account.identity.trim().to_lowercase();
        let lookup = self.db.clone();
        let held = self
            .call(async move {
                lookup
                    .read(move |c| accounts::account_by_email(c, &address))
                    .await
            })
            .await?;
        if let Some(held) = held.filter(|a| a.provider == mailrs_domain::Provider::Gmail) {
            return self.move_goa(held, goa_account).await;
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

    /// Moves `account` to sign in through `goa_account`, which holds its
    /// address.
    pub async fn move_to_goa(&self, account: Account, goa_account: GoaAccount) -> Result<Account> {
        self.move_goa(account, goa_account).await
    }

    async fn move_goa(&self, account: Account, goa_account: GoaAccount) -> Result<Account> {
        if self.demo {
            bail!(gettext("The demo cannot switch accounts."));
        }
        // Refused as adding an account through Online Accounts is, before
        // anything stops.
        goa_link::usable(&goa_account)?;
        let lease = self.move_leases.lease(account.id).ok_or_else(|| {
            anyhow!(gettext(
                "This account is already moving to its new sign-in."
            ))
        })?;
        let engine = self
            .engine
            .current()
            .ok_or_else(|| anyhow!("sync is not running"))?;
        let (db, secrets, outbox) = (
            self.db.clone(),
            self.secrets.clone(),
            std::sync::Arc::clone(&self.outbox),
        );
        let reach = Reach {
            connector: self.connector(),
            demo: self.demo_mail.clone(),
        };
        let goa = self.goa.clone();
        let moved = self
            .call(async move {
                // Held until the move ends, even when the dialog has gone.
                let _lease = lease;
                goa_link::check_token(&goa, &goa_account).await?;
                let port = MovePort {
                    engine,
                    reach,
                    db: db.clone(),
                };
                let mover = route_move::Mover {
                    engine: &port,
                    db: &db,
                    secrets: &secrets,
                    outbox: &outbox,
                };
                let id = account.id;
                // The move from the API to IMAP empties the stored mail,
                // which the first IMAP sync lists again. An account already
                // on IMAP stays there and keeps it.
                let rewrite: route_move::RewriteMail =
                    Box::new(move |c| route_move::empty_for_imap(c, id));
                let moved = mover
                    .to_goa(&account, &goa_account, Some(rewrite), || ())
                    .await?;
                Ok::<_, anyhow::Error>(moved)
            })
            .await?;
        self.finish_google_setup();
        Ok(moved)
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
        let twice = state
            .asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .clone();
        let failures = state.failures.load(Ordering::SeqCst);
        goa_notice::notices(&linked, list, failures, &twice)
    }

    /// Follows Online Accounts for as long as the app runs, once. When it
    /// changes under a linked account, that account stops and starts again
    /// on what it holds now. `on_change` runs on the main loop after each
    /// change and each failure.
    pub fn watch_goa(self: &Rc<Self>, on_change: impl Fn() + 'static) {
        if self.demo || self.goa_watching.replace(true) {
            return;
        }
        let (tx, rx) = async_channel::unbounded::<()>();
        let (goa, state, db) = (self.goa.clone(), self.goa_state.clone(), self.db.clone());
        let reach = Reach {
            connector: self.connector(),
            demo: None,
        };
        let engine = std::sync::Arc::clone(&self.engine);
        let leases = self.move_leases.clone();
        self.runtime.spawn(async move {
            let restarts = Restarts {
                reach: &reach,
                engine: &engine,
                db: &db,
                leases: &leases,
            };
            let mut wait = RETRY_FIRST;
            // Whether the last read failed. A linked account that started
            // then has no mail, so it starts again once a read answers.
            let mut failed = false;
            loop {
                let list = goa.google_accounts().await;
                state.keep(&list);
                if let Ok(linked) = linked_accounts(&db).await {
                    if let Ok(list) = &list {
                        state.count_attention(&linked, list);
                    }
                    if failed && list.is_ok() {
                        restarts.all(&linked).await;
                    }
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
                                if let Ok(list) = &list {
                                    state.count_attention(&linked, list);
                                }
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
    }
}

/// What restarting a linked account needs. An account a move holds is
/// left alone: the move starts it again itself once it ends.
struct Restarts<'a> {
    reach: &'a Reach,
    engine: &'a super::RunningEngine,
    db: &'a Db,
    leases: &'a MoveLeases,
}

impl Restarts<'_> {
    /// Stops and starts again every linked account, once Online Accounts
    /// answers after it did not.
    async fn all(&self, linked: &[(Account, Option<String>)]) {
        let Some(engine) = self.engine.current() else {
            return;
        };
        for (account, _) in linked {
            if self.leases.is_held(account.id) {
                continue;
            }
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
            if !concerns(change, before, account, saved.as_deref())
                || self.leases.is_held(account.id)
            {
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
    fn a_second_request_to_sign_in_is_counted_and_a_cleared_one_is_forgotten() {
        let state = GoaState::default();
        state.count_attention(&[dana()], &asking(true));
        assert!(state.asked.lock().unwrap().1.is_empty());
        state.count_attention(&[dana()], &asking(true));
        assert!(state.asked.lock().unwrap().1.contains(&4));
        state.count_attention(&[dana()], &asking(false));
        assert!(state.asked.lock().unwrap().0.is_empty());
        assert!(state.asked.lock().unwrap().1.is_empty());
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
