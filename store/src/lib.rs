//! SQLite storage for mailrs. Query functions take a `&Connection`.

pub mod accounts;
mod error;
mod schema;

pub use error::{Result, StoreError};
pub use schema::{open_connection, open_in_memory, schema_version};
