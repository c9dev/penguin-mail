//! Gmail REST client: OAuth, quota limiting, and conversion to Penguin Mail domain types.

pub mod address;
pub mod body;
pub mod calendar;
mod client;
pub mod convert;
mod error;
pub mod limiter;
pub mod model;
mod oauth;
pub mod people;
pub mod provenance;
mod token_store;

pub use calendar::{
    Answered, Busy, CALENDAR_API_BASE, CALENDAR_SCOPE, Event, EventFields, EventTime, Guest, Series,
};
pub use client::{
    Authorized, BATCH_LIMIT, GMAIL_API_BASE, GmailClient, authorize, cost, one_click_unsubscribe,
};
pub use convert::{HistoryChange, HistoryPage, html_to_text};
pub use error::GmailError;
pub use limiter::{AccountQuota, Priority, QuotaLimiter, QuotaPool, Waiting};
pub use model::{Draft, LabelColor, MessagePage, MessageRef, Profile, RemoteLabel, SendAs};
pub use oauth::{
    AccessToken, DELETE_SCOPE, GMAIL_SCOPE, LoopbackListener, OAuthClient, Pkce, SETTINGS_SCOPE,
    Tokens, parse_redirect, random_token,
};
pub use people::{
    CONTACTS_SCOPE, CONTACTS_WRITE_SCOPE, ConnectionsPage, ContactFields, Person,
};
pub use token_store::{KeyringTokenStore, MemoryTokenStore, TokenStore};
