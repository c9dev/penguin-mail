//! Contact photos use the app's decoder and a square crop before upload.
use crate::app::App;
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use mailrs_domain::translate::gettext;
use mailrs_store::address_book::Contact;
use std::rc::Rc;

pub(super) fn load(avatar: &adw::Avatar, contact: &Contact, app: &App) {
    let Some(file) = contact.photo_file.as_ref() else {
        return;
    };
    let path = app.core.contacts().photo_dir().join(file);
    let weak = avatar.downgrade();
    glib::spawn_future_local(async move {
        if let Some(texture) = read(&path).await
            && let Some(avatar) = weak.upgrade()
        {
            avatar.set_custom_image(Some(&texture));
        }
    });
}

async fn read(path: &std::path::Path) -> Option<gdk::Texture> {
    use std::io::Read;
    let path = path.to_owned();
    let bytes = super::super::texture::off_thread(super::super::texture::WAIT, move || {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        Some(bytes)
    })
    .await??;
    if bytes.len() > 16 * 1024 * 1024 {
        return None;
    }
    if super::super::texture::decoded_in_process(&bytes)
        && !super::super::texture::declared_size(&bytes)
            .is_some_and(|(w, h)| w > 0 && h > 0 && u64::from(w) * u64::from(h) <= 32_000_000)
    {
        return None;
    }
    let texture = super::super::texture::decode(glib::Bytes::from_owned(bytes)).await?;
    (texture.width() > 0
        && texture.height() > 0
        && i64::from(texture.width()) * i64::from(texture.height()) <= 32_000_000)
        .then_some(texture)
}

pub(super) fn choose(parent: &adw::Dialog, selected: impl Fn(Vec<u8>, gdk::Texture) + 'static) {
    let chooser = gtk::FileDialog::builder()
        .title(gettext("Choose Contact Photo"))
        .build();
    let filter = gtk::FileFilter::new();
    filter.add_mime_type("image/*");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    chooser.set_filters(Some(&filters));
    let weak = parent.downgrade();
    let root = parent.root().and_downcast::<gtk::Window>();
    glib::spawn_future_local(async move {
        let Ok(file) = chooser.open_future(root.as_ref()).await else {
            return;
        };
        let Some(path) = file.path() else {
            return;
        };
        // The crop needs no more than about 1024 pixels a side, so the
        // dialog never holds a camera-sized picture.
        let texture = match read(&path).await {
            Some(texture) => {
                super::super::texture::off_thread(super::super::texture::WAIT, move || {
                    crate::photos::shrunk_to(&texture, 1024)
                })
                .await
            }
            None => None,
        };
        let Some(parent) = weak.upgrade() else {
            return;
        };
        let Some(texture) = texture else {
            let error = adw::AlertDialog::builder()
                .heading(gettext("Could Not Open Photo"))
                .body(gettext(
                    "Choose an image smaller than 16 MB with no more than 32 million pixels.",
                ))
                .build();
            error.add_response("close", &gettext("Close"));
            error.present(Some(&parent));
            return;
        };
        crop_dialog(&parent, texture, selected);
    });
}

fn crop_dialog(
    parent: &adw::Dialog,
    texture: gdk::Texture,
    selected: impl Fn(Vec<u8>, gdk::Texture) + 'static,
) {
    let dialog = adw::Dialog::builder()
        .title(gettext("Crop Photo"))
        .content_width(400)
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_start(16)
        .margin_end(16)
        .margin_top(16)
        .margin_bottom(16)
        .build();
    let picture = gtk::Picture::builder()
        .width_request(300)
        .height_request(300)
        .can_shrink(true)
        .build();
    content.append(&picture);
    let slider = |label: &str, low, high, value| {
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, low, high, 0.01);
        scale.set_value(value);
        super::super::name(&scale, label);
        content.append(&gtk::Label::new(Some(label)));
        content.append(&scale);
        scale
    };
    let zoom = slider(&gettext("Zoom"), 1.0, 4.0, 1.0);
    let x = slider(&gettext("Horizontal Position"), 0.0, 1.0, 0.5);
    let y = slider(&gettext("Vertical Position"), 0.0, 1.0, 0.5);
    let mut downloader = gdk::TextureDownloader::new(&texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8Premultiplied);
    let (bytes, stride) = downloader.download_bytes();
    let pixels = Rc::new((
        bytes,
        stride,
        texture.width() as usize,
        texture.height() as usize,
    ));
    let preview: Rc<dyn Fn() -> gdk::Texture> = {
        let (zoom, x, y, pixels) = (
            zoom.downgrade(),
            x.downgrade(),
            y.downgrade(),
            pixels.clone(),
        );
        Rc::new(move || {
            let (bytes, stride, width, height) = &*pixels;
            let out = crop(
                bytes,
                *stride,
                *width,
                *height,
                zoom.upgrade().map_or(1.0, |s| s.value()),
                x.upgrade().map_or(0.5, |s| s.value()),
                y.upgrade().map_or(0.5, |s| s.value()),
            );
            gdk::MemoryTexture::new(
                256,
                256,
                gdk::MemoryFormat::R8g8b8a8Premultiplied,
                &glib::Bytes::from_owned(out),
                256 * 4,
            )
            .upcast()
        })
    };
    picture.set_paintable(Some(&preview()));
    for slider in [&zoom, &x, &y] {
        let (preview, picture) = (preview.clone(), picture.downgrade());
        slider.connect_value_changed(move |_| {
            if let Some(picture) = picture.upgrade() {
                picture.set_paintable(Some(&preview()));
            }
        });
    }
    let save = gtk::Button::with_label(&gettext("Use Photo"));
    save.add_css_class("suggested-action");
    content.append(&save);
    let weak = dialog.downgrade();
    save.connect_clicked(move |_| {
        let texture = preview();
        selected(texture.save_to_png_bytes().to_vec(), texture);
        if let Some(dialog) = weak.upgrade() {
            dialog.close();
        }
    });
    let view = adw::ToolbarView::builder().content(&content).build();
    view.add_top_bar(&adw::HeaderBar::new());
    dialog.set_child(Some(&view));
    dialog.present(Some(parent));
}

fn crop(
    bytes: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    zoom: f64,
    x: f64,
    y: f64,
) -> Vec<u8> {
    let side = (width.min(height) as f64 / zoom).max(1.0);
    let left = (width as f64 - side) * x;
    let top = (height as f64 - side) * y;
    let mut out = vec![0; 256 * 256 * 4];
    for row in 0..256 {
        for col in 0..256 {
            let sx = ((left + (col as f64 + 0.5) * side / 256.0) as usize).min(width - 1);
            let sy = ((top + (row as f64 + 0.5) * side / 256.0) as usize).min(height - 1);
            let start = sy * stride + sx * 4;
            let dest = (row * 256 + col) * 4;
            out[dest..dest + 4].copy_from_slice(&bytes[start..start + 4]);
        }
    }
    out
}

pub(super) fn view(parent: &adw::Window, contact: &Contact, app: &App) {
    let Some(file) = &contact.photo_file else {
        return;
    };
    let path = app.core.contacts().photo_dir().join(file);
    let weak = parent.downgrade();
    glib::spawn_future_local(async move {
        let texture = read(&path).await;
        let Some(parent) = weak.upgrade() else {
            return;
        };
        let dialog = adw::Dialog::builder()
            .title(gettext("Contact Photo"))
            .content_width(560)
            .content_height(560)
            .build();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        if let Some(texture) = texture {
            let picture = gtk::Picture::for_paintable(&texture);
            picture.set_can_shrink(true);
            view.set_content(Some(&picture));
        } else {
            view.set_content(Some(&gtk::Label::new(Some(&gettext(
                "Could Not Open Photo",
            )))));
        }
        dialog.set_child(Some(&view));
        dialog.present(Some(&parent));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_crop_can_reach_both_edges_and_keeps_alpha() {
        let bytes = [1, 2, 3, 255, 4, 5, 6, 128, 7, 8, 9, 64, 10, 11, 12, 0];
        let left = crop(&bytes, 16, 4, 1, 1.0, 0.0, 0.5);
        let right = crop(&bytes, 16, 4, 1, 1.0, 1.0, 0.5);
        assert_eq!(&left[..4], &bytes[..4]);
        assert_eq!(&right[..4], &bytes[12..]);
    }
}
