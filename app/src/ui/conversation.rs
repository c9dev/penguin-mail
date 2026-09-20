//! The right pane: the whole thread in one WebView.
//!
//! Page JavaScript is off (`enable-javascript-markup` is false), so email
//! cannot run scripts. The app still runs two tiny scripts of its own
//! through the WebKit API: collapsing a message and scrolling to one.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio};
use mailrs_domain::{AccountId, FlagColor, Folder, MessageBody, MessageMeta, system_label};
use webkit::prelude::*;

use super::invitation::{self, EventCard, Showing};
use super::pgp::PgpCard;
use crate::compose::ReplyKind;
use crate::pgp::Mark;
use crate::render::{BodyState, Conversation, MessageView, Theme, render};
use crate::sanitize::sanitize_html;

/// Everything shown for one open thread.
pub struct OpenThread {
    pub account_id: AccountId,
    pub thread_id: String,
    pub subject: String,
    pub messages: Vec<MessageMeta>,
    /// Missing entries are still loading; `Err` holds why a body failed.
    pub bodies: HashMap<String, Result<MessageBody, String>>,
    pub expanded: HashSet<String>,
    pub images_allowed: bool,
    /// Set when the view shows one message of the thread, not all of it.
    pub only_message: Option<String>,
    pub me: Vec<String>,
    /// Inline images per message: `Content-ID` to `data:` URI.
    pub inline_images: HashMap<String, HashMap<String, String>>,
    /// Pictures for the attachment rows: Gmail's attachment id to a small
    /// `data:` URI. Shared across the thread, since an id is unique.
    pub thumbnails: HashMap<String, String>,
    /// Contact photos by lower-case sender address, as `data:` URIs. A
    /// sender with none keeps the initials avatar.
    pub photos: HashMap<String, String>,
    /// Set once the user unsubscribed from this thread's list.
    pub unsubscribed: bool,
    /// What gpg made of the protected message in this thread, once it has
    /// run. It stays here so redrawing the thread never asks gpg again,
    /// and so the card survives the body being replaced by the one that
    /// was inside the encryption.
    pub pgp: Option<Mark>,
    /// Set as soon as gpg is asked about this thread. gpg may hold a
    /// pinentry in front of the person for as long as they take, and
    /// asking twice would put up two of them.
    pub pgp_asked: bool,
    /// The flag colour chosen here, when the thread is flagged.
    pub flag_color: Option<FlagColor>,
}

impl OpenThread {
    pub fn is_draft(&self) -> bool {
        self.messages
            .last()
            .is_some_and(|m| m.has_label(system_label::DRAFT))
    }

    pub fn starred(&self) -> bool {
        self.messages
            .iter()
            .any(|m| m.has_label(system_label::STARRED))
    }

    pub fn unread(&self) -> bool {
        self.messages.iter().any(|m| m.is_unread())
    }

    pub fn muted(&self) -> bool {
        self.messages
            .iter()
            .any(|m| m.has_label(system_label::MUTE))
    }

    /// The message a reply answers: the newest one that is not a draft.
    pub fn reply_target(&self) -> Option<&MessageMeta> {
        self.messages
            .iter()
            .rev()
            .find(|m| !m.has_label(system_label::DRAFT))
    }

    /// The newest message that carries an invitation, with the
    /// `text/calendar` part it arrived in.
    pub fn invitation(&self) -> Option<(&MessageMeta, &str)> {
        self.messages.iter().rev().find_map(|meta| {
            let body = self.bodies.get(&meta.id)?.as_ref().ok()?;
            Some((meta, body.calendar.as_deref()?))
        })
    }

    /// The newest message that arrived under OpenPGP, with the call the
    /// engine needs for it. A thread holds one such message far more often
    /// than two, and the newest is the one being read.
    pub fn protected(&self) -> Option<(&MessageMeta, crate::pgp::Opening)> {
        self.messages.iter().rev().find_map(|meta| {
            let body = self.bodies.get(&meta.id)?.as_ref().ok()?;
            Some((meta, crate::pgp::opening(body)?))
        })
    }

    /// The `List-Unsubscribe` header of the newest message, when it has one.
    pub fn list_unsubscribe(&self) -> Option<(&MessageMeta, &MessageBody)> {
        let target = self.reply_target()?;
        let body = self.bodies.get(&target.id)?.as_ref().ok()?;
        body.list_unsubscribe.is_some().then_some((target, body))
    }

    fn has_remote_images(&self) -> bool {
        self.bodies
            .values()
            .filter_map(|b| b.as_ref().ok())
            .filter_map(|b| b.html.as_deref())
            .any(|html| {
                let lower = html.to_ascii_lowercase();
                lower.contains("src=\"http")
                    || lower.contains("src='http")
                    || lower.contains("url(http")
                    || lower.contains("url('http")
                    || lower.contains("url(\"http")
            })
    }
}

pub enum Action {
    /// The event card asked for something: an answer, or a hand-off to the
    /// desktop calendar.
    Invitation(invitation::Action),
    Reply(ReplyKind),
    EditDraft,
    Archive,
    Trash,
    Junk,
    ToggleStar,
    ToggleRead,
    LoadImages,
    Unsubscribe,
    SaveAttachment {
        message_id: String,
        index: usize,
    },
    /// Show one attachment without leaving the window.
    PreviewAttachment {
        message_id: String,
        index: usize,
    },
    /// Write every attachment of one message into a folder.
    SaveAllAttachments {
        message_id: String,
    },
    Mailto(String),
    /// The card for one sender, asked for by clicking their name.
    ShowContact(String),
}

struct Buttons {
    archive: gtk::Button,
    trash: gtk::Button,
    junk: gtk::Button,
    read: gtk::Button,
    star: adw::SplitButton,
    reply: gtk::Button,
    reply_all: gtk::Button,
    forward: gtk::Button,
    edit: gtk::Button,
    more: gtk::MenuButton,
}

/// A message body after cleaning, with the body length and image count it
/// came from. Either one changing means the body needs cleaning again.
struct CleanBody {
    mark: (usize, usize),
    html: String,
}

pub struct ConversationView {
    pub page: adw::NavigationPage,
    /// Applies or removes labels; the window fills its popover.
    pub label_button: gtk::MenuButton,
    many: adw::StatusPage,
    many_read: gtk::Button,
    many_star: gtk::Button,
    many_mute: gtk::Button,
    many_junk: gtk::Button,
    many_trash: gtk::Button,
    stack: gtk::Stack,
    webview: webkit::WebView,
    content: webkit::UserContentManager,
    banner: adw::Banner,
    /// The event card above the message, shown when the open message
    /// carries an invitation.
    pub card: Rc<EventCard>,
    /// The card above that, shown when gpg has something to say about the
    /// message.
    seal: Rc<PgpCard>,
    /// Cleaned HTML per message. A thread renders at least twice per open.
    sanitized: RefCell<HashMap<String, CleanBody>>,
    list_banner: adw::Banner,
    /// The menu section whose first item adds or removes the sender as a VIP.
    sender_menu: gio::Menu,
    /// The menu section holding Mute, whose wording follows the thread.
    mark_menu: gio::Menu,
    /// Remind Me times, recomputed whenever a conversation opens.
    remind: gio::Menu,
    buttons: Buttons,
    filter: RefCell<Option<webkit::UserContentFilter>>,
    open: RefCell<Option<OpenThread>>,
    scroll_to: RefCell<Option<String>>,
    compact: Cell<bool>,
    detached: Cell<bool>,
}

impl ConversationView {
    pub fn new(on_action: impl Fn(Action) + 'static) -> Rc<ConversationView> {
        let on_action: Rc<dyn Fn(Action)> = Rc::new(on_action);
        let content = webkit::UserContentManager::new();
        let settings = webkit::Settings::new();
        settings.set_enable_javascript(true);
        settings.set_enable_javascript_markup(false);
        settings.set_javascript_can_open_windows_automatically(false);
        settings.set_enable_developer_extras(false);
        settings.set_enable_html5_local_storage(false);
        settings.set_enable_html5_database(false);
        settings.set_enable_page_cache(false);
        settings.set_enable_media(false);
        settings.set_enable_mediasource(false);
        settings.set_enable_encrypted_media(false);
        settings.set_enable_webaudio(false);
        settings.set_enable_webgl(false);
        settings.set_enable_webrtc(false);
        settings.set_enable_fullscreen(false);
        settings.set_enable_back_forward_navigation_gestures(false);
        settings.set_allow_file_access_from_file_urls(false);
        settings.set_enable_smooth_scrolling(true);
        settings.set_auto_load_images(true);
        let session = network_session();
        let webview = webkit::WebView::builder()
            .network_session(&session)
            .user_content_manager(&content)
            .settings(&settings)
            .build();
        webview.set_vexpand(true);
        webview.set_hexpand(true);

        let empty = adw::StatusPage::builder()
            .icon_name("dev.penguinmail.PenguinMail-symbolic")
            .title("No Conversation Selected")
            .build();
        empty.add_css_class("dim-label");
        let banner = adw::Banner::builder()
            .title("Remote images are hidden to protect your privacy")
            .button_label("Load Images")
            .revealed(false)
            .build();
        let list_banner = adw::Banner::builder()
            .title("This message is from a mailing list")
            .button_label("Unsubscribe")
            .revealed(false)
            .build();
        let bulk = adw::WrapBox::builder()
            .child_spacing(10)
            .line_spacing(10)
            .align(0.5)
            .build();
        let (mut many_read, mut many_star, mut many_mute, mut many_junk, mut many_trash) =
            (None, None, None, None, None);
        for (label, action) in [
            ("Archive", "win.archive"),
            ("Mark as Read", "win.toggle-read"),
            ("Flag", "win.toggle-star"),
            ("Mute", "win.mute"),
            ("Junk", "win.junk"),
            ("Move to Trash", "win.trash"),
        ] {
            let pill = gtk::Button::builder()
                .label(label)
                .action_name(action)
                .css_classes(["pill"])
                .build();
            match action {
                "win.archive" => pill.add_css_class("suggested-action"),
                "win.toggle-read" => many_read = Some(pill.clone()),
                "win.toggle-star" => many_star = Some(pill.clone()),
                "win.mute" => many_mute = Some(pill.clone()),
                "win.junk" => many_junk = Some(pill.clone()),
                "win.trash" => many_trash = Some(pill.clone()),
                _ => {}
            }
            bulk.append(&pill);
        }
        let (many_read, many_star, many_mute, many_junk, many_trash) = (
            many_read.expect("the bulk actions include read"),
            many_star.expect("the bulk actions include star"),
            many_mute.expect("the bulk actions include mute"),
            many_junk.expect("the bulk actions include junk"),
            many_trash.expect("the bulk actions include trash"),
        );
        let many = adw::StatusPage::builder()
            .icon_name("penguin-mail-inbox-symbolic")
            .title("Several Conversations Selected")
            .description("Actions and shortcuts apply to all of them. Esc clears the selection.")
            .child(&bulk)
            .build();
        let card = {
            let on_action = Rc::clone(&on_action);
            EventCard::new(move |action| on_action(Action::Invitation(action)))
        };
        let seal = PgpCard::new();
        let web_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        web_box.append(&list_banner);
        web_box.append(&banner);
        web_box.append(&seal.widget);
        web_box.append(&card.widget);
        web_box.append(&webview);
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&web_box, Some("thread"));
        stack.add_named(&many, Some("many"));

        let button = |icon: &str, tip: &str| {
            gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tip)
                .build()
        };
        let buttons = Buttons {
            archive: button("penguin-mail-archive-symbolic", "Archive (E or Ctrl+Alt+A)"),
            trash: button("user-trash-symbolic", "Move to Trash (Delete)"),
            junk: button("mail-mark-junk-symbolic", "Junk (Ctrl+Shift+J)"),
            read: button("mail-unread-symbolic", "Mark as Unread (Ctrl+Shift+U)"),
            star: adw::SplitButton::builder()
                .icon_name("penguin-mail-flag-outline-symbolic")
                .tooltip_text("Flag (Ctrl+Shift+L)")
                .dropdown_tooltip("Flag Color")
                .popover(&flag_colors())
                .build(),
            reply: button("mail-reply-sender-symbolic", "Reply (Ctrl+R)"),
            reply_all: button("mail-reply-all-symbolic", "Reply All (Ctrl+Shift+R)"),
            forward: button("mail-forward-symbolic", "Forward (Ctrl+Shift+F)"),
            edit: gtk::Button::builder()
                .label("Edit Draft")
                .css_classes(["suggested-action"])
                .visible(false)
                .build(),
            more: gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .tooltip_text("More Actions")
                .visible(false)
                .build(),
        };
        let more = gio::Menu::new();
        let replies = gio::Menu::new();
        replies.append(Some("Reply All"), Some("win.reply-all"));
        replies.append(Some("Forward"), Some("win.forward"));
        more.append_section(None, &replies);
        let marks = gio::Menu::new();
        marks.append(Some("Flag or Unflag"), Some("win.toggle-star"));
        marks.append(Some("Mark Read or Unread"), Some("win.toggle-read"));
        marks.append(Some("Mute"), Some("win.mute"));
        marks.append(Some("Junk"), Some("win.junk"));
        marks.append(Some("Labels…"), Some("win.label"));
        let remind_menu = gio::Menu::new();
        marks.append_submenu(Some("Remind Me"), &remind_menu);
        // Shown only in the Follow Up mailbox, where the action is enabled.
        let dismiss = gio::MenuItem::new(Some("Dismiss Follow-Up"), Some("win.dismiss-follow-up"));
        dismiss.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
        marks.append_item(&dismiss);
        more.append_section(None, &marks);
        let views = gio::Menu::new();
        views.append(Some("Open in New Window"), Some("win.open-window"));
        views.append(Some("Print…"), Some("win.print"));
        views.append(Some("View Source"), Some("win.view-source"));
        views.append(Some("Export…"), Some("win.export"));
        more.append_section(None, &views);
        let sender = gio::Menu::new();
        sender.append(Some("Add Sender to VIPs"), Some("win.toggle-vip"));
        sender.append(Some("Unsubscribe…"), Some("win.unsubscribe"));
        sender.append(Some("Always Load Images…"), Some("win.always-load-images"));
        sender.append(Some("Block Sender…"), Some("win.block-sender"));
        let categories = gio::Menu::new();
        for (name, key) in [
            ("Primary", "primary"),
            ("Updates", "updates"),
            ("Promotions", "promotions"),
            ("Social", "social"),
        ] {
            let item = gio::MenuItem::new(Some(name), None);
            item.set_action_and_target_value(
                Some("win.categorize-sender"),
                Some(&key.to_variant()),
            );
            categories.append_item(&item);
        }
        sender.append_submenu(Some("Categorize Sender"), &categories);
        more.append_section(None, &sender);
        buttons.more.set_menu_model(Some(&more));
        let sender_menu = sender.clone();
        let mark_menu = marks.clone();
        let remind = remind_menu.clone();
        let header = adw::HeaderBar::builder()
            .title_widget(&gtk::Label::new(None))
            .build();
        let label_button = gtk::MenuButton::builder()
            .icon_name("penguin-mail-tag-symbolic")
            .tooltip_text("Labels (L)")
            .build();
        for widget in [
            buttons.archive.upcast_ref::<gtk::Widget>(),
            buttons.trash.upcast_ref(),
            buttons.junk.upcast_ref(),
            buttons.read.upcast_ref(),
            buttons.star.upcast_ref(),
        ] {
            header.pack_start(widget);
        }
        header.pack_start(&label_button);
        header.pack_end(&buttons.more);
        for widget in [
            &buttons.reply,
            &buttons.reply_all,
            &buttons.forward,
            &buttons.edit,
        ] {
            header.pack_end(widget);
        }
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        let page = adw::NavigationPage::builder()
            .title("Conversation")
            .tag("thread")
            .child(&toolbar)
            .build();

        let wire = |widget: &gtk::Button, make: fn() -> Action| {
            let on_action = Rc::clone(&on_action);
            widget.connect_clicked(move |_| on_action(make()));
        };
        wire(&buttons.archive, || Action::Archive);
        wire(&buttons.junk, || Action::Junk);
        wire(&buttons.trash, || Action::Trash);
        wire(&buttons.read, || Action::ToggleRead);
        {
            let on_action = Rc::clone(&on_action);
            buttons
                .star
                .connect_clicked(move |_| on_action(Action::ToggleStar));
        }
        wire(&buttons.reply, || Action::Reply(ReplyKind::Reply));
        wire(&buttons.reply_all, || Action::Reply(ReplyKind::ReplyAll));
        wire(&buttons.forward, || Action::Reply(ReplyKind::Forward));
        wire(&buttons.edit, || Action::EditDraft);
        {
            let on_action = Rc::clone(&on_action);
            banner.connect_button_clicked(move |_| on_action(Action::LoadImages));
        }
        {
            let on_action = Rc::clone(&on_action);
            list_banner.connect_button_clicked(move |_| on_action(Action::Unsubscribe));
        }

        let view = Rc::new(ConversationView {
            page,
            label_button,
            many,
            many_read,
            many_star,
            many_mute,
            many_junk,
            many_trash,
            stack,
            webview,
            content,
            banner,
            card,
            seal,
            list_banner,
            sanitized: RefCell::new(HashMap::new()),
            sender_menu,
            mark_menu,
            remind,
            buttons,
            filter: RefCell::new(None),
            open: RefCell::new(None),
            scroll_to: RefCell::new(None),
            compact: Cell::new(false),
            detached: Cell::new(false),
        });

        view.set_buttons_shown(false);
        let weak = Rc::downgrade(&view);
        let actions = Rc::clone(&on_action);
        view.webview
            .connect_decide_policy(move |_, decision, kind| {
                use webkit::PolicyDecisionType as Kind;
                if !matches!(kind, Kind::NavigationAction | Kind::NewWindowAction) {
                    return false;
                }
                let Some(navigation) = decision.downcast_ref::<webkit::NavigationPolicyDecision>()
                else {
                    return false;
                };
                let uri = navigation
                    .navigation_action()
                    .and_then(|action| action.request())
                    .and_then(|request| request.uri())
                    .map(|uri| uri.to_string())
                    .unwrap_or_default();
                if kind == Kind::NavigationAction && uri == "about:blank" {
                    decision.use_();
                    return true;
                }
                decision.ignore();
                if let Some(view) = weak.upgrade() {
                    view.follow(&uri, &actions);
                }
                true
            });
        let weak = Rc::downgrade(&view);
        view.webview.connect_load_changed(move |webview, event| {
            if event != webkit::LoadEvent::Finished {
                return;
            }
            let Some(view) = weak.upgrade() else { return };
            if let Some(id) = view.scroll_to.take() {
                run_script(
                    webview,
                    &format!(
                        "document.getElementById('m-{id}')?.scrollIntoView({{block:'start'}})"
                    ),
                );
            }
        });
        view.webview.connect_context_menu(|_, menu, _| {
            use webkit::ContextMenuAction as Item;
            for item in menu.items() {
                if !matches!(
                    item.stock_action(),
                    Item::Copy
                        | Item::CopyLinkToClipboard
                        | Item::CopyImageToClipboard
                        | Item::SelectAll
                ) {
                    menu.remove(&item);
                }
            }
            menu.items().is_empty()
        });
        let weak = Rc::downgrade(&view);
        adw::StyleManager::default().connect_dark_notify(move |_| {
            if let Some(view) = weak.upgrade() {
                view.render(false);
            }
        });
        view
    }

    /// Words the VIP menu item for whether the sender is one already.
    pub fn set_sender_vip(&self, vip: bool) {
        self.sender_menu.remove(0);
        self.sender_menu.insert(
            0,
            Some(if vip {
                "Remove Sender from VIPs"
            } else {
                "Add Sender to VIPs"
            }),
            Some("win.toggle-vip"),
        );
    }

    /// Words the Mute menu item for whether the thread is muted already.
    /// It sits third in the section, after the two mark items.
    pub fn set_muted(&self, muted: bool) {
        self.mark_menu.remove(2);
        self.mark_menu.insert(
            2,
            Some(if muted { "Unmute" } else { "Mute" }),
            Some("win.mute"),
        );
    }

    /// Opens the print dialog for the conversation on screen.
    pub fn print(&self) {
        if self.open.borrow().is_none() {
            return;
        }
        let window = self.page.root().and_downcast::<gtk::Window>();
        webkit::PrintOperation::new(&self.webview).run_dialog(window.as_ref());
    }

    /// For a conversation in its own window: labels stay in the main window.
    pub fn set_detached(&self) {
        self.label_button.set_visible(false);
        self.detached.set(true);
    }

    /// Installs the compiled filter that blocks remote content.
    pub fn set_filter(&self, filter: webkit::UserContentFilter) {
        self.content.add_filter(&filter);
        *self.filter.borrow_mut() = Some(filter);
    }

    /// Adjusts the trash and junk buttons to the folder on screen: in the
    /// Trash, trash erases the mail; in Junk, junk marks it as not junk.
    pub fn set_folder(&self, folder: Option<Folder>) {
        let (trash_icon, trash_tip) = match folder {
            Some(Folder::Trash) => ("edit-delete-symbolic", "Delete Forever (Delete)"),
            _ => ("user-trash-symbolic", "Move to Trash (Delete)"),
        };
        self.buttons.trash.set_icon_name(trash_icon);
        self.buttons.trash.set_tooltip_text(Some(trash_tip));
        let (junk_icon, junk_tip) = match folder {
            Some(Folder::Junk) => ("mail-mark-notjunk-symbolic", "Not Junk (Ctrl+Shift+J)"),
            _ => ("mail-mark-junk-symbolic", "Junk (Ctrl+Shift+J)"),
        };
        self.buttons.junk.set_icon_name(junk_icon);
        self.buttons.junk.set_tooltip_text(Some(junk_tip));
        self.many_trash.set_label(match folder {
            Some(Folder::Trash) => "Delete Forever",
            _ => "Move to Trash",
        });
        self.many_junk.set_label(match folder {
            Some(Folder::Junk) => "Not Junk",
            _ => "Junk",
        });
    }

    /// Says what the trash button does in a mailbox where it does not
    /// move mail to the Trash.
    pub fn set_trash_tooltip(&self, tip: &str) {
        self.buttons.trash.set_tooltip_text(Some(tip));
    }

    pub fn showing_many(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some("many")
    }

    /// The page for a multiple selection. `any_unread`, `all_starred`, and
    /// `all_muted` decide what the read, star, and mute buttons do.
    pub fn show_many(
        &self,
        count: usize,
        noun: &str,
        any_unread: bool,
        all_starred: bool,
        all_muted: bool,
    ) {
        self.many_read.set_label(if any_unread {
            "Mark as Read"
        } else {
            "Mark as Unread"
        });
        self.many_star
            .set_label(if all_starred { "Unflag" } else { "Flag" });
        self.many_mute
            .set_label(if all_muted { "Unmute" } else { "Mute" });
        *self.open.borrow_mut() = None;
        self.many.set_title(&format!("{count} {noun} Selected"));
        self.stack.set_visible_child_name("many");
        self.banner.set_revealed(false);
        self.list_banner.set_revealed(false);
        self.show_invitation(None);
        self.seal.hide();
        self.set_buttons_shown(true);
        let b = &self.buttons;
        for button in [&b.reply, &b.reply_all, &b.forward, &b.edit] {
            button.set_visible(false);
        }
        b.more.set_visible(false);
    }

    /// Stops the WebKit process that draws mail. It holds about 80 MB, and
    /// a closed window has no use for it. WebKit starts a new one when this
    /// view loads its next message.
    pub fn stop_rendering(&self) {
        self.clear();
        self.webview.terminate_web_process();
    }

    pub fn clear(&self) {
        *self.open.borrow_mut() = None;
        self.stack.set_visible_child_name("empty");
        self.set_buttons_shown(false);
        self.banner.set_revealed(false);
        self.list_banner.set_revealed(false);
        self.show_invitation(None);
        self.seal.hide();
    }

    /// Puts an invitation above the message, or takes the card away when
    /// the message carries none.
    pub fn show_invitation(&self, showing: Option<Showing>) {
        match showing {
            Some(showing) => self.card.show(showing),
            None => self.card.hide(),
        }
    }

    /// Reads what the card shows. `None` means no invitation is on screen.
    pub fn with_invitation<R>(&self, f: impl FnOnce(&Showing) -> R) -> Option<R> {
        self.card.with_showing(f)
    }

    /// Whether `row` is what the view shows now.
    pub fn is_showing_row(&self, row: &mailrs_domain::ThreadSummary) -> bool {
        self.open.borrow().as_ref().is_some_and(|o| {
            o.account_id == row.account_id
                && o.thread_id == row.id
                && o.only_message == row.message_id
        })
    }

    pub fn set_zoom(&self, zoom: f64) {
        self.webview.set_zoom_level(zoom);
    }

    pub fn is_showing(&self, account_id: AccountId, thread_id: &str) -> bool {
        self.open
            .borrow()
            .as_ref()
            .is_some_and(|o| o.account_id == account_id && o.thread_id == thread_id)
    }

    pub fn with_open<R>(&self, f: impl FnOnce(&mut OpenThread) -> R) -> Option<R> {
        self.open.borrow_mut().as_mut().map(f)
    }

    /// Shows a thread. `scroll` jumps to the first expanded message.
    pub fn show(&self, thread: OpenThread, scroll: bool) {
        *self.open.borrow_mut() = Some(thread);
        self.stack.set_visible_child_name("thread");
        self.render(scroll);
    }

    /// Redraws the open thread, for example after its bodies arrive.
    pub fn render(&self, scroll: bool) {
        let open = self.open.borrow();
        let Some(open) = open.as_ref() else { return };
        let manager = &self.content;
        manager.remove_all_filters();
        if !open.images_allowed
            && let Some(filter) = self.filter.borrow().as_ref()
        {
            manager.add_filter(filter);
        }
        let style = adw::StyleManager::default();
        let theme = Theme {
            dark: style.is_dark(),
            accent: style.accent_color_rgba().to_str().to_string(),
        };
        let empty = HashMap::new();
        let mut clean = self.sanitized.borrow_mut();
        clean.retain(|id, _| open.bodies.contains_key(id));
        for meta in &open.messages {
            let Some(Ok(body)) = open.bodies.get(&meta.id) else {
                continue;
            };
            let Some(html) = body.html.as_deref().filter(|h| !h.trim().is_empty()) else {
                continue;
            };
            let images = open.inline_images.get(&meta.id).unwrap_or(&empty);
            let mark = (html.len(), images.len());
            if clean.get(&meta.id).is_none_or(|seen| seen.mark != mark) {
                let body = CleanBody {
                    mark,
                    html: sanitize_html(html, images),
                };
                clean.insert(meta.id.clone(), body);
            }
        }
        let views: Vec<MessageView> = open
            .messages
            .iter()
            .map(|meta| MessageView {
                meta,
                body: match open.bodies.get(&meta.id) {
                    None => BodyState::Loading,
                    Some(Ok(body)) => BodyState::Loaded(body),
                    Some(Err(reason)) => BodyState::Failed(reason),
                },
                expanded: open.expanded.contains(&meta.id),
                inline_images: open.inline_images.get(&meta.id).unwrap_or(&empty),
                thumbnails: &open.thumbnails,
                sanitized: clean.get(&meta.id).map(|body| body.html.as_str()),
            })
            .collect();
        let html = render(
            &Conversation {
                subject: &open.subject,
                messages: views,
                me: &open.me,
                photos: &open.photos,
                allow_remote: open.images_allowed,
            },
            &theme,
        );
        let background = if theme.dark {
            gdk::RGBA::new(0.133, 0.133, 0.149, 1.0)
        } else {
            gdk::RGBA::WHITE
        };
        self.webview.set_background_color(&background);
        if scroll && open.messages.len() > 2 {
            *self.scroll_to.borrow_mut() = open
                .messages
                .iter()
                .find(|m| open.expanded.contains(&m.id))
                .map(|m| script_safe(&m.id));
        }
        self.webview.load_html(&html, None);
        match &open.pgp {
            Some(mark) => self.seal.show(mark),
            None => self.seal.hide(),
        }
        self.banner
            .set_revealed(!open.images_allowed && open.has_remote_images());
        self.update_buttons(open);
    }

    /// Updates the header buttons after label changes, without redrawing.
    pub fn render_buttons(&self) {
        if let Some(open) = self.open.borrow().as_ref() {
            self.update_buttons(open);
        }
    }

    /// Fills Remind Me with times that make sense now.
    fn refresh_remind_menu(&self) {
        self.remind.remove_all();
        let presets = gio::Menu::new();
        for (label, at) in crate::format::remind_presets(chrono::Local::now()) {
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(Some("win.remind-at"), Some(&at.to_variant()));
            presets.append_item(&item);
        }
        self.remind.append_section(None, &presets);
        let custom = gio::Menu::new();
        custom.append(Some("Choose a Time…"), Some("win.remind-custom"));
        self.remind.append_section(None, &custom);
    }

    fn update_buttons(&self, open: &OpenThread) {
        self.refresh_remind_menu();
        self.set_buttons_shown(true);
        let draft = open.is_draft();
        let full = !self.compact.get();
        self.buttons.reply.set_visible(!draft);
        self.buttons.reply_all.set_visible(!draft && full);
        self.buttons.forward.set_visible(!draft && full);
        self.buttons.more.set_visible(!draft);
        self.list_banner
            .set_revealed(!open.unsubscribed && open.list_unsubscribe().is_some());
        self.buttons.edit.set_visible(draft);
        let starred = open.starred();
        let star = &self.buttons.star;
        star.set_icon_name(if starred {
            "penguin-mail-flag-symbolic"
        } else {
            "penguin-mail-flag-outline-symbolic"
        });
        for color in FlagColor::ALL {
            star.remove_css_class(&format!("flag-{}", color.as_str()));
        }
        if starred {
            star.add_css_class(&format!(
                "flag-{}",
                open.flag_color.unwrap_or(FlagColor::Red).as_str()
            ));
        }
        star.set_tooltip_text(Some(if starred {
            "Unflag (Ctrl+Shift+L)"
        } else {
            "Flag (Ctrl+Shift+L)"
        }));
        self.set_muted(open.muted());
        let unread = open.unread();
        self.buttons.read.set_icon_name(if unread {
            "mail-read-symbolic"
        } else {
            "mail-unread-symbolic"
        });
        self.buttons.read.set_tooltip_text(Some(if unread {
            "Mark as Read (U)"
        } else {
            "Mark as Unread (U)"
        }));
    }

    /// On phone widths, secondary actions move into the "more" menu.
    pub fn set_compact(&self, compact: bool) {
        self.compact.set(compact);
        let open = self.open.borrow();
        match open.as_ref() {
            Some(open) => self.update_buttons(open),
            None => self.set_buttons_shown(false),
        }
    }

    /// Shows the thread actions, or hides them all when nothing is open.
    fn set_buttons_shown(&self, shown: bool) {
        let b = &self.buttons;
        let full = shown && !self.compact.get();
        for button in [&b.archive, &b.trash, &b.reply] {
            button.set_visible(shown);
        }
        for button in [&b.junk, &b.read, &b.reply_all, &b.forward] {
            button.set_visible(full);
        }
        b.star.set_visible(full);
        self.label_button.set_visible(full && !self.detached.get());
        b.more.set_visible(shown);
        if !shown {
            b.edit.set_visible(false);
        }
    }

    fn follow(&self, uri: &str, actions: &Rc<dyn Fn(Action)>) {
        if let Some(id) = uri.strip_prefix("mailrs:toggle/") {
            self.toggle(id);
        } else if let Some(rest) = uri.strip_prefix("mailrs:attachment/") {
            if let Some((message_id, index)) = rest.rsplit_once('/')
                && let Ok(index) = index.parse()
            {
                actions(Action::SaveAttachment {
                    message_id: message_id.to_string(),
                    index,
                });
            }
        } else if let Some(rest) = uri.strip_prefix("mailrs:preview/") {
            if let Some((message_id, index)) = rest.rsplit_once('/')
                && let Ok(index) = index.parse()
            {
                actions(Action::PreviewAttachment {
                    message_id: message_id.to_string(),
                    index,
                });
            }
        } else if let Some(message_id) = uri.strip_prefix("mailrs:attachments/") {
            actions(Action::SaveAllAttachments {
                message_id: message_id.to_string(),
            });
        } else if let Some(address) = uri.strip_prefix("mailrs:contact/") {
            actions(Action::ShowContact(address.to_string()));
        } else if let Some(address) = uri.strip_prefix("mailto:") {
            actions(Action::Mailto(
                address.split('?').next().unwrap_or(address).to_string(),
            ));
        } else if uri.starts_with("https://") || uri.starts_with("http://") {
            let window = self.page.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(uri).launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
        }
    }

    fn toggle(&self, id: &str) {
        let expanded = self.with_open(|open| {
            if !open.expanded.remove(id) {
                open.expanded.insert(id.to_string());
            }
            open.expanded.contains(id)
        });
        if expanded.is_some() {
            let id = script_safe(id);
            run_script(
                &self.webview,
                &format!(
                    "(function(){{var m=document.getElementById('m-{id}');if(m){{m.classList.toggle('expanded');m.classList.toggle('collapsed');}}}})()"
                ),
            );
        }
    }
}

/// The flag button's menu: seven colours in a row, then Clear Flag.
fn flag_colors() -> gtk::Popover {
    let row = gtk::Box::builder().spacing(2).build();
    for color in FlagColor::ALL {
        let button = gtk::Button::builder()
            .icon_name("penguin-mail-flag-symbolic")
            .tooltip_text(format!(
                "{} (Ctrl+Alt+{})",
                color.name(),
                color_index(color) + 1
            ))
            .action_name("win.flag-color")
            .action_target(&color.as_str().to_variant())
            .css_classes(["flat", "flag-swatch", &format!("flag-{}", color.as_str())])
            .build();
        row.append(&button);
    }
    let clear = gtk::Button::builder()
        .label("Clear Flag")
        .action_name("win.flag-color")
        .action_target(&"none".to_variant())
        .css_classes(["flat"])
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    content.append(&row);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    content.append(&clear);
    let popover = gtk::Popover::builder().child(&content).build();
    // Picking a colour closes the menu.
    let pop = popover.clone();
    let mut child = row.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            let pop = pop.clone();
            button.connect_clicked(move |_| pop.popdown());
        }
    }
    clear.connect_clicked(move |_| pop.popdown());
    popover
}

fn color_index(color: FlagColor) -> usize {
    FlagColor::ALL.iter().position(|c| *c == color).unwrap_or(0)
}

fn run_script(webview: &webkit::WebView, script: &str) {
    webview.evaluate_javascript(script, None, None, gio::Cancellable::NONE, |_| {});
}

/// Keeps only characters that are safe inside a quoted script string.
fn script_safe(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect()
}

/// One network session for every conversation view. Each session runs its
/// own WebKit network process, and a detached window needs no second one.
/// Ephemeral keeps cookies and caches in memory, so nothing lands on disk.
fn network_session() -> webkit::NetworkSession {
    thread_local! {
        static SESSION: webkit::NetworkSession = webkit::NetworkSession::new_ephemeral();
    }
    SESSION.with(|s| s.clone())
}
