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
    pub details: mailrs_domain::contact::Details,
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
            // An empty file means a check found no photo. A contact that
            // comes back in a refresh has changed, and Graph keeps the same
            // URL for a photo added later, so ask again.
            Some((url, file)) if url == contact.photo_url => file
                .filter(|f| !f.is_empty())
                .or(contact.photo_file.clone()),
            _ => contact.photo_file.clone(),
        };
        conn.execute(
            "INSERT INTO contacts (account_id, resource, name, organization, phone, photo_url, photo_file, details) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT (account_id, resource) DO UPDATE SET \
             name = excluded.name, organization = excluded.organization, phone = excluded.phone, \
             photo_url = excluded.photo_url, photo_file = excluded.photo_file, details = excluded.details",
            params![
                contact.account_id,
                contact.resource,
                contact.name,
                contact.organization,
                contact.phone,
                contact.photo_url,
                photo_file,
                serde_json::to_string(&contact.details).expect("contact details serialize"),
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
        conn.execute(
            "DELETE FROM contact_links WHERE (account_a = ?1 AND resource_a = ?2) OR (account_b = ?1 AND resource_b = ?2)",
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
        "DELETE FROM contact_links WHERE account_a = ?1 OR account_b = ?1",
        [account_id],
    )?;
    conn.execute(
        "DELETE FROM contact_books WHERE account_id = ?1",
        [account_id],
    )?;
    Ok(())
}

/// Every contact of every account, by name.
pub fn list(conn: &Connection) -> Result<Vec<Contact>> {
    let all = read(conn, "1 = 1", [])?;
    let groups: Vec<_> = all.iter().filter(|c| c.details.members.is_some()).collect();
    Ok(all
        .iter()
        .filter(|c| c.details.members.is_none())
        .cloned()
        .map(|mut contact| {
            for group in &groups {
                if group.account_id == contact.account_id
                    && group.resource.rsplit_once('/').map(|(p, _)| p)
                        == contact.resource.rsplit_once('/').map(|(p, _)| p)
                    && group
                        .details
                        .members
                        .as_ref()
                        .is_some_and(|members| members.contains(&contact.details.uid))
                {
                    let name = group.display().to_string();
                    if !contact.details.groups.contains(&name) {
                        contact.details.groups.push(name);
                    }
                }
            }
            contact
        })
        .collect())
}

/// The people who have a birthday, for the birthdays calendar, without
/// the group names `list` joins in.
pub fn birthdays(conn: &Connection) -> Result<Vec<Contact>> {
    read(
        conn,
        "json_extract(c.details, '$.birthday') <> '' AND json_extract(c.details, '$.members') IS NULL",
        [],
    )
}

/// The account's contact `resource`, if it holds one.
pub fn one(conn: &Connection, account_id: AccountId, resource: &str) -> Result<Option<Contact>> {
    Ok(read(
        conn,
        "c.account_id = ?1 AND c.resource = ?2",
        params![account_id, resource],
    )?
    .into_iter()
    .next())
}

/// The resource of every row the account holds, group cards included.
pub fn resources(conn: &Connection, account_id: AccountId) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT resource FROM contacts WHERE account_id = ?1")?;
    let rows = stmt.query_map([account_id], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Keep native group cards in step with a successful edit until the next refresh.
pub fn set_memberships(conn: &Connection, contact: &Contact, names: &[String]) -> Result<()> {
    if contact.details.uid.is_empty() {
        return Ok(());
    }
    let mut groups = read(conn, "1 = 1", [])?;
    groups.retain(|g| {
        g.account_id == contact.account_id
            && g.details.members.is_some()
            && g.resource.rsplit_once('/').map(|(p, _)| p)
                == contact.resource.rsplit_once('/').map(|(p, _)| p)
    });
    for group in &mut groups {
        let include = names.iter().any(|name| name == group.display());
        if let Some(members) = &mut group.details.members {
            members.retain(|uid| uid != &contact.details.uid);
            if include {
                members.push(contact.details.uid.clone());
            }
        }
    }
    save(conn, &groups)
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

/// Whether `account_id`'s address book has a contact with the address
/// `email`, whatever its case.
pub fn holds(conn: &Connection, account_id: AccountId, email: &str) -> Result<bool> {
    let key = email.trim().to_lowercase();
    Ok(conn
        .query_row(
            "SELECT 1 FROM contact_addresses WHERE email = ?1 AND account_id = ?2 LIMIT 1",
            params![key, account_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
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

/// Records that `resource` has no photo, so the next refresh does not ask
/// again. An empty `photo_file` stands for none, and every reader turns it
/// into `None`.
pub fn set_no_photo(conn: &Connection, account_id: AccountId, resource: &str) -> Result<()> {
    set_photo_file(conn, account_id, resource, "")
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

/// Two contacts in different accounts the person said are one human.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub a: (AccountId, String),
    pub b: (AccountId, String),
    /// Whether All contacts shows `a`'s name for the pair rather than `b`'s.
    pub shown_a: bool,
}

/// Links two contacts. Neither contact changes.
pub fn link(conn: &Connection, link: &Link) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO contact_links (account_a, resource_a, account_b, resource_b, shown_a) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![link.a.0, link.a.1, link.b.0, link.b.1, link.shown_a],
    )?;
    Ok(())
}

/// Takes a link away, in whichever order it was made.
pub fn unlink(conn: &Connection, one: &(AccountId, String), other: &(AccountId, String)) -> Result<()> {
    conn.execute(
        "DELETE FROM contact_links WHERE (account_a = ?1 AND resource_a = ?2 AND account_b = ?3 AND resource_b = ?4) \
         OR (account_a = ?3 AND resource_a = ?4 AND account_b = ?1 AND resource_b = ?2)",
        params![one.0, one.1, other.0, other.1],
    )?;
    Ok(())
}

/// Every link, for contacts that are still stored.
pub fn links(conn: &Connection) -> Result<Vec<Link>> {
    let mut stmt = conn.prepare(
        "SELECT l.account_a, l.resource_a, l.account_b, l.resource_b, l.shown_a FROM contact_links l \
         WHERE EXISTS (SELECT 1 FROM contacts c WHERE c.account_id = l.account_a AND c.resource = l.resource_a) \
         AND EXISTS (SELECT 1 FROM contacts c WHERE c.account_id = l.account_b AND c.resource = l.resource_b)",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Link {
            a: (row.get(0)?, row.get(1)?),
            b: (row.get(2)?, row.get(3)?),
            shown_a: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// What the mailbox holds about some addresses, for a person's Mail section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailWith {
    /// The newest message to or from them: account, thread, subject, date.
    pub last: Option<(AccountId, String, String, EpochMillis)>,
    /// Whether one of the accounts sent that message.
    pub last_mine: bool,
    /// How many conversations include them.
    pub conversations: i64,
    /// The newest message one of the accounts sent them.
    pub last_wrote: Option<EpochMillis>,
}

/// How many messages each of `emails` sent, by lower-case address.
pub fn message_counts(
    conn: &Connection,
    emails: &[String],
) -> Result<std::collections::HashMap<String, i64>> {
    let wanted: std::collections::HashSet<String> =
        emails.iter().map(|e| e.to_lowercase()).collect();
    let mut stmt = conn.prepare_cached(
        "SELECT lower(from_addr), count(*) FROM messages WHERE from_addr IS NOT NULL GROUP BY 1",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let (email, count) = row?;
        if wanted.contains(&email) {
            out.insert(email, count);
        }
    }
    Ok(out)
}

/// The mail to or from any of `emails`, in any account.
pub fn mail_with(conn: &Connection, emails: &[String]) -> Result<MailWith> {
    let mut out = MailWith::default();
    let mut threads = std::collections::HashSet::new();
    for email in emails {
        let email = email.to_lowercase();
        let like = format!("%{}%", email.replace('%', "").replace('_', "\\_"));
        let mut stmt = conn.prepare_cached(
            "SELECT account_id, thread_id, subject, date, \
                    lower(from_addr) IN (SELECT lower(email) FROM accounts) \
             FROM messages WHERE lower(from_addr) = ?1 OR lower(to_addrs) LIKE ?2 ESCAPE '\\' OR lower(cc_addrs) LIKE ?2 ESCAPE '\\' \
             ORDER BY date DESC",
        )?;
        let rows = stmt.query_map(params![email, like], |row| {
            Ok((
                row.get::<_, AccountId>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, EpochMillis>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })?;
        for row in rows {
            let (account, thread, subject, date, mine) = row?;
            threads.insert((account, thread.clone()));
            if out.last.as_ref().is_none_or(|l| date > l.3) {
                out.last_mine = mine;
                out.last = Some((account, thread, subject, date));
            }
            if mine && out.last_wrote.is_none_or(|w| date > w) {
                out.last_wrote = Some(date);
            }
        }
    }
    out.conversations = threads.len() as i64;
    Ok(out)
}

/// The contacts `filter` keeps, each with its addresses, by name.
fn query(filter: &str) -> String {
    format!(
        "SELECT c.account_id, c.resource, c.name, c.organization, c.phone, c.photo_url, NULLIF(c.photo_file, ''), \
         (SELECT group_concat(a.email, char(10)) FROM (SELECT email FROM contact_addresses \
          WHERE account_id = c.account_id AND resource = c.resource ORDER BY rank) a) \
         , c.details FROM contacts c WHERE {filter} ORDER BY c.name IS NULL, c.name COLLATE NOCASE, c.resource"
    )
}

fn read<P: rusqlite::Params>(conn: &Connection, filter: &str, params: P) -> Result<Vec<Contact>> {
    let mut stmt = conn.prepare(&query(filter))?;
    let rows = stmt.query_map(params, |row| {
        let emails: Option<String> = row.get(7)?;
        Ok(Contact {
            details: serde_json::from_str(&row.get::<_, String>(8)?).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    8,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
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
    fn native_groups_are_joined_only_with_contacts_in_the_same_book() {
        let conn = open_in_memory().unwrap();
        let account = accounts::insert_account(&conn, "owner@example.org", 0).unwrap();
        let mut person = contact(account, "/book/one.vcf", "Demo", &["demo@example.org"]);
        person.details.uid = "person".into();
        person.details.birthday = "2000-02-29".into();
        let mut group = contact(account, "/book/group.vcf", "Friends", &[]);
        group.details.members = Some(vec!["person".into()]);
        let mut elsewhere = person.clone();
        elsewhere.resource = "/other/one.vcf".into();
        save(&conn, &[person, group.clone(), elsewhere]).unwrap();
        let people = list(&conn).unwrap();
        assert_eq!(people.len(), 2);
        let one = people
            .iter()
            .find(|c| c.resource == "/book/one.vcf")
            .unwrap();
        assert_eq!(one.details.groups, ["Friends"]);
        assert_eq!(one.details.birthday, "2000-02-29");
        assert!(
            people
                .iter()
                .find(|c| c.resource == "/other/one.vcf")
                .unwrap()
                .details
                .groups
                .is_empty()
        );
        set_memberships(&conn, one, &[]).unwrap();
        assert!(
            list(&conn)
                .unwrap()
                .iter()
                .all(|c| c.details.groups.is_empty())
        );
        group.details.members = Some(vec![]);
        save(&conn, &[group]).unwrap();
        assert!(
            list(&conn)
                .unwrap()
                .iter()
                .all(|c| c.details.groups.is_empty())
        );
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
    fn an_address_book_holds_only_its_own_contacts_addresses() {
        let conn = open_in_memory().unwrap();
        let home = accounts::insert_account(&conn, "dana@example.com", 0).unwrap();
        let work = accounts::insert_account(&conn, "dana@work.example", 0).unwrap();
        save(
            &conn,
            &[contact(
                work,
                "people/c1",
                "Mara Okafor",
                &["mara@example.org"],
            )],
        )
        .unwrap();
        assert!(holds(&conn, work, " MARA@example.org").unwrap());
        assert!(!holds(&conn, home, "mara@example.org").unwrap());
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
    fn a_contact_marked_without_a_photo_is_not_missing_one_and_shows_none() {
        let conn = open_in_memory().unwrap();
        let id = accounts::insert_account(&conn, "dana@outlook.com", 0).unwrap();
        let with_url = Contact {
            photo_url: Some("graph:contact/AAMk-c1".into()),
            ..contact(id, "AAMk-c1", "Mara Okafor", &["mara@example.org"])
        };
        save(&conn, &[with_url]).unwrap();
        assert_eq!(missing_photos(&conn, id).unwrap().len(), 1);
        set_no_photo(&conn, id, "AAMk-c1").unwrap();
        assert!(missing_photos(&conn, id).unwrap().is_empty());
        assert_eq!(
            find(&conn, "mara@example.org").unwrap().unwrap().photo_file,
            None
        );
    }

    #[test]
    fn a_refreshed_contact_marked_without_a_photo_is_asked_again() {
        let conn = open_in_memory().unwrap();
        let id = accounts::insert_account(&conn, "dana@outlook.com", 0).unwrap();
        let mara = Contact {
            photo_url: Some("graph:contact/AAMk-c1".into()),
            ..contact(id, "AAMk-c1", "Mara Okafor", &["mara@example.org"])
        };
        save(&conn, std::slice::from_ref(&mara)).unwrap();
        set_no_photo(&conn, id, "AAMk-c1").unwrap();
        save(&conn, &[mara]).unwrap();
        assert_eq!(missing_photos(&conn, id).unwrap().len(), 1);
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

    #[test]
    fn a_link_joins_two_contacts_and_goes_with_either() {
        let conn = open_in_memory().unwrap();
        let work = accounts::insert_account(&conn, "dana@example.com", 0).unwrap();
        let home = accounts::insert_account(&conn, "dana@fernwood.example", 0).unwrap();
        save(&conn, &[contact(work, "a", "Owen Mercer", &["o@a.example"])]).unwrap();
        save(&conn, &[contact(home, "b", "Owen M.", &["o@b.example"])]).unwrap();
        let ends = ((work, "a".to_string()), (home, "b".to_string()));
        link(
            &conn,
            &Link { a: ends.0.clone(), b: ends.1.clone(), shown_a: false },
        )
        .unwrap();
        let found = links(&conn).unwrap();
        assert_eq!(found.len(), 1);
        assert!(!found[0].shown_a);
        // Unlinking works in the other order, too.
        unlink(&conn, &ends.1, &ends.0).unwrap();
        assert!(links(&conn).unwrap().is_empty());
        link(&conn, &Link { a: ends.0.clone(), b: ends.1.clone(), shown_a: true }).unwrap();
        forget(&conn, home, &["b".to_string()]).unwrap();
        assert!(links(&conn).unwrap().is_empty());
        let count: i64 = conn
            .query_row("SELECT count(*) FROM contact_links", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "deleting a contact takes its link");
    }

    #[test]
    fn a_link_survives_a_refresh_that_replaces_its_contacts() {
        let conn = open_in_memory().unwrap();
        let work = accounts::insert_account(&conn, "dana@example.com", 0).unwrap();
        let home = accounts::insert_account(&conn, "dana@fernwood.example", 0).unwrap();
        let (a, b) = (
            contact(work, "a", "Owen Mercer", &["o@a.example"]),
            contact(home, "b", "Owen M.", &["o@b.example"]),
        );
        save(&conn, &[a.clone(), b.clone()]).unwrap();
        link(
            &conn,
            &Link { a: (work, "a".into()), b: (home, "b".into()), shown_a: true },
        )
        .unwrap();
        save(&conn, &[a, b]).unwrap();
        assert_eq!(links(&conn).unwrap().len(), 1);
    }

    fn mail(conn: &Connection, account: AccountId, id: &str, from: &str, to: &str, date: i64) {
        conn.execute(
            "INSERT INTO messages (account_id, id, thread_id, from_addr, to_addrs, cc_addrs, subject, date, snippet, size, has_attachments, sync_gen) \
             VALUES (?1, ?2, ?3, ?4, ?5, '', 'Re', ?6, '', 1, 0, 0)",
            params![account, id, id.split('/').next().unwrap(), from, to, date],
        )
        .unwrap();
    }

    #[test]
    fn mail_with_counts_conversations_and_finds_what_the_owner_wrote() {
        let conn = open_in_memory().unwrap();
        let me = accounts::insert_account(&conn, "dana@example.com", 0).unwrap();
        mail(&conn, me, "t1/1", "ana@x.example", "dana@example.com", 100);
        mail(&conn, me, "t1/2", "dana@example.com", "Ana@X.example", 200);
        mail(&conn, me, "t2/1", "ana@x.example", "dana@example.com", 300);
        mail(&conn, me, "t3/1", "other@x.example", "dana@example.com", 400);
        let found = mail_with(&conn, &["ana@x.example".into()]).unwrap();
        assert_eq!(found.conversations, 2);
        assert_eq!(found.last.as_ref().map(|l| l.3), Some(300));
        assert!(!found.last_mine);
        assert_eq!(found.last_wrote, Some(200));
        let counts = message_counts(&conn, &["ANA@x.example".into()]).unwrap();
        assert_eq!(counts.get("ana@x.example"), Some(&2));
        assert_eq!(counts.get("other@x.example"), None);
    }
}
