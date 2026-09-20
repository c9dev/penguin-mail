//! SQLite storage for Penguin Mail. Query functions take a `&Connection`; `Db` runs
//! them on the writer thread or on pooled readers.

pub mod accounts;
pub mod address_book;
pub mod bodies;
pub mod contacts;
mod db;
mod error;
pub mod flags;
pub mod follow_ups;
pub mod image_senders;
pub mod invitations;
pub mod labels;
pub mod messages;
pub mod outbox;
pub mod reminders;
mod schema;
pub mod templates;
pub mod threads;
pub mod window;

pub use db::Db;
pub use error::{Result, StoreError};
pub use schema::{open_connection, open_in_memory, schema_version};
