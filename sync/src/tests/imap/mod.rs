//! The IMAP adapter against `FakeImap`, through the engine.

mod feed;
mod mailboxes;
mod window;

use mailrs_domain::EpochMillis;
use mailrs_imap::Capabilities;

use crate::fake::FakeImap;

/// A plain message from Ann with `extra` header lines, each ending in
/// CRLF, above the blank line.
pub(super) fn message(id: &str, subject: &str, extra: &str) -> Vec<u8> {
    format!(
        "From: Ann <ann@example.com>\r\nTo: me@example.com\r\nSubject: {subject}\r\n\
         Message-ID: <{id}@example.com>\r\nDate: Thu, 24 Sep 2026 09:00:00 +0000\r\n\
         {extra}\r\nHi.\r\n"
    )
    .into_bytes()
}

/// `days` days before now.
pub(super) fn days_ago(days: i64) -> EpochMillis {
    crate::now_millis() - days * 24 * 60 * 60 * 1000
}

/// A fresh fake server offering what `FakeImap::new()` offers, with
/// `change` made to its capabilities.
pub(super) fn offering(change: impl FnOnce(&mut Capabilities)) -> FakeImap {
    let imap = FakeImap::new();
    imap.with(|s| change(&mut s.capabilities));
    imap
}
