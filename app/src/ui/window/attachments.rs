//! What an attachment row does: show the file, save it, or save every file
//! on a message at once.
//!
//! Quick Look opens a window on the bytes themselves, so reading a photo
//! someone sent costs no trip through the Downloads folder. Anything the
//! window cannot draw goes to a scratch file and opens in whatever program
//! the desktop keeps for it. Either way the file exists on disk by then,
//! which is what lets it be dragged out into a folder.

use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use mailrs_domain::{AccountId, Attachment};

use super::MainWindow;
use crate::ui::conversation::ConversationView;

/// How large a picture may be before the row shows a paperclip instead.
/// Past this the thumbnail costs more to fetch than it earns.
const THUMBNAIL_LIMIT: i64 = 8 * 1024 * 1024;

/// How wide the picture on a row is drawn, in pixels of the stored copy.
/// Twice the 32 the page shows, so it stays sharp on a HiDPI screen.
const THUMBNAIL_EDGE: i32 = 64;

impl MainWindow {
    /// Opens one attachment. A picture appears in a window; everything
    /// else opens in the program the desktop keeps for its type.
    pub(super) fn preview_attachment_from(
        self: &Rc<Self>,
        view: &ConversationView,
        message_id: String,
        index: usize,
    ) {
        let Some((account_id, attachment)) = self.attachment_at(view, &message_id, index) else {
            return;
        };
        let (Some(sync), Some(attachment_id)) = (
            self.core.account(account_id),
            attachment.attachment_id.clone(),
        ) else {
            return;
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let name = attachment.filename.clone();
            let fetched = this
                .core
                .call(async move { sync.attachment(&message_id, &attachment_id).await })
                .await;
            match fetched {
                Ok(data) => this.show_attachment(&attachment, data),
                Err(err) => this.toast(&format!("Could not open {name}: {err}")),
            }
        });
    }

    /// Writes every attachment on one message into a folder the user
    /// picks. Inline pictures stay out of it, the way the rows do.
    pub(super) fn save_all_attachments_from(
        self: &Rc<Self>,
        view: &ConversationView,
        message_id: String,
    ) {
        let found = view.with_open(|open| {
            let body = open.bodies.get(&message_id)?.as_ref().ok()?;
            let files: Vec<Attachment> = body
                .attachments
                .iter()
                .filter(|a| a.attachment_id.is_some())
                .filter(|a| !crate::render::shown_in_body(a, body))
                .cloned()
                .collect();
            Some((open.account_id, files))
        });
        let Some(Some((account_id, files))) = found else {
            return;
        };
        if files.is_empty() {
            return;
        }
        let Some(sync) = self.core.account(account_id) else {
            return;
        };
        let dialog = gtk::FileDialog::builder()
            .title("Save Attachments")
            .modal(true)
            .build();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let Ok(folder) = dialog.select_folder_future(Some(&this.window)).await else {
                return;
            };
            let Some(folder) = folder.path() else { return };
            this.toast(&format!("Saving {} attachments…", files.len()));
            let mut saved = 0usize;
            let mut failed: Vec<String> = Vec::new();
            for attachment in files {
                let Some(attachment_id) = attachment.attachment_id.clone() else {
                    continue;
                };
                let (s, m, folder) = (sync.clone(), message_id.clone(), folder.clone());
                let name = attachment.filename.clone();
                let written = this
                    .core
                    .call(async move {
                        let data = s.attachment(&m, &attachment_id).await?;
                        let path = super::unique_path(&folder, &name);
                        tokio::task::spawn_blocking(move || std::fs::write(&path, data)).await??;
                        Ok::<(), anyhow::Error>(())
                    })
                    .await;
                match written {
                    Ok(()) => saved += 1,
                    Err(_) => failed.push(attachment.filename),
                }
            }
            let where_to = folder
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| folder.to_string_lossy().into_owned());
            if failed.is_empty() {
                this.toast(&format!("Saved {saved} attachments to {where_to}"));
            } else {
                this.toast(&format!(
                    "Saved {saved} to {where_to}. Could not save {}",
                    failed.join(", ")
                ));
            }
        });
    }

    /// The attachment a row stands for, with the account it belongs to.
    fn attachment_at(
        &self,
        view: &ConversationView,
        message_id: &str,
        index: usize,
    ) -> Option<(AccountId, Attachment)> {
        view.with_open(|open| {
            let body = open.bodies.get(message_id)?.as_ref().ok()?;
            Some((open.account_id, body.attachments.get(index)?.clone()))
        })
        .flatten()
    }

    /// Puts `data` on disk under the app's cache, so the desktop and a
    /// drag out of the window both have a real file to work with.
    fn scratch_copy(&self, attachment: &Attachment, data: &[u8]) -> Option<PathBuf> {
        let dir = glib::user_cache_dir().join("penguin-mail").join("previews");
        std::fs::create_dir_all(&dir).ok()?;
        let path = super::unique_path(&dir, &attachment.filename);
        std::fs::write(&path, data).ok()?;
        Some(path)
    }

    /// Shows an attachment the app can draw, and hands the rest to the
    /// desktop.
    fn show_attachment(self: &Rc<Self>, attachment: &Attachment, data: Vec<u8>) {
        let Some(path) = self.scratch_copy(attachment, &data) else {
            self.toast(&format!("Could not open {}", attachment.filename));
            return;
        };
        let file = gio::File::for_path(&path);
        if !attachment.mime_type.starts_with("image/") {
            gtk::FileLauncher::new(Some(&file)).launch(
                Some(&self.window),
                gio::Cancellable::NONE,
                |_| {},
            );
            return;
        }
        let Ok(texture) = gdk::Texture::from_bytes(&glib::Bytes::from_owned(data)) else {
            gtk::FileLauncher::new(Some(&file)).launch(
                Some(&self.window),
                gio::Cancellable::NONE,
                |_| {},
            );
            return;
        };
        self.quick_look(attachment, texture, path);
    }

    /// A window on one picture, with the file behind it: Save puts it in
    /// Downloads, Open hands it to the desktop, and dragging the picture
    /// out drops the file wherever it lands.
    fn quick_look(self: &Rc<Self>, attachment: &Attachment, texture: gdk::Texture, path: PathBuf) {
        let picture = gtk::Picture::for_paintable(&texture);
        picture.set_content_fit(gtk::ContentFit::ScaleDown);
        picture.set_can_shrink(true);
        picture.set_vexpand(true);

        let drag = gtk::DragSource::new();
        drag.set_actions(gdk::DragAction::COPY);
        let dragged = gio::File::for_path(&path);
        drag.connect_prepare(move |_, _, _| {
            Some(gdk::ContentProvider::for_value(&dragged.to_value()))
        });
        let dragged = texture.clone();
        drag.connect_drag_begin(move |source, _| {
            source.set_icon(Some(&dragged), 0, 0);
        });
        picture.add_controller(drag);

        let header = adw::HeaderBar::new();
        let save = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("Save to Downloads")
            .build();
        let open = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Open in Another Program")
            .build();
        header.pack_end(&open);
        header.pack_end(&save);
        header.set_title_widget(Some(&adw::WindowTitle::new(
            &attachment.filename,
            &crate::format::human_size(attachment.size),
        )));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&picture));
        let (width, height) = fit(texture.width(), texture.height());
        let window = adw::Window::builder()
            .transient_for(&self.window)
            .default_width(width)
            .default_height(height)
            .title(&attachment.filename)
            .content(&toolbar)
            .build();

        let source = path.clone();
        let this = Rc::clone(self);
        save.connect_clicked(move |_| this.copy_into_downloads(&source));
        let source = gio::File::for_path(&path);
        let parent = window.clone();
        open.connect_clicked(move |_| {
            gtk::FileLauncher::new(Some(&source)).launch(
                Some(&parent),
                gio::Cancellable::NONE,
                |_| {},
            );
        });

        // Escape closes it, the way every other quick view does.
        let keys = gtk::EventControllerKey::new();
        let closing = window.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                closing.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        window.add_controller(keys);
        window.present();
    }

    /// Copies a previewed file into Downloads under a free name.
    fn copy_into_downloads(self: &Rc<Self>, source: &PathBuf) {
        let downloads =
            glib::user_special_dir(glib::UserDirectory::Downloads).unwrap_or_else(glib::home_dir);
        let name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attachment".into());
        let target = super::unique_path(&downloads, &name);
        match std::fs::copy(source, &target) {
            Ok(_) => {
                let shown = target
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or(name);
                self.toast(&format!("Saved {shown} to Downloads"));
            }
            Err(err) => self.toast(&format!("Could not save {name}: {err}")),
        }
    }

    /// Fetches a small picture for each image attachment the rows list, so
    /// a row shows what it holds. Gmail charges for each one, so this asks
    /// only for pictures under the limit, skips what the cache already
    /// has, and runs in the background where the user's own calls come
    /// first.
    pub(super) async fn thumbnails(
        &self,
        account_id: AccountId,
        sync: &std::sync::Arc<crate::core::Sync>,
        loaded: &[(String, Result<mailrs_domain::MessageBody, String>)],
    ) -> std::collections::HashMap<String, String> {
        let mut out = std::collections::HashMap::new();
        for (message_id, body) in loaded {
            let Ok(body) = body else { continue };
            for attachment in &body.attachments {
                let Some(attachment_id) = attachment.attachment_id.clone() else {
                    continue;
                };
                if !attachment.mime_type.starts_with("image/")
                    || attachment.size > THUMBNAIL_LIMIT
                    || crate::render::shown_in_body(attachment, body)
                    || out.contains_key(&attachment_id)
                {
                    continue;
                }
                let key = (account_id, message_id.clone(), attachment_id.clone());
                if let Some(held) = self.thumbnail_cache.borrow().get(&key) {
                    out.insert(attachment_id, held.clone());
                    continue;
                }
                let (s, m, a) = (sync.clone(), message_id.clone(), attachment_id.clone());
                let Ok(data) = self
                    .core
                    .call(mailrs_gmail::limiter::background(async move {
                        s.attachment(&m, &a).await
                    }))
                    .await
                else {
                    continue;
                };
                let Some(uri) = shrink(&data) else { continue };
                let mut cache = self.thumbnail_cache.borrow_mut();
                if cache.len() >= THUMBNAIL_CACHE {
                    cache.clear();
                }
                cache.insert(key, uri.clone());
                out.insert(attachment_id, uri);
            }
        }
        out
    }
}

/// How many thumbnails to hold before starting over.
const THUMBNAIL_CACHE: usize = 200;

/// A picture small enough to sit in the page, as a PNG `data:` URI.
/// Returns None when the bytes are not a picture this machine can read.
fn shrink(data: &[u8]) -> Option<String> {
    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from(data));
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
        &stream,
        THUMBNAIL_EDGE,
        THUMBNAIL_EDGE,
        true,
        gio::Cancellable::NONE,
    )
    .ok()?;
    let bytes = pixbuf.save_to_bufferv("png", &[]).ok()?;
    let encoded =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes.as_slice());
    Some(format!("data:image/png;base64,{encoded}"))
}

/// A window size that holds the picture without covering the screen.
fn fit(width: i32, height: i32) -> (i32, i32) {
    const MAX: i32 = 1100;
    const MIN: i32 = 360;
    if width <= 0 || height <= 0 {
        return (720, 540);
    }
    let scale = (MAX as f64 / width as f64)
        .min(MAX as f64 / height as f64)
        .min(1.0);
    (
        ((width as f64 * scale) as i32).max(MIN),
        ((height as f64 * scale) as i32).max(MIN) + 46,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preview_window_holds_the_picture_without_covering_the_screen() {
        // A large photo scales down and keeps its shape.
        let (w, h) = fit(4000, 3000);
        assert!(w <= 1100 && h <= 1100 + 46, "{w}x{h}");
        assert_eq!(w * 3 / 4, h - 46, "three by four, plus the header");
        // A small one opens at its own size, above a floor worth looking at.
        assert_eq!(fit(400, 300), (400, 360 + 46));
        assert_eq!(fit(800, 600), (800, 600 + 46));
        // A picture one pixel tall still opens a usable window.
        let (w, h) = fit(2000, 1);
        assert!(w >= 360 && h >= 360, "{w}x{h}");
        assert_eq!(fit(0, 0), (720, 540));
    }
}
