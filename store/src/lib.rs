//! SQLite storage for Penguin Mail. Query functions take a `&Connection`; `Db` runs
//! them on the writer thread or on pooled readers.

pub mod accounts;
pub mod address_book;
pub mod bodies;
pub mod calendar;
pub mod calendar_list;
pub mod contacts;
mod db;
pub mod drafts;
mod error;
pub mod event_reminders;
pub mod flags;
pub mod follow_ups;
pub mod image_senders;
pub mod invitations;
pub mod labels;
pub mod local_messages;
pub mod local_rules;
pub mod mailboxes;
pub mod messages;
pub mod newsletters;
pub mod outbox;
pub mod pop3;
pub mod query;
pub mod reminders;
pub mod remote_refs;
pub mod rule_changes;
mod schema;
pub mod servers;
pub mod services;
pub mod templates;
pub mod threading;
pub mod threads;
pub mod unsubscribes;
pub mod window;

pub use db::Db;
pub use error::{Result, StoreError};
pub use schema::{open_connection, open_in_memory, schema_version};

#[cfg(test)]
pub(crate) mod testing {
    use mailrs_domain::{AccountId, MailboxKind, RemoteMailbox};
    use rusqlite::Connection;

    use crate::mailboxes;

    /// Lists Gmail's role mailboxes, as a bootstrap's first listing does,
    /// so mail filed in them lists under their roles.
    pub(crate) fn list_gmail_roles(conn: &Connection, account_id: AccountId) {
        for (id, role) in mailrs_gmail::labels::ROLES {
            let mailbox = RemoteMailbox {
                id: id.into(),
                name: id.into(),
                kind: MailboxKind::System,
                role: Some(role),
                color: None,
                hidden: false,
            };
            mailboxes::upsert(conn, account_id, &mailbox).unwrap();
        }
    }
}
