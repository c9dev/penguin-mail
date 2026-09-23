//! People to suggest as recipients. Two sources feed the list: the address
//! book each account reads from Google, and the addresses in stored mail.
//! A contact comes first, whatever the mail says; among people known only
//! from mail, someone you wrote to ranks above someone who only wrote to
//! you.

use std::collections::{HashMap, HashSet};

use mailrs_domain::{Address, EpochMillis};
use rusqlite::Connection;

use crate::Result;
use crate::address_book;

/// Weight of one message you sent to someone, against one they sent you.
const SENT_WEIGHT: i64 = 5;

/// Someone found in stored mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correspondent {
    pub name: Option<String>,
    pub email: String,
    pub score: i64,
    pub last_seen: EpochMillis,
}

/// One row of the recipient suggestions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub name: Option<String>,
    pub email: String,
    /// The contact's employer, when the address book names one.
    pub organization: Option<String>,
    /// The file under the photo directory holding this person's photo.
    pub photo_file: Option<String>,
    /// Set for someone in an account's address book. These rank first.
    pub known: bool,
    pub score: i64,
    pub last_seen: EpochMillis,
}

impl Suggestion {
    pub fn address(&self) -> Address {
        Address {
            name: self.name.clone(),
            email: self.email.clone(),
        }
    }

    /// The name to show, falling back to the address.
    pub fn display(&self) -> &str {
        self.name
            .as_deref()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or(&self.email)
    }
}

/// Everyone worth suggesting, best first: the contacts in every account's
/// address book, then the correspondents mail turned up.
pub fn suggestions(conn: &Connection) -> Result<Vec<Suggestion>> {
    let mut found: HashMap<String, Suggestion> = HashMap::new();
    for contact in address_book::list(conn)? {
        for email in &contact.emails {
            let key = email.trim().to_lowercase();
            if key.is_empty() {
                continue;
            }
            found.entry(key).or_insert_with(|| Suggestion {
                name: contact.name.clone(),
                email: email.clone(),
                organization: contact.organization.clone(),
                photo_file: contact.photo_file.clone(),
                known: true,
                score: 0,
                last_seen: 0,
            });
        }
    }
    for person in list_correspondents(conn)? {
        let key = person.email.trim().to_lowercase();
        match found.get_mut(&key) {
            // Mail says how often this contact comes up, which orders the
            // contacts among themselves. Google's name wins over the one
            // a sender put in a header.
            Some(known) => {
                known.score += person.score;
                known.last_seen = known.last_seen.max(person.last_seen);
                if known.name.is_none() {
                    known.name = person.name;
                }
            }
            None => {
                found.insert(
                    key,
                    Suggestion {
                        name: person.name,
                        email: person.email,
                        organization: None,
                        photo_file: None,
                        known: false,
                        score: person.score,
                        last_seen: person.last_seen,
                    },
                );
            }
        }
    }
    let mut all: Vec<Suggestion> = found.into_values().collect();
    all.sort_by(|a, b| {
        b.known
            .cmp(&a.known)
            .then(b.score.cmp(&a.score))
            .then(b.last_seen.cmp(&a.last_seen))
            .then_with(|| a.email.cmp(&b.email))
    });
    Ok(all)
}

/// Every correspondent across all accounts, best first. Leaves out the
/// accounts' own addresses and automated senders.
pub fn list_correspondents(conn: &Connection) -> Result<Vec<Correspondent>> {
    let own: HashSet<String> = conn
        .prepare("SELECT lower(email) FROM accounts")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    // Only sent mail needs its recipients, so only sent mail hands over
    // the JSON that holds them.
    let mut stmt = conn.prepare(
        "SELECT m.from_name, m.from_addr, \
         CASE WHEN s.message_id IS NULL THEN NULL ELSE m.to_addrs END, \
         CASE WHEN s.message_id IS NULL THEN NULL ELSE m.cc_addrs END, m.date \
         FROM messages m LEFT JOIN message_mailboxes s \
         ON s.account_id = m.account_id AND s.message_id = m.id \
         AND s.mailbox IN (SELECT key FROM mailboxes WHERE role = 'sent')",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, EpochMillis>(4)?,
        ))
    })?;
    let mut found: HashMap<String, Correspondent> = HashMap::new();
    let mut note = |address: Address, weight: i64, date: EpochMillis| {
        let key = address.email.trim().to_lowercase();
        if key.is_empty() || !key.contains('@') || own.contains(&key) || is_automated(&key) {
            return;
        }
        let name = address
            .name
            .filter(|n| !n.trim().is_empty() && !n.contains('@'));
        let entry = found.entry(key).or_insert_with(|| Correspondent {
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
        let (from_name, from_addr, to, cc, date) = row?;
        if let (Some(to), Some(cc)) = (to, cc) {
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
    let mut contacts: Vec<Correspondent> = found.into_values().collect();
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

#[cfg(test)]
mod tests {
    use mailrs_domain::{MessageMeta, system_label};

    use super::*;
    use crate::{accounts, messages, open_in_memory};

    fn message(
        account_id: mailrs_domain::AccountId,
        id: &str,
        from: (&str, &str),
        to: &[(&str, &str)],
        label: &str,
    ) -> MessageMeta {
        MessageMeta {
            account_id,
            id: id.into(),
            thread_id: id.into(),
            rfc822_msgid: None,
            from: Some(Address {
                name: Some(from.0.into()),
                email: from.1.into(),
            }),
            to: to
                .iter()
                .map(|(name, email)| Address {
                    name: Some((*name).into()),
                    email: (*email).into(),
                })
                .collect(),
            cc: Vec::new(),
            subject: "Hello".into(),
            date: 1_000,
            snippet: String::new(),
            size: 10,
            has_attachments: false,
            label_ids: vec![label.into()],
            list_unsubscribe: None,
            one_click: false,
        }
    }

    #[test]
    fn contacts_come_before_addresses_seen_only_in_mail() {
        let conn = open_in_memory().unwrap();
        let id = accounts::insert_account(&conn, "dana@example.com", 0).unwrap();
        // Ten messages from Theo, none from Mara, who is a contact.
        for n in 0..10 {
            messages::apply(
                &conn,
                id,
                &[messages::Change::Upsert {
                    meta: Box::new(message(
                        id,
                        &format!("m{n}"),
                        ("Theo Lang", "theo@example.org"),
                        &[("Dana", "dana@example.com")],
                        system_label::INBOX,
                    )),
                    generation: 1,
                }],
            )
            .unwrap();
        }
        address_book::save(
            &conn,
            &[address_book::Contact {
                account_id: id,
                resource: "people/c1".into(),
                name: Some("Mara Okafor".into()),
                emails: vec!["mara@example.org".into()],
                organization: Some("Fernwood".into()),
                photo_file: Some("1-c1.jpg".into()),
                ..address_book::Contact::default()
            }],
        )
        .unwrap();

        let all = suggestions(&conn).unwrap();
        let emails: Vec<&str> = all.iter().map(|s| s.email.as_str()).collect();
        assert_eq!(emails, ["mara@example.org", "theo@example.org"]);
        assert!(all[0].known);
        assert_eq!(all[0].organization.as_deref(), Some("Fernwood"));
        assert_eq!(all[0].photo_file.as_deref(), Some("1-c1.jpg"));
        assert!(!all[1].known);
        assert_eq!(all[1].score, 10);
    }

    #[test]
    fn mail_orders_the_contacts_and_google_names_them() {
        let conn = open_in_memory().unwrap();
        let id = accounts::insert_account(&conn, "dana@example.com", 0).unwrap();
        messages::apply(
            &conn,
            id,
            &[messages::Change::Upsert {
                meta: Box::new(message(
                    id,
                    "m1",
                    ("Dana", "dana@example.com"),
                    &[("t", "theo@example.org")],
                    system_label::SENT,
                )),
                generation: 1,
            }],
        )
        .unwrap();
        messages::apply(
            &conn,
            id,
            &[messages::Change::Upsert {
                meta: Box::new(message(
                    id,
                    "m2",
                    ("M. O.", "mara@example.org"),
                    &[("Dana", "dana@example.com")],
                    system_label::INBOX,
                )),
                generation: 1,
            }],
        )
        .unwrap();
        let contact = |resource: &str, name: &str, email: &str| address_book::Contact {
            account_id: id,
            resource: resource.into(),
            name: Some(name.into()),
            emails: vec![email.into()],
            ..address_book::Contact::default()
        };
        address_book::save(
            &conn,
            &[
                contact("people/c1", "Mara Okafor", "mara@example.org"),
                contact("people/c2", "Theo Lang", "theo@example.org"),
            ],
        )
        .unwrap();

        let all = suggestions(&conn).unwrap();
        assert!(all.iter().all(|s| s.known));
        // Writing to Theo weighs five times a message from Mara.
        assert_eq!(all[0].email, "theo@example.org");
        assert_eq!(all[0].name.as_deref(), Some("Theo Lang"));
        assert_eq!(all[1].name.as_deref(), Some("Mara Okafor"));
    }
}
