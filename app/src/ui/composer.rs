//! The composer window: Markdown in, `multipart/alternative` out.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::{AccountId, Address};
use webkit::prelude::*;

use crate::compose::{
    Draft, OutgoingAttachment, build_mime, format_recipients, markdown_to_html, new_message_id,
    parse_recipients,
};
use crate::core::Core;
use crate::format::human_size;

/// An address the user can send from.
#[derive(Debug, Clone)]
pub struct Identity {
    pub account_id: AccountId,
    pub address: Address,
}

pub struct Composer {
    core: Rc<Core>,
    window: adw::Window,
    toasts: adw::ToastOverlay,
    title: adw::WindowTitle,
    from: gtk::DropDown,
    to: gtk::Entry,
    cc: gtk::Entry,
    subject: gtk::Entry,
    body: gtk::TextView,
    stack: gtk::Stack,
    preview: webkit::WebView,
    chips: gtk::FlowBox,
    send: gtk::Button,
    identities: Vec<Identity>,
    base: RefCell<Draft>,
    attachments: RefCell<Vec<OutgoingAttachment>>,
    dirty: Cell<bool>,
    closing: Cell<bool>,
    on_sent: Box<dyn Fn(AccountId)>,
}

impl Composer {
    /// Opens a composer for `draft`. `identities` lists every account; the
    /// draft's account is preselected.
    pub fn open(
        core: Rc<Core>,
        identities: Vec<Identity>,
        draft: Draft,
        on_sent: impl Fn(AccountId) + 'static,
    ) -> Rc<Composer> {
        let title = adw::WindowTitle::new("New Message", "");
        let send = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("mail-send-symbolic")
                    .label("Send")
                    .build(),
            )
            .css_classes(["suggested-action"])
            .tooltip_text("Send (Ctrl+Enter)")
            .build();
        let attach = gtk::Button::builder()
            .icon_name("mail-attachment-symbolic")
            .tooltip_text("Attach Files")
            .build();
        let preview_toggle = gtk::ToggleButton::builder()
            .icon_name("view-reveal-symbolic")
            .tooltip_text("Preview")
            .build();
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_end(&send);
        header.pack_end(&preview_toggle);
        header.pack_end(&attach);

        let labels: Vec<String> = identities
            .iter()
            .map(|i| format_recipients(std::slice::from_ref(&i.address)))
            .collect();
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let from = gtk::DropDown::from_strings(&label_refs);
        from.set_hexpand(true);
        from.add_css_class("flat");
        let selected = identities
            .iter()
            .position(|i| i.account_id == draft.account_id)
            .unwrap_or(0);
        from.set_selected(selected as u32);
        let to = entry("Recipients", &format_recipients(&draft.to));
        let cc = entry("", &format_recipients(&draft.cc));
        let subject = entry("", &draft.subject);

        let fields = gtk::Box::new(gtk::Orientation::Vertical, 0);
        fields.append(&field("From", &from));
        fields.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        fields.append(&field("To", &to));
        fields.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        fields.append(&field("Cc", &cc));
        fields.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        fields.append(&field("Subject", &subject));
        fields.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let body = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(18)
            .bottom_margin(18)
            .left_margin(20)
            .right_margin(20)
            .css_classes(["composer-body"])
            .vexpand(true)
            .build();
        let quote = gtk::TextTag::builder()
            .name("quote")
            .foreground("#777777")
            .left_margin(34)
            .build();
        body.buffer().tag_table().add(&quote);
        body.buffer().set_text(&draft.markdown);
        style_quotes(&body.buffer());
        body.buffer().place_cursor(&body.buffer().start_iter());
        let settings = webkit::Settings::new();
        settings.set_enable_javascript(false);
        let preview = webkit::WebView::builder().settings(&settings).build();
        preview.set_vexpand(true);
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .vexpand(true)
            .build();
        stack.add_named(
            &gtk::ScrolledWindow::builder().child(&body).build(),
            Some("edit"),
        );
        stack.add_named(&preview, Some("preview"));

        let chips = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .column_spacing(6)
            .row_spacing(6)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(10)
            .margin_top(4)
            .visible(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&fields);
        content.append(&stack);
        content.append(&chips);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&content));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&toolbar));
        let window = adw::Window::builder()
            .default_width(720)
            .default_height(640)
            .title("New Message")
            .content(&toasts)
            .build();

        let attachments = draft.attachments.clone();
        let composer = Rc::new(Composer {
            core,
            window,
            toasts,
            title,
            from,
            to,
            cc,
            subject,
            body,
            stack,
            preview,
            chips,
            send,
            identities,
            base: RefCell::new(draft),
            attachments: RefCell::new(attachments),
            dirty: Cell::new(false),
            closing: Cell::new(false),
            on_sent: Box::new(on_sent),
        });
        composer.refresh_chips();
        composer.update_title();
        composer.wire(&attach, &preview_toggle);
        composer.window.present();
        if composer.to.text().is_empty() {
            composer.to.grab_focus();
        } else {
            composer.body.grab_focus();
        }
        composer
    }

    pub fn window(&self) -> adw::Window {
        self.window.clone()
    }

    fn wire(self: &Rc<Self>, attach: &gtk::Button, preview_toggle: &gtk::ToggleButton) {
        let weak = Rc::downgrade(self);
        let mark_dirty = move || {
            if let Some(c) = weak.upgrade() {
                c.dirty.set(true);
                c.update_title();
            }
        };
        for entry in [&self.to, &self.cc, &self.subject] {
            let mark = mark_dirty.clone();
            entry.connect_changed(move |_| mark());
        }
        let mark = mark_dirty.clone();
        self.body.buffer().connect_changed(move |buffer| {
            style_quotes(buffer);
            mark();
        });

        let weak = Rc::downgrade(self);
        self.send.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.send();
            }
        });
        let weak = Rc::downgrade(self);
        attach.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.pick_files();
            }
        });
        let weak = Rc::downgrade(self);
        preview_toggle.connect_toggled(move |toggle| {
            let Some(c) = weak.upgrade() else { return };
            if toggle.is_active() {
                let dark = adw::StyleManager::default().is_dark();
                let html = format!(
                    "<!doctype html><html><head><meta charset=\"utf-8\"><style>body{{margin:24px;{}}}</style></head><body>{}</body></html>",
                    if dark { "background:#1e1e1e;filter:invert(0.92) hue-rotate(180deg)" } else { "background:#fff" },
                    markdown_to_html(&c.markdown())
                );
                c.preview.load_html(&html, None);
                c.stack.set_visible_child_name("preview");
            } else {
                c.stack.set_visible_child_name("edit");
            }
        });

        let shortcuts = gtk::ShortcutController::new();
        let add = |trigger: &str, run: Box<dyn Fn(&Rc<Composer>)>| {
            let weak = Rc::downgrade(self);
            shortcuts.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(gtk::CallbackAction::new(move |_, _| {
                    if let Some(c) = weak.upgrade() {
                        run(&c);
                    }
                    glib::Propagation::Stop
                })),
            ));
        };
        add("<Control>Return", Box::new(|c| c.send()));
        add("<Control>s", Box::new(|c| c.save_draft(false)));
        add("Escape", Box::new(|c| c.window.close()));
        self.window.add_controller(shortcuts);

        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            let Some(c) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if c.closing.get() || !c.dirty.get() || c.is_blank() {
                return glib::Propagation::Proceed;
            }
            c.confirm_close();
            glib::Propagation::Stop
        });
    }

    fn markdown(&self) -> String {
        let buffer = self.body.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    fn is_blank(&self) -> bool {
        self.to.text().trim().is_empty()
            && self.subject.text().trim().is_empty()
            && self.markdown().trim().is_empty()
            && self.attachments.borrow().is_empty()
    }

    fn identity(&self) -> Option<&Identity> {
        self.identities.get(self.from.selected() as usize)
    }

    fn update_title(&self) {
        let subject = self.subject.text();
        let title = if subject.trim().is_empty() {
            "New Message".to_string()
        } else {
            subject.to_string()
        };
        self.title.set_title(&title);
        self.window.set_title(Some(&title));
    }

    /// The draft as the fields describe it now.
    fn collect(&self) -> Option<Draft> {
        let identity = self.identity()?.clone();
        let base = self.base.borrow();
        let mut draft = base.clone();
        if draft.account_id != identity.account_id {
            // A different account cannot reply inside another account's thread.
            draft.thread_id = None;
            draft.draft_id = None;
        }
        draft.account_id = identity.account_id;
        draft.from = identity.address;
        draft.to = parse_recipients(&self.to.text());
        draft.cc = parse_recipients(&self.cc.text());
        draft.subject = self.subject.text().trim().to_string();
        draft.markdown = self.markdown();
        draft.attachments = self.attachments.borrow().clone();
        Some(draft)
    }

    fn toast(&self, text: &str) {
        self.toasts
            .add_toast(adw::Toast::builder().title(text).timeout(4).build());
    }

    fn send(self: &Rc<Self>) {
        let Some(draft) = self.collect() else { return };
        if let Some(problem) = draft.problem() {
            self.toast(&problem);
            return;
        }
        let raw = match build_mime(&draft, now_secs(), &new_message_id(&draft.from.email)) {
            Ok(raw) => raw,
            Err(err) => return self.toast(&format!("Could not build the message: {err}")),
        };
        let Some(account) = self.core.account(draft.account_id) else {
            return self.toast("That account is not connected. Check its status in the sidebar.");
        };
        self.set_busy(true);
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let (thread, draft_id) = (draft.thread_id.clone(), draft.draft_id.clone());
            match this
                .core
                .call(async move { account.send(raw, thread, draft_id).await })
                .await
            {
                Ok(_) => {
                    this.closing.set(true);
                    this.window.close();
                    (this.on_sent)(draft.account_id);
                }
                Err(err) => {
                    this.set_busy(false);
                    this.toast(&format!("Not sent: {err}"));
                }
            }
        });
    }

    /// Saves to Gmail drafts. With `then_close`, closes the window afterwards.
    fn save_draft(self: &Rc<Self>, then_close: bool) {
        let Some(draft) = self.collect() else { return };
        let Some(account) = self.core.account(draft.account_id) else {
            return self.toast("That account is not connected.");
        };
        let raw = match build_mime(&draft, now_secs(), &new_message_id(&draft.from.email)) {
            Ok(raw) => raw,
            Err(err) => return self.toast(&format!("Could not save: {err}")),
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let (thread, draft_id) = (draft.thread_id.clone(), draft.draft_id.clone());
            match this
                .core
                .call(async move { account.save_draft(raw, thread, draft_id).await })
                .await
            {
                Ok(id) => {
                    this.base.borrow_mut().draft_id = Some(id);
                    this.dirty.set(false);
                    if then_close {
                        this.closing.set(true);
                        this.window.close();
                    } else {
                        this.toast("Draft saved");
                    }
                    this.core.poke(draft.account_id);
                }
                Err(err) => this.toast(&format!("Draft not saved: {err}")),
            }
        });
    }

    fn confirm_close(self: &Rc<Self>) {
        let dialog = adw::AlertDialog::new(
            Some("Save as Draft?"),
            Some("The draft is kept in Gmail, so you can finish it later on any device."),
        );
        dialog.add_responses(&[
            ("discard", "Discard"),
            ("cancel", "Cancel"),
            ("save", "Save Draft"),
        ]);
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            match dialog.choose_future(Some(&this.window)).await.as_str() {
                "discard" => {
                    this.closing.set(true);
                    this.window.close();
                }
                "save" => this.save_draft(true),
                _ => {}
            }
        });
    }

    fn set_busy(&self, busy: bool) {
        self.send.set_sensitive(!busy);
        self.window.set_sensitive(!busy);
    }

    fn pick_files(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Attach Files")
            .modal(true)
            .build();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let Ok(files) = dialog.open_multiple_future(Some(&this.window)).await else {
                return;
            };
            for index in 0..files.n_items() {
                let Some(file) = files.item(index).and_downcast::<gio::File>() else {
                    continue;
                };
                match file.load_contents_future().await {
                    Ok((bytes, _)) => {
                        let filename = file
                            .basename()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "attachment".into());
                        let (mime, _) = gio::content_type_guess(Some(&filename), &bytes[..]);
                        let mime_type = gio::content_type_get_mime_type(&mime)
                            .map(|m| m.to_string())
                            .unwrap_or_else(|| "application/octet-stream".into());
                        this.attachments.borrow_mut().push(OutgoingAttachment {
                            filename,
                            mime_type,
                            data: bytes.to_vec(),
                        });
                        this.dirty.set(true);
                    }
                    Err(err) => this.toast(&format!("Could not read the file: {err}")),
                }
            }
            this.refresh_chips();
        });
    }

    fn refresh_chips(self: &Rc<Self>) {
        self.chips.remove_all();
        let attachments = self.attachments.borrow();
        self.chips.set_visible(!attachments.is_empty());
        for (index, attachment) in attachments.iter().enumerate() {
            let chip = gtk::Box::builder()
                .spacing(6)
                .css_classes(["attachment-chip"])
                .build();
            chip.append(&gtk::Image::from_icon_name("mail-attachment-symbolic"));
            chip.append(
                &gtk::Label::builder()
                    .label(format!(
                        "{}  {}",
                        attachment.filename,
                        human_size(attachment.data.len() as i64)
                    ))
                    .build(),
            );
            let remove = gtk::Button::builder()
                .icon_name("window-close-symbolic")
                .css_classes(["flat", "circular"])
                .tooltip_text("Remove")
                .build();
            let weak = Rc::downgrade(self);
            remove.connect_clicked(move |_| {
                if let Some(c) = weak.upgrade() {
                    c.attachments.borrow_mut().remove(index);
                    c.dirty.set(true);
                    c.refresh_chips();
                }
            });
            chip.append(&remove);
            self.chips.append(&chip);
        }
    }
}

/// Dims lines that start with `>`, so quoted text reads as quoted.
fn style_quotes(buffer: &gtk::TextBuffer) {
    buffer.remove_tag_by_name("quote", &buffer.start_iter(), &buffer.end_iter());
    for line in 0..buffer.line_count() {
        let Some(start) = buffer.iter_at_line(line) else {
            continue;
        };
        let mut end = start;
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        if buffer
            .text(&start, &end, false)
            .trim_start()
            .starts_with('>')
        {
            buffer.apply_tag_by_name("quote", &start, &end);
        }
    }
}

fn entry(placeholder: &str, text: &str) -> gtk::Entry {
    gtk::Entry::builder()
        .placeholder_text(placeholder)
        .text(text)
        .hexpand(true)
        .has_frame(false)
        .build()
}

fn field(label: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::builder()
        .spacing(8)
        .css_classes(["composer-field"])
        .build();
    row.append(
        &gtk::Label::builder()
            .label(label)
            .xalign(0.0)
            .css_classes(["dim-label"])
            .build(),
    );
    row.append(widget);
    row
}

fn now_secs() -> i64 {
    mailrs_sync::now_millis() / 1000
}
