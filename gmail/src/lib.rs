//! Gmail REST client: OAuth, quota limiting, and conversion to mailrs domain types.

mod error;
pub mod model;

pub use error::GmailError;
pub use model::{MessagePage, MessageRef, Profile, RemoteLabel};
