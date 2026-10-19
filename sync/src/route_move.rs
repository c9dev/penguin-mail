//! Moving a Gmail account to a new way of signing in, keeping its id, its
//! settings and its outbox. Queued work goes out through the old route
//! first; the store changes only once the new sign-in succeeded. A move
//! that stays on the Gmail API keeps the stored mail. A move from the API
//! to IMAP empties it, and the first IMAP sync lists it all again from
//! Gmail.

use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::{Account, AccountId, EpochMillis, MailRoute, Provider, SignInClient};
use mailrs_gmail::{Authorized, Granted};
use mailrs_store::accounts::{self, GoogleSignIn, RouteCommit};
use mailrs_store::{Db, outbox, servers, switch};
use rusqlite::Connection;

use crate::connect::{STEP_WAIT, read_secret};
use crate::goa::GoaAccount;
use crate::passwords::{PasswordStore, Secrets};
use crate::sign_in::app_password::NewGmailImap;
use crate::sign_in::goa_link::Linked;
use crate::sign_in::own_client::OwnClient;
use crate::sign_in::{BrowserSignInError, google_signed_in, other_address, save_passwords};
use crate::{AccountSync, Accounts, BackendError, Outbox, SyncError, now_millis};

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

/// Rewrites an account's stored mail for the route it moves to, inside the
/// move's own transaction.
pub type RewriteMail = Box<dyn FnOnce(&Connection) -> mailrs_store::Result<()> + Send>;

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
        self.run_move(account, false, || {
            to_own_client(self.db, self.secrets, account, own, authorized)
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

    /// Moves `account` to the Online Accounts account `goa_account`, which
    /// reads its mail `linked`. As [`Mover::to_own_client`], the move ends
    /// before anything stops when the address differs, and the account
    /// keeps its id and its stored mail. A change of route needs
    /// `rewrite`, which renames the stored mail for the new route; with
    /// none, the move is refused before anything stops. The sync starts
    /// over only when the route changes.
    pub async fn to_goa(
        &self,
        account: &Account,
        goa_account: &GoaAccount,
        linked: Linked,
        rewrite: Option<RewriteMail>,
        committed: impl FnOnce(),
    ) -> Result<Account, MoveError> {
        same_email(account, &goa_account.identity)?;
        let id = account.id;
        let (before, client) = self
            .db
            .read(move |c| Ok((accounts::mail_route(c, id)?, accounts::sign_in_client(c, id)?)))
            .await
            .map_err(SyncError::from)?;
        // An app password the move leaves behind goes with its servers.
        let drops_password = before == MailRoute::Imap && client != SignInClient::Goa;
        // An account already on IMAP stays there: Online Accounts' token
        // reads IMAP whatever the probe found, and nothing renames the
        // stored mail back for the API.
        let route = match before {
            MailRoute::Imap => MailRoute::Imap,
            _ => linked.route(),
        };
        let changes = before != route;
        if changes && rewrite.is_none() {
            return Err(MoveError::Sync(SyncError::Backend(
                BackendError::Unsupported,
            )));
        }
        committed();
        let goa_id = goa_account.id.clone();
        self.run_move(account, changes, || async move {
            commit(
                self.db,
                id,
                RouteCommit {
                    route,
                    client: SignInClient::Goa,
                    restart_sync: changes,
                },
                move |c| {
                    accounts::set_goa_id(c, id, Some(&goa_id))?;
                    if drops_password {
                        servers::forget(c, id)?;
                    }
                    match rewrite {
                        Some(rewrite) if changes => rewrite(c),
                        _ => Ok(()),
                    }
                },
            )
            .await?;
            if drops_password {
                self.forget_password(id).await;
            }
            self.db
                .read(move |c| accounts::account(c, id))
                .await
                .map_err(SyncError::from)?
                .ok_or(MoveError::Sync(SyncError::UnknownAccount(id)))
        })
        .await
    }
}

impl<E: MoveEngine, A: Accounts> Mover<'_, E, A> {
    /// Moves `account` to Gmail's IMAP and SMTP, signed in with the app
    /// password `new` holds. The steps are those of [`Mover::to_own_client`]:
    /// a different address ends the move first; then `committed` runs, the
    /// account stops, the old route sends what is due, and the keyring
    /// takes the password. Only then does one transaction write the route,
    /// the servers and the rewritten mail. The account starts again either
    /// way. The calendar token stays.
    pub async fn to_imap(
        &self,
        account: &Account,
        new: NewGmailImap,
        committed: impl FnOnce(),
    ) -> Result<Account, MoveError> {
        same_email(account, &new.address)?;
        let id = account.id;
        let before = self
            .db
            .read(move |c| accounts::mail_route(c, id))
            .await
            .map_err(SyncError::from)?;
        committed();
        self.run_move(account, before != MailRoute::Imap, || {
            self.write_imap(id, before, new)
        })
        .await
    }

    async fn write_imap(
        &self,
        id: AccountId,
        before: MailRoute,
        new: NewGmailImap,
    ) -> Result<Account, MoveError> {
        let NewGmailImap {
            servers: saved,
            password,
            ..
        } = new;
        save_passwords(Arc::clone(&self.secrets.passwords), id, password, None)
            .await
            .map_err(SyncError::from)?;
        let (client, leaves_goa) = self.client_off_goa(id).await?;
        let route = RouteCommit {
            route: MailRoute::Imap,
            client,
            restart_sync: before != MailRoute::Imap,
        };
        commit(self.db, id, route, move |c| {
            servers::save(c, id, &saved)?;
            if leaves_goa {
                accounts::set_goa_id(c, id, None)?;
            }
            // An account already on IMAP (through Online Accounts) keeps
            // its mail.
            match before {
                MailRoute::Imap => Ok(()),
                _ => empty_for_imap(c, id),
            }
        })
        .await?;
        let moved = self
            .db
            .read(move |c| accounts::account(c, id))
            .await
            .map_err(SyncError::from)?;
        moved.ok_or_else(|| {
            SyncError::from(mailrs_store::StoreError::Sqlite(
                rusqlite::Error::QueryReturnedNoRows,
            ))
            .into()
        })
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
                    if let Err(err) = self
                        .db
                        .write(move |c| outbox::set_raw(c, row, &raw))
                        .await
                    {
                        tracing::warn!(account = id, error = %err, "could not keep a waiting message's bytes");
                    }
                }
                Err(err) => {
                    tracing::warn!(account = id, error = %err, "could not read a waiting message's draft before the move");
                }
            }
        }
    }

    /// Deletes the app password `id` kept, once nothing reads it. A
    /// keyring that fails leaves it, unread, and says so in the log.
    async fn forget_password(&self, id: AccountId) {
        let kept = Arc::clone(&self.secrets.passwords);
        let gone = tokio::task::spawn_blocking(move || {
            kept.delete(id)?;
            kept.delete_outgoing(id)
        })
        .await;
        if !matches!(gone, Ok(Ok(()))) {
            tracing::warn!(account = id, "could not delete the app password the move left behind");
        }
    }

    /// The client `id` signs in with once it leaves Online Accounts, and
    /// whether it is on Online Accounts now. An account on another client
    /// keeps it. One that leaves Online Accounts takes its own project
    /// when the keyring keeps one for it, else the client built in.
    async fn client_off_goa(&self, id: AccountId) -> Result<(SignInClient, bool), MoveError> {
        let client = self
            .db
            .read(move |c| accounts::sign_in_client(c, id))
            .await
            .map_err(SyncError::from)?;
        if client != SignInClient::Goa {
            return Ok((client, false));
        }
        let kept = read_secret(Arc::clone(&self.secrets.google_clients), id, STEP_WAIT).await?;
        let client = match kept {
            Some(_) => SignInClient::Own,
            None => SignInClient::BuiltIn,
        };
        Ok((client, true))
    }
}

/// Empties the account's stored mail for Gmail's IMAP, which lists it all
/// again, and names each outbox row by its draft's message id, which is
/// the draft's id on IMAP. The outbox, the local rules and the settings
/// stay.
pub fn empty_for_imap(c: &Connection, id: AccountId) -> mailrs_store::Result<()> {
    outbox::draft_ids_to_message_ids(c, id)?;
    switch::empty_mail(c, id)
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

/// Moves `account` to `own`, the client `authorized` signed in through.
/// The address must be the account's, whatever its case. The route stays
/// the API's and so do the sync state and the stored mail. On a failure
/// the client, the token and the consent stay as they were.
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
/// The move from the API to IMAP passes [`empty_for_imap`], which empties
/// the stored mail for the first IMAP sync to list again.
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
