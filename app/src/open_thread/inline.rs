//! The pictures an HTML body names by `cid:`, as the open thread holds them.
//!
//! The page asks for each by an address of the app's own scheme,
//! `mailrs-cid:<account>/<message>/<version>/<content id>`, and the view
//! answers from here. The bytes never enter the page itself: as `data:`
//! URIs, eight 900 KB pictures made a 2.9 MB thread a 12.6 MB document,
//! and every page load sent all of it to WebKit again.
//!
//! A body's text goes on screen first. Its pictures arrive later, as a
//! named change of their own, and until then the page's requests for them
//! wait. A message whose pictures have arrived is settled: a picture it
//! lacks is missing for good.
//!
//! The version is in the address because WebKit keeps a picture it was
//! given under its address for as long as the page lives, and longer. An
//! engine that opens a signed or encrypted message replaces its pictures
//! with the ones from the part it checked, and those must not be answered
//! from what WebKit kept of the ones before.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::{AccountId, MessageBody};

use super::OpenThread;

/// The scheme the page asks for inline pictures by.
pub const SCHEME: &str = "mailrs-cid";

/// One inline picture: its type and its bytes, shared with the picture
/// module's cache rather than copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineImage {
    pub mime: String,
    pub bytes: Arc<[u8]>,
}

/// What the page gets when it asks for a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Served {
    Ready(InlineImage),
    /// The message's pictures have not arrived yet.
    Waiting,
    /// There is no such picture, or it belongs to a version of the message
    /// that is no longer on screen.
    Gone,
}

/// Where the page asks for a picture, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub account_id: AccountId,
    pub message_id: String,
    pub version: u32,
    pub cid: String,
}

impl Address {
    /// Reads `mailrs-cid:<account>/<message>/<version>/<content id>`,
    /// with the content id escaped as [`prefix`] writes it.
    pub fn parse(uri: &str) -> Option<Address> {
        let rest = uri.strip_prefix(SCHEME)?.strip_prefix(':')?;
        let mut parts = rest.splitn(4, '/');
        let account_id = parts.next()?.parse().ok()?;
        let message_id = parts.next()?.to_string();
        let version = parts.next()?.parse().ok()?;
        let cid = unescape(parts.next()?)?;
        Some(Address {
            account_id,
            message_id,
            version,
            cid,
        })
    }
}

/// The start of the address of every picture one version of a message
/// names, which cleaning puts in front of each content id.
pub fn prefix(account_id: AccountId, message_id: &str, version: u32) -> String {
    format!("{SCHEME}:{account_id}/{message_id}/{version}/")
}

/// A content id as it goes into an address: letters, digits and `-._~@`
/// as they are, every other byte as `%XX`.
pub fn escape(cid: &str) -> String {
    let mut out = String::with_capacity(cid.len());
    for byte in cid.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn unescape(escaped: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(escaped.len());
    let mut rest = escaped.as_bytes();
    while let Some((&byte, after)) = rest.split_first() {
        if byte == b'%' {
            let hex = std::str::from_utf8(after.get(..2)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &after[2..];
        } else {
            bytes.push(byte);
            rest = after;
        }
    }
    String::from_utf8(bytes).ok()
}

impl OpenThread {
    /// The start of the address of each picture this message names now.
    pub fn picture_prefix(&self, message_id: &str) -> String {
        prefix(self.account_id, message_id, self.version_of(message_id))
    }

    pub(super) fn version_of(&self, message_id: &str) -> u32 {
        self.image_versions.get(message_id).copied().unwrap_or(0)
    }

    /// The bodies that name a picture by `cid:` and whose pictures have
    /// not arrived. A body the store kept counts as much as one Gmail just
    /// sent: the store keeps no pictures.
    pub fn wanting_images(&self) -> Vec<(String, MessageBody)> {
        self.bodies
            .iter()
            .filter(|(id, _)| !self.inline_images.contains_key(*id))
            .filter_map(|(id, body)| Some((id, body.as_ref().ok()?)))
            .filter(|(_, body)| body.html.as_deref().is_some_and(|h| h.contains("cid:")))
            .map(|(id, body)| (id.clone(), body.clone()))
            .collect()
    }

    /// The pictures Gmail sent, by message and content id. Each message
    /// named here is settled, whatever it holds. A message settled already
    /// keeps what it has: the engine may have opened it meanwhile, and its
    /// pictures come from the part the engine checked, not from Gmail.
    pub fn take_images(&mut self, found: HashMap<String, HashMap<String, InlineImage>>) {
        for (message_id, pictures) in found {
            self.inline_images.entry(message_id).or_insert(pictures);
        }
    }

    /// Puts the pictures cut from a message's own files in place of any it
    /// had, under a new version, so no address the page asked for before
    /// reaches them.
    pub(super) fn replace_images(
        &mut self,
        message_id: &str,
        pictures: HashMap<String, InlineImage>,
    ) {
        *self
            .image_versions
            .entry(message_id.to_string())
            .or_insert(0) += 1;
        self.inline_images.insert(message_id.to_string(), pictures);
    }

    /// What the page gets for the picture `cid` of this version of the
    /// message.
    pub fn picture(&self, message_id: &str, version: u32, cid: &str) -> Served {
        if !self.messages.iter().any(|m| m.id == message_id)
            || version != self.version_of(message_id)
        {
            return Served::Gone;
        }
        match self.inline_images.get(message_id) {
            None => Served::Waiting,
            Some(pictures) => pictures
                .get(cid)
                .cloned()
                .map_or(Served::Gone, Served::Ready),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Address, escape, prefix};

    #[test]
    fn an_address_reads_back_what_was_written() {
        let cid = "image001.png@01DA2B3C.4D5E6F70";
        let written = format!("{}{}", prefix(3, "18c2f", 1), escape(cid));
        assert_eq!(
            written,
            "mailrs-cid:3/18c2f/1/image001.png@01DA2B3C.4D5E6F70"
        );
        let read = Address::parse(&written).expect("it reads");
        assert_eq!(
            (read.account_id, read.message_id.as_str(), read.version),
            (3, "18c2f", 1)
        );
        assert_eq!(read.cid, cid);
    }

    #[test]
    fn a_content_id_with_odd_characters_survives_the_trip() {
        let cid = "<part 1/2>&\"é\"";
        let written = format!("{}{}", prefix(1, "m1", 0), escape(cid));
        assert!(!written[prefix(1, "m1", 0).len()..].contains(['/', ' ', '"', '<']));
        assert_eq!(
            Address::parse(&written).map(|a| a.cid).as_deref(),
            Some(cid)
        );
    }

    #[test]
    fn anything_else_is_no_address() {
        for uri in [
            "mailrs-cid:1/m1/logo",
            "mailrs-cid:x/m1/0/logo",
            "mailrs:toggle/m1",
            "mailrs-cid:1/m1/0/%zz",
        ] {
            assert_eq!(Address::parse(uri), None, "{uri}");
        }
    }
}
