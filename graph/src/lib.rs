//! Microsoft Graph as Penguin Mail uses it: the HTTP core that talks only
//! to graph.microsoft.com, Microsoft's sign-in, and (from Task 7) the
//! models and calls the Microsoft adapter in `mailrs-sync` makes.

pub mod auth;
mod error;
mod http;

pub use auth::{
    AccessToken, Authorized, Granted, IdToken, Loopback, Me, MicrosoftClient, PERSONAL_TENANT,
    Pkce, SCOPES, Session, Tenant, Tokens, authorize, built_in_client, client_from, parse_redirect,
    random_token,
};
pub use error::{GraphError, classify};
pub use http::{
    BATCH_LIMIT, BatchRequest, BatchResponse, DeltaPage, GRAPH_BASE, Graph, MAX_INLINE_WAIT,
    Method, PAGE_SIZE, Page,
};
