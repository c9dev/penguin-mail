//! The IMAP adapter against `FakeImap`, through the engine.

mod bodies;
mod feed;
mod mailboxes;
mod sending;
mod uidvalidity;
mod window;
mod writes;

use std::sync::Arc;

use mailrs_domain::EpochMillis;
use mailrs_imap::Capabilities;

use crate::fake::{FakeImap, FakeSmtp};
use crate::services::Imap;
use crate::tests::fake_settings;

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

/// The adapter alone on `imap`, with no engine or store around it, for a
/// test that measures what one call costs.
pub(super) fn adapter(imap: FakeImap) -> (Arc<FakeImap>, Imap<FakeImap, FakeSmtp>) {
    let imap = Arc::new(imap);
    let adapter = Imap::new(Arc::clone(&imap), Arc::new(FakeSmtp::default()), fake_settings());
    (imap, adapter)
}

/// Fills `mailbox` with `count` small unread messages from yesterday.
pub(super) fn fill(imap: &FakeImap, mailbox: &str, count: u32) {
    let date = days_ago(1);
    for _ in 0..count {
        imap.deliver(mailbox, b"Subject: Note\r\n\r\nHi.\r\n".to_vec(), date);
    }
}
