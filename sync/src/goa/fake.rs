//! An Online Accounts that lives in memory, for tests and the demo.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use futures::future::BoxFuture;
use futures::stream::BoxStream;

use super::{GoaAccount, GoaChange, GoaError, GoaToken, OnlineAccounts, bounded};

/// A stand-in for Online Accounts. Its tokens are `t1`, `t2` and so on, in
/// the order they were asked for, each good for 3599 seconds.
#[derive(Default)]
pub struct FakeAccounts {
    accounts: Mutex<Vec<GoaAccount>>,
    watchers: Mutex<Vec<UnboundedSender<GoaChange>>>,
    refuse: Mutex<Option<GoaError>>,
    hanging: AtomicBool,
    tokens: AtomicUsize,
    ensures: AtomicUsize,
}

impl FakeAccounts {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn add(&self, account: GoaAccount) {
        self.accounts.lock().unwrap().push(account.clone());
        self.tell(GoaChange::Added(account));
    }

    pub fn remove(&self, id: &str) {
        self.accounts.lock().unwrap().retain(|a| a.id != id);
        self.tell(GoaChange::Removed { id: id.into() });
    }

    pub fn set(&self, id: &str, change: impl FnOnce(&mut GoaAccount)) {
        let changed = {
            let mut list = self.accounts.lock().unwrap();
            let Some(account) = list.iter_mut().find(|a| a.id == id) else {
                return;
            };
            change(account);
            account.clone()
        };
        self.tell(GoaChange::Changed(changed));
    }

    /// The next `access_token` fails with `error`.
    pub fn refuse_next_token(&self, error: GoaError) {
        *self.refuse.lock().unwrap() = Some(error);
    }

    /// While set, every call waits forever.
    pub fn hang(&self, hanging: bool) {
        self.hanging.store(hanging, Ordering::SeqCst);
    }

    pub fn ensure_calls(&self) -> usize {
        self.ensures.load(Ordering::SeqCst)
    }

    pub fn token_calls(&self) -> usize {
        self.tokens.load(Ordering::SeqCst)
    }

    fn tell(&self, change: GoaChange) {
        self.watchers
            .lock()
            .unwrap()
            .retain(|w| w.unbounded_send(change.clone()).is_ok());
    }

    async fn wait_if_hung(&self) {
        if self.hanging.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
    }

    fn has(&self, id: &str) -> bool {
        self.accounts.lock().unwrap().iter().any(|a| a.id == id)
    }
}

impl OnlineAccounts for FakeAccounts {
    fn google_accounts(&self) -> BoxFuture<'_, Result<Vec<GoaAccount>, GoaError>> {
        Box::pin(bounded(async {
            self.wait_if_hung().await;
            Ok(self.accounts.lock().unwrap().clone())
        }))
    }

    fn access_token<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<GoaToken, GoaError>> {
        Box::pin(bounded(async move {
            self.wait_if_hung().await;
            if let Some(error) = self.refuse.lock().unwrap().take() {
                return Err(error);
            }
            if !self.has(id) {
                return Err(GoaError::NoAccount);
            }
            let n = self.tokens.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(GoaToken {
                token: format!("t{n}"),
                expires_in: Duration::from_secs(3599),
            })
        }))
    }

    fn ensure_credentials<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), GoaError>> {
        Box::pin(bounded(async move {
            self.wait_if_hung().await;
            self.ensures.fetch_add(1, Ordering::SeqCst);
            if self.has(id) {
                Ok(())
            } else {
                Err(GoaError::NoAccount)
            }
        }))
    }

    fn changes(&self) -> BoxFuture<'_, Result<BoxStream<'static, GoaChange>, GoaError>> {
        Box::pin(bounded(async {
            self.wait_if_hung().await;
            let (sender, receiver) = unbounded();
            self.watchers.lock().unwrap().push(sender);
            Ok(receiver.boxed())
        }))
    }
}
