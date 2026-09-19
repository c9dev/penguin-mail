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
use mailrs_domain::{AccountId, MessageBody, MessageMeta};
use webkit::prelude::*;

use super::Folder;
use crate::compose::ReplyKind;
use crate::render::{BodyState, Conversation, MessageView, Theme, render};

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
    /// Set once the user unsubscribed from this thread's list.
    pub unsubscribed: bool,
}

impl OpenThread {
    pub fn is_draft(&self) -> bool {
        self.messages.last().is_some_and(|m| m.has_label("DRAFT"))
    }

    pub fn starred(&self) -> bool {
        self.messages.iter().any(|m| m.has_label("STARRED"))
    }

    pub fn unread(&self) -> bool {
        self.messages.iter().any(|m| m.is_unread())
    }

    /// The message a reply answers: the newest one that is not a draft.
    pub fn reply_target(&self) -> Option<&MessageMeta> {
        self.messages.iter().rev().find(|m| !m.has_label("DRAFT"))
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
    Reply(ReplyKind),
    EditDraft,
    Archive,
    Trash,
    Junk,
    ToggleStar,
    ToggleRead,
    LoadImages,
    Unsubscribe,
    SaveAttachment { message_id: String, index: usize },
    Mailto(String),
}

struct Buttons {
    archive: gtk::Button,
    trash: gtk::Button,
    junk: gtk::Button,
    read: gtk::Button,
    star: gtk::Button,
    reply: gtk::Button,
    reply_all: gtk::Button,
    forward: gtk::Button,
    edit: gtk::Button,
    more: gtk::MenuButton,
}

pub struct ConversationView {
    pub page: adw::NavigationPage,
    /// Applies or removes labels; the window fills its popover.
    pub label_button: gtk::MenuButton,
    many: adw::StatusPage,
    many_read: gtk::Button,
    many_star: gtk::Button,
    many_junk: gtk::Button,
    many_trash: gtk::Button,
    stack: gtk::Stack,
    webview: webkit::WebView,
    content: webkit::UserContentManager,
    banner: adw::Banner,
    list_banner: adw::Banner,
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
        let session = webkit::NetworkSession::new_ephemeral();
        let webview = webkit::WebView::builder()
            .network_session(&session)
            .user_content_manager(&content)
            .settings(&settings)
            .build();
        webview.set_vexpand(true);
        webview.set_hexpand(true);

        let empty = adw::StatusPage::builder()
            .icon_name("dev.mailrs.Mailrs-symbolic")
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
        let (mut many_read, mut many_star, mut many_junk, mut many_trash) =
            (None, None, None, None);
        for (label, action) in [
            ("Archive", "win.archive"),
            ("Mark as Read", "win.toggle-read"),
            ("Star", "win.toggle-star"),
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
                "win.junk" => many_junk = Some(pill.clone()),
                "win.trash" => many_trash = Some(pill.clone()),
                _ => {}
            }
            bulk.append(&pill);
        }
        let (many_read, many_star, many_junk, many_trash) = (
            many_read.expect("the bulk actions include read"),
            many_star.expect("the bulk actions include star"),
            many_junk.expect("the bulk actions include junk"),
            many_trash.expect("the bulk actions include trash"),
        );
        let many = adw::StatusPage::builder()
            .icon_name("mailrs-inbox-symbolic")
            .title("Several Conversations Selected")
            .description("Actions and shortcuts apply to all of them. Esc clears the selection.")
            .child(&bulk)
            .build();
        let web_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        web_box.append(&list_banner);
        web_box.append(&banner);
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
            archive: button("mailrs-archive-symbolic", "Archive (E or Ctrl+Alt+A)"),
            trash: button("user-trash-symbolic", "Move to Trash (Delete)"),
            junk: button("mail-mark-junk-symbolic", "Junk (Ctrl+Shift+J)"),
            read: button("mail-unread-symbolic", "Mark as Unread (Ctrl+Shift+U)"),
            star: button("non-starred-symbolic", "Star (Ctrl+Shift+L)"),
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
        marks.append(Some("Star or Unstar"), Some("win.toggle-star"));
        marks.append(Some("Mark Read or Unread"), Some("win.toggle-read"));
        marks.append(Some("Junk"), Some("win.junk"));
        marks.append(Some("Labels…"), Some("win.label"));
        more.append_section(None, &marks);
        let views = gio::Menu::new();
        views.append(Some("Open in New Window"), Some("win.open-window"));
        views.append(Some("Print…"), Some("win.print"));
        views.append(Some("View Source"), Some("win.view-source"));
        more.append_section(None, &views);
        let sender = gio::Menu::new();
        sender.append(Some("Unsubscribe…"), Some("win.unsubscribe"));
        sender.append(Some("Block Sender…"), Some("win.block-sender"));
        more.append_section(None, &sender);
        buttons.more.set_menu_model(Some(&more));
        let header = adw::HeaderBar::builder()
            .title_widget(&gtk::Label::new(None))
            .build();
        let label_button = gtk::MenuButton::builder()
            .icon_name("mailrs-tag-symbolic")
            .tooltip_text("Labels (L)")
            .build();
        for widget in [
            &buttons.archive,
            &buttons.trash,
            &buttons.junk,
            &buttons.read,
            &buttons.star,
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
        wire(&buttons.star, || Action::ToggleStar);
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
            many_junk,
            many_trash,
            stack,
            webview,
            content,
            banner,
            list_banner,
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
    /// Trash, trash puts mail back; in Junk, junk marks it as not junk.
    pub fn set_folder(&self, folder: Option<Folder>) {
        let (trash_icon, trash_tip) = match folder {
            Some(Folder::Trash) => ("mailrs-inbox-symbolic", "Move to Inbox"),
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
            Some(Folder::Trash) => "Move to Inbox",
            _ => "Move to Trash",
        });
        self.many_junk.set_label(match folder {
            Some(Folder::Junk) => "Not Junk",
            _ => "Junk",
        });
    }

    pub fn showing_many(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some("many")
    }

    /// The page for a multiple selection. `any_unread` and `all_starred`
    /// decide what the read and star buttons do.
    pub fn show_many(&self, count: usize, noun: &str, any_unread: bool, all_starred: bool) {
        self.many_read.set_label(if any_unread {
            "Mark as Read"
        } else {
            "Mark as Unread"
        });
        self.many_star
            .set_label(if all_starred { "Unstar" } else { "Star" });
        *self.open.borrow_mut() = None;
        self.many.set_title(&format!("{count} {noun} Selected"));
        self.stack.set_visible_child_name("many");
        self.banner.set_revealed(false);
        self.list_banner.set_revealed(false);
        self.set_buttons_shown(true);
        let b = &self.buttons;
        for button in [&b.reply, &b.reply_all, &b.forward, &b.edit] {
            button.set_visible(false);
        }
        b.more.set_visible(false);
    }

    pub fn clear(&self) {
        *self.open.borrow_mut() = None;
        self.stack.set_visible_child_name("empty");
        self.set_buttons_shown(false);
        self.banner.set_revealed(false);
        self.list_banner.set_revealed(false);
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
            })
            .collect();
        let html = render(
            &Conversation {
                subject: &open.subject,
                messages: views,
                me: &open.me,
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

    fn update_buttons(&self, open: &OpenThread) {
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
        self.buttons.star.set_icon_name(if starred {
            "starred-symbolic"
        } else {
            "non-starred-symbolic"
        });
        self.buttons
            .star
            .set_tooltip_text(Some(if starred { "Unstar (S)" } else { "Star (S)" }));
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
        for button in [&b.junk, &b.read, &b.star, &b.reply_all, &b.forward] {
            button.set_visible(full);
        }
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

fn run_script(webview: &webkit::WebView, script: &str) {
    webview.evaluate_javascript(script, None, None, gio::Cancellable::NONE, |_| {});
}

/// Keeps only characters that are safe inside a quoted script string.
fn script_safe(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect()
}
