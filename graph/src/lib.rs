//! Microsoft Graph as Penguin Mail uses it: the HTTP core that talks only
//! to graph.microsoft.com, Microsoft's sign-in, and (from Task 7) the
//! models and calls the Microsoft adapter in `mailrs-sync` makes.

pub mod auth;
pub mod calendar;
mod error;
mod http;
pub mod mail;
pub mod model;
pub mod people;
pub mod settings;

pub use auth::{
    AccessToken, Authorized, Granted, IdToken, Loopback, Me, MicrosoftClient, PERSONAL_TENANT,
    Pkce, SCOPES, Session, Tenant, Tokens, authorize, built_in_client, client_from, parse_redirect,
    random_token,
};
pub use calendar::{
    Attendee, CALENDAR_COLORS, GraphCalendar, GraphEvent, Location, OnlineMeeting,
    PatternedRecurrence, RecurrencePattern, RecurrenceRange, Response, ResponseStatus,
    nearest_color,
};
pub use error::{GraphError, classify};
pub use http::{
    BATCH_LIMIT, BatchRequest, BatchResponse, DeltaPage, GRAPH_BASE, Graph, MAX_INLINE_WAIT,
    Method, PAGE_SIZE, Page,
};
pub use mail::{
    AttachmentInfo, ExtendedProperty, Fields, Flag, Header, Listing, MailFolder, MasterCategory,
    Message, MessageBody, MessagePatch, Override, SIZE_PROPERTY, Write,
};
pub use model::{DateTimeZone, EmailAddress, ItemBody, Recipient, Removed};
pub use people::{ContactFolder, GraphContact};
pub use settings::{AutomaticReplies, MessageRule, RuleActions, RulePredicates};
