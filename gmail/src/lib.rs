//! Gmail REST client: OAuth, quota limiting, and conversion to mailrs domain types.

pub mod address;
pub mod convert;
mod error;
pub mod model;

pub use convert::{HistoryChange, HistoryPage};
pub use error::GmailError;
pub use model::{MessagePage, MessageRef, Profile, RemoteLabel};
