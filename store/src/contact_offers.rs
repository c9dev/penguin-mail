//! What the person said when Penguin Mail offered to save a message's new
//! recipients to the sending account's contacts. An address they turned
//! down, or one already saved, is not offered again for that account.

use std::collections::HashSet;

use mailrs_domain::{AccountId, EpochMillis};
use rusqlite::{Connection, params};

use crate::Result;

/// How an offer ended for one address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// The offer went away without Save.
    Declined,
    /// Save made a contact of it.
    Saved,
}

/// Records `answer` for each of `emails` on `account_id`. A later answer
/// replaces an earlier one.
pub fn record(
    conn: &Connection,
    account_id: AccountId,
    emails: &[String],
    answer: Answer,
    at: EpochMillis,
) -> Result<()> {
    let code = match answer {
        Answer::Declined => "declined",
        Answer::Saved => "saved",
    };
    let mut stmt = conn.prepare(
        "INSERT OR REPLACE INTO contact_offers (account_id, email, answer, answered_at) \
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for email in emails {
        let key = email.trim().to_lowercase();
        if !key.is_empty() {
            stmt.execute(params![account_id, key, code, at])?;
        }
    }
    Ok(())
}

/// The lower-case addresses `account_id` has answered an offer for.
pub fn answered(conn: &Connection, account_id: AccountId) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT email FROM contact_offers WHERE account_id = ?1")?;
    let rows = stmt.query_map([account_id], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{accounts, open_in_memory};

    #[test]
    fn an_answer_is_kept_per_account_and_address() {
        let conn = open_in_memory().unwrap();
        let home = accounts::insert_account(&conn, "dana@example.com", 0).unwrap();
        let work = accounts::insert_account(&conn, "dana@work.example", 0).unwrap();
        record(
            &conn,
            work,
            &["Ana@Example.pt ".into()],
            Answer::Declined,
            10,
        )
        .unwrap();
        record(
            &conn,
            work,
            &["ana@example.pt".into(), "rui@example.pt".into()],
            Answer::Saved,
            20,
        )
        .unwrap();
        let held = answered(&conn, work).unwrap();
        assert_eq!(
            held,
            HashSet::from(["ana@example.pt".into(), "rui@example.pt".into()])
        );
        assert!(answered(&conn, home).unwrap().is_empty());
    }

    #[test]
    fn the_answers_go_with_the_account() {
        let conn = open_in_memory().unwrap();
        let work = accounts::insert_account(&conn, "dana@work.example", 0).unwrap();
        record(
            &conn,
            work,
            &["ana@example.pt".into()],
            Answer::Declined,
            10,
        )
        .unwrap();
        conn.execute("DELETE FROM accounts WHERE id = ?1", [work])
            .unwrap();
        let left: i64 = conn
            .query_row("SELECT count(*) FROM contact_offers", [], |row| row.get(0))
            .unwrap();
        assert_eq!(left, 0);
    }
}
