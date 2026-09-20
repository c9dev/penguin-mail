//! The composer window: rich text or Markdown in, `multipart/alternative`
//! out.
//!
//! In rich text the buffer holds the styles themselves, so bold reads as
//! bold while you write it, and [`richbuffer`] turns the buffer into the
//! rich body the message goes out as. In Markdown the buffer holds the
//! source, as it always did, and Format Markdown moves a body from one to
//! the other.

mod recipients;
mod richbuffer;
pub mod spell;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use mailrs_store::templates::Template;
use webkit::prelude::*;

use self::recipients::Recipients;
use super::autocomplete::Contacts;
use crate::attachcheck::{self, Promise};
use crate::compose::{
    Draft, LinePrefix, OutgoingAttachment, SendWhen, build_mime, format_recipients,
    markdown_to_html, new_message_id, opening_identity, restyle_signature, toggle_prefix,
};
use crate::core::Core;
use crate::format::{future_date, human_size, send_later_presets};
use crate::richtext::{Block, BlockKind, RichBody, Style};
use crate::settings::ComposeFormat;
use crate::templates::{self, Filling};

pub use crate::compose::Identity;

/// What the composer needs from the app beyond the draft itself: every
/// address the accounts send as, the one each account last used, the
/// dictionaries, and somewhere to report what the writer taught it.
pub struct Writing {
    pub identities: Vec<Identity>,
    /// Account address to the send-as address it last sent from.
    pub last_used: Vec<(String, String)>,
    /// The dictionaries every composer shares. Reading one takes long
    /// enough to stutter a window, so this settles after the composer opens.
    pub dictionaries: futures::future::LocalBoxFuture<'static, Rc<spell::Dictionaries>>,
    /// Called with the address a message goes out from, and with every word
    /// Add to Dictionary keeps, so Preferences remembers both.
    pub remember: Rc<dyn Fn(Remembered)>,
    /// Ask before a message that promises a file goes without one.
    pub check_attachments: bool,
    /// Start with Sign on.
    pub sign_by_default: bool,
    /// Turn Encrypt on as soon as gpg holds a key for every recipient.
    pub encrypt_when_possible: bool,
}

/// Something the composer learned that outlives it.
pub enum Remembered {
    /// This account sent from this address.
    SentFrom { account: String, email: String },
    /// Add to Dictionary was used on this word.
    Word(String),
}

type ComposerAction = Box<dyn Fn(&Rc<Composer>)>;

pub struct Composer {
    core: Rc<Core>,
    window: adw::Window,
    toasts: adw::ToastOverlay,
    title: adw::WindowTitle,
    from: gtk::DropDown,
    to: Rc<Recipients>,
    cc: Rc<Recipients>,
    bcc: Rc<Recipients>,
    /// The Cc and Bcc rows, hidden until someone wants them.
    more: Vec<gtk::Widget>,
    more_button: gtk::ToggleButton,
    subject: gtk::Entry,
    body: gtk::TextView,
    stack: gtk::Stack,
    preview: webkit::WebView,
    /// The attachment rows and the box that holds them.
    files: gtk::Box,
    /// The strip naming the message this draft forwards.
    forwarded: gtk::Box,
    send: adw::SplitButton,
    /// Sign and Encrypt, which this computer's gpg answers for. Both stay
    /// out of the window when there is no gpg to run.
    sign: gtk::ToggleButton,
    encrypt: gtk::ToggleButton,
    /// The addresses the key check last asked gpg about, so a writer
    /// typing an address does not start a gpg for every letter.
    asked_keys: RefCell<Vec<String>>,
    /// Bumped whenever the recipients change; a check that finds it moved
    /// on stands down.
    key_check: Cell<u64>,
    /// Set while the composer works the Sign and Encrypt toggles itself,
    /// so doing so does not read as the writer choosing.
    filling_keys: Cell<bool>,
    /// Set once the writer worked Encrypt themselves, after which
    /// "Encrypt when I can" leaves it alone.
    encrypt_chosen: Cell<bool>,
    encrypt_when_possible: bool,
    /// The formatting bar's toggles, each with the tag it stands for.
    toggles: RefCell<Vec<(gtk::ToggleButton, &'static str)>>,
    identities: Vec<Identity>,
    /// Which identity the From row is on, so a change knows what to undo.
    showing: Cell<usize>,
    /// The spell checker marking up the body, when one could start.
    spell: RefCell<Option<Rc<spell::SpellCheck>>>,
    /// The saved templates, and the menu section that lists them.
    templates: RefCell<Vec<Template>>,
    template_items: gio::Menu,
    remember: Rc<dyn Fn(Remembered)>,
    base: RefCell<Draft>,
    attachments: RefCell<Vec<OutgoingAttachment>>,
    anchors: RefCell<richbuffer::Anchors>,
    format: Cell<ComposeFormat>,
    /// The style the next typed character takes, and where it applies.
    typing: RefCell<Option<(i32, Style, Option<String>)>>,
    /// Text just inserted, waiting for its style: offset and length.
    inserted: RefCell<Vec<(i32, i32)>>,
    /// True while the composer edits the buffer itself.
    busy: Cell<bool>,
    /// Whether a message that promises a file is worth asking about.
    check_attachments: bool,
    /// Set once Send Anyway answered the missing attachment dialog, so the
    /// same message is not asked about twice.
    asked: Cell<bool>,
    dirty: Cell<bool>,
    closing: Cell<bool>,
    on_send: Box<dyn Fn(Draft, SendWhen)>,
}

impl Composer {
    /// Opens a composer for `draft`. `writing` carries every address the
    /// accounts send as; the From row starts on the one the draft names.
    /// `format` is what a message starts as. `on_send` receives the
    /// finished message; the composer closes itself.
    pub fn open(
        core: Rc<Core>,
        writing: Writing,
        contacts: Contacts,
        draft: Draft,
        format: ComposeFormat,
        on_send: impl Fn(Draft, SendWhen) + 'static,
    ) -> Rc<Composer> {
        let Writing {
            identities,
            last_used,
            dictionaries,
            remember,
            check_attachments,
            sign_by_default,
            encrypt_when_possible,
        } = writing;
        let has_gpg = core.has_gpg();
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
            .tooltip_text("Send (Ctrl+Enter)")
            .build();
        let attach = gtk::Button::builder()
            .icon_name("mail-attachment-symbolic")
            .tooltip_text("Attach Files (Ctrl+Shift+A)")
            .build();
        let preview_toggle = gtk::ToggleButton::builder()
            .icon_name("view-reveal-symbolic")
            .tooltip_text("Preview")
            .build();
        let template_items = gio::Menu::new();
        let template_menu = gio::Menu::new();
        template_menu.append_section(None, &template_items);
        let saving = gio::Menu::new();
        saving.append(Some("Save as Template…"), Some("composer.save-template"));
        template_menu.append_section(None, &saving);
        let template_button = gtk::MenuButton::builder()
            .icon_name("insert-text-symbolic")
            .tooltip_text("Templates")
            .menu_model(&template_menu)
            .build();
        let sign = gtk::ToggleButton::builder()
            .label("Sign")
            .tooltip_text("Sign this message with your own key")
            .active(has_gpg && sign_by_default)
            .build();
        let encrypt = gtk::ToggleButton::builder()
            .label("Encrypt")
            .tooltip_text("Add a recipient whose key gpg holds.")
            .sensitive(false)
            .build();
        let protection = gtk::Box::builder()
            .css_classes(["linked"])
            .visible(has_gpg)
            .build();
        protection.append(&sign);
        protection.append(&encrypt);
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_end(&send);
        header.pack_end(&preview_toggle);
        header.pack_end(&attach);
        header.pack_end(&template_button);
        header.pack_end(&protection);

        let from = from_dropdown(&identities);
        let last = last_used
            .iter()
            .find(|(account, _)| {
                identities
                    .iter()
                    .any(|i| i.account_id == draft.account_id && i.account_email == *account)
            })
            .map(|(_, email)| email.as_str());
        let selected =
            opening_identity(&identities, draft.account_id, &draft.from, last).unwrap_or(0);
        from.set_selected(selected as u32);
        let to = Recipients::new("Recipients", &draft.to, Rc::clone(&contacts));
        let cc = Recipients::new("Carbon copy", &draft.cc, Rc::clone(&contacts));
        let bcc = Recipients::new("Blind carbon copy", &draft.bcc, contacts);
        let subject = gtk::Entry::builder()
            .placeholder_text("Subject")
            .text(&draft.subject)
            .hexpand(true)
            .has_frame(false)
            .build();

        let more_button = gtk::ToggleButton::builder()
            .label("Cc/Bcc")
            .tooltip_text("Show Cc and Bcc")
            .css_classes(["flat", "cc-toggle"])
            // Stays on the first line when the chips wrap below it.
            .valign(gtk::Align::Start)
            .can_focus(false)
            .active(!draft.cc.is_empty() || !draft.bcc.is_empty())
            .build();
        let fields = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // One size group holds the label column to a single width, so every
        // field starts at the same edge whichever labels are on show.
        let column = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        let from_row = field("From", &from, &column);
        if identities.len() > 1 {
            fields.append(&from_row);
            fields.append(&line());
        }
        let to_row = field("To", &to.field, &column);
        to_row.append(&more_button);
        fields.append(&to_row);
        fields.append(&line());
        let cc_row = field("Cc", &cc.field, &column);
        let cc_line = line();
        let bcc_row = field("Bcc", &bcc.field, &column);
        let bcc_line = line();
        for widget in [
            cc_row.clone().upcast::<gtk::Widget>(),
            cc_line.clone().upcast(),
            bcc_row.clone().upcast(),
            bcc_line.clone().upcast(),
        ] {
            fields.append(&widget);
        }
        fields.append(&field("Subject", &subject, &column));
        fields.append(&line());

        let body = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(18)
            .bottom_margin(18)
            .left_margin(20)
            .right_margin(20)
            .accepts_tab(false)
            .css_classes(["composer-body"])
            .vexpand(true)
            .build();
        richbuffer::install(&body.buffer());
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

        let files = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(10)
            .margin_top(6)
            .visible(false)
            .build();
        let forwarded = gtk::Box::builder()
            .spacing(8)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .css_classes(["attachment-row"])
            .visible(false)
            .build();
        let format_bar = gtk::Box::builder()
            .spacing(2)
            .margin_start(10)
            .margin_end(10)
            .margin_top(4)
            .margin_bottom(4)
            .css_classes(["format-bar"])
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&fields);
        content.append(&format_bar);
        content.append(&line());
        content.append(&stack);
        content.append(&forwarded);
        content.append(&files);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&content));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&toolbar));
        let window = adw::Window::builder()
            .default_width(760)
            .default_height(660)
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
            bcc,
            more: vec![
                cc_row.upcast(),
                cc_line.upcast(),
                bcc_row.upcast(),
                bcc_line.upcast(),
            ],
            more_button,
            subject,
            body,
            stack,
            preview,
            files,
            forwarded,
            send,
            sign,
            encrypt,
            asked_keys: RefCell::new(Vec::new()),
            key_check: Cell::new(0),
            filling_keys: Cell::new(false),
            encrypt_chosen: Cell::new(false),
            encrypt_when_possible: has_gpg && encrypt_when_possible,
            toggles: RefCell::new(Vec::new()),
            identities,
            showing: Cell::new(selected),
            spell: RefCell::new(None),
            templates: RefCell::new(Vec::new()),
            template_items,
            remember,
            base: RefCell::new(draft),
            attachments: RefCell::new(attachments),
            anchors: RefCell::new(Vec::new()),
            format: Cell::new(format),
            typing: RefCell::new(None),
            inserted: RefCell::new(Vec::new()),
            busy: Cell::new(false),
            check_attachments,
            asked: Cell::new(false),
            dirty: Cell::new(false),
            closing: Cell::new(false),
            on_send: Box::new(on_send),
        });
        composer.fill_body();
        composer.refresh_files();
        composer.refresh_forwarded();
        composer.update_title();
        composer.show_more(composer.more_button.is_active());
        composer.wire(&attach, &preview_toggle);
        composer.fill_format_bar(&format_bar);
        composer.accept_images();
        composer.check_send();
        composer.check_keys();
        composer.load_templates();
        // Preferences may add a template while this window is open, so the
        // list is read again each time the menu is asked for.
        let weak = Rc::downgrade(&composer);
        template_button.set_create_popup_func(move |_| {
            if let Some(c) = weak.upgrade() {
                c.load_templates();
            }
        });
        let this = Rc::clone(&composer);
        glib::spawn_future_local(async move { this.check_spelling(dictionaries.await) });
        composer.window.present();
        if composer.to.is_empty() {
            composer.to.entry.grab_focus();
        } else {
            composer.body.grab_focus();
        }
        composer
    }

    pub fn window(&self) -> adw::Window {
        self.window.clone()
    }

    /// Puts the draft's body in the buffer, styled or as Markdown.
    fn fill_body(self: &Rc<Self>) {
        let markdown = self.base.borrow().markdown.clone();
        self.busy.set(true);
        match self.format.get() {
            ComposeFormat::Rich => {
                // A reopened draft brings its styling with it; everything
                // else arrives as Markdown.
                let body = match self.base.borrow().rich.clone() {
                    Some(rich) => rich,
                    None => {
                        let mut body = RichBody::from_markdown(&markdown);
                        // A reply and a forward start with blank lines to
                        // write on, which Markdown drops and the writer
                        // wants back.
                        let room = markdown.chars().take_while(|c| *c == '\n').count().min(2);
                        for _ in 0..room {
                            body.blocks.insert(0, Block::default());
                        }
                        body
                    }
                };
                richbuffer::write(
                    &self.body,
                    &body,
                    &self.attachments.borrow(),
                    &mut self.anchors.borrow_mut(),
                );
            }
            ComposeFormat::Markdown => {
                self.body.buffer().set_text(&markdown);
                style_quotes(&self.body.buffer());
            }
        }
        self.body
            .buffer()
            .place_cursor(&self.body.buffer().start_iter());
        self.busy.set(false);
    }

    fn wire(self: &Rc<Self>, attach: &gtk::Button, preview_toggle: &gtk::ToggleButton) {
        let weak = Rc::downgrade(self);
        let mark_dirty = move || {
            if let Some(c) = weak.upgrade() {
                c.dirty.set(true);
                c.update_title();
                c.check_send();
            }
        };
        let mark = mark_dirty.clone();
        self.subject.connect_changed(move |_| mark());
        for field in [&self.to, &self.cc, &self.bcc] {
            let (mark, weak) = (mark_dirty.clone(), Rc::downgrade(self));
            field.on_change(move || {
                mark();
                if let Some(c) = weak.upgrade() {
                    c.check_keys();
                }
            });
        }
        // Tab walks the fields in reading order, skipping Cc and Bcc while
        // they are hidden.
        for (field, back) in [
            (&self.to, None),
            (&self.cc, Some(&self.to)),
            (&self.bcc, Some(&self.cc)),
        ] {
            let weak = Rc::downgrade(self);
            let here = Rc::downgrade(field);
            let back = back.map(Rc::downgrade);
            field.on_tab(
                move || {
                    if let (Some(c), Some(here)) = (weak.upgrade(), here.upgrade()) {
                        c.after(&here);
                    }
                },
                move || {
                    if let Some(previous) = back.as_ref().and_then(|b| b.upgrade()) {
                        previous.entry.grab_focus();
                    }
                },
            );
        }

        // Typed text takes the style it follows, and the kind of its line.
        let weak = Rc::downgrade(self);
        self.body.buffer().connect_insert_text(move |_, at, text| {
            if let Some(c) = weak.upgrade() {
                let length = text.chars().count() as i32;
                c.inserted.borrow_mut().push((at.offset(), length));
            }
        });
        let mark = mark_dirty.clone();
        let weak = Rc::downgrade(self);
        self.body.buffer().connect_changed(move |buffer| {
            if let Some(c) = weak.upgrade() {
                c.after_edit(buffer);
            }
            mark();
        });
        let weak = Rc::downgrade(self);
        self.body.buffer().connect_cursor_position_notify(move |_| {
            if let Some(c) = weak.upgrade() {
                c.follow_cursor();
            }
        });

        let weak = Rc::downgrade(self);
        self.send.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.send();
            }
        });
        let weak = Rc::downgrade(self);
        self.from.connect_selected_notify(move |row| {
            if let Some(c) = weak.upgrade() {
                c.identity_changed(row.selected() as usize);
            }
        });
        let weak = Rc::downgrade(self);
        self.more_button.connect_toggled(move |toggle| {
            if let Some(c) = weak.upgrade() {
                c.show_more(toggle.is_active());
            }
        });
        let weak = Rc::downgrade(self);
        self.sign.connect_toggled(move |_| {
            if let Some(c) = weak.upgrade() {
                c.dirty.set(true);
            }
        });
        let weak = Rc::downgrade(self);
        self.encrypt.connect_toggled(move |_| {
            let Some(c) = weak.upgrade() else { return };
            if !c.filling_keys.get() {
                c.encrypt_chosen.set(true);
            }
            c.dirty.set(true);
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
        let block = gio::SimpleAction::new("block", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        block.connect_activate(move |_, kind| {
            let (Some(c), Some(kind)) =
                (weak.upgrade(), kind.and_then(|k| k.str().map(String::from)))
            else {
                return;
            };
            c.set_block(match kind.as_str() {
                "heading1" => BlockKind::Heading(1),
                "heading2" => BlockKind::Heading(2),
                "heading3" => BlockKind::Heading(3),
                "code" => BlockKind::Code,
                _ => BlockKind::Paragraph,
            });
        });
        actions.add_action(&block);
        let template = gio::SimpleAction::new("template", Some(glib::VariantTy::INT64));
        let weak = Rc::downgrade(self);
        template.connect_activate(move |_, id| {
            if let (Some(c), Some(id)) = (weak.upgrade(), id.and_then(|v| v.get::<i64>())) {
                c.insert_template(id);
            }
        });
        actions.add_action(&template);
        for (name, run) in [
            (
                "format-markdown",
                Box::new(|c: &Rc<Composer>| c.format_markdown()) as ComposerAction,
            ),
            ("edit-markdown", Box::new(|c| c.edit_as_markdown())),
            ("clear-format", Box::new(|c| c.clear_format())),
            ("save-template", Box::new(|c| c.save_as_template())),
        ] {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(c) = weak.upgrade() {
                    run(&c);
                }
            });
            actions.add_action(&action);
        }
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
                    c.with_inline_images(c.html())
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
        for (trigger, run) in [
            (
                "<Control>b",
                Box::new(|c: &Rc<Composer>| c.style("bold")) as ComposerAction,
            ),
            ("<Control>i", Box::new(|c| c.style("italic"))),
            ("<Control><Shift>x", Box::new(|c| c.style("strike"))),
            ("<Control>e", Box::new(|c| c.style("code"))),
            ("<Control>k", Box::new(|c| c.link())),
            ("<Control><Shift>8", Box::new(|c| c.list(BlockKind::Bullet))),
            (
                "<Control><Shift>7",
                Box::new(|c| c.list(BlockKind::Numbered)),
            ),
            ("<Control><Shift>9", Box::new(|c| c.list(BlockKind::Quote))),
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
                    run(&c);
                    glib::Propagation::Stop
                })),
            ));
        }
        self.body.add_controller(formatting);

        // Enter carries a list on, and leaves it when the line is empty.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(c) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if !matches!(key, gdk::Key::Return | gdk::Key::KP_Enter)
                || modifiers.contains(gdk::ModifierType::CONTROL_MASK)
                || c.format.get() != ComposeFormat::Rich
            {
                return glib::Propagation::Proceed;
            }
            c.new_line()
        });
        self.body.add_controller(keys);

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

    /// Moves the focus to whatever follows `field`.
    fn after(&self, field: &Rc<Recipients>) {
        let showing = self.more_button.is_active();
        let next = if Rc::ptr_eq(field, &self.to) && showing {
            Some(&self.cc)
        } else if Rc::ptr_eq(field, &self.cc) && showing {
            Some(&self.bcc)
        } else {
            None
        };
        match next {
            Some(field) => field.entry.grab_focus(),
            None => self.subject.grab_focus(),
        };
    }

    /// Shows or hides the Cc and Bcc rows.
    fn show_more(self: &Rc<Self>, show: bool) {
        for widget in &self.more {
            widget.set_visible(show);
        }
        self.more_button.set_tooltip_text(Some(if show {
            "Hide Cc and Bcc"
        } else {
            "Show Cc and Bcc"
        }));
        if show && self.cc.is_empty() {
            self.cc.entry.grab_focus();
        }
    }

    /// The body as Markdown, whichever way it is being written.
    fn markdown(&self) -> String {
        match self.format.get() {
            ComposeFormat::Rich => self.rich().to_markdown(),
            ComposeFormat::Markdown => self.source(),
        }
    }

    /// The text in the buffer, markers and all.
    fn source(&self) -> String {
        let buffer = self.body.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    fn rich(&self) -> RichBody {
        richbuffer::read(&self.body.buffer(), &self.anchors.borrow())
    }

    fn html(&self) -> String {
        let mut html = match self.format.get() {
            ComposeFormat::Rich => self.rich().to_html(),
            ComposeFormat::Markdown => markdown_to_html(&self.source()),
        };
        if let Some(forwarded) = &self.base.borrow().forwarded {
            html.push_str(&forwarded.to_html());
        }
        html
    }

    fn is_blank(&self) -> bool {
        let empty = match self.format.get() {
            ComposeFormat::Rich => self.rich().is_empty(),
            ComposeFormat::Markdown => self.source().trim().is_empty(),
        };
        self.to.is_empty()
            && self.subject.text().trim().is_empty()
            && empty
            && self.attachments.borrow().is_empty()
            && self.base.borrow().forwarded.is_none()
    }

    /// The strip under the body naming the message this draft forwards,
    /// with a way to drop it. It is hidden when nothing is forwarded.
    fn refresh_forwarded(self: &Rc<Self>) {
        while let Some(child) = self.forwarded.first_child() {
            self.forwarded.remove(&child);
        }
        let label = {
            let base = self.base.borrow();
            base.forwarded.as_ref().map(|f| {
                let who = if f.from.is_empty() {
                    "a message"
                } else {
                    &f.from
                };
                match f.subject.trim() {
                    "" => format!("Forwarding {who}"),
                    subject => format!("Forwarding “{subject}” from {who}"),
                }
            })
        };
        let Some(text) = label else {
            self.forwarded.set_visible(false);
            return;
        };
        self.forwarded.set_visible(true);
        self.forwarded
            .append(&gtk::Image::from_icon_name("mail-forward-symbolic"));
        self.forwarded.append(
            &gtk::Label::builder()
                .label(&text)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .hexpand(true)
                .xalign(0.0)
                .build(),
        );
        let drop = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Do Not Forward the Original")
            .css_classes(["flat", "circular"])
            .build();
        let weak = Rc::downgrade(self);
        drop.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.base.borrow_mut().forwarded = None;
                c.dirty.set(true);
                c.refresh_forwarded();
            }
        });
        self.forwarded.append(&drop);
    }

    fn identity(&self) -> Option<&Identity> {
        self.identities.get(self.from.selected() as usize)
    }

    /// Follows the From row with the signature, since Gmail keeps one per
    /// send-as address and a message signed by the wrong one looks careless.
    fn identity_changed(self: &Rc<Self>, to: usize) {
        let was = self.showing.replace(to);
        let (Some(old), Some(new)) = (self.identities.get(was), self.identities.get(to)) else {
            return;
        };
        if old.signature == new.signature {
            return;
        }
        let markdown = self.markdown();
        let swapped = restyle_signature(&markdown, &old.signature, &new.signature);
        if swapped == markdown {
            return;
        }
        self.base.borrow_mut().markdown = swapped;
        // The buffer holds the styling, so the body is written out again
        // from the swapped Markdown rather than patched in place.
        self.base.borrow_mut().rich = None;
        self.fill_body();
        self.check_send();
    }

    /// Underlines misspellings as the writer types, when a dictionary is
    /// installed. With none, the composer is a plain text view and says
    /// nothing about it.
    fn check_spelling(self: &Rc<Self>, dictionaries: Rc<spell::Dictionaries>) {
        let remember = Rc::clone(&self.remember);
        *self.spell.borrow_mut() =
            spell::SpellCheck::attach(&self.body, dictionaries, move |word| {
                remember(Remembered::Word(word.to_string()));
            });
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

    /// Greys out Send while the message cannot go anywhere, and says why.
    /// It asks the recipients only, so every keystroke stays cheap.
    fn check_send(&self) {
        let problem = match self.identity() {
            None => Some("No account to send from.".to_string()),
            Some(identity) => {
                let mut draft = Draft::new(identity.account_id, identity.address.clone());
                draft.to = self.to.addresses();
                draft.cc = self.cc.addresses();
                draft.bcc = self.bcc.addresses();
                draft.problem()
            }
        };
        self.send.set_sensitive(problem.is_none());
        self.send
            .set_tooltip_text(Some(problem.as_deref().unwrap_or("Send (Ctrl+Enter)")));
    }

    /// Asks gpg which recipients it can encrypt to and lets the Encrypt
    /// toggle follow. It waits for the typing to settle and skips a list
    /// it has already asked about, so the keyring is read once per change
    /// rather than once per letter.
    fn check_keys(self: &Rc<Self>) {
        if !self.core.has_gpg() {
            return;
        }
        let generation = self.key_check.get().wrapping_add(1);
        self.key_check.set(generation);
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
            let Some(c) = weak.upgrade() else { return };
            if c.key_check.get() == generation {
                c.ask_about_keys();
            }
        });
    }

    fn ask_about_keys(self: &Rc<Self>) {
        let addresses = self.recipient_addresses();
        if *self.asked_keys.borrow() == addresses {
            return;
        }
        *self.asked_keys.borrow_mut() = addresses.clone();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let wanted = addresses.clone();
            let held = this.core.gpg(move |pgp| pgp.keys_for(&wanted)).await;
            // The recipients moved on while gpg was answering.
            if *this.asked_keys.borrow() != addresses {
                return;
            }
            match held {
                Ok(held) => this.show_keys(&held),
                Err(err) => {
                    tracing::info!(error = %err, "could not ask gpg about the recipients")
                }
            }
        });
    }

    /// Offers encryption when gpg can do it, and says what is in the way
    /// when it cannot.
    fn show_keys(&self, held: &[mailrs_pgp::Recipient]) {
        let problem = crate::pgp::cannot_encrypt(held, !self.bcc.is_empty());
        self.encrypt.set_sensitive(problem.is_none());
        self.encrypt.set_tooltip_text(Some(
            problem
                .as_deref()
                .unwrap_or("Encrypt this message to the recipients' keys"),
        ));
        self.filling_keys.set(true);
        if problem.is_some() {
            self.encrypt.set_active(false);
        } else if self.encrypt_when_possible && !self.encrypt_chosen.get() {
            self.encrypt.set_active(true);
        }
        self.filling_keys.set(false);
    }

    /// Every address the message would go to, lower case and each once.
    /// A half-typed address is left out until it is one.
    fn recipient_addresses(&self) -> Vec<String> {
        let mut found: Vec<String> = [&self.to, &self.cc, &self.bcc]
            .into_iter()
            .flat_map(|field| field.addresses())
            .map(|address| address.email.trim().to_lowercase())
            .filter(|email| crate::compose::is_address(email))
            .collect();
        found.sort();
        found.dedup();
        found
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
        draft.to = self.to.addresses();
        draft.cc = self.cc.addresses();
        draft.bcc = self.bcc.addresses();
        draft.subject = self.subject.text().trim().to_string();
        draft.markdown = self.markdown();
        draft.rich = match self.format.get() {
            ComposeFormat::Rich => Some(self.rich()),
            ComposeFormat::Markdown => None,
        };
        draft.attachments = self.attachments.borrow().clone();
        draft.sign = self.sign.is_active();
        draft.encrypt = self.encrypt.is_active() && self.encrypt.is_sensitive();
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
        if let Some(promise) = self.unkept_promise(&draft) {
            self.ask_about_attachment(&promise, when);
            return;
        }
        if let Some(identity) = self.identity() {
            (self.remember)(Remembered::SentFrom {
                account: identity.account_email.clone(),
                email: identity.address.email.clone(),
            });
        }
        self.closing.set(true);
        self.window.close();
        (self.on_send)(draft, when);
    }

    /// The file this message promises and does not carry. An image pasted
    /// into the text keeps a promise of something to look at, since it
    /// arrives with the message either way, but not a promise of a file:
    /// only an attachment comes out of the reader's mail as one.
    fn unkept_promise(&self, draft: &Draft) -> Option<Promise> {
        if !self.check_attachments || self.asked.get() {
            return None;
        }
        let promise = attachcheck::promised(&draft.subject, &draft.markdown)?;
        let files = draft.attachments.iter().any(|a| a.content_id.is_none());
        let images = draft.attachments.iter().any(|a| a.content_id.is_some());
        let kept = files || (images && !promise.names_a_file);
        (!kept).then_some(promise)
    }

    /// Asks before a message that promises a file goes without one. Send
    /// Anyway sends it as it stands, Add Attachment opens the file picker
    /// and leaves the message open, and closing the dialog does neither.
    fn ask_about_attachment(self: &Rc<Self>, promise: &Promise, when: SendWhen) {
        let dialog = adw::AlertDialog::new(
            Some("Attachment Missing?"),
            Some(&format!(
                "The message says “{}” and carries no file.",
                promise.sentence
            )),
        );
        dialog.add_responses(&[("attach", "Add Attachment"), ("send", "Send Anyway")]);
        dialog.set_response_appearance("send", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("attach"));
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            match dialog.choose_future(Some(&this.window)).await.as_str() {
                "send" => {
                    this.asked.set(true);
                    this.hand_over(when);
                }
                "attach" => this.pick_files(),
                _ => {}
            }
        });
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

    /// The attachment rows: every file with its size and a way out.
    fn refresh_files(self: &Rc<Self>) {
        while let Some(child) = self.files.first_child() {
            self.files.remove(&child);
        }
        let attachments = self.attachments.borrow();
        let listed: Vec<(usize, &OutgoingAttachment)> = attachments
            .iter()
            .enumerate()
            .filter(|(_, a)| a.content_id.is_none())
            .collect();
        self.files.set_visible(!listed.is_empty());
        if listed.is_empty() {
            return;
        }
        let total: i64 = listed.iter().map(|(_, a)| a.data.len() as i64).sum();
        let count = listed.len();
        self.files.append(
            &gtk::Label::builder()
                .label(format!(
                    "{count} {}  ·  {}",
                    if count == 1 {
                        "attachment"
                    } else {
                        "attachments"
                    },
                    human_size(total)
                ))
                .xalign(0.0)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
        for (index, attachment) in listed {
            let row = gtk::Box::builder()
                .spacing(8)
                .css_classes(["attachment-row"])
                .build();
            row.append(&gtk::Image::from_icon_name("mail-attachment-symbolic"));
            row.append(
                &gtk::Label::builder()
                    .label(&attachment.filename)
                    .xalign(0.0)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .build(),
            );
            row.append(
                &gtk::Label::builder()
                    .label(human_size(attachment.data.len() as i64))
                    .css_classes(["dim-label", "caption"])
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
                    c.refresh_files();
                }
            });
            row.append(&remove);
            self.files.append(&row);
        }
    }
}

impl Composer {
    fn fill_format_bar(self: &Rc<Self>, bar: &gtk::Box) {
        let group = || {
            let group = gtk::Box::builder()
                .spacing(1)
                .css_classes(["format-group"])
                .build();
            bar.append(&group);
            group
        };
        let label = |markup: &str| {
            gtk::Label::builder()
                .label(markup)
                .use_markup(true)
                .width_chars(2)
                .build()
        };
        // Letters read better than the text-style icons at this size.
        let styles = group();
        for (markup, tip, tag) in [
            ("<b>B</b>", "Bold (Ctrl+B)", "bold"),
            ("<i>I</i>", "Italic (Ctrl+I)", "italic"),
            ("<s>S</s>", "Strikethrough (Ctrl+Shift+X)", "strike"),
            ("<tt>&lt;/&gt;</tt>", "Code (Ctrl+E)", "code"),
        ] {
            let button = gtk::ToggleButton::builder()
                .child(&label(markup))
                .tooltip_text(tip)
                .css_classes(["flat"])
                .can_focus(false)
                .build();
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(c) = weak.upgrade() {
                    c.style(tag);
                }
            });
            styles.append(&button);
            self.toggles.borrow_mut().push((button, tag));
        }

        let blocks = group();
        let button = |bar: &gtk::Box, icon: &str, tip: &str| {
            let button = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tip)
                .css_classes(["flat"])
                .can_focus(false)
                .build();
            bar.append(&button);
            button
        };
        let weak = Rc::downgrade(self);
        button(&blocks, "penguin-mail-link-symbolic", "Link (Ctrl+K)").connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.link();
            }
        });
        for (icon, tip, kind) in [
            (
                "view-list-bullet-symbolic",
                "Bulleted List (Ctrl+Shift+8)",
                BlockKind::Bullet,
            ),
            (
                "view-list-ordered-symbolic",
                "Numbered List (Ctrl+Shift+7)",
                BlockKind::Numbered,
            ),
            (
                "format-indent-more-symbolic",
                "Quote (Ctrl+Shift+9)",
                BlockKind::Quote,
            ),
        ] {
            let weak = Rc::downgrade(self);
            button(&blocks, icon, tip).connect_clicked(move |_| {
                if let Some(c) = weak.upgrade() {
                    c.list(kind);
                }
            });
        }

        let extras = group();
        let weak = Rc::downgrade(self);
        button(&extras, "image-x-generic-symbolic", "Insert Image").connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.pick_images();
            }
        });
        let menu = gio::Menu::new();
        let paragraph = gio::Menu::new();
        for (label, kind) in [
            ("Paragraph", "paragraph"),
            ("Heading 1", "heading1"),
            ("Heading 2", "heading2"),
            ("Heading 3", "heading3"),
            ("Code Block", "code"),
        ] {
            let item = gio::MenuItem::new(Some(label), None);
            item.set_action_and_target_value(Some("composer.block"), Some(&kind.to_variant()));
            paragraph.append_item(&item);
        }
        menu.append_section(None, &paragraph);
        let rest = gio::Menu::new();
        rest.append(Some("Format Markdown"), Some("composer.format-markdown"));
        rest.append(Some("Clear Formatting"), Some("composer.clear-format"));
        menu.append_section(None, &rest);
        let switch = gio::Menu::new();
        switch.append(Some("Edit as Markdown"), Some("composer.edit-markdown"));
        menu.append_section(None, &switch);
        let more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("More Formatting")
            .menu_model(&menu)
            .css_classes(["flat"])
            .can_focus(false)
            .build();
        extras.append(&more);
        self.follow_cursor();
    }

    /// Turns a style on or off: over the selection, or for what comes next.
    fn style(self: &Rc<Self>, tag: &'static str) {
        let buffer = self.body.buffer();
        if self.format.get() == ComposeFormat::Markdown {
            let (before, after) = match tag {
                "bold" => ("**", "**"),
                "italic" => ("*", "*"),
                "strike" => ("~~", "~~"),
                _ => ("`", "`"),
            };
            wrap_selection(&buffer, before, after);
            self.body.grab_focus();
            self.follow_cursor();
            return;
        }
        self.busy.set(true);
        if let Some((start, end)) = buffer.selection_bounds() {
            let on = !whole_selection_has(&buffer, tag);
            if on {
                buffer.apply_tag_by_name(tag, &start, &end);
            } else {
                buffer.remove_tag_by_name(tag, &start, &end);
            }
        } else {
            let cursor = buffer.iter_at_mark(&buffer.get_insert());
            let (mut style, link) = self.next_style(&cursor);
            let on = !richbuffer::has(style, tag);
            match tag {
                "bold" => style.bold = on,
                "italic" => style.italic = on,
                "strike" => style.strike = on,
                _ => style.code = on,
            }
            *self.typing.borrow_mut() = Some((cursor.offset(), style, link));
        }
        self.busy.set(false);
        self.body.grab_focus();
        self.refresh_toggles();
    }

    /// The style typing at `at` would take, pending toggles included.
    fn next_style(&self, at: &gtk::TextIter) -> (Style, Option<String>) {
        if let Some((offset, style, link)) = self.typing.borrow().as_ref()
            && *offset == at.offset()
        {
            return (*style, link.clone());
        }
        richbuffer::style_before(at)
    }

    /// Makes the lines the cursor touches a list, a quote, or plain again.
    fn list(self: &Rc<Self>, kind: BlockKind) {
        if self.format.get() == ComposeFormat::Markdown {
            let prefix = match kind {
                BlockKind::Numbered => LinePrefix::Numbered,
                BlockKind::Quote => LinePrefix::Quote,
                _ => LinePrefix::Bullet,
            };
            prefix_lines(&self.body.buffer(), prefix);
            self.body.grab_focus();
            return;
        }
        self.set_block_lines(kind, true);
    }

    /// The menu's paragraph kinds, which do not toggle back off.
    fn set_block(self: &Rc<Self>, kind: BlockKind) {
        if self.format.get() == ComposeFormat::Markdown {
            let marks = match kind {
                BlockKind::Heading(level) => "#".repeat(level as usize) + " ",
                BlockKind::Code => "    ".to_string(),
                _ => String::new(),
            };
            let buffer = self.body.buffer();
            let line = buffer.iter_at_mark(&buffer.get_insert()).line();
            let mut at = buffer
                .iter_at_line(line)
                .unwrap_or_else(|| buffer.end_iter());
            buffer.insert(&mut at, &marks);
            self.body.grab_focus();
            return;
        }
        self.set_block_lines(kind, false);
    }

    fn set_block_lines(self: &Rc<Self>, kind: BlockKind, toggles: bool) {
        let buffer = self.body.buffer();
        let (first, last) = match buffer.selection_bounds() {
            Some((start, end)) => (start.line(), end.line()),
            None => {
                let cursor = buffer.iter_at_mark(&buffer.get_insert()).line();
                (cursor, cursor)
            }
        };
        let same = (first..=last).all(|line| richbuffer::kind_at(&buffer, line) == kind);
        let wanted = if toggles && same {
            BlockKind::Paragraph
        } else {
            kind
        };
        self.busy.set(true);
        buffer.begin_user_action();
        for line in first..=last {
            richbuffer::set_kind(&buffer, line, wanted);
        }
        richbuffer::renumber(&buffer);
        buffer.end_user_action();
        self.busy.set(false);
        self.dirty.set(true);
        self.body.grab_focus();
    }

    /// Asks for an address and links the selected words to it.
    fn link(self: &Rc<Self>) {
        let buffer = self.body.buffer();
        if self.format.get() == ComposeFormat::Markdown {
            wrap_selection(&buffer, "[", "]()");
            self.body.grab_focus();
            return;
        }
        // The dialog takes the focus, so hold the words being linked with
        // marks, which survive the wait and any edit under them.
        let (start, end) = buffer.selection_bounds().unwrap_or_else(|| {
            let cursor = buffer.iter_at_mark(&buffer.get_insert());
            (cursor, cursor)
        });
        let selected = buffer.text(&start, &end, false).to_string();
        let held = (
            buffer.create_mark(None, &start, true),
            buffer.create_mark(None, &end, false),
        );
        let dialog = adw::AlertDialog::new(Some("Add a Link"), None);
        let fields = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .build();
        let text = gtk::Entry::builder()
            .placeholder_text("Text")
            .text(&selected)
            .build();
        let url = gtk::Entry::builder()
            .placeholder_text("https://example.com")
            .activates_default(true)
            .build();
        fields.append(&text);
        fields.append(&url);
        dialog.set_extra_child(Some(&fields));
        dialog.add_responses(&[("cancel", "Cancel"), ("add", "Add Link")]);
        dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("add"));
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let chosen = dialog.choose_future(Some(&this.window)).await;
            let address = url.text().trim().to_string();
            if chosen.as_str() != "add" || address.is_empty() {
                let buffer = this.body.buffer();
                buffer.delete_mark(&held.0);
                buffer.delete_mark(&held.1);
                return;
            }
            let address = match address.contains(':') || address.starts_with("//") {
                true => address,
                false if address.contains('@') => format!("mailto:{address}"),
                false => format!("https://{address}"),
            };
            let shown = text.text().trim().to_string();
            let shown = if shown.is_empty() {
                address.clone()
            } else {
                shown
            };
            this.insert_link(&shown, &address, held);
        });
    }

    /// Puts `text`, linked to `url`, between the marks that held the words.
    fn insert_link(self: &Rc<Self>, text: &str, url: &str, held: (gtk::TextMark, gtk::TextMark)) {
        let buffer = self.body.buffer();
        let tag = richbuffer::link_tag(&buffer, url);
        self.busy.set(true);
        buffer.begin_user_action();
        let (mut start, mut end) = (buffer.iter_at_mark(&held.0), buffer.iter_at_mark(&held.1));
        let kind = richbuffer::block_tag(richbuffer::kind_at(&buffer, start.line()));
        buffer.delete(&mut start, &mut end);
        let offset = start.offset();
        buffer.insert(&mut start, text);
        let (from, to) = (
            buffer.iter_at_offset(offset),
            buffer.iter_at_offset(offset + text.chars().count() as i32),
        );
        buffer.apply_tag(&tag, &from, &to);
        buffer.apply_tag_by_name(kind, &from, &to);
        buffer.place_cursor(&to);
        buffer.delete_mark(&held.0);
        buffer.delete_mark(&held.1);
        buffer.end_user_action();
        self.busy.set(false);
        self.dirty.set(true);
        self.body.grab_focus();
    }

    /// Reads the Markdown in the body and styles it, marks gone.
    fn format_markdown(self: &Rc<Self>) {
        let body = RichBody::from_markdown(&self.source());
        self.format.set(ComposeFormat::Rich);
        self.busy.set(true);
        richbuffer::write(
            &self.body,
            &body,
            &self.attachments.borrow(),
            &mut self.anchors.borrow_mut(),
        );
        self.busy.set(false);
        self.dirty.set(true);
        self.refresh_toggles();
        self.toast("Markdown formatted");
    }

    /// Switches between the two ways of writing, keeping the body.
    fn edit_as_markdown(self: &Rc<Self>) {
        if self.format.get() == ComposeFormat::Markdown {
            return self.format_markdown();
        }
        let markdown = self.rich().to_markdown();
        self.format.set(ComposeFormat::Markdown);
        self.busy.set(true);
        self.anchors.borrow_mut().clear();
        let buffer = self.body.buffer();
        buffer.set_text(&markdown);
        style_quotes(&buffer);
        self.busy.set(false);
        self.dirty.set(true);
        self.refresh_toggles();
        self.toast("Editing as Markdown");
    }

    /// Takes every style off the selection, or off the whole body.
    fn clear_format(self: &Rc<Self>) {
        if self.format.get() == ComposeFormat::Markdown {
            return;
        }
        let buffer = self.body.buffer();
        let (start, end) = buffer
            .selection_bounds()
            .unwrap_or_else(|| (buffer.start_iter(), buffer.end_iter()));
        let (first, last) = (start.line(), end.line());
        self.busy.set(true);
        buffer.begin_user_action();
        buffer.remove_all_tags(&start, &end);
        for line in first..=last {
            richbuffer::set_kind(&buffer, line, BlockKind::Paragraph);
        }
        buffer.end_user_action();
        self.busy.set(false);
        self.dirty.set(true);
        self.refresh_toggles();
    }

    /// Styles text as it is typed and keeps list numbers in order.
    fn after_edit(self: &Rc<Self>, buffer: &gtk::TextBuffer) {
        let ranges: Vec<(i32, i32)> = self.inserted.borrow_mut().drain(..).collect();
        if self.busy.get() {
            return;
        }
        if self.format.get() == ComposeFormat::Markdown {
            self.busy.set(true);
            style_quotes(buffer);
            self.busy.set(false);
            return;
        }
        self.busy.set(true);
        for (offset, length) in ranges {
            let (from, to) = (
                buffer.iter_at_offset(offset),
                buffer.iter_at_offset(offset + length),
            );
            let (style, link) = self.next_style(&from);
            for tag in richbuffer::STYLES {
                if richbuffer::has(style, tag) {
                    buffer.apply_tag_by_name(tag, &from, &to);
                } else {
                    buffer.remove_tag_by_name(tag, &from, &to);
                }
            }
            if let Some(url) = &link {
                buffer.apply_tag(&richbuffer::link_tag(buffer, url), &from, &to);
            }
            // The line keeps its kind, so typing at its end stays in it.
            let kind = richbuffer::kind_at(buffer, from.line());
            buffer.apply_tag_by_name(richbuffer::block_tag(kind), &from, &to);
            *self.typing.borrow_mut() = Some((to.offset(), style, link));
        }
        richbuffer::renumber(buffer);
        self.busy.set(false);
    }

    /// Enter inside a list or quote: another item, or out of the list.
    fn new_line(self: &Rc<Self>) -> glib::Propagation {
        let buffer = self.body.buffer();
        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        let line = cursor.line();
        let kind = richbuffer::kind_at(&buffer, line);
        if matches!(kind, BlockKind::Paragraph | BlockKind::Heading(_)) {
            return glib::Propagation::Proceed;
        }
        self.busy.set(true);
        buffer.begin_user_action();
        if richbuffer::is_empty_line(&buffer, line) {
            richbuffer::set_kind(&buffer, line, BlockKind::Paragraph);
        } else {
            let mut at = buffer.iter_at_mark(&buffer.get_insert());
            buffer.insert(&mut at, "\n");
            let line = buffer.iter_at_mark(&buffer.get_insert()).line();
            richbuffer::set_kind(&buffer, line, kind);
            let end = richbuffer::text_start(&buffer, line);
            buffer.place_cursor(&end);
        }
        richbuffer::renumber(&buffer);
        buffer.end_user_action();
        self.busy.set(false);
        self.dirty.set(true);
        glib::Propagation::Stop
    }

    /// Keeps the formatting bar showing what the cursor sits in.
    fn follow_cursor(self: &Rc<Self>) {
        let buffer = self.body.buffer();
        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        let stale = self
            .typing
            .borrow()
            .as_ref()
            .is_some_and(|(offset, _, _)| *offset != cursor.offset());
        if stale {
            *self.typing.borrow_mut() = None;
        }
        self.refresh_toggles();
    }

    fn refresh_toggles(self: &Rc<Self>) {
        let rich = self.format.get() == ComposeFormat::Rich;
        let buffer = self.body.buffer();
        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        let style = match (rich, buffer.selection_bounds()) {
            (false, _) => Style::default(),
            (true, Some((start, _))) => richbuffer::style_at(&start).0,
            (true, None) => self.next_style(&cursor).0,
        };
        for (button, tag) in self.toggles.borrow().iter() {
            let wanted = rich && richbuffer::has(style, tag);
            if button.is_active() != wanted {
                button.set_active(wanted);
            }
        }
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
                    self.refresh_files();
                    self.check_send();
                }
            }
            Err(err) => self.toast(&format!("Could not read the file: {err}")),
        }
    }

    /// Adds an image and shows it at the cursor, or names it there while
    /// the body is Markdown.
    fn add_inline_image(self: &Rc<Self>, filename: String, mime_type: String, data: Vec<u8>) {
        let cid = format!("{}@mailrs", mailrs_gmail::random_token(9));
        let buffer = self.body.buffer();
        self.busy.set(true);
        match self.format.get() {
            ComposeFormat::Rich => {
                let mut at = buffer.iter_at_mark(&buffer.get_insert());
                richbuffer::insert_image(
                    &self.body,
                    &mut at,
                    &cid,
                    &data,
                    &mut self.anchors.borrow_mut(),
                );
            }
            ComposeFormat::Markdown => {
                let alt: String = filename
                    .chars()
                    .filter(|c| !matches!(c, '[' | ']'))
                    .collect();
                buffer.insert_at_cursor(&format!("![{alt}](cid:{cid})"));
            }
        }
        self.busy.set(false);
        self.attachments.borrow_mut().push(OutgoingAttachment {
            filename,
            mime_type,
            data,
            content_id: Some(cid),
        });
        self.dirty.set(true);
        self.refresh_files();
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

    /// Reads the saved templates in and lists them in the header menu.
    /// Preferences may have changed them while this window was open, so
    /// this runs again after every save.
    fn load_templates(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            match this.core.read(mailrs_store::templates::list).await {
                Ok(saved) => {
                    *this.templates.borrow_mut() = saved;
                    this.fill_template_menu();
                }
                Err(err) => tracing::warn!(error = %err, "could not read the templates"),
            }
        });
    }

    fn fill_template_menu(&self) {
        self.template_items.remove_all();
        let templates = self.templates.borrow();
        if templates.is_empty() {
            // Nothing answers this action, which is what greys the item out.
            self.template_items
                .append(Some("No Templates Yet"), Some("composer.none"));
            return;
        }
        for template in templates.iter() {
            let item = gio::MenuItem::new(Some(&template.name), None);
            item.set_action_and_target_value(
                Some("composer.template"),
                Some(&template.id.to_variant()),
            );
            self.template_items.append_item(&item);
        }
    }

    /// What a template's placeholders stand for in this message.
    fn filling(&self) -> Filling {
        Filling {
            recipient: self.to.addresses().first().cloned(),
            subject: self.subject.text().trim().to_string(),
            date: templates::today(chrono::Local::now()),
        }
    }

    /// Puts a template in at the cursor, its placeholders filled. One with
    /// a subject gives it to a message that has none of its own.
    fn insert_template(self: &Rc<Self>, id: i64) {
        let found = self.templates.borrow().iter().find(|t| t.id == id).cloned();
        let Some(template) = found else { return };
        let mut filling = self.filling();
        if filling.subject.is_empty() && !template.subject.trim().is_empty() {
            self.subject
                .set_text(&templates::expand(&template.subject, &filling));
            filling.subject = self.subject.text().trim().to_string();
        }
        let body = templates::fill(&RichBody::from_markdown(&template.markdown), &filling);
        let buffer = self.body.buffer();
        self.busy.set(true);
        buffer.begin_user_action();
        match self.format.get() {
            ComposeFormat::Rich => richbuffer::insert(&buffer, &body),
            ComposeFormat::Markdown => buffer.insert_at_cursor(&body.to_markdown()),
        }
        buffer.end_user_action();
        self.busy.set(false);
        self.dirty.set(true);
        self.body.grab_focus();
    }

    /// Keeps what is written as a template, under a name the writer gives
    /// it. The body is saved as written, so its placeholders fill in again
    /// the next time it goes into a message.
    fn save_as_template(self: &Rc<Self>) {
        let written = Template {
            id: 0,
            name: String::new(),
            subject: self.subject.text().trim().to_string(),
            markdown: self.markdown(),
        };
        let dialog = adw::AlertDialog::new(
            Some("Save as Template"),
            Some("Placeholders such as {{first_name}} fill in each time you use it."),
        );
        let name = gtk::Entry::builder()
            .placeholder_text("Name")
            .text(&written.subject)
            .activates_default(true)
            .build();
        dialog.set_extra_child(Some(&name));
        dialog.add_responses(&[("cancel", "Cancel"), ("save", "Save")]);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if dialog.choose_future(Some(&this.window)).await.as_str() != "save" {
                return;
            }
            let template = Template {
                name: name.text().trim().to_string(),
                ..written
            };
            if template.name.is_empty() {
                return this.toast("Give the template a name");
            }
            match this
                .core
                .write(move |c| mailrs_store::templates::add(c, &template))
                .await
            {
                Ok(_) => {
                    this.toast("Template saved");
                    this.load_templates();
                }
                Err(err) => this.toast(&format!("Template not saved: {err}")),
            }
        });
    }
}

/// Whether every character of the selection carries `tag`.
fn whole_selection_has(buffer: &gtk::TextBuffer, tag: &str) -> bool {
    let Some((start, end)) = buffer.selection_bounds() else {
        return false;
    };
    let Some(tag) = buffer.tag_table().lookup(tag) else {
        return false;
    };
    let mut iter = start;
    while iter < end {
        if !iter.has_tag(&tag) {
            return false;
        }
        iter.forward_char();
    }
    true
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

/// Dims lines that start with `>`, so quoted text reads as quoted while
/// the body is Markdown.
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

/// The From row: every address the accounts send as, in one list with a
/// heading per account, so two accounts with aliases stay readable.
fn from_dropdown(identities: &[Identity]) -> gtk::DropDown {
    let sections = gio::ListStore::new::<gtk::StringList>();
    let mut account = String::new();
    for identity in identities {
        if identity.account_email != account || sections.n_items() == 0 {
            account = identity.account_email.clone();
            sections.append(&gtk::StringList::new(&[]));
        }
        if let Some(section) = sections
            .item(sections.n_items() - 1)
            .and_downcast::<gtk::StringList>()
        {
            section.append(&format_recipients(std::slice::from_ref(&identity.address)));
        }
    }
    // A flattened list of lists is a section model, which is what gives the
    // popup its headings.
    let model = gtk::FlattenListModel::new(Some(sections));
    let from = gtk::DropDown::builder()
        .model(&model)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "composer-from"])
        .build();
    let owners: Vec<String> = identities.iter().map(|i| i.account_email.clone()).collect();
    // One account needs no heading. Several do, and the heading names the
    // account that owns the addresses under it.
    if owners.windows(2).any(|pair| pair[0] != pair[1]) {
        let headers = gtk::SignalListItemFactory::new();
        headers.connect_setup(|_, item| {
            let Some(header) = item.downcast_ref::<gtk::ListHeader>() else {
                return;
            };
            header.set_child(Some(
                &gtk::Label::builder()
                    .xalign(0.0)
                    .css_classes(["dim-label", "composer-from-heading"])
                    .build(),
            ));
        });
        headers.connect_bind(move |_, item| {
            let Some(header) = item.downcast_ref::<gtk::ListHeader>() else {
                return;
            };
            if let Some(label) = header.child().and_downcast::<gtk::Label>() {
                // A section starts at the first address of one account.
                label.set_label(
                    owners
                        .get(header.start() as usize)
                        .map_or("", String::as_str),
                );
            }
        });
        from.set_header_factory(Some(&headers));
    }
    from
}

fn line() -> gtk::Separator {
    gtk::Separator::new(gtk::Orientation::Horizontal)
}

/// One header row: its label in the shared column, its field beside it.
///
/// `column` holds every label to one width, so the fields all start at the
/// same edge however long the labels are. The label sits at the top of the
/// row rather than its middle, because a Cc row whose chips wrap grows
/// downwards and the label belongs on the first line either way.
fn field(label: &str, widget: &impl IsA<gtk::Widget>, column: &gtk::SizeGroup) -> gtk::Box {
    let row = gtk::Box::builder()
        .spacing(0)
        .css_classes(["composer-field"])
        .build();
    let label = gtk::Label::builder()
        .label(label)
        .xalign(1.0)
        .valign(gtk::Align::Start)
        .css_classes(["dim-label", "composer-label"])
        .build();
    column.add_widget(&label);
    row.append(&label);
    row.append(widget);
    row
}

fn now_secs() -> i64 {
    mailrs_sync::now_millis() / 1000
}
