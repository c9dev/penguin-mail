//! Signing in to Gmail with an app password: reading what was pasted,
//! checking Gmail's servers took it, and keeping the account.

use std::sync::Arc;

use mailrs_domain::{Account, AccountState, EpochMillis, MailRoute, Provider};
use mailrs_imap::{CheckError, Checked, ImapClient, Login};
use mailrs_store::servers::{self, Saved, Security, Servers};
use mailrs_store::{Db, accounts};

use super::{ImapSignInError, save_passwords, taken_error};
use crate::passwords::PasswordStore;

/// What a person pasted, read as Google prints an app password: sixteen
/// letters, shown in groups of four.
#[derive(Clone, PartialEq, Eq)]
pub enum Pasted {
    AppPassword(String),
    /// Anything else, kept as typed without the spaces around it.
    Other(String),
}

impl std::fmt::Debug for Pasted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Pasted::AppPassword(_) => "Pasted::AppPassword(..)",
            Pasted::Other(_) => "Pasted::Other(..)",
        })
    }
}

/// Reads `typed`. Every white space character goes, the non-breaking
/// spaces and line breaks a phone or a password manager adds included;
/// sixteen ASCII letters left are an app password, in lower case.
pub fn read_pasted(typed: &str) -> Pasted {
    let squeezed: String = typed.chars().filter(|c| !c.is_whitespace()).collect();
    if squeezed.len() == 16 && squeezed.chars().all(|c| c.is_ascii_alphabetic()) {
        Pasted::AppPassword(squeezed.to_ascii_lowercase())
    } else {
        Pasted::Other(typed.trim().to_string())
    }
}

impl Pasted {
    /// What goes to the server.
    pub fn secret(&self) -> &str {
        match self {
            Pasted::AppPassword(secret) | Pasted::Other(secret) => secret,
        }
    }

    /// The paste is not sixteen letters, so it is probably the person's
    /// own password. It still goes to Gmail, which has the last word.
    pub fn looks_wrong(&self) -> bool {
        matches!(self, Pasted::Other(_))
    }
}

/// Gmail's servers as the store keeps them, with the mailbox that holds
/// all the mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmailChecked {
    pub servers: Servers,
    /// The name the server gives All Mail, in its own encoding.
    pub all_mail: String,
}

#[derive(Debug, thiserror::Error)]
pub enum GmailCheckError {
    #[error(transparent)]
    Check(#[from] CheckError),
    /// The server took the login but does not speak Gmail's extensions.
    #[error("the server is not Gmail's")]
    NotGmail,
    /// Gmail leaves All Mail out of IMAP until the person turns it on.
    #[error("All Mail is hidden from IMAP")]
    AllMailHidden,
}

/// Signs in to Gmail's servers with `password`, and looks for All Mail.
pub async fn check_gmail(address: &str, password: &str) -> Result<GmailChecked, GmailCheckError> {
    let google = mailrs_discover::google_servers().ok_or(GmailCheckError::NotGmail)?;
    let checked = mailrs_imap::check(
        &google.imap,
        &google.smtp,
        address,
        address,
        password,
        password,
    )
    .await?;
    if !checked.capabilities.gmail {
        return Err(GmailCheckError::NotGmail);
    }
    let client = ImapClient::new(
        google.imap.clone(),
        Login::new(checked.imap_user.as_str(), password),
    );
    let listed = client.list().await.map_err(CheckError::Imap)?;
    judge(&google, &checked, &listed)
}

/// What a signed-in check and a mailbox listing say about Gmail.
fn judge(
    google: &mailrs_discover::GoogleServers,
    checked: &Checked,
    listed: &[mailrs_imap::Listed],
) -> Result<GmailChecked, GmailCheckError> {
    if !checked.capabilities.gmail {
        return Err(GmailCheckError::NotGmail);
    }
    let all_mail = listed
        .iter()
        .find(|mailbox| mailbox.special_use == Some(mailrs_imap::SpecialUse::All))
        .map(|mailbox| mailbox.name.clone())
        .ok_or(GmailCheckError::AllMailHidden)?;
    let saved = |server: &mailrs_discover::Server, user: &str| Saved {
        host: server.host.clone(),
        port: server.port,
        security: match server.security {
            mailrs_discover::Security::Tls => Security::Tls,
            mailrs_discover::Security::StartTls => Security::StartTls,
        },
        user_name: user.to_string(),
    };
    Ok(GmailChecked {
        servers: Servers {
            imap: saved(&google.imap, &checked.imap_user),
            smtp: saved(&google.smtp, &checked.smtp_user),
        },
        all_mail,
    })
}

/// A Gmail account whose login worked. No `Debug`: the password must not
/// reach a log line.
pub struct NewGmailImap {
    pub address: String,
    pub servers: Servers,
    pub password: String,
}

/// Keeps a Gmail account that reads its mail over IMAP. A new address
/// becomes a Gmail account on the IMAP route. An address a Gmail account
/// on that route holds already is a sign-in again: the new password and
/// servers replace the old, and Needs Sign-In ends. Any other holder
/// makes the address taken, a Gmail account on the API route included:
/// that one changes route through the move.
pub async fn gmail_imap_signed_in<P: PasswordStore + 'static>(
    db: &Db,
    passwords: Arc<P>,
    new: NewGmailImap,
    now: EpochMillis,
) -> Result<Account, ImapSignInError> {
    let NewGmailImap {
        address,
        servers,
        password,
    } = new;
    let email = address.clone();
    let before = db
        .read(move |c| {
            let held = accounts::account_by_email(c, &email)?;
            let route = held
                .as_ref()
                .map(|held| accounts::mail_route(c, held.id))
                .transpose()?;
            Ok((held, route))
        })
        .await?;
    match before {
        (Some(held), Some(MailRoute::Imap)) if held.provider == Provider::Gmail => {
            save_passwords(Arc::clone(&passwords), held.id, password, None).await?;
            let (id, again) = (held.id, held.state);
            db.write(move |c| {
                servers::save(c, id, &servers)?;
                if again == AccountState::NeedsReauth {
                    accounts::set_state(c, id, AccountState::Ok)?;
                }
                Ok(())
            })
            .await?;
        }
        (Some(held), _) => {
            return Err(taken_error(&address, held.provider_name()));
        }
        (None, _) => {
            let (email, kept) = (address.clone(), servers);
            let id = db
                .write(move |c| {
                    let id = accounts::insert_account(c, &email, now)?;
                    accounts::set_mail_route(c, id, MailRoute::Imap)?;
                    servers::save(c, id, &kept)?;
                    Ok(id)
                })
                .await?;
            if let Err(err) = save_passwords(Arc::clone(&passwords), id, password, None).await {
                db.write(move |c| accounts::delete_account(c, id)).await?;
                return Err(err.into());
            }
        }
    }
    db.read(move |c| accounts::account_by_email(c, &address))
        .await?
        .ok_or(ImapSignInError::Store(mailrs_store::StoreError::Sqlite(
            rusqlite::Error::QueryReturnedNoRows,
        )))
}

#[cfg(test)]
mod tests {
    use mailrs_discover::google_servers;
    use mailrs_imap::{Capabilities, Listed, SpecialUse};

    use super::*;
    use crate::passwords::MemoryPasswords;

    fn store() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mail.db")).unwrap();
        (dir, db)
    }

    fn servers() -> Servers {
        let saved = |host: &str, port| Saved {
            host: host.into(),
            port,
            security: Security::Tls,
            user_name: "me@gmail.com".into(),
        };
        Servers {
            imap: saved("imap.gmail.com", 993),
            smtp: saved("smtp.gmail.com", 465),
        }
    }

    fn new_account(password: &str) -> NewGmailImap {
        NewGmailImap {
            address: "me@gmail.com".into(),
            servers: servers(),
            password: password.into(),
        }
    }

    fn checked(gmail: bool) -> Checked {
        let mut capabilities = Capabilities::all();
        capabilities.gmail = gmail;
        Checked {
            imap_user: "me@gmail.com".into(),
            smtp_user: "me@gmail.com".into(),
            capabilities,
        }
    }

    #[test]
    fn a_paste_with_nonbreaking_spaces_reads_as_sixteen_letters() {
        assert_eq!(
            read_pasted("abcd\u{a0}efgh ijkl\tMNOP\n"),
            Pasted::AppPassword("abcdefghijklmnop".into())
        );
    }

    #[test]
    fn a_normal_password_goes_as_typed_with_a_note() {
        let pasted = read_pasted("hunter2 ");
        assert_eq!(pasted, Pasted::Other("hunter2".into()));
        assert!(pasted.looks_wrong());
        assert_eq!(pasted.secret(), "hunter2");
    }

    #[test]
    fn fifteen_letters_are_not_an_app_password() {
        let pasted = read_pasted("abcd efgh ijkl mno");
        assert!(pasted.looks_wrong());
        assert!(!read_pasted("abcdefghijklmnop").looks_wrong());
    }

    #[test]
    fn a_server_without_x_gm_ext_1_is_not_gmail() {
        let google = google_servers().unwrap();
        let all = Listed::new("[Gmail]/All Mail", Some('/'), Some(SpecialUse::All), false);
        let err = judge(&google, &checked(false), &[all]).unwrap_err();
        assert!(matches!(err, GmailCheckError::NotGmail));
    }

    #[test]
    fn no_all_mailbox_means_all_mail_is_hidden() {
        let google = google_servers().unwrap();
        let inbox = Listed::new("INBOX", Some('/'), None, false);
        let err = judge(&google, &checked(true), &[inbox]).unwrap_err();
        assert!(matches!(err, GmailCheckError::AllMailHidden));
    }

    #[test]
    fn the_all_mailbox_is_found_by_its_special_use_whatever_its_name() {
        let google = google_servers().unwrap();
        let all = Listed::new("[Gmail]/Todos", Some('/'), Some(SpecialUse::All), false);
        let found = judge(&google, &checked(true), &[all]).unwrap();
        assert_eq!(found.all_mail, "[Gmail]/Todos");
        assert_eq!(found.servers, servers());
    }

    #[tokio::test]
    async fn a_gmail_imap_account_keeps_its_route_servers_and_password() {
        let (_dir, db) = store();
        let passwords = Arc::new(MemoryPasswords::default());
        let account =
            gmail_imap_signed_in(&db, passwords.clone(), new_account("abcdefghijklmnop"), 5)
                .await
                .unwrap();
        let id = account.id;
        assert_eq!(account.provider, Provider::Gmail);
        assert_eq!(
            db.read(move |c| accounts::mail_route(c, id)).await.unwrap(),
            MailRoute::Imap
        );
        assert_eq!(
            db.read(move |c| servers::load(c, id)).await.unwrap(),
            Some(servers())
        );
        assert_eq!(
            passwords.load(id).unwrap().as_deref(),
            Some("abcdefghijklmnop")
        );
    }

    #[tokio::test]
    async fn signing_in_again_replaces_the_password_and_ends_needs_sign_in() {
        let (_dir, db) = store();
        let passwords = Arc::new(MemoryPasswords::default());
        let first =
            gmail_imap_signed_in(&db, passwords.clone(), new_account("aaaaaaaaaaaaaaaa"), 5)
                .await
                .unwrap();
        let id = first.id;
        db.write(move |c| accounts::set_state(c, id, AccountState::NeedsReauth))
            .await
            .unwrap();
        let again =
            gmail_imap_signed_in(&db, passwords.clone(), new_account("bbbbbbbbbbbbbbbb"), 6)
                .await
                .unwrap();
        assert_eq!((again.id, again.state), (id, AccountState::Ok));
        assert_eq!(
            passwords.load(id).unwrap().as_deref(),
            Some("bbbbbbbbbbbbbbbb")
        );
    }

    #[tokio::test]
    async fn an_api_route_address_is_taken() {
        let (_dir, db) = store();
        db.write(|c| accounts::insert_account(c, "me@gmail.com", 0))
            .await
            .unwrap();
        let err = gmail_imap_signed_in(
            &db,
            Arc::new(MemoryPasswords::default()),
            new_account("abcdefghijklmnop"),
            5,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ImapSignInError::Taken { .. }));
    }

    #[tokio::test]
    async fn a_keyring_that_refuses_leaves_no_new_account() {
        let (_dir, db) = store();
        let passwords = Arc::new(MemoryPasswords::default());
        passwords.refuse_saves();
        let err = gmail_imap_signed_in(&db, passwords, new_account("abcdefghijklmnop"), 5)
            .await
            .unwrap_err();
        assert!(matches!(err, ImapSignInError::Password(_)));
        let held = db
            .read(|c| accounts::account_by_email(c, "me@gmail.com"))
            .await
            .unwrap();
        assert_eq!(held, None);
    }
}
