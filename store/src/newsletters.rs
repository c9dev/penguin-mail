//! The senders whose mail reads as a newsletter, grouped from the stored
//! messages.
//!
//! A newsletter is a sender whose mail carries a `List-Unsubscribe`
//! header, and Gmail's Promotions, Updates and Forums categories catch the
//! rest: bulk mail that arrived before the header was stored, or that
//! never carried one. Each sender's newest message names the thread the
//! app unsubscribes through and holds the header to act on.

use std::collections::HashMap;

use mailrs_domain::{AccountId, EpochMillis, system_label};
use rusqlite::{Connection, params};

use crate::Result;

/// One sender the newsletters list shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sender {
    /// The newest message's display name, or the address when it has none.
    pub name: String,
    /// The address, lower-cased, which is what the grouping went by.
    pub email: String,
    /// How many of the sender's messages counted, inside the window.
    pub messages: u32,
    /// When the newest of them arrived.
    pub last: EpochMillis,
    pub message_id: String,
    pub thread_id: String,
    /// The newest message's `List-Unsubscribe` header, when it has one.
    pub header: Option<String>,
    /// The newest message promised RFC 8058 one-click.
    pub one_click: bool,
}

/// The account's newsletter senders, newest first, counting the messages
/// that arrived at or after `since`.
///
/// A message counts when it carries an unsubscribe header or one of the
/// three bulk categories. The sender's newest counting message gives the
/// name, the ids and the header, so a caller that wants to leave the list
/// has the thread to act on and the header to act with.
pub fn list(conn: &Connection, account_id: AccountId, since: EpochMillis) -> Result<Vec<Sender>> {
    let mut stmt = conn.prepare(
        "SELECT m.from_name, m.from_addr, m.id, m.thread_id, m.date, m.list_unsubscribe, m.one_click \
         FROM messages m \
         WHERE m.account_id = ?1 AND m.date >= ?2 AND m.from_addr IS NOT NULL AND m.from_addr <> '' \
         AND (m.list_unsubscribe IS NOT NULL OR EXISTS ( \
             SELECT 1 FROM message_labels ml \
             WHERE ml.account_id = m.account_id AND ml.message_id = m.id \
             AND ml.label_id IN (?3, ?4, ?5))) \
         ORDER BY m.date DESC, m.id DESC",
    )?;
    let rows = stmt.query_map(
        params![
            account_id,
            since,
            system_label::CATEGORY_PROMOTIONS,
            system_label::CATEGORY_UPDATES,
            system_label::CATEGORY_FORUMS,
        ],
        |row| {
            let name: Option<String> = row.get(0)?;
            let email: String = row.get(1)?;
            Ok(Sender {
                name: name
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| email.clone()),
                email: email.to_lowercase(),
                messages: 1,
                message_id: row.get(2)?,
                thread_id: row.get(3)?,
                last: row.get(4)?,
                header: row.get(5)?,
                one_click: row.get(6)?,
            })
        },
    )?;
    // The rows arrive newest first, so the first one seen for an address
    // is the newest message and the order of `senders` is already the
    // order the list shows.
    let mut senders: Vec<Sender> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for row in rows {
        let sender = row?;
        match seen.get(&sender.email) {
            Some(&at) => senders[at].messages += 1,
            None => {
                seen.insert(sender.email.clone(), senders.len());
                senders.push(sender);
            }
        }
    }
    Ok(senders)
}
