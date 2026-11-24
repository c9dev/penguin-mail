//! How the open thread holds and serves its inline pictures.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_client::inline::{InlineImage, Served, prefix};
use mailrs_domain::MessageBody;

use super::OpenThread;

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
    pub fn wanting_images(&self) -> Vec<(String, Arc<MessageBody>)> {
        self.bodies
            .iter()
            .filter(|(id, _)| !self.inline_images.contains_key(*id))
            .filter_map(|(id, body)| Some((id, body.as_ref().ok()?)))
            .filter(|(_, body)| body.html.as_deref().is_some_and(|h| h.contains("cid:")))
            .map(|(id, body)| (id.clone(), Arc::clone(body)))
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
