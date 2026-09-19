//! Gmail REST client: OAuth, quota limiting, and conversion to mailrs domain types.

pub mod address;
pub mod body;
mod client;
pub mod convert;
mod error;
mod limiter;
pub mod model;
mod oauth;

pub use client::{Authorized, GMAIL_API_BASE, GmailClient, authorize};
pub use convert::{HistoryChange, HistoryPage};
pub use error::GmailError;
pub use limiter::QuotaLimiter;
pub use model::{MessagePage, MessageRef, Profile, RemoteLabel};
pub use oauth::{
    AccessToken, GMAIL_SCOPE, LoopbackListener, OAuthClient, Pkce, Tokens, parse_redirect, random_token,
};
