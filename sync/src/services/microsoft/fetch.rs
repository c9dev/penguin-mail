//! Metadata, whole conversations, the window and the inbox check, raw
//! messages, and the structure and files of a large one.

use mailrs_domain::mailbox::keyword::{DRAFT, FLAGGED, SEEN};
use mailrs_domain::{Address, Memberships, MessageMeta, Role, category};
use mailrs_graph::{Fields, GraphError, Listing, Message, Recipient};
use mailrs_mime::{Part, Parts};

use super::{GraphApi, Microsoft, backend, tag_id, tag_name};
use crate::BackendError;
use crate::services::{Backfill, Found, LIST_PAGE_SIZE, RawMessage, RemoteRef, Want};

/// The most a whole-message fetch takes: a message the export or the
/// engine asks for raw. Graph's own limit on a message is 150 MB, which no
/// caller here wants in memory.
const RAW_FETCH_LIMIT: usize = 64 << 20;

/// The most one attachment fetch takes.
const PART_LIMIT: usize = 64 << 20;

/// The most messages of one conversation a whole-thread fetch keeps.
const MOST_IN_THREAD: usize = 500;

/// The most ids a window or inbox listing returns.
const MOST_IDS: usize = 20_000;

fn address(recipient: &Recipient) -> Option<Address> {
    let email = recipient.email_address.address.clone()?;
    Some(Address { name: recipient.email_address.name.clone().filter(|n| !n.is_empty()), email })
}

fn remote_ref(message: &Message) -> RemoteRef {
    RemoteRef {
        id: message.id.clone(),
        thread_id: message.conversation_id.clone().unwrap_or_else(|| message.id.clone()),
    }
}

impl<G: GraphApi> Microsoft<G> {
    pub(super) fn meta_of(&self, message: &Message) -> MessageMeta {
        let folder = message.parent_folder_id.clone().unwrap_or_default();
        let role = self.known().roles.iter().find(|(_, id)| **id == folder).map(|(r, _)| *r);
        let mut held = Memberships { mailboxes: vec![folder], ..Memberships::default() };
        if message.is_read == Some(true) {
            held.keywords.push(SEEN.into());
        }
        if message.is_flagged() {
            held.keywords.push(FLAGGED.into());
        }
        if message.is_draft == Some(true) {
            held.keywords.push(DRAFT.into());
        }
        if message.is_other() {
            held.categories.push(category::OTHER.into());
        }
        held.mailboxes.extend(message.categories.iter().flatten().map(|c| tag_id(c)));
        MessageMeta {
            account_id: 0,
            id: message.id.clone(),
            thread_id: remote_ref(message).thread_id,
            rfc822_msgid: message.internet_message_id.as_deref().map(|m| m.trim().trim_matches(['<', '>']).to_string()),
            from: message.from.as_ref().and_then(address),
            to: message.to_recipients.iter().filter_map(address).collect(),
            cc: message.cc_recipients.iter().filter_map(address).collect(),
            subject: message.subject.clone().unwrap_or_default(),
            date: message.received_millis().unwrap_or_default(),
            // Graph keeps the body's line breaks in the preview; a
            // preview is one paragraph wherever it is shown.
            snippet: message.body_preview.as_deref().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" "),
            // Without the size property the message goes by its
            // structure, as a size of 0 does.
            size: message.size().unwrap_or(0),
            has_attachments: message.has_attachments.unwrap_or(false),
            held,
            roles: role.into_iter().collect(),
            list_unsubscribe: message.header("List-Unsubscribe").map(str::to_string),
            one_click: message
                .header("List-Unsubscribe-Post")
                .is_some_and(|v| v.to_ascii_lowercase().contains("one-click")),
        }
    }

    pub(super) async fn fetch_metas(&self, wants: Vec<Want>) -> Result<Found, BackendError> {
        // A message's role comes from the folder listing.
        self.synced().await?;
        let ids: Vec<String> = wants.into_iter().map(|w| w.id).collect();
        let answers = self.graph().messages(&ids).await.map_err(backend)?;
        let mut found = Found::default();
        for (id, answer) in ids.into_iter().zip(answers) {
            match answer {
                Ok(message) => found.metas.push(self.meta_of(&message)),
                Err(GraphError::NotFound) => found.gone.push(id),
                Err(err) => return Err(backend(err)),
            }
        }
        Ok(found)
    }

    pub(super) async fn fetch_threads(&self, threads: Vec<String>) -> Result<Found, BackendError> {
        self.synced().await?;
        let mut found = Found::default();
        for thread in threads {
            let listing = Listing { conversation: Some(thread.clone()), top: 100, fields: Fields::Meta, ..Listing::default() };
            let metas = self.list_all(&listing, MOST_IN_THREAD, |m| self.meta_of(m)).await?;
            if metas.is_empty() {
                found.gone_threads.push(thread);
                continue;
            }
            found.metas.extend(metas.iter().cloned());
            found.whole.push(metas);
        }
        Ok(found)
    }

    /// Every page of `listing`, mapped, up to `most` items.
    async fn list_all<T>(&self, listing: &Listing, most: usize, map: impl Fn(&Message) -> T) -> Result<Vec<T>, BackendError> {
        let mut out = Vec::new();
        let mut next: Option<String> = None;
        loop {
            let page = self.graph().list_messages(listing, next.as_deref()).await.map_err(backend)?;
            out.extend(page.value.iter().map(&map));
            if out.len() >= most {
                out.truncate(most);
                tracing::warn!("a listing passed {most} messages; keeping the first");
                return Ok(out);
            }
            match page.next_link {
                Some(link) => next = Some(link),
                None => return Ok(out),
            }
        }
    }

    pub(super) async fn backfill_page(&self, days: i64, cursor: Option<&str>) -> Result<Backfill, BackendError> {
        let since = (chrono::Utc::now() - chrono::Duration::days(days.max(1))).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let listing = Listing { received_since: Some(since), top: LIST_PAGE_SIZE, fields: Fields::Ids, ..Listing::default() };
        let page = match self.graph().list_messages(&listing, cursor).await {
            Ok(page) => page,
            // Graph no longer takes the page link it gave.
            Err(GraphError::NotFound | GraphError::SyncStateLost | GraphError::Http { status: 400, .. }) if cursor.is_some() => {
                return Err(BackendError::StateLost);
            }
            Err(err) => return Err(backend(err)),
        };
        Ok(Backfill { refs: page.value.iter().map(remote_ref).collect(), next: page.next_link })
    }

    pub(super) async fn ids_in(&self, days: Option<i64>, folder: Option<String>) -> Result<Vec<RemoteRef>, BackendError> {
        let since = days.map(|d| (chrono::Utc::now() - chrono::Duration::days(d.max(1))).to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        // A category's tag id is no folder: Graph refuses it in a folder's
        // place, so a tag lists the whole mailbox filtered by its name.
        let (folder, category) = match folder.as_deref().and_then(tag_name) {
            Some(name) => (None, Some(name.to_string())),
            None => (folder, None),
        };
        let listing = Listing { folder, category, received_since: since, top: 1000, fields: Fields::Ids, ..Listing::default() };
        self.list_all(&listing, MOST_IDS, remote_ref).await
    }

    pub(super) async fn sent_with(&self, message_id: &str) -> Result<Option<String>, BackendError> {
        let Some(sent) = self.known().roles.get(&Role::Sent).cloned() else {
            return Ok(None);
        };
        let listing = Listing {
            folder: Some(sent),
            internet_message_id: Some(format!("<{}>", message_id.trim_matches(['<', '>']))),
            top: 1,
            ..Listing::default()
        };
        let page = self.graph().list_messages(&listing, None).await.map_err(backend)?;
        Ok(page.value.into_iter().next().map(|m| m.id))
    }

    pub(super) async fn raws(&self, ids: &[String]) -> Result<Vec<RawMessage>, BackendError> {
        let mut raws = Vec::with_capacity(ids.len());
        for id in ids {
            let bytes = self.graph().raw(id, RAW_FETCH_LIMIT).await.map_err(backend)?;
            raws.push(RawMessage { id: id.clone(), bytes });
        }
        Ok(raws)
    }

    /// The message's text and its files' names, numbered as the raw reader
    /// numbers a multipart message: the body "1", each file after it "2",
    /// "3" and on, in Graph's order. The files' bytes come one at a time.
    pub(super) async fn structure(&self, id: &str) -> Result<Parts, BackendError> {
        let got = self.graph().body(id).await.map_err(backend)?;
        let text_type = match got.body.content_type.as_str() {
            "html" => "text/html",
            _ => "text/plain",
        };
        let mut children = vec![Part {
            path: "1".into(),
            mime_type: text_type.into(),
            charset: Some("utf-8".into()),
            size: got.body.content.len() as i64,
            data: Some(got.body.content.into_bytes()),
            ..Part::default()
        }];
        let mut handles = Vec::new();
        for (i, file) in got.attachments.into_iter().enumerate() {
            let path = (i + 2).to_string();
            handles.push((path.clone(), file.id));
            children.push(Part {
                path,
                mime_type: file.content_type.to_ascii_lowercase(),
                filename: Some(file.name),
                content_id: file.content_id.map(|c| c.trim_matches(['<', '>']).to_string()),
                attachment: !file.is_inline,
                size: file.size,
                data: None,
                ..Part::default()
            });
        }
        self.remember(id, handles);
        Ok(Parts {
            headers: Vec::new(),
            root: Part { mime_type: "multipart/mixed".into(), children, ..Part::default() },
            incomplete: false,
        })
    }

    pub(super) async fn part(&self, id: &str, path: &str) -> Result<Vec<u8>, BackendError> {
        if self.remembered(id, path).is_none() {
            self.structure(id).await?;
        }
        let attachment = self.remembered(id, path).ok_or(BackendError::NotFound)?;
        self.graph().attachment(id, &attachment, PART_LIMIT).await.map_err(backend)
    }
}
