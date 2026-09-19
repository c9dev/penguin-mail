//! People to suggest as recipients, built from stored mail. Someone you
//! wrote to ranks above someone who only wrote to you.

use std::collections::{HashMap, HashSet};

use mailrs_domain::{Address, EpochMillis};
use rusqlite::Connection;

use crate::Result;

/// Weight of one message you sent to someone, against one they sent you.
const SENT_WEIGHT: i64 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub name: Option<String>,
    pub email: String,
    pub score: i64,
    pub last_seen: EpochMillis,
}

impl Contact {
    pub fn address(&self) -> Address {
        Address {
            name: self.name.clone(),
            email: self.email.clone(),
        }
    }
}

/// Every correspondent across all accounts, best first. Leaves out the
/// accounts' own addresses and automated senders.
pub fn list_contacts(conn: &Connection) -> Result<Vec<Contact>> {
    let own: HashSet<String> = conn
        .prepare("SELECT lower(email) FROM accounts")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut stmt = conn.prepare(
        "SELECT m.from_name, m.from_addr, m.to_addrs, m.cc_addrs, m.date, \
         EXISTS (SELECT 1 FROM message_labels l WHERE l.account_id = m.account_id \
                 AND l.message_id = m.id AND l.label_id = 'SENT') \
         FROM messages m",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, EpochMillis>(4)?,
            row.get::<_, bool>(5)?,
        ))
    })?;
    let mut found: HashMap<String, Contact> = HashMap::new();
    let mut note = |address: Address, weight: i64, date: EpochMillis| {
        let key = address.email.trim().to_lowercase();
        if key.is_empty() || !key.contains('@') || own.contains(&key) || is_automated(&key) {
            return;
        }
        let name = address
            .name
            .filter(|n| !n.trim().is_empty() && !n.contains('@'));
        let entry = found.entry(key).or_insert_with(|| Contact {
            name: None,
            email: address.email.trim().to_string(),
            score: 0,
            last_seen: date,
        });
        entry.score += weight;
        if date >= entry.last_seen || entry.name.is_none() {
            entry.name = name.or(entry.name.take());
            entry.last_seen = entry.last_seen.max(date);
        }
    };
    for row in rows {
        let (from_name, from_addr, to, cc, date, sent) = row?;
        if sent {
            let to: Vec<Address> = serde_json::from_str(&to).unwrap_or_default();
            let cc: Vec<Address> = serde_json::from_str(&cc).unwrap_or_default();
            for address in to.into_iter().chain(cc) {
                note(address, SENT_WEIGHT, date);
            }
        } else if let Some(email) = from_addr {
            note(
                Address {
                    name: from_name,
                    email,
                },
                1,
                date,
            );
        }
    }
    let mut contacts: Vec<Contact> = found.into_values().collect();
    contacts.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.last_seen.cmp(&a.last_seen))
            .then_with(|| a.email.cmp(&b.email))
    });
    Ok(contacts)
}

/// Addresses nobody writes to: no-reply senders and notification robots.
fn is_automated(email: &str) -> bool {
    let local = email.split('@').next().unwrap_or(email);
    [
        "noreply",
        "no-reply",
        "no_reply",
        "donotreply",
        "do-not-reply",
        "mailer-daemon",
        "bounce",
    ]
    .iter()
    .any(|p| local.contains(p))
        || local == "notifications"
        || local == "notification"
}
