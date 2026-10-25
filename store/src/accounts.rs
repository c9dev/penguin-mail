//! Accounts and their sync cursors.

use mailrs_domain::{
    Account, AccountId, AccountState, EpochMillis, GmailSignIn, MailRoute, Provider, RemoveSetting,
    SignInClient,
};
use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, StoreError};

/// Where sync left off for one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncCursor {
    /// Where the account's mail backend left its feed of changes, in the
    /// backend's own words. `None` until the first bootstrap.
    pub state: Option<String>,
    /// Page token for the next window page.
    pub backfill_cursor: Option<String>,
    pub backfill_done: bool,
    /// The backfill running now lengthens the window over mail the store
    /// already holds: it keeps what it holds rather than fetching it again.
    pub backfill_extends: bool,
    pub sync_gen: i64,
}

/// Adds an account, or returns the existing id for that email.
pub fn insert_account(conn: &Connection, email: &str, now: EpochMillis) -> Result<AccountId> {
    conn.execute(
        "INSERT OR IGNORE INTO accounts (email, added_at) VALUES (?1, ?2)",
        params![email, now],
    )?;
    Ok(conn.query_row(
        "SELECT id FROM accounts WHERE email = ?1",
        params![email],
        |row| row.get(0),
    )?)
}

/// Every account the store holds, oldest first. A row this build cannot
/// read is left out and logged: one a newer build added for a provider
/// or a state this one does not know. Going back to an older build then
/// costs that account alone, and the others keep syncing.
pub fn list_accounts(conn: &Connection) -> Result<Vec<Account>> {
    let mut stmt =
        conn.prepare("SELECT id, email, state, provider, provider_name FROM accounts ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        ))
    })?;
    let mut accounts = Vec::new();
    for row in rows {
        let (id, email, state, provider, provider_name) = row?;
        match to_account(id, email, state, provider, provider_name) {
            Ok(account) => accounts.push(account),
            Err(StoreError::Corrupt { column, value }) => {
                tracing::warn!(
                    account = id,
                    column,
                    value,
                    "left out an account this build cannot read"
                );
            }
            Err(err) => return Err(err),
        }
    }
    Ok(accounts)
}

/// The account by id, such as `CalendarCopy` reads to name a Google
/// account's primary calendar by its own address when the calendar list
/// is out of reach.
pub fn account(conn: &Connection, id: AccountId) -> Result<Option<Account>> {
    let row: Option<(AccountId, String, String, String, Option<String>)> = conn
        .query_row(
            "SELECT id, email, state, provider, provider_name FROM accounts WHERE id = ?1",
            params![id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    row.map(|(id, email, state, provider, provider_name)| {
        to_account(id, email, state, provider, provider_name)
    })
    .transpose()
}

pub fn account_by_email(conn: &Connection, email: &str) -> Result<Option<Account>> {
    let row: Option<(AccountId, String, String, String, Option<String>)> = conn
        .query_row(
            "SELECT id, email, state, provider, provider_name FROM accounts WHERE email = ?1",
            params![email],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    row.map(|(id, email, state, provider, provider_name)| {
        to_account(id, email, state, provider, provider_name)
    })
    .transpose()
}

/// Adds an IMAP account that `provider_name` serves, or finds the IMAP
/// account already here for `email` and records the name it signed in
/// under this time. `None` when the address belongs to an account
/// another provider serves: that account's mail and sign-in stay as
/// they are.
pub fn insert_imap_account(
    conn: &Connection,
    email: &str,
    provider_name: &str,
    now: EpochMillis,
) -> Result<Option<AccountId>> {
    if let Some(existing) = account_by_email(conn, email)? {
        if existing.provider != Provider::Imap {
            return Ok(None);
        }
        conn.execute(
            "UPDATE accounts SET provider_name = ?2 WHERE id = ?1",
            params![existing.id, provider_name],
        )?;
        return Ok(Some(existing.id));
    }
    conn.execute(
        "INSERT INTO accounts (email, added_at, provider, provider_name) VALUES (?1, ?2, ?3, ?4)",
        params![email, now, Provider::Imap.as_str(), provider_name],
    )?;
    Ok(Some(conn.last_insert_rowid()))
}

/// Adds a calendar account under `name`. `None` when an account holds
/// the name already, a calendar account too: it stays as it is, and the
/// caller decides whether it is the login being signed in to. So `Some`
/// is always a row this call added. The name stands in the column a mail
/// account keeps its address in, which keeps it unique.
pub fn insert_caldav_account(
    conn: &Connection,
    name: &str,
    now: EpochMillis,
) -> Result<Option<AccountId>> {
    if account_by_email(conn, name)?.is_some() {
        return Ok(None);
    }
    conn.execute(
        "INSERT INTO accounts (email, added_at, provider) VALUES (?1, ?2, ?3)",
        params![name, now, Provider::CalDav.as_str()],
    )?;
    Ok(Some(conn.last_insert_rowid()))
}

/// Adds a POP3 account with its removal setting, or finds the one already
/// here for the address and takes the new `provider_name`. `None` when an
/// account of another provider holds the address. Signing in again keeps
/// the setting the account had; Server Settings changes it through
/// [`set_pop3_remove`].
pub fn insert_pop3_account(
    conn: &Connection,
    email: &str,
    provider_name: &str,
    remove: RemoveSetting,
    now: EpochMillis,
) -> Result<Option<AccountId>> {
    if let Some(existing) = account_by_email(conn, email)? {
        if existing.provider != Provider::Pop3 {
            return Ok(None);
        }
        conn.execute(
            "UPDATE accounts SET provider_name = ?2 WHERE id = ?1",
            params![existing.id, provider_name],
        )?;
        return Ok(Some(existing.id));
    }
    let (mode, days) = remove.columns();
    conn.execute(
        "INSERT INTO accounts (email, added_at, provider, provider_name, pop3_remove, pop3_remove_days) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![email, now, Provider::Pop3.as_str(), provider_name, mode, days],
    )?;
    Ok(Some(conn.last_insert_rowid()))
}

/// Adds a JMAP account that `provider_name` serves, or finds the JMAP
/// account already here for the address and takes the new name. `None`
/// when an account of another provider holds the address.
pub fn insert_jmap_account(
    conn: &Connection,
    email: &str,
    provider_name: &str,
    now: EpochMillis,
) -> Result<Option<AccountId>> {
    if let Some(existing) = account_by_email(conn, email)? {
        if existing.provider != Provider::Jmap {
            return Ok(None);
        }
        conn.execute(
            "UPDATE accounts SET provider_name = ?2 WHERE id = ?1",
            params![existing.id, provider_name],
        )?;
        return Ok(Some(existing.id));
    }
    conn.execute(
        "INSERT INTO accounts (email, added_at, provider, provider_name) VALUES (?1, ?2, ?3, ?4)",
        params![email, now, Provider::Jmap.as_str(), provider_name],
    )?;
    Ok(Some(conn.last_insert_rowid()))
}

/// What a POP3 account does with mail on the server. Any other account
/// answers `Never`, the column's default.
pub fn pop3_remove(conn: &Connection, id: AccountId) -> Result<RemoveSetting> {
    let (mode, days): (String, Option<i64>) = conn.query_row(
        "SELECT pop3_remove, pop3_remove_days FROM accounts WHERE id = ?1",
        params![id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let days = days.and_then(|d| u32::try_from(d).ok());
    RemoveSetting::from_columns(&mode, days).ok_or(StoreError::Corrupt {
        column: "accounts.pop3_remove",
        value: mode,
    })
}

pub fn set_pop3_remove(conn: &Connection, id: AccountId, setting: RemoveSetting) -> Result<()> {
    let (mode, days) = setting.columns();
    conn.execute(
        "UPDATE accounts SET pop3_remove = ?2, pop3_remove_days = ?3 WHERE id = ?1",
        params![id, mode, days],
    )?;
    Ok(())
}

/// Adds a Microsoft account, or finds the one already here for the address
/// and takes the new `provider_name` ("Outlook" or "Microsoft 365").
/// `None` when an account of another provider holds the address.
pub fn insert_microsoft_account(
    conn: &Connection,
    email: &str,
    provider_name: &str,
    now: EpochMillis,
) -> Result<Option<AccountId>> {
    if let Some(existing) = account_by_email(conn, email)? {
        if existing.provider != Provider::Microsoft {
            return Ok(None);
        }
        conn.execute(
            "UPDATE accounts SET provider_name = ?2 WHERE id = ?1",
            params![existing.id, provider_name],
        )?;
        return Ok(Some(existing.id));
    }
    conn.execute(
        "INSERT INTO accounts (email, added_at, provider, provider_name) VALUES (?1, ?2, ?3, ?4)",
        params![email, now, Provider::Microsoft.as_str(), provider_name],
    )?;
    Ok(Some(conn.last_insert_rowid()))
}

/// Deletes the account and, through foreign keys, all of its mail.
pub fn delete_account(conn: &Connection, id: AccountId) -> Result<()> {
    conn.execute("DELETE FROM accounts WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn set_state(conn: &Connection, id: AccountId, state: AccountState) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET state = ?2 WHERE id = ?1",
        params![id, state.as_str()],
    )?;
    Ok(())
}

/// How the Gmail account `id` signs in. A row of another provider reads as
/// the columns' defaults, the built-in client on the API.
pub fn gmail_sign_in(conn: &Connection, id: AccountId) -> Result<GmailSignIn> {
    let (client, route, goa_id) = conn.query_row(
        "SELECT oauth_client, mail_route, goa_id FROM accounts WHERE id = ?1",
        params![id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        },
    )?;
    decode_sign_in(client, route, goa_id)
}

/// The value the `oauth_client` column holds for an account on Online
/// Accounts. It names no Google client: that account's Google token is the
/// built-in client's.
const ON_ONLINE_ACCOUNTS: &str = "goa";

/// Reads the three columns that together say how a Gmail account signs
/// in. An Online Accounts account reads its mail over IMAP whatever the
/// route column says, as it always has.
fn decode_sign_in(client: String, route: String, goa_id: Option<String>) -> Result<GmailSignIn> {
    if client == ON_ONLINE_ACCOUNTS {
        let goa_id = goa_id.ok_or_else(|| StoreError::Corrupt {
            column: "accounts.goa_id",
            value: String::new(),
        })?;
        return Ok(GmailSignIn::OnlineAccounts { goa_id });
    }
    let client: SignInClient = client.parse().map_err(|_| StoreError::Corrupt {
        column: "accounts.oauth_client",
        value: client.clone(),
    })?;
    let route: MailRoute = route.parse().map_err(|_| StoreError::Corrupt {
        column: "accounts.mail_route",
        value: route.clone(),
    })?;
    Ok(match (route, client) {
        (MailRoute::GmailApi, SignInClient::BuiltIn) => GmailSignIn::BuiltInApi,
        (MailRoute::GmailApi, SignInClient::Own) => GmailSignIn::OwnProject,
        (MailRoute::Imap, google) => GmailSignIn::AppPassword { google },
    })
}

/// The three columns `sign_in` is kept in: the client, the route and the
/// Online Accounts id.
fn encode_sign_in(sign_in: &GmailSignIn) -> (&'static str, &'static str, Option<&str>) {
    let route = sign_in.route().as_str();
    match sign_in {
        GmailSignIn::OnlineAccounts { goa_id } => (ON_ONLINE_ACCOUNTS, route, Some(goa_id)),
        other => (other.client().as_str(), route, None),
    }
}

/// Records how `id` signs in, all three columns in one statement, so a
/// failure leaves none of them changed. The sync state stays.
pub fn set_gmail_sign_in(conn: &Connection, id: AccountId, sign_in: &GmailSignIn) -> Result<()> {
    let (client, route, goa_id) = encode_sign_in(sign_in);
    conn.execute(
        "UPDATE accounts SET oauth_client = ?2, mail_route = ?3, goa_id = ?4 WHERE id = ?1",
        params![id, client, route, goa_id],
    )?;
    Ok(())
}

/// Points the Online Accounts account `id` links to at `new`, while it
/// still links to `old`.
pub fn relink_goa(conn: &Connection, id: AccountId, old: &str, new: &str) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET goa_id = ?3 \
         WHERE id = ?1 AND oauth_client = ?4 AND goa_id = ?2",
        params![id, old, new, ON_ONLINE_ACCOUNTS],
    )?;
    Ok(())
}

/// The sign-in of every Gmail account, read in one query. A row this
/// build cannot read, such as one a newer build wrote, is left out and
/// logged, as [`list_accounts`] leaves it out, so it stops no other
/// account.
pub fn google_sign_ins(conn: &Connection) -> Result<HashMap<AccountId, GmailSignIn>> {
    let mut stmt = conn
        .prepare("SELECT id, oauth_client, mail_route, goa_id FROM accounts WHERE provider = ?1")?;
    let rows = stmt.query_map(params![Provider::Gmail.as_str()], |row| {
        Ok((
            row.get::<_, AccountId>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut all = HashMap::new();
    for row in rows {
        let (id, client, route, goa_id) = row?;
        match decode_sign_in(client, route, goa_id) {
            Ok(sign_in) => {
                all.insert(id, sign_in);
            }
            Err(StoreError::Corrupt { column, value }) => {
                tracing::warn!(
                    account = id,
                    column,
                    value,
                    "left out a sign-in this build cannot read"
                );
            }
            Err(err) => return Err(err),
        }
    }
    Ok(all)
}

/// When the account's inbox was last checked against Gmail's, if ever.
pub fn checked_at(conn: &Connection, id: AccountId) -> Result<Option<EpochMillis>> {
    Ok(conn
        .query_row(
            "SELECT checked_at FROM accounts WHERE id = ?1",
            params![id],
            |row| row.get::<_, Option<EpochMillis>>(0),
        )
        .optional()?
        .flatten())
}

pub fn set_checked_at(conn: &Connection, id: AccountId, at: EpochMillis) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET checked_at = ?2 WHERE id = ?1",
        params![id, at],
    )?;
    Ok(())
}

pub fn sync_cursor(conn: &Connection, id: AccountId) -> Result<SyncCursor> {
    Ok(conn.query_row(
        "SELECT sync_state, backfill_cursor, backfill_done, sync_gen, backfill_extends \
         FROM accounts WHERE id = ?1",
        params![id],
        |row| {
            Ok(SyncCursor {
                state: row.get(0)?,
                backfill_cursor: row.get(1)?,
                backfill_done: row.get(2)?,
                sync_gen: row.get(3)?,
                backfill_extends: row.get(4)?,
            })
        },
    )?)
}

pub fn set_sync_state(conn: &Connection, id: AccountId, state: &str) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET sync_state = ?2 WHERE id = ?1",
        params![id, state],
    )?;
    Ok(())
}

pub fn set_backfill(
    conn: &Connection,
    id: AccountId,
    cursor: Option<&str>,
    done: bool,
) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET backfill_cursor = ?2, backfill_done = ?3, \
         backfill_extends = backfill_extends AND NOT ?3 WHERE id = ?1",
        params![id, cursor, done],
    )?;
    Ok(())
}

/// Starts the backfill again from the top in the same generation, to
/// bring in mail a longer window now reaches; see
/// [`SyncCursor::backfill_extends`].
pub fn extend_backfill(conn: &Connection, id: AccountId) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET backfill_cursor = NULL, backfill_done = 0, backfill_extends = 1 \
         WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// The sync window, in days, the account's stored mail was loaded for: 0
/// for all mail, `None` before an account first ran under a version that
/// records it.
pub fn window_days(conn: &Connection, id: AccountId) -> Result<Option<i64>> {
    Ok(conn.query_row(
        "SELECT window_days FROM accounts WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?)
}

pub fn set_window_days(conn: &Connection, id: AccountId, days: i64) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET window_days = ?2 WHERE id = ?1",
        params![id, days],
    )?;
    Ok(())
}

/// Starts a new sync generation from the sync state `state`, clears the
/// backfill cursor, and returns the new generation number.
pub fn start_generation(conn: &Connection, id: AccountId, state: &str) -> Result<i64> {
    Ok(conn.query_row(
        "UPDATE accounts SET sync_state = ?2, backfill_cursor = NULL, backfill_done = 0, \
         backfill_extends = 0, sync_gen = sync_gen + 1 WHERE id = ?1 RETURNING sync_gen",
        params![id, state],
        |row| row.get(0),
    )?)
}

/// What Google has told this account about its own consent. `granted` is
/// the space-joined scopes the last token refresh or exchange reported;
/// `asked` is what the last sign-in or Grant Access asked Google for.
/// Either is `None` while it is not known: `granted` until the account's
/// next refresh, `asked` for an account that has never been through a
/// consent asking for every scope.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Consent {
    pub granted: Option<String>,
    pub asked: Option<String>,
}

pub fn consent(conn: &Connection, id: AccountId) -> Result<Consent> {
    Ok(conn.query_row(
        "SELECT granted_scopes, asked_scopes FROM accounts WHERE id = ?1",
        params![id],
        |row| {
            Ok(Consent {
                granted: row.get(0)?,
                asked: row.get(1)?,
            })
        },
    )?)
}

/// Every account's consent, keyed by id. The window's Grant Access
/// banner reads this once for every account rather than once per
/// account.
pub fn all_consent(conn: &Connection) -> Result<std::collections::HashMap<AccountId, Consent>> {
    let mut stmt = conn.prepare("SELECT id, granted_scopes, asked_scopes FROM accounts")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, AccountId>(0)?,
            Consent {
                granted: row.get(1)?,
                asked: row.get(2)?,
            },
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Records the scopes a token refresh or exchange reported for `id`.
pub fn set_granted(conn: &Connection, id: AccountId, scope: &str) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET granted_scopes = ?2 WHERE id = ?1",
        params![id, scope],
    )?;
    Ok(())
}

/// Puts back a consent read with [`consent`], an unknown half included.
pub fn set_consent(conn: &Connection, id: AccountId, consent: &Consent) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET granted_scopes = ?2, asked_scopes = ?3 WHERE id = ?1",
        params![id, consent.granted, consent.asked],
    )?;
    Ok(())
}

/// Records the scopes the last consent for `id` asked Google for.
pub fn set_asked(conn: &Connection, id: AccountId, scope: &str) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET asked_scopes = ?2 WHERE id = ?1",
        params![id, scope],
    )?;
    Ok(())
}

fn to_account(
    id: AccountId,
    email: String,
    state: String,
    provider: String,
    provider_name: Option<String>,
) -> Result<Account> {
    let state = state
        .parse::<AccountState>()
        .map_err(|_| StoreError::Corrupt {
            column: "accounts.state",
            value: state.clone(),
        })?;
    let provider = provider
        .parse::<Provider>()
        .map_err(|_| StoreError::Corrupt {
            column: "accounts.provider",
            value: provider.clone(),
        })?;
    Ok(Account {
        id,
        email,
        state,
        provider,
        provider_name,
    })
}

#[cfg(test)]
mod tests {
    use crate::open_in_memory;

    use super::*;

    #[test]
    fn an_account_this_build_cannot_read_leaves_the_others_listed() {
        let conn = open_in_memory().unwrap();
        let kept = insert_account(&conn, "me@example.com", 0).unwrap();
        let newer = insert_account(&conn, "you@example.com", 0).unwrap();
        conn.execute(
            "UPDATE accounts SET provider = 'carrier-pigeon' WHERE id = ?1",
            params![newer],
        )
        .unwrap();
        let listed = list_accounts(&conn).unwrap();
        assert_eq!(listed.iter().map(|a| a.id).collect::<Vec<_>>(), [kept]);
    }

    #[test]
    fn consent_round_trips() {
        let conn = open_in_memory().unwrap();
        let id = insert_account(&conn, "me@example.com", 0).unwrap();
        assert_eq!(consent(&conn, id).unwrap(), Consent::default());
        set_granted(&conn, id, "gmail.modify settings").unwrap();
        set_asked(&conn, id, "gmail.modify settings calendar.events").unwrap();
        assert_eq!(
            consent(&conn, id).unwrap(),
            Consent {
                granted: Some("gmail.modify settings".into()),
                asked: Some("gmail.modify settings calendar.events".into()),
            }
        );
    }

    #[test]
    fn all_consent_reads_every_account_in_one_query() {
        let conn = open_in_memory().unwrap();
        let a = insert_account(&conn, "a@example.com", 0).unwrap();
        let b = insert_account(&conn, "b@example.com", 0).unwrap();
        set_granted(&conn, a, "gmail.modify").unwrap();
        let every = all_consent(&conn).unwrap();
        assert_eq!(every.len(), 2);
        assert_eq!(every[&a].granted.as_deref(), Some("gmail.modify"));
        assert_eq!(every[&b], Consent::default());
    }
}
