//! Gmail REST client: OAuth, quota limiting, and conversion to Penguin Mail domain types.

pub mod calendar;
mod client;
pub mod convert;
mod error;
pub mod labels;
pub mod limiter;
pub mod model;
mod oauth;
pub mod people;
pub mod query;
pub mod structure;
mod token_store;

pub use calendar::{
    Answered, Busy, CALENDAR_API_BASE, CALENDAR_LIST_SCOPE, CALENDAR_SCOPE, Event, EventFields,
    EventTime, Guest, Series, google_event,
};
pub use client::{
    Authorized, BATCH_LIMIT, GMAIL_API_BASE, GmailClient, authorize, cost, one_click_unsubscribe,
};
pub use convert::{HistoryChange, HistoryPage};
pub use error::{GmailError, OneClickError};
pub use limiter::{AccountQuota, Priority, QuotaLimiter, QuotaPool, Waiting};
pub use model::{
    Draft, LabelColor, MessagePage, MessageRef, Profile, RemoteLabel, SendAs, is_reserved_label_name,
};
pub use oauth::{
    AccessToken, DELETE_SCOPE, GMAIL_SCOPE, LoopbackListener, OAuthClient, Pkce, SETTINGS_SCOPE,
    Tokens, built_in_client, built_in_microsoft_client_id, client_from, parse_redirect,
    random_token,
};
pub use people::{CONTACTS_SCOPE, CONTACTS_WRITE_SCOPE, ConnectionsPage, ContactFields, Person};
pub use token_store::{KeyringTokenStore, MemoryTokenStore, TokenStore};

#[cfg(test)]
pub(crate) mod tests {
    use mailrs_domain::MessageMeta;

    /// A message in account 1 with nothing in it or on it.
    pub(crate) fn blank_meta() -> MessageMeta {
        MessageMeta {
            account_id: 1,
            id: "m1".into(),
            thread_id: "t1".into(),
            rfc822_msgid: None,
            from: None,
            to: vec![],
            cc: vec![],
            subject: String::new(),
            date: 0,
            snippet: String::new(),
            size: 0,
            has_attachments: false,
            held: Default::default(),
            roles: vec![],
            list_unsubscribe: None,
            one_click: false,
        }
    }
}
