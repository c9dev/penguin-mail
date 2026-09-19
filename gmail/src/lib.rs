//! Gmail REST client: OAuth, quota limiting, and conversion to Penguin Mail domain types.

pub mod address;
pub mod body;
mod client;
pub mod convert;
mod error;
mod limiter;
pub mod model;
mod oauth;
mod token_store;

pub use client::{Authorized, GMAIL_API_BASE, GmailClient, authorize, one_click_unsubscribe};
pub use convert::{HistoryChange, HistoryPage, html_to_text};
pub use error::GmailError;
pub use limiter::QuotaLimiter;
pub use model::{Draft, LabelColor, MessagePage, MessageRef, Profile, RemoteLabel, SendAs};
pub use oauth::{
    AccessToken, GMAIL_SCOPE, LoopbackListener, OAuthClient, Pkce, SETTINGS_SCOPE, Tokens,
    parse_redirect, random_token,
};
pub use token_store::{KeyringTokenStore, MemoryTokenStore, TokenStore};
