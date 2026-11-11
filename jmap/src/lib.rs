//! JMAP (RFC 8620, RFC 8621) as Penguin Mail uses it: the session, method
//! calls batched in one request with back-references, blobs under a cap,
//! and the sign-in check. The client sends the account's credentials only
//! to the hosts the session names, and every answer is read under a cap.
//! The mail adapter in `mailrs-sync` maps what this crate answers onto the
//! services every provider shares.

mod auth;
pub mod blob;
pub mod calendars;
mod check;
pub mod contacts;
pub mod error;
pub mod http;
pub mod identity;
pub mod mail;
pub mod masked;
pub mod mdn;
pub mod oauth;
pub mod push;
pub mod quota;
mod request;
pub mod session;
pub mod sieve;
pub mod submission;
pub mod vacation;

pub use auth::{Auth, AuthKind};
pub use blob::{MOST_BLOB_BYTES, Uploaded};
pub use calendars::{
    CYRUS_JSCALENDAR_BIS, CalendarEvents, CalendarRights, EventWrite, EventsPage, JmapCalendar,
    fits,
};
pub use check::{Checked, Offered, check, same_server};
pub use contacts::{AddressBook, CardChanges, CardWrite, CardsPage};
pub use error::JmapError;
pub use http::{JmapClient, PushLimits};
pub use identity::{EmailAddress, Identity};
pub use mail::{
    BODY, BodyPart, ChangesResponse, Email, EmailQuery, Got, Header, Hit, Imported, Listed, META,
    MailChanges, Mailbox, MailboxWrite, PLACES, Queried, Saved, SetError, SetResponse, Submission,
    Thread, Threads, pointer,
};
pub use masked::{MOST_MASKED, MaskedEmail, MaskedState};
pub use mdn::{MDN, MdnOut};
pub use push::{Pushed, StateChange};
pub use quota::{QUOTA, Quota, mail_storage};
pub use request::{CallId, Request, Response};
pub use session::{
    CALENDARS, CONTACTS, CORE, Limits, MAIL, MASKED_EMAIL, MOST_AT_ONCE, SIEVE, SUBMISSION,
    Session, SessionAccount, VACATION,
};
pub use sieve::{MOST_SIEVE_BYTES, SieveScript};
pub use submission::{
    Canceled, Envelope, EnvelopeAddress, HELD, HeldSubmission, Hold, SubmissionState,
    max_delayed_send,
};
pub use vacation::{VacationPatch, VacationResponse};
