//! What an attachment row does: show the file, save it, or save every file
//! on a message at once.
//!
//! Quick Look opens a window on the bytes themselves, so reading a photo
//! someone sent costs no trip through the Downloads folder. Anything the
//! window cannot draw goes to a scratch file and opens in whatever program
//! the desktop keeps for it. Either way the file exists on disk by then,
//! which is what lets it be dragged out into a folder.

use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use mailrs_domain::{AccountId, Attachment};

use super::MainWindow;
use crate::ui::conversation::ConversationView;
use mailrs_domain::translate::{fill, fill_plural, gettext, with_reason};

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
        // A file out of an encrypted message is already here. Gmail holds
        // the ciphertext, so there is nothing to fetch and nothing to wait
        // for.
        if let Some(data) = opened_file(view, &message_id, index) {
            self.show_attachment(attachment, data, true);
            return;
        }
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
                Ok(data) => this.show_attachment(attachment, data, false),
                Err(err) => this.toast(&with_reason(
                    &gettext("Could not open {file}: {reason}"),
                    &err,
                    &[("file", &name)],
                )),
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
        let found = view.find(|open| {
            let held = open
                .opened_files
                .get(&message_id)
                .cloned()
                .unwrap_or_default();
            let body = open.bodies.get(&message_id)?.as_ref().ok()?;
            // A file that came out of an encrypted message has no
            // attachment id and does not need one; its bytes are in `held`
            // at the same index.
            let files: Vec<(usize, Attachment)> = body
                .attachments
                .iter()
                .enumerate()
                .filter(|(index, a)| a.attachment_id.is_some() || held.len() > *index)
                .filter(|(_, a)| !crate::render::shown_in_body(a, body))
                .map(|(index, a)| (index, a.clone()))
                .collect();
            Some((open.account_id, files, held))
        });
        let Some((account_id, files, held)) = found else {
            return;
        };
        if files.is_empty() {
            return;
        }
        let Some(sync) = self.core.account(account_id) else {
            return;
        };
        let dialog = gtk::FileDialog::builder()
            .title(gettext("Save Attachments"))
            .modal(true)
            .build();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let Ok(folder) = dialog.select_folder_future(Some(&this.window)).await else {
                return;
            };
            let Some(folder) = folder.path() else { return };
            let count = files.len();
            this.toast(&fill_plural(
                "Saving {count} attachment…",
                "Saving {count} attachments…",
                count,
                &[("count", &count.to_string())],
            ));
            let mut saved = 0usize;
            let mut failed: Vec<String> = Vec::new();
            // The index is the one the message's own attachment list
            // gives, which is what `held` is keyed by. Filtering the rows
            // above would otherwise have shifted it.
            for (index, attachment) in files {
                if let Some(data) = held.get(index).cloned() {
                    let (folder, name) = (folder.clone(), attachment.filename.clone());
                    let written = gio::spawn_blocking(move || {
                        save_under_free_name(&folder, &name, |path| std::fs::write(path, data))
                    })
                    .await;
                    match written {
                        Ok(Ok(_)) => saved += 1,
                        _ => failed.push(attachment.filename),
                    }
                    continue;
                }
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
                this.toast(&fill_plural(
                    "Saved {count} attachment to {folder}",
                    "Saved {count} attachments to {folder}",
                    saved,
                    &[("count", &saved.to_string()), ("folder", &where_to)],
                ));
            } else {
                this.toast(&fill(
                    &gettext("Saved {count} to {folder}. Could not save {files}"),
                    &[
                        ("count", &saved.to_string()),
                        ("folder", &where_to),
                        ("files", &failed.join(", ")),
                    ],
                ));
            }
        });
    }

    /// Saves a file the open thread already holds, and says whether it
    /// did, so the caller knows to stop rather than ask Gmail.
    pub(super) fn save_opened_file_from(
        self: &Rc<Self>,
        view: &ConversationView,
        message_id: &str,
        index: usize,
        attachment: &Attachment,
    ) -> bool {
        match opened_file(view, message_id, index) {
            Some(data) => {
                self.save_opened_file(attachment, data);
                true
            }
            None => false,
        }
    }

    /// Writes a file that came out of an encrypted message to Downloads.
    fn save_opened_file(self: &Rc<Self>, attachment: &Attachment, data: Vec<u8>) {
        self.save_to_downloads(attachment.filename.clone(), move |path| {
            std::fs::write(path, data)
        });
    }

    /// Puts a file into Downloads under a free name and says how it went.
    /// `write` makes the file at the path it is given. It runs on a worker
    /// thread, since a large file or a slow disk would otherwise freeze the
    /// window while it writes.
    fn save_to_downloads(
        self: &Rc<Self>,
        name: String,
        write: impl FnOnce(&Path) -> io::Result<()> + Send + 'static,
    ) {
        let downloads =
            glib::user_special_dir(glib::UserDirectory::Downloads).unwrap_or_else(glib::home_dir);
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let wanted = name.clone();
            let saved = gio::spawn_blocking(move || save_under_free_name(&downloads, &wanted, write))
                .await
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
            match saved {
                Ok(path) => {
                    let shown = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or(name);
                    this.toast(&fill(
                        &gettext("Saved {file} to Downloads"),
                        &[("file", &shown)],
                    ));
                }
                Err(err) => this.toast(&with_reason(
                    &gettext("Could not save {file}: {reason}"),
                    &err,
                    &[("file", &name)],
                )),
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
        view.find(|open| {
            let body = open.bodies.get(message_id)?.as_ref().ok()?;
            Some((open.account_id, body.attachments.get(index)?.clone()))
        })
    }

    /// Shows an attachment the app can draw, and hands the rest to the
    /// desktop. Either way the bytes go to a scratch copy first, so the
    /// desktop and a drag out of the window both have a real file to work
    /// with. Writing the copy and decoding a photo can take a good part of
    /// a second, so both happen off the GTK thread. `decrypted` marks a
    /// file out of an encrypted message, whose copy goes when the window
    /// closes.
    fn show_attachment(self: &Rc<Self>, attachment: Attachment, data: Vec<u8>, decrypted: bool) {
        let (name, picture) = (
            attachment.filename.clone(),
            attachment.mime_type.starts_with("image/"),
        );
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let made = gio::spawn_blocking(move || {
                let path = super::previews::write(&super::previews::folder(), &name, &data)?;
                let texture = picture
                    .then(|| gdk::Texture::from_bytes(&glib::Bytes::from_owned(data)).ok())
                    .flatten();
                Ok::<_, std::io::Error>((path, texture))
            })
            .await;
            let Ok(Ok((path, texture))) = made else {
                return this.toast(&fill(
                    &gettext("Could not open {file}"),
                    &[("file", &attachment.filename)],
                ));
            };
            this.previews.kept(path.clone(), decrypted);
            match texture {
                Some(texture) => this.quick_look(&attachment, texture, path),
                None => gtk::FileLauncher::new(Some(&gio::File::for_path(&path))).launch(
                    Some(&this.window),
                    gio::Cancellable::NONE,
                    |_| {},
                ),
            }
        });
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
            .tooltip_text(gettext("Save to Downloads"))
            .build();
        let open = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text(gettext("Open in Another Program"))
            .build();
        crate::ui::name(&save, &gettext("Save to Downloads"));
        crate::ui::name(&open, &gettext("Open in Another Program"));
        crate::ui::name(
            &picture,
            &fill(
                &gettext("{file}, preview"),
                &[("file", &attachment.filename)],
            ),
        );
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

        // The copy goes with the window. Save to Downloads made its own.
        let (gone, weak) = (path.clone(), Rc::downgrade(self));
        window.connect_destroy(move |_| {
            if let Some(win) = weak.upgrade() {
                win.previews.forget(&gone);
            }
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
    fn copy_into_downloads(self: &Rc<Self>, source: &Path) {
        let name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attachment".into());
        let source = source.to_path_buf();
        self.save_to_downloads(name, move |target| {
            std::fs::copy(&source, target).map(drop)
        });
    }
}

/// Writes a file into `folder` under `name`, or `name (2)` and so on when
/// that is taken, and returns where it went. It touches the disk, so the
/// window calls it on a worker thread.
fn save_under_free_name(
    folder: &Path,
    name: &str,
    write: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let path = super::unique_path(folder, name);
    write(&path)?;
    Ok(path)
}

/// The bytes of one file that came out of an encrypted message, when the
/// open thread holds them.
fn opened_file(view: &ConversationView, message_id: &str, index: usize) -> Option<Vec<u8>> {
    view.find(|open| open.opened_files.get(message_id)?.get(index).cloned())
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
    fn saving_a_file_twice_keeps_both_copies() {
        let folder = tempfile::tempdir().unwrap();
        let write = |text: &'static str| {
            save_under_free_name(folder.path(), "note.txt", move |path| {
                std::fs::write(path, text)
            })
            .unwrap()
        };
        let first = write("one");
        let second = write("two");
        assert_eq!(first, folder.path().join("note.txt"));
        assert_eq!(second, folder.path().join("note (2).txt"));
        assert_eq!(std::fs::read_to_string(first).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(second).unwrap(), "two");
    }

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
