//! Gmail REST client: OAuth, quota limiting, and conversion to mailrs domain types.

pub mod address;
pub mod body;
pub mod convert;
mod error;
pub mod model;
mod oauth;

pub use convert::{HistoryChange, HistoryPage};
pub use error::GmailError;
pub use model::{MessagePage, MessageRef, Profile, RemoteLabel};
pub use oauth::{
    AccessToken, GMAIL_SCOPE, LoopbackListener, OAuthClient, Pkce, Tokens, parse_redirect, random_token,
};
