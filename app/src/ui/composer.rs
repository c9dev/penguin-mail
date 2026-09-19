//! The composer window: Markdown in, `multipart/alternative` out.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use mailrs_domain::{AccountId, Address};
use webkit::prelude::*;

use super::autocomplete::{self, Contacts};
use crate::compose::{
    Draft, LinePrefix, OutgoingAttachment, SendWhen, build_mime, format_recipients,
    markdown_to_html, new_message_id, parse_recipients, toggle_prefix,
};
use crate::core::Core;
use crate::format::{future_date, human_size, send_later_presets};

/// An address the user can send from.
#[derive(Debug, Clone)]
pub struct Identity {
    pub account_id: AccountId,
    pub address: Address,
}

type ComposerAction = Box<dyn Fn(&Rc<Composer>)>;

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
    send: adw::SplitButton,
    identities: Vec<Identity>,
    base: RefCell<Draft>,
    attachments: RefCell<Vec<OutgoingAttachment>>,
    dirty: Cell<bool>,
    closing: Cell<bool>,
    on_send: Box<dyn Fn(Draft, SendWhen)>,
}

impl Composer {
    /// Opens a composer for `draft`. `identities` lists every account; the
    /// draft's account is preselected. `on_send` receives the finished
    /// message; the composer closes itself.
    pub fn open(
        core: Rc<Core>,
        identities: Vec<Identity>,
        contacts: Contacts,
        draft: Draft,
        on_send: impl Fn(Draft, SendWhen) + 'static,
    ) -> Rc<Composer> {
        let title = adw::WindowTitle::new("New Message", "");
        let later = gio::Menu::new();
        for (label, at) in send_later_presets(chrono::Local::now()) {
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(Some("composer.send-at"), Some(&at.to_variant()));
            later.append_item(&item);
        }
        let custom = gio::Menu::new();
        custom.append(Some("Choose a Time…"), Some("composer.send-later"));
        later.append_section(None, &custom);
        let send = adw::SplitButton::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("mail-send-symbolic")
                    .label("Send")
                    .build(),
            )
            .menu_model(&later)
            .dropdown_tooltip("Send Later")
            .css_classes(["suggested-action"])
            .tooltip_text("Send (Ctrl+Shift+D)")
            .build();
        let attach = gtk::Button::builder()
            .icon_name("mail-attachment-symbolic")
            .tooltip_text("Attach Files (Ctrl+Shift+A)")
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
        autocomplete::attach(&to, Rc::clone(&contacts));
        autocomplete::attach(&cc, contacts);

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
        let format_bar = gtk::Box::builder()
            .spacing(2)
            .margin_start(12)
            .margin_end(12)
            .margin_top(4)
            .margin_bottom(4)
            .css_classes(["format-bar"])
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&fields);
        content.append(&format_bar);
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
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
            on_send: Box::new(on_send),
        });
        composer.refresh_chips();
        composer.update_title();
        composer.wire(&attach, &preview_toggle);
        composer.fill_format_bar(&format_bar);
        composer.accept_images();
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
        let actions = gio::SimpleActionGroup::new();
        let send_at = gio::SimpleAction::new("send-at", Some(glib::VariantTy::INT64));
        let weak = Rc::downgrade(self);
        send_at.connect_activate(move |_, at| {
            if let (Some(c), Some(at)) = (weak.upgrade(), at.and_then(|v| v.get::<i64>())) {
                c.hand_over(SendWhen::At(at));
            }
        });
        actions.add_action(&send_at);
        let choose = gio::SimpleAction::new("send-later", None);
        let weak = Rc::downgrade(self);
        choose.connect_activate(move |_, _| {
            if let Some(c) = weak.upgrade() {
                c.choose_send_time();
            }
        });
        actions.add_action(&choose);
        self.window.insert_action_group("composer", Some(&actions));

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
                    c.with_inline_images(markdown_to_html(&c.markdown()))
                );
                c.preview.load_html(&html, None);
                c.stack.set_visible_child_name("preview");
            } else {
                c.stack.set_visible_child_name("edit");
            }
        });

        // Capture phase: the text view binds Ctrl+Shift+A to "unselect all".
        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_propagation_phase(gtk::PropagationPhase::Capture);
        let add = |trigger: &str, run: ComposerAction| {
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
        add("<Control><Shift>d", Box::new(|c| c.send()));
        add("<Control><Shift>a", Box::new(|c| c.pick_files()));
        add("<Control>s", Box::new(|c| c.save_draft(false)));
        add("Escape", Box::new(|c| c.window.close()));
        self.window.add_controller(shortcuts);

        // Formatting keys run before the text view's own bindings.
        let formatting = gtk::ShortcutController::new();
        formatting.set_propagation_phase(gtk::PropagationPhase::Capture);
        for (trigger, before, after) in [
            ("<Control>b", "**", "**"),
            ("<Control>i", "*", "*"),
            ("<Control>k", "[", "]()"),
        ] {
            let weak = Rc::downgrade(self);
            formatting.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(gtk::CallbackAction::new(move |_, _| {
                    let Some(c) = weak.upgrade() else {
                        return glib::Propagation::Proceed;
                    };
                    if !c.body.has_focus() {
                        return glib::Propagation::Proceed;
                    }
                    wrap_selection(&c.body.buffer(), before, after);
                    glib::Propagation::Stop
                })),
            ));
        }
        self.body.add_controller(formatting);

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
        let subtitle = self.base.borrow().send_at.map(|at| {
            format!(
                "Scheduled to send {}",
                future_date(at, chrono::Local::now())
            )
        });
        self.title.set_subtitle(subtitle.as_deref().unwrap_or(""));
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

    /// Treats the message as unsaved, so closing asks before discarding it.
    pub fn mark_unsaved(&self) {
        self.dirty.set(true);
    }

    pub fn toast(&self, text: &str) {
        self.toasts
            .add_toast(adw::Toast::builder().title(text).timeout(4).build());
    }

    fn send(self: &Rc<Self>) {
        self.hand_over(SendWhen::Now);
    }

    /// Passes the finished message on for sending and closes.
    fn hand_over(self: &Rc<Self>, when: SendWhen) {
        let Some(draft) = self.collect() else { return };
        if let Some(problem) = draft.problem() {
            self.toast(&problem);
            return;
        }
        if let SendWhen::At(at) = when
            && at <= mailrs_sync::now_millis()
        {
            self.toast("Choose a time in the future");
            return;
        }
        if let Err(err) = build_mime(&draft, now_secs(), &new_message_id(&draft.from.email)) {
            self.toast(&format!("Could not build the message: {err}"));
            return;
        }
        self.closing.set(true);
        self.window.close();
        (self.on_send)(draft, when);
    }

    /// Asks for a date and time, then schedules the message.
    fn choose_send_time(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if let Some(at) = super::when::pick_time(
                &this.window,
                "Send Later",
                "Penguin Mail sends it at this time while it runs, even in the tray.",
                "Schedule",
            )
            .await
            {
                this.hand_over(SendWhen::At(at));
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
                Ok(saved) => {
                    let (account_id, draft_id) = (draft.account_id, saved.draft_id.clone());
                    this.base.borrow_mut().draft_id = Some(saved.draft_id.clone());
                    // A scheduled draft keeps its time; point it at the new message.
                    this.core.spawn_write(move |c| {
                        mailrs_store::scheduled::set_message(
                            c,
                            account_id,
                            &draft_id,
                            &saved.message_id,
                            &saved.thread_id,
                        )
                    });
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
                if let Some(file) = files.item(index).and_downcast::<gio::File>() {
                    this.add_file(&file, false).await;
                }
            }
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
            chip.append(&gtk::Image::from_icon_name(
                if attachment.content_id.is_some() {
                    "image-x-generic-symbolic"
                } else {
                    "mail-attachment-symbolic"
                },
            ));
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
                    let removed = c.attachments.borrow_mut().remove(index);
                    if let Some(cid) = removed.content_id {
                        c.remove_image_reference(&cid);
                    }
                    c.dirty.set(true);
                    c.refresh_chips();
                }
            });
            chip.append(&remove);
            self.chips.append(&chip);
        }
    }
}

impl Composer {
    fn fill_format_bar(self: &Rc<Self>, bar: &gtk::Box) {
        let button = |icon: &str, tip: &str| {
            let button = gtk::Button::builder()
                .tooltip_text(tip)
                .css_classes(["flat"])
                .can_focus(false)
                .build();
            // Letters read better than the text-style icons at this size.
            match icon.strip_prefix("text:") {
                Some(markup) => button.set_child(Some(
                    &gtk::Label::builder()
                        .label(markup)
                        .use_markup(true)
                        .width_chars(2)
                        .build(),
                )),
                None => button.set_icon_name(icon),
            }
            bar.append(&button);
            button
        };
        let wraps: [(&str, &str, &'static str, &'static str); 4] = [
            ("text:<b>B</b>", "Bold (Ctrl+B)", "**", "**"),
            ("text:<i>I</i>", "Italic (Ctrl+I)", "*", "*"),
            ("text:<s>S</s>", "Strikethrough", "~~", "~~"),
            ("penguin-mail-link-symbolic", "Link (Ctrl+K)", "[", "]()"),
        ];
        for (icon, tip, before, after) in wraps {
            let weak = Rc::downgrade(self);
            button(icon, tip).connect_clicked(move |_| {
                if let Some(c) = weak.upgrade() {
                    wrap_selection(&c.body.buffer(), before, after);
                    c.body.grab_focus();
                }
            });
        }
        bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        let prefixes = [
            (
                "view-list-bullet-symbolic",
                "Bulleted List",
                LinePrefix::Bullet,
            ),
            (
                "view-list-ordered-symbolic",
                "Numbered List",
                LinePrefix::Numbered,
            ),
            ("format-indent-more-symbolic", "Quote", LinePrefix::Quote),
        ];
        for (icon, tip, prefix) in prefixes {
            let weak = Rc::downgrade(self);
            button(icon, tip).connect_clicked(move |_| {
                if let Some(c) = weak.upgrade() {
                    prefix_lines(&c.body.buffer(), prefix);
                    c.body.grab_focus();
                }
            });
        }
        bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        let weak = Rc::downgrade(self);
        button("image-x-generic-symbolic", "Insert Image").connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.pick_images();
            }
        });
    }

    fn pick_images(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Images"));
        filter.add_mime_type("image/*");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Insert Image")
            .modal(true)
            .filters(&filters)
            .build();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if let Ok(files) = dialog.open_multiple_future(Some(&this.window)).await {
                for index in 0..files.n_items() {
                    if let Some(file) = files.item(index).and_downcast::<gio::File>() {
                        this.add_file(&file, true).await;
                    }
                }
            }
        });
    }

    /// Reads `file` in. Images go into the text when `inline` allows it;
    /// everything else becomes an attachment.
    async fn add_file(self: &Rc<Self>, file: &gio::File, inline: bool) {
        match file.load_contents_future().await {
            Ok((bytes, _)) => {
                let filename = file
                    .basename()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "attachment".into());
                let (guess, _) = gio::content_type_guess(Some(&filename), &bytes[..]);
                let mime_type = gio::content_type_get_mime_type(&guess)
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "application/octet-stream".into());
                if inline && mime_type.starts_with("image/") {
                    self.add_inline_image(filename, mime_type, bytes.to_vec());
                } else {
                    self.attachments.borrow_mut().push(OutgoingAttachment {
                        filename,
                        mime_type,
                        data: bytes.to_vec(),
                        content_id: None,
                    });
                    self.dirty.set(true);
                    self.refresh_chips();
                }
            }
            Err(err) => self.toast(&format!("Could not read the file: {err}")),
        }
    }

    /// Adds an image and puts `![name](cid:…)` at the cursor.
    fn add_inline_image(self: &Rc<Self>, filename: String, mime_type: String, data: Vec<u8>) {
        let cid = format!("{}@mailrs", mailrs_gmail::random_token(9));
        let buffer = self.body.buffer();
        let alt: String = filename
            .chars()
            .filter(|c| !matches!(c, '[' | ']'))
            .collect();
        buffer.insert_at_cursor(&format!("![{alt}](cid:{cid})"));
        self.attachments.borrow_mut().push(OutgoingAttachment {
            filename,
            mime_type,
            data,
            content_id: Some(cid),
        });
        self.dirty.set(true);
        self.refresh_chips();
    }

    /// Takes the image reference for `cid` out of the text.
    fn remove_image_reference(&self, cid: &str) {
        let text = self.markdown();
        let needle = format!("](cid:{cid})");
        let Some(end) = text.find(&needle) else {
            return;
        };
        let Some(start) = text[..end].rfind("![") else {
            return;
        };
        let buffer = self.body.buffer();
        let offset = |byte: usize| text[..byte].chars().count() as i32;
        let (mut from, mut to) = (
            buffer.iter_at_offset(offset(start)),
            buffer.iter_at_offset(offset(end + needle.len())),
        );
        buffer.delete(&mut from, &mut to);
    }

    /// `html` with inline images pointed at their data, for the preview.
    fn with_inline_images(&self, mut html: String) -> String {
        for attachment in self.attachments.borrow().iter() {
            if let Some(cid) = &attachment.content_id {
                let data = base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    &attachment.data,
                );
                html = html.replace(
                    &format!("cid:{cid}"),
                    &format!("data:{};base64,{data}", attachment.mime_type),
                );
            }
        }
        html
    }

    /// Pasted images go into the text; dropped files go in or attach.
    fn accept_images(self: &Rc<Self>) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(c) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let paste = modifiers.contains(gdk::ModifierType::CONTROL_MASK)
                && matches!(key, gdk::Key::v | gdk::Key::V);
            let clipboard = c.body.clipboard();
            let formats = clipboard.formats();
            if !paste
                || !formats.contains_type(gdk::Texture::static_type())
                || formats.contain_mime_type("text/plain")
            {
                return glib::Propagation::Proceed;
            }
            glib::spawn_future_local(async move {
                match clipboard.read_texture_future().await {
                    Ok(Some(texture)) => {
                        let png = texture.save_to_png_bytes();
                        c.add_inline_image(
                            "pasted-image.png".into(),
                            "image/png".into(),
                            png.to_vec(),
                        );
                    }
                    _ => c.toast("Could not paste the image"),
                }
            });
            glib::Propagation::Stop
        });
        self.body.add_controller(keys);

        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let weak = Rc::downgrade(self);
        drop.connect_drop(move |_, value, _, _| {
            let (Some(c), Ok(list)) = (weak.upgrade(), value.get::<gdk::FileList>()) else {
                return false;
            };
            glib::spawn_future_local(async move {
                for file in list.files() {
                    c.add_file(&file, true).await;
                }
            });
            true
        });
        self.window.add_controller(drop);
    }
}

/// Adds or removes a list or quote prefix on every line the selection
/// touches, then selects the changed lines.
fn prefix_lines(buffer: &gtk::TextBuffer, prefix: LinePrefix) {
    let (mut start, mut end) = buffer.selection_bounds().unwrap_or_else(|| {
        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        (cursor, cursor)
    });
    start.set_line_offset(0);
    if !end.ends_line() {
        end.forward_to_line_end();
    }
    let text = buffer.text(&start, &end, false).to_string();
    let changed = toggle_prefix(&text, prefix);
    buffer.begin_user_action();
    let offset = start.offset();
    buffer.delete(&mut start, &mut end);
    buffer.insert(&mut start, &changed);
    let first = buffer.iter_at_offset(offset);
    let last = buffer.iter_at_offset(offset + changed.chars().count() as i32);
    buffer.select_range(&first, &last);
    buffer.end_user_action();
}

/// Puts Markdown markers around the selection, or around the cursor when
/// nothing is selected. A link leaves the cursor between its parentheses.
/// Pressed again right before the closing marker, moves past it.
fn wrap_selection(buffer: &gtk::TextBuffer, before: &str, after: &str) {
    let (mut start, mut end) = buffer.selection_bounds().unwrap_or_else(|| {
        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        (cursor, cursor)
    });
    let selected = start != end;
    // Pressed again inside empty markers: step out, or into a link's URL.
    if !selected {
        let mut ahead = start;
        ahead.forward_chars(after.chars().count() as i32);
        if buffer.text(&start, &ahead, false) == after {
            let step = if after == "]()" {
                2
            } else {
                after.len() as i32
            };
            buffer.place_cursor(&buffer.iter_at_offset(start.offset() + step));
            return;
        }
    }
    let text = buffer.text(&start, &end, false).to_string();
    buffer.begin_user_action();
    buffer.delete(&mut start, &mut end);
    let offset = start.offset();
    buffer.insert(&mut start, &format!("{before}{text}{after}"));
    let cursor = match (selected, after) {
        (true, "]()") => offset + (before.len() + text.chars().count() + 2) as i32,
        (true, _) => offset + (before.len() + text.chars().count() + after.len()) as i32,
        (false, _) => offset + before.len() as i32,
    };
    buffer.place_cursor(&buffer.iter_at_offset(cursor));
    buffer.end_user_action();
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
