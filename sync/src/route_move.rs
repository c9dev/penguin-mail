//! Moving a Gmail account to a new way of signing in, keeping its id, its
//! settings and its outbox. Queued work goes out through the old route
//! first; the store changes only once the new sign-in succeeded. A move
//! that stays on the Gmail API keeps the stored mail. A move from the API
//! to IMAP empties it, and the first IMAP sync lists it all again from
//! Gmail.

use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::{Account, AccountId, EpochMillis, GmailSignIn};
use mailrs_gmail::{Authorized, Granted};
use mailrs_store::{Db, accounts, outbox};

use crate::goa::GoaAccount;
use crate::passwords::Secrets;
use crate::sign_in::app_password::NewGmailImap;
use crate::sign_in::change::{
    self, Change, ChangeError, NewPassword, NewToken, Whose, change_sign_in,
};
use crate::sign_in::own_client::OwnClient;
use crate::sign_in::{BrowserSignInError, SignInError, other_address};
use crate::{AccountSync, Accounts, Outbox, SyncError, now_millis};

pub use crate::sign_in::change::empty_for_imap;

/// How long a move waits for the old route to send what is due and to
/// finish the writes under way.
pub const DRAIN_WAIT: Duration = Duration::from_secs(30);

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

impl From<ChangeError> for MoveError {
    fn from(err: ChangeError) -> Self {
        match err {
            ChangeError::Token(reason) => {
                MoveError::SignIn(BrowserSignInError::Kept(SignInError::Token(reason)))
            }
            ChangeError::Password(err) => MoveError::Sync(SyncError::from(err)),
            ChangeError::Store(err) => MoveError::Sync(SyncError::from(err)),
        }
    }
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
    /// new one. From IMAP the move empties the stored mail, which the API
    /// lists again, ends any Online Accounts link and drops an app
    /// password.
    pub async fn to_own_client(
        &self,
        account: &Account,
        own: &OwnClient,
        authorized: Authorized,
        committed: impl FnOnce(),
    ) -> Result<Account, MoveError> {
        same_address(account, &authorized)?;
        let from = sign_in_of(self.db, account.id).await?;
        committed();
        let change = own_project(account, own, authorized);
        self.run_change(account, from, change).await
    }

    /// Moves `account` to the Online Accounts account `goa_account`, whose
    /// token reads Gmail's IMAP and SMTP. As [`Mover::to_own_client`], the
    /// move ends before anything stops when the address differs, and the
    /// account keeps its id. A move off the Gmail API empties the stored
    /// mail for IMAP ([`empty_for_imap`]) and starts the sync over; one
    /// already on IMAP keeps both. An app password it leaves behind goes
    /// with its servers.
    pub async fn to_goa(
        &self,
        account: &Account,
        goa_account: &GoaAccount,
        committed: impl FnOnce(),
    ) -> Result<Account, MoveError> {
        same_email(account, &goa_account.identity)?;
        let from = sign_in_of(self.db, account.id).await?;
        committed();
        let to = GmailSignIn::OnlineAccounts {
            goa_id: goa_account.id.clone(),
        };
        self.run_change(account, from, Change::to(to)).await
    }

    /// Moves `account` to Gmail's IMAP and SMTP, signed in with the app
    /// password `new` holds. The steps are those of [`Mover::to_own_client`]:
    /// a different address ends the move first; then `committed` runs, the
    /// account stops, the old route sends what is due, and the keyring
    /// takes the password. Only then does one transaction write the sign-in,
    /// the servers and the emptied mail. The account starts again either
    /// way. The calendar token stays, and the client it refreshes through
    /// is the one [`change::with_app_password`] names.
    pub async fn to_imap(
        &self,
        account: &Account,
        new: NewGmailImap,
        committed: impl FnOnce(),
    ) -> Result<Account, MoveError> {
        same_email(account, &new.address)?;
        let from = sign_in_of(self.db, account.id).await?;
        committed();
        let to = change::with_app_password(self.secrets, Some(account.id), Some(&from)).await?;
        let change = Change {
            password: Some(NewPassword {
                servers: new.servers,
                password: new.password,
            }),
            ..Change::to(to)
        };
        self.run_change(account, from, change).await
    }

    /// Runs `change` as a move from `from`: bringing the bytes of waiting
    /// messages along when the route changes.
    async fn run_change(
        &self,
        account: &Account,
        from: GmailSignIn,
        change: Change,
    ) -> Result<Account, MoveError> {
        let id = account.id;
        let changes_route = from.route() != change.to.route();
        self.run_move(account, changes_route, || async move {
            change_sign_in(self.db, self.secrets, Whose::Held { id, from }, change).await?;
            account_of(self.db, id).await
        })
        .await
    }

    /// Stops `account`, lets the old route send what is due and finish its
    /// writes, holds the outbox while `write` changes the store, and
    /// starts the account again: as `write` left it, or as the store
    /// holds it when `write` failed. With `bring_bytes`, a move off the
    /// Gmail API, each waiting message whose bytes are only in a Gmail
    /// draft has them copied into the outbox first, so it goes out over
    /// SMTP from them.
    async fn run_move<F, Fut>(
        &self,
        account: &Account,
        bring_bytes: bool,
        write: F,
    ) -> Result<Account, MoveError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Account, MoveError>>,
    {
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
            if bring_bytes {
                self.bring_bytes(&sync, id).await;
            }
        }
        let _held = self.outbox.hold(id).await;
        let moved = write().await;
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

impl<E, A: Accounts> Mover<'_, E, A> {
    /// Copies into the outbox the bytes of each of `id`'s waiting messages
    /// that the old route keeps only in a Gmail draft. One that cannot be
    /// read stays as it is; after the move it is sent from the draft, or
    /// asks the person to check Sent, and never counts as sent unread.
    async fn bring_bytes(&self, sync: &AccountSync, id: AccountId) {
        let waiting = match self.db.read(move |c| outbox::bytes_on_server(c, id)).await {
            Ok(waiting) => waiting,
            Err(err) => {
                tracing::warn!(account = id, error = %err, "could not list the outbox before the move");
                return;
            }
        };
        for (row, message_id) in waiting {
            match sync.raw(&message_id).await {
                Ok(raw) => {
                    if let Err(err) = self.db.write(move |c| outbox::set_raw(c, row, &raw)).await {
                        tracing::warn!(account = id, error = %err, "could not keep a waiting message's bytes");
                    }
                }
                Err(err) => {
                    tracing::warn!(account = id, error = %err, "could not read a waiting message's draft before the move");
                }
            }
        }
    }
}

/// How `id` signs in now.
async fn sign_in_of(db: &Db, id: AccountId) -> Result<GmailSignIn, MoveError> {
    Ok(db
        .read(move |c| accounts::gmail_sign_in(c, id))
        .await
        .map_err(SyncError::from)?)
}

/// The account `id` as the store holds it now.
async fn account_of(db: &Db, id: AccountId) -> Result<Account, MoveError> {
    db.read(move |c| accounts::account(c, id))
        .await
        .map_err(SyncError::from)?
        .ok_or(MoveError::Sync(SyncError::UnknownAccount(id)))
}

/// The change to `own`'s project with the token `authorized` holds.
fn own_project(account: &Account, own: &OwnClient, authorized: Authorized) -> Change {
    let token = NewToken {
        email: account.email.clone(),
        refresh_token: authorized.refresh_token,
        own: Some(own.clone()),
        granted: authorized.granted.as_ref().map(Granted::to_scope),
        asked: own.oauth().scopes().join(" "),
    };
    Change {
        token: Some(token),
        ..Change::to(GmailSignIn::OwnProject)
    }
}

/// `OtherAddress` unless `authorized` signed in as `account`, whatever the
/// case.
fn same_address(account: &Account, authorized: &Authorized) -> Result<(), MoveError> {
    same_email(account, &authorized.email)
}

/// `OtherAddress` unless `signed_in` is `account`'s address, trimmed and
/// whatever the case.
fn same_email(account: &Account, signed_in: &str) -> Result<(), MoveError> {
    let signed_in = signed_in.trim();
    if signed_in.eq_ignore_ascii_case(account.email.trim()) {
        return Ok(());
    }
    Err(MoveError::OtherAddress {
        signed_in: signed_in.to_string(),
        wanted: account.email.clone(),
    })
}

/// Moves `account` to `own`, the client `authorized` signed in through,
/// with no engine to stop: the store and the keyring change as in
/// [`Mover::to_own_client`]. The address must be the account's, whatever
/// its case. On a failure the client, the token and the consent stay as
/// they were.
pub async fn to_own_client(
    db: &Db,
    secrets: &Secrets,
    account: &Account,
    own: &OwnClient,
    authorized: Authorized,
) -> Result<Account, MoveError> {
    same_address(account, &authorized)?;
    let id = account.id;
    let from = sign_in_of(db, id).await?;
    let change = own_project(account, own, authorized);
    change_sign_in(db, secrets, Whose::Held { id, from }, change).await?;
    account_of(db, id).await
}
