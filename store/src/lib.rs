//! SQLite storage for mailrs. Query functions take a `&Connection`; `Db` runs
//! them on the writer thread or on pooled readers.

pub mod accounts;
pub mod bodies;
mod db;
mod error;
pub mod labels;
pub mod messages;
mod schema;
pub mod threads;
pub mod window;

pub use db::Db;
pub use error::{Result, StoreError};
pub use schema::{open_connection, open_in_memory, schema_version};
