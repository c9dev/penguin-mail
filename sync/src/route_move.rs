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
    if !authorized.email.eq_ignore_ascii_case(&account.email) {
        return Err(MoveError::OtherAddress {
            signed_in: authorized.email,
            wanted: account.email.clone(),
        });
    }
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
