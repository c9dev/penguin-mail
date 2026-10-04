//! Changes to the rules of an account whose ManageSieve server did not
//! answer, kept in order until it does. The Rules dialog lists them
//! beside the rules last read, and the app's minute timer sends them.

use mailrs_domain::{AccountId, Filter};
use rusqlite::{Connection, params};

use crate::{Result, StoreError};

// A change is built, queued and dropped; boxing the filter would change the
// shape every caller matches on for no saving.
#[expect(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleChange {
    /// Add this rule, which carries the id it will keep.
    Create(Filter),
    /// Delete the rule with this id.
    Delete(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedRule {
    pub seq: i64,
    pub change: RuleChange,
}

/// Queues `change` after the account's others and answers its place.
pub fn enqueue(conn: &Connection, account_id: AccountId, change: &RuleChange) -> Result<i64> {
    let (kind, id, filter) = match change {
        RuleChange::Create(filter) => {
            let text = serde_json::to_string(filter)
                .map_err(|err| StoreError::Corrupt { column: "rule_changes.filter", value: err.to_string() })?;
            ("create", filter.id.clone().unwrap_or_default(), Some(text))
        }
        RuleChange::Delete(id) => ("delete", id.clone(), None),
    };
    conn.execute(
        "INSERT INTO rule_changes (account_id, kind, filter_id, filter) VALUES (?1, ?2, ?3, ?4)",
        params![account_id, kind, id, filter],
    )?;
    Ok(conn.last_insert_rowid())
}

/// The account's waiting changes, oldest first.
pub fn queued(conn: &Connection, account_id: AccountId) -> Result<Vec<QueuedRule>> {
    let mut stmt = conn.prepare(
        "SELECT seq, kind, filter_id, filter FROM rule_changes WHERE account_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map([account_id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, Option<String>>(3)?))
    })?;
    let mut queued = Vec::new();
    for row in rows {
        let (seq, kind, id, filter) = row?;
        let change = match (kind.as_str(), filter) {
            ("create", Some(text)) => {
                let mut filter: Filter = serde_json::from_str(&text)
                    .map_err(|_| StoreError::Corrupt { column: "rule_changes.filter", value: text.clone() })?;
                filter.id = Some(id);
                RuleChange::Create(filter)
            }
            ("delete", _) => RuleChange::Delete(id),
            _ => return Err(StoreError::Corrupt { column: "rule_changes.kind", value: kind }),
        };
        queued.push(QueuedRule { seq, change });
    }
    Ok(queued)
}

pub fn dequeue(conn: &Connection, seq: i64) -> Result<()> {
    conn.execute("DELETE FROM rule_changes WHERE seq = ?1", [seq])?;
    Ok(())
}
