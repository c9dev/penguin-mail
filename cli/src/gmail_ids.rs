//! `gmail-ids`: checks that Gmail's X-GM-MSGID, in hex, is the id the Gmail
//! API stored for the same message. It prints ids, verdicts and links, never
//! a subject, a sender or a body.

use std::collections::HashMap;
use std::io::BufRead;

use anyhow::{Context, Result, bail};
use mailrs_domain::Provider;
use mailrs_imap::{ImapClient, Login, SpecialUse, UidSet};
use mailrs_store::{Db, accounts, messages};

/// How the stored copy of a message compares with what Gmail's IMAP gave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The stored row under this id has the fetched Message-ID.
    Same,
    /// The stored row under this id has another Message-ID.
    Differs,
    /// No stored row under this id, or none that has a Message-ID.
    Absent,
}

impl Verdict {
    fn word(self) -> &'static str {
        match self {
            Verdict::Same => "same",
            Verdict::Differs => "differs",
            Verdict::Absent => "absent",
        }
    }
}

/// Compares the Message-ID the store holds under an id with the one fetched
/// for that id, ignoring angle brackets and surrounding spaces.
pub fn verdict(stored: Option<&str>, fetched: Option<&str>) -> Verdict {
    let bare = |id: &str| {
        id.trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .to_string()
    };
    match (stored, fetched) {
        (None, _) => Verdict::Absent,
        (Some(stored), Some(fetched)) if bare(stored) == bare(fetched) => Verdict::Same,
        (Some(_), _) => Verdict::Differs,
    }
}

/// 0 when at least five messages matched and none differed, 1 when any
/// differed, 2 when too few could be compared.
pub fn exit_code(same: usize, differs: usize) -> u8 {
    match (same, differs) {
        (_, 1..) => 1,
        (5.., 0) => 0,
        _ => 2,
    }
}

/// Reads the app password from one line of stdin.
fn read_password() -> Result<String> {
    eprintln!("App password for Gmail, then Enter:");
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let password: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    if password.is_empty() {
        bail!("no password on stdin");
    }
    Ok(password)
}

pub async fn run(db: &Db, address: &str, count: usize) -> Result<u8> {
    let owned = address.to_string();
    let account = db
        .read(move |c| accounts::account_by_email(c, &owned))
        .await?
        .with_context(|| format!("no account {address} in the store"))?;
    if account.provider != Provider::Gmail {
        bail!("{address} is not a Gmail account");
    }
    let client = ImapClient::new(
        mailrs_discover::Server {
            host: "imap.gmail.com".into(),
            port: 993,
            security: mailrs_discover::Security::Tls,
            user_name: mailrs_discover::UserName::Address,
        },
        Login::new(address, read_password()?),
    );
    let all = client
        .list()
        .await?
        .into_iter()
        .find(|m| m.special_use == Some(SpecialUse::All))
        .context(
            "Gmail lists no All Mail folder; tick Show in IMAP for it in Gmail's Labels settings",
        )?;
    let uids = client.search(&all.name, "ALL").await?;
    let newest = &uids[uids.len().saturating_sub(count)..];
    let fetched = client
        .gmail_headers(&all.name, &UidSet::from_uids(newest.iter().copied()))
        .await?;
    let ids: Vec<String> = fetched
        .iter()
        .filter_map(|f| f.gmail.as_ref().map(|g| g.id()))
        .collect();
    let account_id = account.id;
    let wanted = ids.clone();
    let stored: HashMap<String, Option<String>> = db
        .read(move |c| messages::by_ids(c, account_id, &wanted))
        .await?
        .into_iter()
        .map(|m| (m.id, m.rfc822_msgid))
        .collect();
    let (mut same, mut differs) = (0, 0);
    for item in &fetched {
        let Some(gmail) = &item.gmail else { continue };
        let id = gmail.id();
        let held = stored.get(&id).and_then(|m| m.as_deref());
        let verdict = verdict(held, item.message_id.as_deref());
        match verdict {
            Verdict::Same => same += 1,
            Verdict::Differs => differs += 1,
            Verdict::Absent => {}
        }
        println!("{id} {} {}", gmail.thread_id(), verdict.word());
        println!("  https://mail.google.com/mail/u/0/#all/{id}");
    }
    println!("same {same}, differs {differs}, compared {}", fetched.len());
    Ok(exit_code(same, differs))
}

#[cfg(test)]
mod tests {
    use super::{Verdict, exit_code, verdict};

    #[test]
    fn a_stored_row_with_the_same_message_id_is_same() {
        assert_eq!(verdict(Some("a@b.pt"), Some("<a@b.pt>")), Verdict::Same);
    }

    #[test]
    fn a_stored_row_with_another_message_id_differs() {
        assert_eq!(verdict(Some("a@b.pt"), Some("<c@d.pt>")), Verdict::Differs);
    }

    #[test]
    fn no_row_is_absent() {
        assert_eq!(verdict(None, Some("<a@b.pt>")), Verdict::Absent);
    }

    #[test]
    fn five_matches_and_no_difference_exit_zero() {
        assert_eq!(exit_code(5, 0), 0);
        assert_eq!(exit_code(20, 1), 1);
        assert_eq!(exit_code(4, 0), 2);
        assert_eq!(exit_code(0, 0), 2);
    }
}
