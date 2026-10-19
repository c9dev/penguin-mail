//! Moving a Gmail account to a new way of signing in, keeping its id and
//! everything stored. Queued work goes out through the old route first;
//! the store changes only once the new sign-in succeeded.

use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::{Account, AccountId, EpochMillis, Provider};
use mailrs_gmail::{Authorized, Granted};
use mailrs_store::Db;
use mailrs_store::accounts::{self, GoogleSignIn, RouteCommit};
use rusqlite::Connection;

use crate::passwords::Secrets;
use crate::sign_in::own_client::OwnClient;
use crate::sign_in::{BrowserSignInError, google_signed_in, other_address};
use crate::{AccountSync, Accounts, Outbox, SyncError, now_millis};

/// How long a move waits for the old route to send what is due and to
/// finish the writes under way.
pub const DRAIN_WAIT: Duration = Duration::from_secs(30);

/// Whether `provider`'s account, signing in as `sign_in` says, has to move
/// before its mail arrives again.
pub fn needs_move(provider: Provider, sign_in: GoogleSignIn) -> bool {
    sign_in.needs_move(provider)
}

#[derive(Debug, thiserror::Error)]
pub enum MoveError {
    /// The browser signed in to another Google account.
    #[error("{}", other_address(.signed_in, .wanted))]
    OtherAddress { signed_in: String, wanted: String },
    #[error(transparent)]
    SignIn(BrowserSignInError),
    #[error(transparent)]
    Sync(#[from] SyncError),
}

/// The account a move stopped, which its drain still sends through.
pub struct Stopped(pub Arc<AccountSync>);

impl Accounts for Stopped {
    fn account(&self, account_id: AccountId) -> Option<Arc<AccountSync>> {
        (account_id == self.0.account_id()).then(|| Arc::clone(&self.0))
    }
}

/// Sends what `outbox` has due at `now`, then waits for the writes `sync`
/// still has under way, for `within` at most. True when both finished in
/// time. A send that fails stays in the outbox for the new route.
pub async fn drain<A: Accounts>(
    sync: &AccountSync,
    outbox: &Outbox<A>,
    now: EpochMillis,
    within: Duration,
) -> bool {
    let both = async {
        if let Err(err) = outbox.send_due(now).await {
            tracing::warn!(
                account = sync.account_id(),
                error = %err,
                "could not send the due mail before the move"
            );
        }
        sync.settle_writes().await;
    };
    tokio::time::timeout(within, both).await.is_ok()
}

/// What a move asks of the engine that runs the account.
pub trait MoveEngine: Send + Sync {
    /// The running account, which the drain still sends through once it
    /// has stopped.
    fn running(&self, account: AccountId) -> Option<Arc<AccountSync>>;
    fn stop(&self, account: AccountId);
    /// Starts `account` again on the sign-in the store names for it now.
    fn start_again(&self, account: &Account) -> impl Future<Output = ()> + Send;
}

/// What a move runs with: the engine, the store, the keyring and the
/// app's outbox, which the move holds while the store changes.
pub struct Mover<'a, E, A: Accounts> {
    pub engine: &'a E,
    pub db: &'a Db,
    pub secrets: &'a Secrets,
    pub outbox: &'a Outbox<A>,
}

impl<E: MoveEngine, A: Accounts> Mover<'_, E, A> {
    /// Moves `account` to `own` once `authorized` came back from the
    /// browser. A sign-in as another address ends the move before anything
    /// stops. Otherwise `committed` runs, since the move can no longer be
    /// cancelled; the account stops, the old route sends what is due and
    /// finishes its writes, the store takes the new sign-in, and the
    /// account starts again, on its old sign-in when the store refused the
    /// new one.
    pub async fn to_own_client(
        &self,
        account: &Account,
        own: &OwnClient,
        authorized: Authorized,
        committed: impl FnOnce(),
    ) -> Result<Account, MoveError> {
        same_address(account, &authorized)?;
        committed();
        let id = account.id;
        let kept = self.engine.running(id);
        self.engine.stop(id);
        if let Some(sync) = kept {
            let draining = Outbox::new(Arc::new(Stopped(Arc::clone(&sync))), self.db.clone());
            if !drain(&sync, &draining, now_millis(), DRAIN_WAIT).await {
                tracing::warn!(
                    account = id,
                    "moving with work still under way on the old sign-in"
                );
            }
        }
        let _held = self.outbox.hold(id).await;
        let moved = to_own_client(self.db, self.secrets, account, own, authorized).await;
        let now = match &moved {
            Ok(moved) => Some(moved.clone()),
            Err(_) => self
                .db
                .read(move |c| accounts::account(c, id))
                .await
                .ok()
                .flatten(),
        };
        self.engine
            .start_again(now.as_ref().unwrap_or(account))
            .await;
        moved
    }
}

/// `OtherAddress` unless `authorized` signed in as `account`, whatever the
/// case.
fn same_address(account: &Account, authorized: &Authorized) -> Result<(), MoveError> {
    if authorized.email.eq_ignore_ascii_case(&account.email) {
        return Ok(());
    }
    Err(MoveError::OtherAddress {
        signed_in: authorized.email.clone(),
        wanted: account.email.clone(),
    })
}

/// Moves `account` to `own`, the client `authorized` signed in through.
/// The address must be the account's, whatever its case. The route stays
/// the API's and the sync state stays, so nothing downloads again. On a
/// failure the client, the token and the consent stay as they were.
pub async fn to_own_client(
    db: &Db,
    secrets: &Secrets,
    account: &Account,
    own: &OwnClient,
    authorized: Authorized,
) -> Result<Account, MoveError> {
    same_address(account, &authorized)?;
    let granted = authorized.granted.as_ref().map(Granted::to_scope);
    google_signed_in(
        db,
        secrets,
        &account.email,
        &authorized.refresh_token,
        now_millis(),
        granted.as_deref(),
        &own.oauth().scopes().join(" "),
        Some(own),
    )
    .await
    .map_err(|err| MoveError::SignIn(err.into()))
}

/// Writes `commit` for `id` and runs `rewrite` in the same transaction,
/// so a failure in either leaves the account as it was.
///
/// The move from the API to IMAP passes the closure that rewrites the
/// stored mail for its new names. That closure must give user `mailboxes`
/// rows their full name as id, keep the system rows `INBOX`, `SENT`,
/// `TRASH`, `SPAM`, `DRAFT`, `STARRED` and `IMPORTANT` as they are, delete
/// the account's `drafts` rows, and leave the `CATEGORY_*` rows and
/// `$muted` alone. It writes no `remote_refs`: the first IMAP sync writes
/// them through `RemoteChange::Located`.
pub async fn commit(
    db: &Db,
    id: AccountId,
    commit: RouteCommit,
    rewrite: impl FnOnce(&Connection) -> mailrs_store::Result<()> + Send + 'static,
) -> Result<(), SyncError> {
    db.write(move |c| {
        accounts::commit_route(c, id, &commit)?;
        rewrite(c)
    })
    .await?;
    Ok(())
}
