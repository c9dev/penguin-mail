//! SQLite storage for mailrs. Query functions take a `&Connection`.

pub mod accounts;
pub mod bodies;
mod error;
pub mod labels;
pub mod messages;
mod schema;
pub mod threads;
pub mod window;

pub use error::{Result, StoreError};
pub use schema::{open_connection, open_in_memory, schema_version};
