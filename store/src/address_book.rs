//! The Google contacts of each account: names, addresses, photos,
//! organizations, and phone numbers, as the People API gives them.
//!
//! The `contacts` module holds whoever wrote to the account; this holds
//! whoever the account owner wrote down. Recipient suggestions read both,
//! and a contact outranks an address that only ever appeared in mail.

use mailrs_domain::{AccountId, EpochMillis};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// One person in an account's address book.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contact {
    pub account_id: AccountId,
    /// What the People API calls this person, such as `people/c17`.
    pub resource: String,
    pub name: Option<String>,
    /// Every address Google holds, the primary one first.
    pub emails: Vec<String>,
    pub organization: Option<String>,
    pub phone: Option<String>,
    /// Where Google serves the photo, or `None` for a contact without one.
    pub photo_url: Option<String>,
    /// The file under the photo directory that holds the bytes, once they
    /// have been downloaded.
    pub photo_file: Option<String>,
}

impl Contact {
    /// The first address, which is the one to write to.
    pub fn email(&self) -> Option<&str> {
        self.emails.first().map(String::as_str)
    }

    /// The name to show, falling back to the first address.
    pub fn display(&self) -> &str {
        self.name
            .as_deref()
            .filter(|n| !n.trim().is_empty())
            .or_else(|| self.email())
            .unwrap_or("")
    }
}

/// Stores contacts, replacing what each one had before. A photo already on
/// disk stays with its contact while Google serves it from the same URL, so
/// refreshing an address book downloads no photo twice.
pub fn save(conn: &Connection, contacts: &[Contact]) -> Result<()> {
    for contact in contacts {
        let kept: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT photo_url, photo_file FROM contacts WHERE account_id = ?1 AND resource = ?2",
                params![contact.account_id, contact.resource],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let photo_file = match kept {
            Some((url, file)) if url == contact.photo_url => file.or(contact.photo_file.clone()),
            _ => contact.photo_file.clone(),
        };
        conn.execute(
            "INSERT INTO contacts (account_id, resource, name, organization, phone, photo_url, photo_file) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT (account_id, resource) DO UPDATE SET \
             name = excluded.name, organization = excluded.organization, phone = excluded.phone, \
             photo_url = excluded.photo_url, photo_file = excluded.photo_file",
            params![
                contact.account_id,
                contact.resource,
                contact.name,
                contact.organization,
                contact.phone,
                contact.photo_url,
                photo_file,
            ],
        )?;
        conn.execute(
            "DELETE FROM contact_addresses WHERE account_id = ?1 AND resource = ?2",
            params![contact.account_id, contact.resource],
        )?;
        for (rank, email) in contact.emails.iter().enumerate() {
            let email = email.trim();
            if email.is_empty() {
                continue;
            }
            conn.execute(
                "INSERT OR REPLACE INTO contact_addresses (account_id, resource, email, rank) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    contact.account_id,
                    contact.resource,
                    email.to_lowercase(),
                    rank as i64
                ],
            )?;
        }
    }
    Ok(())
}

/// Drops the contacts Google says the account owner deleted.
pub fn forget(conn: &Connection, account_id: AccountId, resources: &[String]) -> Result<()> {
    for resource in resources {
        conn.execute(
            "DELETE FROM contacts WHERE account_id = ?1 AND resource = ?2",
            params![account_id, resource],
        )?;
    }
    Ok(())
}

/// Drops everything stored for an account, for when the owner turns
/// contacts off.
pub fn clear(conn: &Connection, account_id: AccountId) -> Result<()> {
    conn.execute("DELETE FROM contacts WHERE account_id = ?1", [account_id])?;
    conn.execute(
        "DELETE FROM contact_books WHERE account_id = ?1",
        [account_id],
    )?;
    Ok(())
}

/// Every contact of every account, by name.
pub fn list(conn: &Connection) -> Result<Vec<Contact>> {
    read(conn, "1 = 1", [])
}

/// The contacts every word of `query` finds in a name, an address or an
/// organization, whatever the case, by name. The address books hold a few
/// thousand people at most, so the words are matched here rather than in
/// SQL, which would need the addresses joined in once per word.
pub fn search(conn: &Connection, query: &str) -> Result<Vec<Contact>> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Ok(Vec::new());
    }
    Ok(list(conn)?
        .into_iter()
        .filter(|contact| {
            let text = [
                contact.name.as_deref().unwrap_or_default(),
                contact.organization.as_deref().unwrap_or_default(),
                &contact.emails.join(" "),
            ]
            .join(" ")
            .to_lowercase();
            words.iter().all(|word| text.contains(word))
        })
        .collect())
}

/// The contacts that hold the address `?1`. It names them from
/// `contact_addresses_by_email`, where a correlated `EXISTS` would read
/// every contact and probe its addresses.
const HOLDS_EMAIL: &str = "(c.account_id, c.resource) IN \
     (SELECT account_id, resource FROM contact_addresses WHERE email = ?1)";

/// The contact holding `email`, whichever account wrote it down.
pub fn find(conn: &Connection, email: &str) -> Result<Option<Contact>> {
    let key = email.trim().to_lowercase();
    let found = read(conn, HOLDS_EMAIL, [key])?;
    Ok(found.into_iter().next())
}

/// The photos Google serves that are not on disk yet, as resource and URL.
pub fn missing_photos(conn: &Connection, account_id: AccountId) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT resource, photo_url FROM contacts \
         WHERE account_id = ?1 AND photo_url IS NOT NULL AND photo_file IS NULL",
    )?;
    let rows = stmt.query_map([account_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Records where a downloaded photo landed.
pub fn set_photo_file(
    conn: &Connection,
    account_id: AccountId,
    resource: &str,
    file: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE contacts SET photo_file = ?3 WHERE account_id = ?1 AND resource = ?2",
        params![account_id, resource, file],
    )?;
    Ok(())
}

/// What the last refresh of this address book left behind: the sync token
/// to hand back to Google, and when it ran.
pub fn book(
    conn: &Connection,
    account_id: AccountId,
) -> Result<Option<(Option<String>, EpochMillis)>> {
    Ok(conn
        .query_row(
            "SELECT sync_token, synced_at FROM contact_books WHERE account_id = ?1",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

/// Records a finished refresh.
pub fn set_book(
    conn: &Connection,
    account_id: AccountId,
    sync_token: Option<&str>,
    at: EpochMillis,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO contact_books (account_id, sync_token, synced_at) \
         VALUES (?1, ?2, ?3)",
        params![account_id, sync_token, at],
    )?;
    Ok(())
}

/// The contacts `filter` keeps, each with its addresses, by name.
fn query(filter: &str) -> String {
    format!(
        "SELECT c.account_id, c.resource, c.name, c.organization, c.phone, c.photo_url, c.photo_file, \
         (SELECT group_concat(a.email, char(10)) FROM (SELECT email FROM contact_addresses \
          WHERE account_id = c.account_id AND resource = c.resource ORDER BY rank) a) \
         FROM contacts c WHERE {filter} ORDER BY c.name IS NULL, c.name COLLATE NOCASE, c.resource"
    )
}

fn read<P: rusqlite::Params>(conn: &Connection, filter: &str, params: P) -> Result<Vec<Contact>> {
    let mut stmt = conn.prepare(&query(filter))?;
    let rows = stmt.query_map(params, |row| {
        let emails: Option<String> = row.get(7)?;
        Ok(Contact {
            account_id: row.get(0)?,
            resource: row.get(1)?,
            name: row.get(2)?,
            organization: row.get(3)?,
            phone: row.get(4)?,
            photo_url: row.get(5)?,
            photo_file: row.get(6)?,
            emails: emails
                .unwrap_or_default()
                .lines()
                .filter(|e| !e.is_empty())
                .map(str::to_string)
                .collect(),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{accounts, open_in_memory};

    fn contact(account_id: AccountId, resource: &str, name: &str, emails: &[&str]) -> Contact {
        Contact {
            account_id,
            resource: resource.into(),
            name: Some(name.into()),
            emails: emails.iter().map(|e| e.to_string()).collect(),
            ..Contact::default()
        }
    }

    #[test]
    fn finding_by_address_starts_from_the_address_index() {
        let conn = open_in_memory().unwrap();
        let plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {}", query(HOLDS_EMAIL)))
            .unwrap()
            .query_map(["mara@example.org"], |row| row.get::<_, String>(3))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert!(
            plan.iter()
                .any(|p| p.contains("contact_addresses_by_email")),
            "{plan:?}"
        );
        assert!(!plan.iter().any(|p| p.starts_with("SCAN c")), "{plan:?}");
    }

    #[test]
    fn a_contact_keeps_every_address_in_order() {
        let conn = open_in_memory().unwrap();
        let id = accounts::insert_account(&conn, "dana@example.com", 0).unwrap();
        save(
            &conn,
            &[contact(
                id,
                "people/c1",
                "Mara Okafor",
                &["Mara@example.org", "mara.work@example.org"],
            )],
        )
        .unwrap();
        let found = find(&conn, "MARA.WORK@example.org").unwrap().unwrap();
        assert_eq!(found.emails, ["mara@example.org", "mara.work@example.org"]);
        assert_eq!(found.email(), Some("mara@example.org"));
        assert_eq!(found.display(), "Mara Okafor");
    }

    #[test]
    fn search_finds_every_word_in_a_name_an_address_or_an_organization() {
        let conn = open_in_memory().unwrap();
        let id = accounts::insert_account(&conn, "dana@example.com", 0).unwrap();
        let mut mara = contact(id, "people/c1", "Mara Okafor", &["mara@example.org"]);
        mara.organization = Some("Fernwood Kites".into());
        save(
            &conn,
            &[
                mara,
                contact(id, "people/c2", "Theo Lang", &["theo@fernwood.example"]),
            ],
        )
        .unwrap();
        let names = |query: &str| -> Vec<String> {
            search(&conn, query)
                .unwrap()
                .iter()
                .map(|c| c.display().to_string())
                .collect()
        };
        assert_eq!(names("mara"), ["Mara Okafor"]);
        assert_eq!(names("FERNWOOD"), ["Mara Okafor", "Theo Lang"]);
        assert_eq!(names("fernwood theo"), ["Theo Lang"]);
        assert!(names("priya").is_empty());
        assert!(names("  ").is_empty(), "no words find nobody");
    }

    #[test]
    fn a_refresh_replaces_addresses_and_keeps_the_photo() {
        let conn = open_in_memory().unwrap();
        let id = accounts::insert_account(&conn, "dana@example.com", 0).unwrap();
        let mut mara = contact(id, "people/c1", "Mara Okafor", &["mara@example.org"]);
        mara.photo_url = Some("https://photos.example/mara".into());
        save(&conn, &[mara.clone()]).unwrap();
        assert_eq!(
            missing_photos(&conn, id).unwrap(),
            [(
                "people/c1".to_string(),
                "https://photos.example/mara".to_string()
            )]
        );
        set_photo_file(&conn, id, "people/c1", "1-c1.jpg").unwrap();

        mara.emails = vec!["mara@example.org".into(), "m.okafor@work.example".into()];
        save(&conn, &[mara.clone()]).unwrap();
        let found = find(&conn, "m.okafor@work.example").unwrap().unwrap();
        assert_eq!(found.photo_file.as_deref(), Some("1-c1.jpg"));
        assert!(missing_photos(&conn, id).unwrap().is_empty());

        // A new URL means a new photo, so the file on disk stops counting.
        mara.photo_url = Some("https://photos.example/mara-2".into());
        save(&conn, &[mara]).unwrap();
        assert_eq!(missing_photos(&conn, id).unwrap().len(), 1);
    }

    #[test]
    fn forgetting_a_contact_takes_its_addresses_with_it() {
        let conn = open_in_memory().unwrap();
        let id = accounts::insert_account(&conn, "dana@example.com", 0).unwrap();
        save(
            &conn,
            &[
                contact(id, "people/c1", "Mara Okafor", &["mara@example.org"]),
                contact(id, "people/c2", "Theo Lang", &["theo@example.org"]),
            ],
        )
        .unwrap();
        forget(&conn, id, &["people/c1".to_string()]).unwrap();
        assert!(find(&conn, "mara@example.org").unwrap().is_none());
        assert_eq!(list(&conn).unwrap().len(), 1);
        clear(&conn, id).unwrap();
        assert!(list(&conn).unwrap().is_empty());
    }

    #[test]
    fn the_sync_token_survives_a_refresh() {
        let conn = open_in_memory().unwrap();
        let id = accounts::insert_account(&conn, "dana@example.com", 0).unwrap();
        assert_eq!(book(&conn, id).unwrap(), None);
        set_book(&conn, id, Some("token-1"), 5_000).unwrap();
        assert_eq!(
            book(&conn, id).unwrap(),
            Some((Some("token-1".to_string()), 5_000))
        );
        set_book(&conn, id, None, 9_000).unwrap();
        assert_eq!(book(&conn, id).unwrap(), Some((None, 9_000)));
    }
}
