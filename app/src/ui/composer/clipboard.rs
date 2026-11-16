//! Image paste shares the text view's action, including its menu and keyboard paths.
use super::*;

fn offers_image(texture: bool, mime_types: &[&str]) -> bool {
    texture || mime_types.iter().any(|mime| mime.starts_with("image/"))
}

impl Composer {
    pub(super) fn paste_image(self: &Rc<Self>, view: &gtk::TextView) -> bool {
        let clipboard = view.clipboard();
        let formats = clipboard.formats();
        let offered = formats.mime_types();
        if !offers_image(
            formats.contains_type(gdk::Texture::static_type()),
            &offered.iter().map(|mime| mime.as_str()).collect::<Vec<_>>(),
        ) {
            return false;
        }
        view.stop_signal_emission_by_name("paste-clipboard");
        if self.locked.get() || self.work.is_busy() || !view.is_editable() {
            return true;
        }
        let Some(ticket) = self.work.begin() else {
            return true;
        };
        self.show_busy(false);
        let buffer = view.buffer();
        let mark = buffer.create_mark(None, &buffer.iter_at_mark(&buffer.get_insert()), true);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let image = clipboard.read_texture_future().await;
            let at = buffer.iter_at_mark(&mark);
            buffer.delete_mark(&mark);
            let Some(c) = weak.upgrade() else { return };
            if !c.work.finish(ticket) || !c.window.is_visible() || c.locked.get() {
                return;
            }
            c.show_idle();
            match image {
                Ok(Some(texture)) => {
                    let png = texture.save_to_png_bytes();
                    buffer.place_cursor(&at);
                    c.add_inline_image(
                        "pasted-image.png".into(),
                        "",
                        "image/png".into(),
                        png.to_vec(),
                    );
                }
                _ => c.toast(&gettext("Could not paste the image")),
            }
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::offers_image;

    #[test]
    fn an_image_wins_over_its_text_fallback_and_plain_text_stays_text() {
        assert!(offers_image(true, &["text/plain"]));
        assert!(offers_image(false, &["image/tiff", "text/plain"]));
        assert!(offers_image(false, &["image/png"]));
        assert!(!offers_image(false, &["text/plain", "text/html"]));
        assert!(!offers_image(false, &["text/uri-list"]));
    }
}

#[cfg(test)]
pub(super) mod checks {
    use super::*;

    pub fn run() {
        let core = Core::open(true).expect("isolated demo core");
        let address = Address {
            name: Some("Writer".into()),
            email: "writer@example.com".into(),
        };
        let c = Composer::open(
            core,
            Writing {
                identities: vec![Identity {
                    suffixes: Default::default(),
                    account_id: 1,
                    account_email: address.email.clone(),
                    address: address.clone(),
                    signature: String::new(),
                    default: true,
                    masks: false,
                }],
                signature: String::new(),
                last_used: Vec::new(),
                dictionaries: Box::pin(async { Rc::new(spell::Dictionaries::load(&[], &[])) }),
                remember: Rc::new(|_| {}),
                check_attachments: false,
                sign_by_default: false,
                encrypt_when_possible: false,
            },
            Default::default(),
            Draft::new(1, address),
            ComposeFormat::Rich,
            |_, _, _| {},
        );
        let texture = gdk::MemoryTexture::new(
            1,
            1,
            gdk::MemoryFormat::R8g8b8a8,
            &glib::Bytes::from_static(&[0, 120, 255, 255]),
            4,
        );
        let clipboard = c.body.clipboard();
        let offer = gdk::ContentProvider::new_union(&[
            gdk::ContentProvider::for_value(&texture.upcast::<gdk::Texture>().to_value()),
            gdk::ContentProvider::for_value(&"image fallback".to_value()),
        ]);
        clipboard.set_content(Some(&offer)).unwrap();
        let context = glib::MainContext::default();
        for format in [ComposeFormat::Rich, ComposeFormat::Markdown] {
            c.editor.switch_format(format, &c.attachments.borrow());
            let before = c.attachments.borrow().len();
            c.body.emit_by_name::<()>("paste-clipboard", &[]);
            context.block_on(async {
                for _ in 0..100 {
                    if c.attachments.borrow().len() > before {
                        return;
                    }
                    glib::timeout_future(std::time::Duration::from_millis(10)).await;
                }
                panic!("image paste never arrived");
            });
            assert_eq!(c.attachments.borrow().len(), before + 1);
            let files = c.attachments.borrow();
            let file = files.last().unwrap();
            assert_eq!(file.mime_type, "image/png");
            assert!(file.content_id.is_some());
            assert!(
                c.editor
                    .html()
                    .contains(&format!("cid:{}", file.content_id.as_ref().unwrap()))
            );
            assert!(!c.editor.source().contains("image fallback"));
        }
        let mut file = c.attachments.borrow()[0].clone();
        file.content_id = None;
        file.filename = "preview.png".into();
        c.attachments.borrow_mut().push(file.clone());
        c.refresh_files();
        let row = c
            .files
            .last_child()
            .unwrap()
            .downcast::<gtk::Box>()
            .unwrap();
        let open = row
            .first_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        open.emit_clicked();
        context.block_on(async {
            for _ in 0..100 {
                if c.window.visible_dialog().is_some() {
                    return;
                }
                glib::timeout_future(std::time::Duration::from_millis(10)).await;
            }
            panic!("the attachment preview never opened");
        });
        let preview = c.window.visible_dialog().unwrap();
        assert_eq!(preview.title(), "preview.png");
        assert_eq!(c.attachments.borrow().last(), Some(&file));
        preview.close();
        let before = c.attachments.borrow().len();
        c.locked.set(true);
        c.body.emit_by_name::<()>("paste-clipboard", &[]);
        assert_eq!(c.attachments.borrow().len(), before);
        clipboard
            .set_content(None::<&gdk::ContentProvider>)
            .unwrap();
        c.closing.set(true);
        c.window.close();
    }
}
