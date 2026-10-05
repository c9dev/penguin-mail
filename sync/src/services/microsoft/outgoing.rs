//! Sending and drafts. A message goes to Graph as the MIME the composer
//! wrote, base64 in `sendMail`, so OpenPGP and S/MIME reach the recipient
//! untouched, and Graph files the copy in Sent Items. A draft is a message
//! in Drafts; saving it again makes the new one before deleting the old,
//! so a failure never leaves the person without their draft.

use mailrs_domain::translate::gettext;
use mailrs_domain::{Address, Attachment, Role};
use mailrs_graph::{Fields, GraphError, Listing};
use mailrs_mime::{Part, Parts};
use serde_json::{Value, json};

use super::{GraphApi, Microsoft, backend};
use crate::BackendError;
use crate::api::{DraftRef, SavedDraft};

/// The Message-ID of `raw` without its angle brackets, the only name the
/// sent message has until Graph lists it in Sent Items.
fn message_id_of(raw: &[u8]) -> Option<String> {
    mailrs_mime::parts(raw).and_then(|parts| parts.header("Message-ID").map(|id| id.trim().trim_matches(['<', '>']).to_string()))
}

/// The largest MIME message `sendMail` takes: 3 MB grows to 4 MB as
/// base64, Graph's limit on one request.
pub(super) const MIME_LIMIT: usize = 3 << 20;

/// One piece of an upload: Graph wants a multiple of 320 KiB.
const CHUNK: usize = 10 * 320 * 1024;

/// Top-level types whose bytes a signature or a cipher covers. Rebuilding
/// such a message as a JSON draft would break it.
const PROTECTED: [&str; 4] = ["multipart/signed", "multipart/encrypted", "application/pkcs7-mime", "application/x-pkcs7-mime"];

/// A header's addresses as Graph's recipient objects.
fn recipients(parts: &Parts, header: &str) -> Vec<Value> {
    let list = parts.header(header).map(mailrs_mime::address::parse_address_list).unwrap_or_default();
    list.into_iter()
        .map(|Address { name, email }| json!({ "emailAddress": { "address": email, "name": name.unwrap_or_default() } }))
        .collect()
}

/// Hands the bytes of the part at `path` over and leaves the part empty,
/// so each file's copy is freed once it is uploaded.
fn take_data(part: &mut Part, path: &str) -> Option<Vec<u8>> {
    if part.path == path {
        return part.data.take();
    }
    part.children.iter_mut().find_map(|child| take_data(child, path))
}

/// Whether the part at `path` says `Content-Disposition: attachment`.
fn is_attachment(part: &Part, path: &str) -> bool {
    match part.path == path {
        true => part.attachment,
        false => part.children.iter().any(|child| is_attachment(child, path)),
    }
}

impl<G: GraphApi> Microsoft<G> {
    pub(super) async fn send_raw(&self, raw: &[u8]) -> Result<String, BackendError> {
        let id = message_id_of(raw).ok_or_else(|| BackendError::Refused("the message has no Message-ID".into()))?;
        match raw.len() > MIME_LIMIT {
            false => self.graph().send_mime(raw).await.map_err(backend)?,
            true => self.send_large(raw, &id).await?,
        }
        Ok(id)
    }

    /// A message too big for `sendMail`: a JSON draft, each file uploaded
    /// in pieces, then the draft sent. Graph takes no `In-Reply-To` or
    /// `References` on a draft made this way, so a reply sent here does
    /// not thread by header.
    async fn send_large(&self, raw: &[u8], id: &str) -> Result<(), BackendError> {
        let mut parts = mailrs_mime::parts(raw).ok_or_else(|| BackendError::Refused("the message cannot be read".into()))?;
        if PROTECTED.contains(&parts.root.mime_type.as_str()) {
            return Err(BackendError::Refused(gettext(
                "Microsoft cannot send a signed or encrypted message larger than 3 MB. Make the files smaller or send them apart.",
            )));
        }
        let body = mailrs_mime::body(&parts);
        let (kind, content) = match (&body.html, &body.text) {
            (Some(html), _) => ("html", html.as_str()),
            (None, text) => ("text", text.as_deref().unwrap_or_default()),
        };
        let draft = json!({
            "subject": parts.header("Subject").unwrap_or_default().trim(),
            "body": { "contentType": kind, "content": content },
            "toRecipients": recipients(&parts, "To"),
            "ccRecipients": recipients(&parts, "Cc"),
            "bccRecipients": recipients(&parts, "Bcc"),
            "internetMessageId": format!("<{id}>"),
        });
        let made = self.graph().create_draft(&draft).await.map_err(backend)?;
        match self.upload_and_send(&made.id, &body.attachments, &mut parts).await {
            Ok(()) => Ok(()),
            Err(err) => {
                // A retry would otherwise leave this draft behind.
                if let Err(gone) = self.graph().delete_message(&made.id).await {
                    tracing::warn!(%gone, "the unsent large message stays in Drafts");
                }
                Err(err)
            }
        }
    }

    async fn upload_and_send(&self, draft: &str, files: &[Attachment], parts: &mut Parts) -> Result<(), BackendError> {
        for file in files {
            let Some(bytes) = take_data(&mut parts.root, &file.part_id) else { continue };
            let name = if file.filename.is_empty() { "file" } else { file.filename.as_str() };
            let total = bytes.len() as u64;
            // A picture the HTML refers to by `cid:` stays inline. A part
            // with a Content-ID and no `attachment` disposition is one.
            let inline = file.content_id.is_some() && !is_attachment(&parts.root, &file.part_id);
            let content_id = file.content_id.as_deref().filter(|_| inline);
            let url = self.graph().upload_session(draft, name, total, inline, content_id).await.map_err(backend)?;
            let mut offset = 0;
            for piece in bytes.chunks(CHUNK) {
                let done = self.graph().upload_chunk(&url, offset, total, piece).await.map_err(backend)?;
                offset += piece.len() as u64;
                if done {
                    break;
                }
            }
        }
        self.graph().send_draft(draft).await.map_err(backend)
    }

    pub(super) async fn save(&self, old: Option<&str>, raw: &[u8]) -> Result<SavedDraft, BackendError> {
        let made = self.graph().create_draft_mime(raw).await.map_err(backend)?;
        if let Some(old) = old {
            match self.graph().delete_message(old).await {
                Ok(()) | Err(GraphError::NotFound) => {}
                Err(err) => tracing::warn!(%err, "the draft before this one stays in Drafts"),
            }
        }
        Ok(SavedDraft {
            draft_id: made.id.clone(),
            message_id: made.id.clone(),
            thread_id: made.conversation_id.unwrap_or(made.id),
        })
    }

    pub(super) async fn send_saved(&self, draft: &str) -> Result<String, BackendError> {
        let held = self.graph().messages(&[draft.to_string()]).await.map_err(backend)?;
        let message_id = held
            .into_iter()
            .next()
            .ok_or(BackendError::NotFound)?
            .map_err(backend)?
            .internet_message_id
            .map(|m| m.trim_matches(['<', '>']).to_string())
            .unwrap_or_else(|| draft.to_string());
        self.graph().send_draft(draft).await.map_err(backend)?;
        Ok(message_id)
    }

    pub(super) async fn drop_draft(&self, draft: &str) -> Result<(), BackendError> {
        match self.graph().delete_message(draft).await {
            Ok(()) | Err(GraphError::NotFound) => Ok(()),
            Err(err) => Err(backend(err)),
        }
    }

    /// The newest hundred drafts; nobody keeps more open, and the page
    /// bound holds memory.
    pub(super) async fn drafts(&self) -> Result<Vec<DraftRef>, BackendError> {
        self.synced().await?;
        let Some(drafts) = self.known().roles.get(&Role::Drafts).cloned() else {
            return Ok(Vec::new());
        };
        let listing = Listing { folder: Some(drafts), top: 100, fields: Fields::Ids, ..Listing::default() };
        let page = self.graph().list_messages(&listing, None).await.map_err(backend)?;
        Ok(page.value.into_iter().map(|m| DraftRef { draft_id: m.id.clone(), message_id: m.id }).collect())
    }
}
