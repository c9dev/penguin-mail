//! WebDAV, CalDAV and CardDAV for an IMAP account's calendars and
//! contacts: the XML the protocols speak, a client on reqwest behind
//! `DavApi`, and the iCalendar and vCard mapping onto Penguin Mail's own
//! events and contacts through calcard. It knows nothing of the store or
//! the window.

pub mod client;
mod error;
pub mod ical;
pub mod ids;
pub mod vcard;
pub mod xml;
mod zones;

/// A CalDAV and CardDAV server in memory. The crate's own tests always
/// have it; anyone else asks for the `fake` feature.
#[cfg(any(test, feature = "fake"))]
pub mod fake;

pub use error::DavError;

/// The most bytes one multistatus answer may hold. A calendar's etag
/// listing of ten thousand events is about 1.5 MB.
pub const MOST_BYTES: usize = 4 << 20;

/// The most bytes one calendar resource or vCard may hold.
pub const MOST_RESOURCE_BYTES: usize = 1 << 20;

/// How many resources one multiget asks for.
pub const MULTIGET_BATCH: usize = 50;
