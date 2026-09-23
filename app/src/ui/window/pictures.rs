//! The pictures a conversation shows out of its attachments: the inline
//! images an HTML body names with `cid:`, and the small pictures on the
//! attachment rows.
//!
//! Gmail charges 5 units for each attachment and a conversation is often
//! opened again, so [`Pictures`] keeps what it fetched, the most recently
//! used first, up to a number of bytes rather than a number of pictures:
//! one inline photo can be 7 MB as a `data:` URI and a row's picture a
//! few kilobytes. It fetches what it lacks a few at a time, and encodes
//! and shrinks each picture on a worker thread, since a 24 megapixel JPEG
//! takes the GTK thread 130 to 175 ms to decode.
//!
//! The thread run's ports call it; nothing here draws.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use futures::StreamExt;
use futures::future::LocalBoxFuture;
use mailrs_domain::{AccountId, MessageBody};

use crate::core::{Core, Sync};

/// Largest inline image embedded into a page.
const INLINE_LIMIT: i64 = 5 * 1024 * 1024;

/// How large a picture may be before the row shows a paperclip instead.
/// Past this the thumbnail costs more to fetch than it earns.
const THUMBNAIL_LIMIT: i64 = 8 * 1024 * 1024;

/// How wide the picture on a row is drawn, in pixels of the stored copy.
/// Twice the 32 the page shows, so it stays sharp on a HiDPI screen.
const THUMBNAIL_EDGE: i32 = 64;

/// Pictures fetched at once. Each is a 5 unit Gmail call, and the bodies
/// of the same thread may be arriving beside them.
const FETCHES: usize = 6;

/// Bytes of inline images kept: about seven of the largest, or hundreds
/// of the logos most mail carries.
const INLINE_BYTES: usize = 48 * 1024 * 1024;

/// Bytes of row pictures kept, which is several hundred of them.
const THUMBNAIL_BYTES: usize = 4 * 1024 * 1024;

/// A picture by account, message and Gmail's attachment id.
pub(super) type Key = (AccountId, String, String);

/// Pictures kept by how recently they were used, up to `limit` bytes.
#[derive(Debug)]
pub(super) struct Recent {
    limit: usize,
    used: usize,
    clock: u64,
    held: HashMap<Key, (String, u64)>,
    /// The keys by when they were last used, oldest first.
    order: BTreeMap<u64, Key>,
}

impl Recent {
    pub(super) fn new(limit: usize) -> Recent {
        Recent {
            limit,
            used: 0,
            clock: 0,
            held: HashMap::new(),
            order: BTreeMap::new(),
        }
    }

    /// The picture under `key`, which now counts as the most recent.
    pub(super) fn get(&mut self, key: &Key) -> Option<String> {
        self.clock += 1;
        let (value, used_at) = self.held.get_mut(key)?;
        self.order.remove(used_at);
        *used_at = self.clock;
        self.order.insert(self.clock, key.clone());
        Some(value.clone())
    }

    /// Keeps `value` under `key`, letting the least recent pictures go
    /// until it fits. A picture larger than the whole limit is not kept.
    pub(super) fn put(&mut self, key: Key, value: String) {
        if let Some((old, used_at)) = self.held.remove(&key) {
            self.used -= old.len();
            self.order.remove(&used_at);
        }
        if value.len() > self.limit {
            return;
        }
        while self.used + value.len() > self.limit {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some((gone, _)) = self.held.remove(&oldest) {
                self.used -= gone.len();
            }
        }
        self.clock += 1;
        self.used += value.len();
        self.order.insert(self.clock, key.clone());
        self.held.insert(key, (value, self.clock));
    }

    /// Bytes kept now.
    #[cfg(test)]
    pub(super) fn used(&self) -> usize {
        self.used
    }
}

/// One attachment to show as a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Picture {
    pub message_id: String,
    pub attachment_id: String,
    pub mime_type: String,
    /// The `Content-ID` an HTML body names it by, for an inline image.
    pub cid: Option<String>,
}

/// The inline images the HTML bodies name, small enough to embed.
pub(super) fn inline_wanted(bodies: &[(String, Result<MessageBody, String>)]) -> Vec<Picture> {
    let mut wanted = Vec::new();
    for (message_id, body) in bodies {
        let Ok(body) = body else { continue };
        if !body.html.as_deref().is_some_and(|h| h.contains("cid:")) {
            continue;
        }
        for attachment in &body.attachments {
            let (Some(cid), Some(attachment_id)) =
                (&attachment.content_id, &attachment.attachment_id)
            else {
                continue;
            };
            if attachment.mime_type.starts_with("image/") && attachment.size <= INLINE_LIMIT {
                wanted.push(Picture {
                    message_id: message_id.clone(),
                    attachment_id: attachment_id.clone(),
                    mime_type: attachment.mime_type.clone(),
                    cid: Some(cid.clone()),
                });
            }
        }
    }
    wanted
}

/// The pictures the attachment rows show: every image attachment under
/// the limit that the body does not already draw, once each.
pub(super) fn thumbnails_wanted(bodies: &[(String, MessageBody)]) -> Vec<Picture> {
    let mut wanted: Vec<Picture> = Vec::new();
    for (message_id, body) in bodies {
        for attachment in &body.attachments {
            let Some(attachment_id) = &attachment.attachment_id else {
                continue;
            };
            if !attachment.mime_type.starts_with("image/")
                || attachment.size > THUMBNAIL_LIMIT
                || crate::render::shown_in_body(attachment, body)
                || wanted.iter().any(|w| &w.attachment_id == attachment_id)
            {
                continue;
            }
            wanted.push(Picture {
                message_id: message_id.clone(),
                attachment_id: attachment_id.clone(),
                mime_type: attachment.mime_type.clone(),
                cid: None,
            });
        }
    }
    wanted
}

/// Answers each picture from `recent`, and the rest from `fetch`, a few at
/// a time. What `fetch` makes is kept for next time; a picture it could
/// not make is left out.
pub(super) async fn gather<'a>(
    recent: &RefCell<Recent>,
    account_id: AccountId,
    wanted: Vec<Picture>,
    fetch: impl Fn(Picture) -> LocalBoxFuture<'a, Option<String>>,
) -> Vec<(Picture, String)> {
    let key = |p: &Picture| (account_id, p.message_id.clone(), p.attachment_id.clone());
    let mut found = Vec::new();
    let mut missing: Vec<Picture> = Vec::new();
    for picture in wanted {
        let held = recent.borrow_mut().get(&key(&picture));
        match held {
            Some(value) => found.push((picture, value)),
            None if missing.iter().any(|m| key(m) == key(&picture)) => {}
            None => missing.push(picture),
        }
    }
    let fetched: Vec<(Picture, Option<String>)> = futures::stream::iter(missing)
        .map(|picture| {
            let made = fetch(picture.clone());
            async move { (picture, made.await) }
        })
        .buffer_unordered(FETCHES)
        .collect()
        .await;
    for (picture, made) in fetched {
        if let Some(value) = made {
            recent.borrow_mut().put(key(&picture), value.clone());
            found.push((picture, value));
        }
    }
    found
}

/// What the window keeps of the pictures it fetched.
pub(super) struct Pictures {
    core: Rc<Core>,
    inline: RefCell<Recent>,
    thumbnails: RefCell<Recent>,
}

impl Pictures {
    pub(super) fn new(core: Rc<Core>) -> Pictures {
        Pictures {
            core,
            inline: RefCell::new(Recent::new(INLINE_BYTES)),
            thumbnails: RefCell::new(Recent::new(THUMBNAIL_BYTES)),
        }
    }

    /// The `cid:` images the bodies name, as `data:` URIs by message and
    /// then by content id. A body that names none is left out.
    pub(super) async fn inline(
        &self,
        account_id: AccountId,
        sync: &Arc<Sync>,
        bodies: &[(String, Result<MessageBody, String>)],
    ) -> HashMap<String, HashMap<String, String>> {
        let mut out: HashMap<String, HashMap<String, String>> = bodies
            .iter()
            .filter(|(_, body)| {
                body.as_ref()
                    .is_ok_and(|b| b.html.as_deref().is_some_and(|h| h.contains("cid:")))
            })
            .map(|(id, _)| (id.clone(), HashMap::new()))
            .collect();
        let fetch = |picture: Picture| -> LocalBoxFuture<'static, Option<String>> {
            let (core, sync) = (Rc::clone(&self.core), Arc::clone(sync));
            Box::pin(async move {
                let made = core
                    .call(async move {
                        let bytes = sync
                            .attachment(&picture.message_id, &picture.attachment_id)
                            .await?;
                        let mime = picture.mime_type;
                        Ok::<_, anyhow::Error>(
                            tokio::task::spawn_blocking(move || data_uri(&mime, &bytes)).await?,
                        )
                    })
                    .await;
                made.ok()
            })
        };
        let found = gather(&self.inline, account_id, inline_wanted(bodies), fetch).await;
        for (picture, uri) in found {
            if let Some(cid) = picture.cid {
                out.entry(picture.message_id)
                    .or_default()
                    .insert(cid, uri);
            }
        }
        out
    }

    /// Small pictures for the attachment rows of `bodies`, by Gmail's
    /// attachment id. They come from the background share of the quota,
    /// since the message is already on screen without them.
    pub(super) async fn thumbnails(
        &self,
        account_id: AccountId,
        sync: &Arc<Sync>,
        bodies: &[(String, MessageBody)],
    ) -> HashMap<String, String> {
        let fetch = |picture: Picture| -> LocalBoxFuture<'static, Option<String>> {
            let (core, sync) = (Rc::clone(&self.core), Arc::clone(sync));
            Box::pin(async move {
                let made = core
                    .call(async move {
                        let bytes = mailrs_sync::background(async move {
                            sync.attachment(&picture.message_id, &picture.attachment_id)
                                .await
                        })
                        .await?;
                        Ok::<_, anyhow::Error>(
                            tokio::task::spawn_blocking(move || shrink(&bytes)).await?,
                        )
                    })
                    .await;
                made.ok().flatten()
            })
        };
        gather(
            &self.thumbnails,
            account_id,
            thumbnails_wanted(bodies),
            fetch,
        )
        .await
        .into_iter()
        .map(|(picture, uri)| (picture.attachment_id, uri))
        .collect()
    }
}

/// `bytes` as a `data:` URI of type `mime`.
fn data_uri(mime: &str, bytes: &[u8]) -> String {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:{mime};base64,{encoded}")
}

/// A picture small enough to sit in the page, as a PNG `data:` URI, or
/// `None` when the bytes are not a picture this machine can read. Runs on
/// a worker thread: GdkPixbuf needs no GTK thread, and a large photo
/// takes long enough to decode to hold up everything drawn on that one.
fn shrink(data: &[u8]) -> Option<String> {
    use gtk::{gio, glib};
    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from(data));
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
        &stream,
        THUMBNAIL_EDGE,
        THUMBNAIL_EDGE,
        true,
        gio::Cancellable::NONE,
    )
    .ok()?;
    let png = pixbuf.save_to_bufferv("png", &[]).ok()?;
    Some(data_uri("image/png", &png))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use mailrs_domain::Attachment;

    use super::*;

    fn key(n: u8) -> Key {
        (1, format!("m{n}"), format!("a{n}"))
    }

    #[test]
    fn the_least_recent_picture_goes_first_once_the_bytes_run_out() {
        let mut recent = Recent::new(10);
        recent.put(key(1), "aaaa".into());
        recent.put(key(2), "bbbb".into());
        // Using the first makes the second the oldest.
        assert_eq!(recent.get(&key(1)).as_deref(), Some("aaaa"));
        recent.put(key(3), "cccc".into());
        assert_eq!(recent.get(&key(2)), None);
        assert!(recent.get(&key(1)).is_some() && recent.get(&key(3)).is_some());
        assert_eq!(recent.used(), 8);
    }

    #[test]
    fn a_picture_larger_than_the_whole_cache_is_not_kept() {
        let mut recent = Recent::new(4);
        recent.put(key(1), "ab".into());
        recent.put(key(2), "abcdefgh".into());
        assert_eq!(recent.get(&key(2)), None);
        assert_eq!(recent.get(&key(1)).as_deref(), Some("ab"), "nothing made room");
    }

    #[test]
    fn keeping_a_picture_again_replaces_its_bytes() {
        let mut recent = Recent::new(100);
        recent.put(key(1), "abc".into());
        recent.put(key(1), "abcdef".into());
        assert_eq!(recent.used(), 6);
        assert_eq!(recent.get(&key(1)).as_deref(), Some("abcdef"));
    }

    #[test]
    fn many_large_images_stay_under_the_byte_limit() {
        let mut recent = Recent::new(INLINE_BYTES);
        let large = "x".repeat(6_700_000);
        for n in 0..64 {
            recent.put(key(n), large.clone());
        }
        assert!(recent.used() <= INLINE_BYTES);
        assert!(recent.get(&key(63)).is_some(), "the newest is kept");
    }

    fn image(id: &str, cid: Option<&str>, size: i64) -> Attachment {
        Attachment {
            filename: format!("{id}.png"),
            mime_type: "image/png".into(),
            size,
            attachment_id: Some(id.into()),
            content_id: cid.map(Into::into),
            part_id: String::new(),
        }
    }

    fn body(html: Option<&str>, attachments: Vec<Attachment>) -> MessageBody {
        MessageBody {
            html: html.map(Into::into),
            attachments,
            ..MessageBody::default()
        }
    }

    #[test]
    fn only_images_a_body_names_are_fetched_inline() {
        let bodies = vec![
            (
                "m1".to_string(),
                Ok(body(
                    Some(r#"<img src="cid:logo">"#),
                    vec![
                        image("a1", Some("logo"), 1_000),
                        image("a2", None, 1_000),
                        image("a3", Some("huge"), INLINE_LIMIT + 1),
                    ],
                )),
            ),
            (
                "m2".to_string(),
                Ok(body(Some("<p>no pictures</p>"), vec![image("a4", Some("x"), 10)])),
            ),
            ("m3".to_string(), Err("offline".to_string())),
        ];
        let wanted = inline_wanted(&bodies);
        assert_eq!(wanted.len(), 1);
        assert_eq!(wanted[0].attachment_id, "a1");
        assert_eq!(wanted[0].cid.as_deref(), Some("logo"));
    }

    #[test]
    fn a_row_picture_is_fetched_once_and_only_for_what_the_body_leaves_out() {
        let shown = body(
            Some(r#"<img src="cid:drawn">"#),
            vec![
                image("a1", Some("drawn"), 10),
                image("a2", None, 10),
                image("a3", None, THUMBNAIL_LIMIT + 1),
            ],
        );
        let again = body(None, vec![image("a2", None, 10)]);
        let wanted = thumbnails_wanted(&[("m1".into(), shown), ("m2".into(), again)]);
        let ids: Vec<&str> = wanted.iter().map(|w| w.attachment_id.as_str()).collect();
        assert_eq!(ids, ["a2"]);
    }

    fn picture(n: u8) -> Picture {
        Picture {
            message_id: format!("m{n}"),
            attachment_id: format!("a{n}"),
            mime_type: "image/png".into(),
            cid: None,
        }
    }

    /// What a fake fetch saw: how many calls, how many ran at once.
    #[derive(Default)]
    struct Seen {
        calls: Cell<usize>,
        running: Cell<usize>,
        most: Cell<usize>,
    }

    #[tokio::test]
    async fn pictures_arrive_several_at_a_time_and_the_cache_answers_next_time() {
        let recent = RefCell::new(Recent::new(1_000));
        let seen = Rc::new(Seen::default());
        let fetch = |p: Picture| -> LocalBoxFuture<'static, Option<String>> {
            let seen = Rc::clone(&seen);
            Box::pin(async move {
                seen.calls.set(seen.calls.get() + 1);
                seen.running.set(seen.running.get() + 1);
                seen.most.set(seen.most.get().max(seen.running.get()));
                tokio::task::yield_now().await;
                seen.running.set(seen.running.get() - 1);
                Some(format!("uri-{}", p.attachment_id))
            })
        };
        let wanted: Vec<Picture> = (0..10).map(picture).collect();
        let found = gather(&recent, 1, wanted.clone(), &fetch).await;
        assert_eq!(found.len(), 10);
        assert_eq!(seen.most.get(), FETCHES, "a few at once, never one by one");
        assert_eq!(seen.calls.get(), 10);
        let again = gather(&recent, 1, wanted, &fetch).await;
        assert_eq!(again.len(), 10);
        assert_eq!(seen.calls.get(), 10, "the second opening fetched nothing");
    }

    #[tokio::test]
    async fn a_picture_that_would_not_come_is_left_out_and_asked_for_again() {
        let recent = RefCell::new(Recent::new(1_000));
        let seen = Rc::new(Seen::default());
        let fetch = |_: Picture| -> LocalBoxFuture<'static, Option<String>> {
            let seen = Rc::clone(&seen);
            Box::pin(async move {
                seen.calls.set(seen.calls.get() + 1);
                None
            })
        };
        assert!(gather(&recent, 1, vec![picture(1)], &fetch).await.is_empty());
        assert!(gather(&recent, 1, vec![picture(1)], &fetch).await.is_empty());
        assert_eq!(seen.calls.get(), 2);
    }
}
