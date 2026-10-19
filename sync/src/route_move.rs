//! Moving a Gmail account to a new way of signing in, keeping its id and
//! everything stored. Queued work goes out through the old route first;
//! the store changes only once the new sign-in succeeded.

use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::{
    Account, AccountId, EpochMillis, MailRoute, MailboxKind, Provider, RemoteMailbox, SignInClient,
};
use mailrs_gmail::{Authorized, Granted};
use mailrs_store::accounts::{self, GoogleSignIn, RouteCommit};
use mailrs_store::messages::{self, Change};
use mailrs_store::{Db, drafts, mailboxes, servers};
use rusqlite::Connection;

use crate::goa::GoaAccount;
use crate::passwords::Secrets;
use crate::sign_in::app_password::NewGmailImap;
use crate::sign_in::goa_link::Linked;
use crate::sign_in::own_client::OwnClient;
use crate::sign_in::{BrowserSignInError, google_signed_in, other_address, save_passwords};
use crate::{AccountSync, Accounts, BackendError, Outbox, SyncError, now_millis};

/// How long a move waits for the old route to send what is due and to
/// finish the writes under way.
pub const DRAIN_WAIT: Duration = Duration::from_secs(30);

/// Whether X-GM-MSGID in lower-case hex is the Gmail API's message id, as
/// the probe `penguin-mail-cli gmail-ids` checks on a live account. When it
/// is, a move to IMAP keeps every stored message. When it is not,
/// [`rewrite_for_imap`] drops the messages so the first IMAP sync lists
/// them again under the ids IMAP gives.
pub const API_IDS_MATCH: bool = true;

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
        if !goa_account
            .identity
            .trim()
            .eq_ignore_ascii_case(account.email.trim())
        {
            return Err(MoveError::OtherAddress {
                signed_in: goa_account.identity.trim().to_string(),
                wanted: account.email.clone(),
            });
        }
        let id = account.id;
        let before = self
            .db
            .read(move |c| accounts::mail_route(c, id))
            .await
            .map_err(SyncError::from)?;
        let route = linked.route();
        let changes = before != route;
        if changes && rewrite.is_none() {
            return Err(MoveError::Sync(SyncError::Backend(
                BackendError::Unsupported,
            )));
        }
        committed();
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
        let goa_id = goa_account.id.clone();
        let moved = commit(
            self.db,
            id,
            RouteCommit {
                route,
                client: SignInClient::Goa,
                restart_sync: changes,
            },
            move |c| {
                accounts::set_goa_id(c, id, Some(&goa_id))?;
                match rewrite {
                    Some(rewrite) if changes => rewrite(c),
                    _ => Ok(()),
                }
            },
        )
        .await;
        let now = self
            .db
            .read(move |c| accounts::account(c, id))
            .await
            .ok()
            .flatten();
        self.engine
            .start_again(now.as_ref().unwrap_or(account))
            .await;
        moved?;
        now.ok_or_else(|| MoveError::Sync(SyncError::UnknownAccount(id)))
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
        let moved = self.write_imap(id, new).await;
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

    async fn write_imap(&self, id: AccountId, new: NewGmailImap) -> Result<Account, MoveError> {
        let NewGmailImap {
            servers: saved,
            password,
            ..
        } = new;
        save_passwords(Arc::clone(&self.secrets.passwords), id, password, None)
            .await
            .map_err(SyncError::from)?;
        let client = self
            .db
            .read(move |c| accounts::sign_in_client(c, id))
            .await
            .map_err(SyncError::from)?;
        let route = RouteCommit {
            route: MailRoute::Imap,
            client,
            restart_sync: true,
        };
        commit(self.db, id, route, move |c| {
            servers::save(c, id, &saved)?;
            rewrite_for_imap(c, id)
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

/// Renames the account's stored mail for Gmail's IMAP: each user label's
/// row takes its full name as id, so its mail stays filed, and the
/// `drafts` rows go, since IMAP has message ids and no draft ids. System
/// rows, categories and `$muted` stay. With [`API_IDS_MATCH`] false the
/// account's messages go too, and the first IMAP sync lists them again.
pub fn rewrite_for_imap(c: &Connection, id: AccountId) -> mailrs_store::Result<()> {
    let listed = mailboxes::listed(c, id)?;
    for (from, name) in mailboxes::user_ids_and_names(c, id)? {
        let Some(old) = listed.iter().find(|mailbox| mailbox.id == from) else {
            continue;
        };
        let renamed = RemoteMailbox {
            id: name.clone(),
            name,
            kind: MailboxKind::Label,
            role: None,
            color: old.color.clone(),
            hidden: old.hidden,
        };
        mailboxes::rename(c, id, &from, &renamed)?;
    }
    drafts::forget_account(c, id)?;
    if !API_IDS_MATCH {
        let mut stmt = c.prepare("SELECT id FROM messages WHERE account_id = ?1")?;
        let gone: Vec<Change> = stmt
            .query_map([id], |row| row.get::<_, String>(0))?
            .map(|message_id| message_id.map(|message_id| Change::Gone { message_id }))
            .collect::<Result<_, _>>()?;
        messages::apply(c, id, &gone)?;
    }
    Ok(())
}

/// `OtherAddress` unless `authorized` signed in as `account`, whatever the
/// case.
fn same_address(account: &Account, authorized: &Authorized) -> Result<(), MoveError> {
    same_email(account, &authorized.email)
}

fn same_email(account: &Account, signed_in: &str) -> Result<(), MoveError> {
    if signed_in.eq_ignore_ascii_case(&account.email) {
        return Ok(());
    }
    Err(MoveError::OtherAddress {
        signed_in: signed_in.to_string(),
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
