//! IMAP accounts. The adapter reaches the server through the traits in
//! [`api`], served by `mailrs_imap`'s clients and by the fakes.

mod api;

pub use api::{ImapApi, Submit};
