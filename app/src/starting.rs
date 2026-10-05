//! Starts every account's sync when the engine starts.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::{Account, AccountId, AccountState};

/// What connecting an account came to, short of an error.
pub(crate) enum Connected<S> {
    /// The services to run the account's sync on.
    Ready(S),
    /// Only the person can fix this, by signing in again, so nothing
    /// tries the account again.
    NeedsSignIn,
}

/// How long one connect may take, and the pauses before trying a failed
/// one again.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Waits {
    pub connect: Duration,
    pub first_retry: Duration,
    pub longest_retry: Duration,
}

impl Waits {
    /// A connect reads the keyring and the store and makes no network
    /// call, and each of those steps gives up after `STEP_WAIT`, so a
    /// minute covers every step with room to spare.
    pub(crate) const APP: Waits = Waits {
        connect: Duration::from_secs(60),
        first_retry: Duration::from_secs(60),
        longest_retry: Duration::from_secs(30 * 60),
    };
}

/// Where starting an account reports to: the engine, the store and the
/// sidebar in the app, a record in tests.
pub(crate) trait Starting<S>: Send + Sync + 'static {
    /// Runs the account's sync on `services`.
    fn start(&self, account: AccountId, services: S);
    /// Records the account's state and tells the window.
    fn report(&self, account: AccountId, state: AccountState) -> impl Future<Output = ()> + Send;
    /// Whether the account should still start: it still exists, and the
    /// engine it was meant for still runs.
    fn wanted(&self, account: AccountId) -> impl Future<Output = bool> + Send;
}

/// Connects every account in `accounts`, each in a task of its own, and
/// starts each one that connects. An account whose connect fails, or
/// takes longer than `waits.connect`, is reported as backing off and
/// tried again after a pause that doubles each time. The accounts used
/// to start one after another, and one connect that never finished kept
/// every account after it from syncing.
pub(crate) async fn start_each<S, P, C, F>(accounts: Vec<Account>, connect: C, port: Arc<P>, waits: Waits)
where
    S: Send + 'static,
    P: Starting<S>,
    C: Fn(Account) -> F + Clone + Send + Sync + 'static,
    F: Future<Output = anyhow::Result<Connected<S>>> + Send + 'static,
{
    for account in accounts {
        tokio::spawn(start_one(account, connect.clone(), Arc::clone(&port), waits));
    }
}

async fn start_one<S, P, C, F>(account: Account, connect: C, port: Arc<P>, waits: Waits)
where
    P: Starting<S>,
    C: Fn(Account) -> F,
    F: Future<Output = anyhow::Result<Connected<S>>>,
{
    let mut pause = waits.first_retry;
    loop {
        match tokio::time::timeout(waits.connect, connect(account.clone())).await {
            Ok(Ok(Connected::Ready(services))) => return port.start(account.id, services),
            Ok(Ok(Connected::NeedsSignIn)) => return port.report(account.id, AccountState::NeedsReauth).await,
            Ok(Err(err)) => tracing::warn!(
                account = %account.email,
                error = %err,
                retry_in_secs = pause.as_secs(),
                "could not start syncing; trying again later"
            ),
            Err(_) => tracing::warn!(
                account = %account.email,
                waited_secs = waits.connect.as_secs(),
                retry_in_secs = pause.as_secs(),
                "starting the account took too long; trying again later"
            ),
        }
        port.report(account.id, AccountState::BackingOff).await;
        tokio::time::sleep(pause).await;
        if !port.wanted(account.id).await {
            return;
        }
        pause = (pause * 2).min(waits.longest_retry);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use mailrs_domain::Provider;

    use super::*;

    const SHORT: Waits = Waits {
        connect: Duration::from_millis(50),
        first_retry: Duration::from_millis(10),
        longest_retry: Duration::from_millis(40),
    };

    /// What starting did, in the order it did it.
    #[derive(Default)]
    struct Record {
        started: Mutex<Vec<AccountId>>,
        reported: Mutex<Vec<(AccountId, AccountState)>>,
        unwanted: Mutex<Vec<AccountId>>,
    }

    impl Starting<&'static str> for Record {
        fn start(&self, account: AccountId, _: &'static str) {
            self.started.lock().unwrap().push(account);
        }

        async fn report(&self, account: AccountId, state: AccountState) {
            self.reported.lock().unwrap().push((account, state));
        }

        async fn wanted(&self, account: AccountId) -> bool {
            !self.unwanted.lock().unwrap().contains(&account)
        }
    }

    /// How each account's connects go, one answer per try; once the
    /// answers run out, the connect never finishes.
    #[derive(Clone, Default)]
    struct Script(Arc<Mutex<HashMap<AccountId, Vec<Answer>>>>);

    #[derive(Clone, Copy)]
    enum Answer {
        Ready,
        Hang,
        Fail,
        SignIn,
    }

    impl Script {
        fn with(self, account: AccountId, answers: &[Answer]) -> Self {
            self.0.lock().unwrap().insert(account, answers.to_vec());
            self
        }

        fn tries(&self, account: AccountId) -> usize {
            self.0.lock().unwrap().get(&(account + 1000)).map_or(0, Vec::len)
        }

        fn connect(&self) -> impl Fn(Account) -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<Connected<&'static str>>> + Send>> + Clone + Send + Sync + 'static {
            let script = self.clone();
            move |account: Account| {
                let next = {
                    let mut answers = script.0.lock().unwrap();
                    // Counts the tries under an id no account uses.
                    answers.entry(account.id + 1000).or_default().push(Answer::Ready);
                    let mine = answers.entry(account.id).or_default();
                    (!mine.is_empty()).then(|| mine.remove(0)).unwrap_or(Answer::Hang)
                };
                Box::pin(async move {
                    match next {
                        Answer::Ready => Ok(Connected::Ready("services")),
                        Answer::SignIn => Ok(Connected::NeedsSignIn),
                        Answer::Fail => Err(anyhow::anyhow!("the keyring refused")),
                        Answer::Hang => std::future::pending().await,
                    }
                })
            }
        }
    }

    fn account(id: AccountId) -> Account {
        Account {
            id,
            email: format!("{id}@example.com"),
            state: AccountState::Ok,
            provider: Provider::Imap,
            provider_name: None,
        }
    }

    /// Waits until `done` holds, for at most two seconds.
    async fn until(done: impl Fn() -> bool) {
        let started = tokio::time::Instant::now();
        while !done() {
            assert!(started.elapsed() < Duration::from_secs(2), "the accounts started in time");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn an_account_that_never_connects_does_not_hold_up_the_next() {
        let script = Script::default().with(9, &[Answer::Hang]).with(10, &[Answer::Ready]);
        let record = Arc::new(Record::default());
        tokio::spawn(start_each(vec![account(9), account(10)], script.connect(), Arc::clone(&record), SHORT));
        until(|| record.started.lock().unwrap().contains(&10)).await;
    }

    #[tokio::test]
    async fn an_account_that_takes_too_long_is_reported_and_tried_again() {
        let script = Script::default().with(9, &[Answer::Hang, Answer::Ready]);
        let record = Arc::new(Record::default());
        tokio::spawn(start_each(vec![account(9)], script.connect(), Arc::clone(&record), SHORT));
        until(|| record.started.lock().unwrap().contains(&9)).await;
        assert_eq!(*record.reported.lock().unwrap(), vec![(9, AccountState::BackingOff)]);
    }

    #[tokio::test]
    async fn an_account_that_fails_to_connect_is_tried_again() {
        let script = Script::default().with(9, &[Answer::Fail, Answer::Ready]);
        let record = Arc::new(Record::default());
        tokio::spawn(start_each(vec![account(9)], script.connect(), Arc::clone(&record), SHORT));
        until(|| record.started.lock().unwrap().contains(&9)).await;
        assert_eq!(script.tries(9), 2);
    }

    #[tokio::test]
    async fn an_account_that_needs_to_sign_in_is_reported_once_and_left() {
        let script = Script::default().with(9, &[Answer::SignIn]);
        let record = Arc::new(Record::default());
        start_each(vec![account(9)], script.connect(), Arc::clone(&record), SHORT).await;
        until(|| !record.reported.lock().unwrap().is_empty()).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(*record.reported.lock().unwrap(), vec![(9, AccountState::NeedsReauth)]);
        assert_eq!(script.tries(9), 1);
    }

    #[tokio::test]
    async fn an_account_removed_while_it_waits_is_not_tried_again() {
        let script = Script::default().with(9, &[Answer::Fail, Answer::Ready]);
        let record = Arc::new(Record::default());
        record.unwanted.lock().unwrap().push(9);
        tokio::spawn(start_each(vec![account(9)], script.connect(), Arc::clone(&record), SHORT));
        until(|| script.tries(9) == 1).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(script.tries(9), 1);
        assert!(record.started.lock().unwrap().is_empty());
    }
}
