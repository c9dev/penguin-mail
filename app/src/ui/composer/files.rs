//! Preview the bytes attached to this draft, even if the original file has changed.
use super::*;

impl Composer {
    pub(super) fn preview_file(self: &Rc<Self>, index: usize) {
        let Some(file) = self.attachments.borrow().get(index).cloned() else {
            return;
        };
        let weak = Rc::downgrade(self);
        let folder = self
            .core
            .demo_folder()
            .map(|p| p.join("previews"))
            .unwrap_or_else(super::super::window::previews::folder);
        glib::spawn_future_local(async move {
            if file.mime_type.starts_with("image/") {
                let bytes = glib::Bytes::from(&file.data[..]);
                if let Some(texture) = super::super::texture::decode(bytes).await {
                    if let Some(c) = weak.upgrade().filter(|c| c.window.is_visible()) {
                        let picture = gtk::Picture::for_paintable(&texture);
                        picture.set_content_fit(gtk::ContentFit::ScaleDown);
                        name(
                            &picture,
                            &fill(&gettext("{file}, preview"), &[("file", &file.filename)]),
                        );
                        let toolbar = adw::ToolbarView::new();
                        toolbar.add_top_bar(&adw::HeaderBar::new());
                        toolbar.set_content(Some(&picture));
                        adw::Dialog::builder()
                            .title(&file.filename)
                            .content_width(640)
                            .content_height(480)
                            .child(&toolbar)
                            .build()
                            .present(Some(&c.window));
                    }
                    return;
                }
            }
            let saved = file.clone();
            let written = gio::spawn_blocking(move || {
                super::super::window::previews::write(&folder, &saved.filename, &saved.data)
            })
            .await;
            let Some(c) = weak.upgrade().filter(|c| c.window.is_visible()) else {
                if let Ok(Ok(path)) = written {
                    let _ = std::fs::remove_file(path);
                }
                return;
            };
            let path = match written {
                Ok(Ok(path)) => path,
                _ => {
                    c.toast(&fill(
                        &gettext("Could not open {file}"),
                        &[("file", &file.filename)],
                    ));
                    return;
                }
            };
            let source = gio::File::for_path(&path);
            // Drafts may contain unsent private files. Their scratch copies go with the composer.
            c.window.connect_destroy(move |_| {
                if let Err(error) = std::fs::remove_file(&path)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(%error, "could not delete a draft attachment preview");
                }
            });
            let parent = c.window.clone();
            let weak = Rc::downgrade(&c);
            gtk::FileLauncher::new(Some(&source)).launch(
                Some(&parent),
                gio::Cancellable::NONE,
                move |result| {
                    if let Err(error) = result
                        && !error.matches(gtk::DialogError::Dismissed)
                        && let Some(c) = weak.upgrade()
                    {
                        c.toast(&with_reason(
                            &gettext("Could not open {file}: {reason}"),
                            &error,
                            &[("file", &file.filename)],
                        ));
                    }
                },
            );
        });
    }
}
