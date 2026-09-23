//! The right pane: the whole thread in one WebView.
//!
//! Page JavaScript is off (`enable-javascript-markup` is false), so email
//! cannot run scripts. The app still runs a few tiny scripts of its own
//! through the WebKit API: collapsing a message, scrolling to one, and
//! [`MENU_SCRIPT`], which asks for the menu of the message under the
//! pointer. Finding text is WebKit's own, through [`FindBar`].

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_domain::{Category, FlagColor, Folder, MessageBody, MessageMeta, Target};
use webkit::prelude::*;

use super::find::FindBar;
use super::invitation::{self, EventCard, Showing};
use super::pgp::PgpCard;
use super::queued::QueuedCard;
use super::translation::TranslationCard;
use super::{name, name_with_shortcut};
use crate::compose::ReplyKind;
use crate::open_thread::{OpenThread, Unsent};
use crate::protection::run::{Claimed, Installed};
use crate::protection::{self};
use crate::render::{BodyState, Conversation, MessageView, Theme, render};
use crate::sanitize::sanitize_html;
use crate::translation::{Body, Prose, Translation};

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
    /// The menu for one message of the thread, asked for by a right click
    /// or the Menu key. The point is where the pointer was, in the web
    /// view's own coordinates.
    MessageMenu {
        message_id: String,
        x: f64,
        y: f64,
    },
    /// The translation card's button: translate the open message, or turn
    /// the translation it already has over.
    Translate,
}

/// The one script the page carries of its own accord. WebKit injects it
/// after each load, so it outlives the redraws a thread goes through.
///
/// It exists because the app cannot tell which message the pointer is
/// over: WebKit's own `context-menu` signal arrives with a hit test and no
/// way to run JavaScript, and the message bodies sit inside shadow roots
/// where the signal's node says little. The script reads the message under
/// the pointer and asks for its menu through the `mailrs:` scheme the app
/// already intercepts, by clicking a link the way the page's own links are
/// clicked.
///
/// A right click over selected words keeps WebKit's own menu, so Copy
/// still works on a quotation the reader picked out.
const MENU_SCRIPT: &str = r#"(function () {
  var asked = 0;
  var ask = function (article, x, y) {
    var id = article.id.substring(2);
    var link = document.createElement('a');
    link.href = 'mailrs:menu/' + id + '/' + Math.round(x) + '/' + Math.round(y) +
      '/' + (++asked);
    document.body.appendChild(link);
    link.click();
    link.remove();
  };
  var messageAt = function (node) {
    return node && node.closest ? node.closest('.message') : null;
  };
  document.addEventListener('contextmenu', function (event) {
    if (String(window.getSelection())) return;
    var article = messageAt(event.target);
    if (!article) return;
    event.preventDefault();
    ask(article, event.clientX, event.clientY);
  }, true);
  window.mailrsMenuKey = function () {
    var article = messageAt(document.activeElement) ||
      document.querySelector('.message.expanded');
    if (!article) return;
    var box = article.getBoundingClientRect();
    ask(article, box.left + 16, box.top + 16);
  };
})()"#;

/// Asks the page for the menu of the message holding the focus, as the Menu
/// key or Shift+F10 does. The app sends the keys here instead of letting the
/// page see them: WebKit keeps an empty text field inside the view, and GTK
/// gives that field the same two keys for its Cut and Paste menu, which then
/// opens in the window's corner and leaves no room for the message's own.
const MENU_KEY_SCRIPT: &str = "window.mailrsMenuKey && window.mailrsMenuKey()";

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

/// A message body after cleaning, with a mark of the HTML and the inline
/// images it was made from. A different mark means the body needs
/// cleaning again.
struct CleanBody {
    mark: u64,
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
    /// The card between the event card and the message, shown when the
    /// message is in a language the interface is not in.
    pub translate: Rc<TranslationCard>,
    /// The card at the top, shown for a queued message: when it goes, or
    /// why it has not gone, and what the person can do about it.
    queued: QueuedCard,
    /// Ctrl+F over the message. WebKit finds the text; the bar says where
    /// in the matches the reader is.
    find: Rc<FindBar>,
    /// The messages the find bar opened, kept so they close again when it
    /// goes away.
    find_closed: RefCell<Vec<String>>,
    /// Cleaned HTML per message. A thread renders at least twice per open.
    sanitized: RefCell<HashMap<String, CleanBody>>,
    list_banner: adw::Banner,
    /// The menu section whose first item adds or removes the sender as a VIP.
    sender_menu: gio::Menu,
    /// The menu section holding Mute, whose wording follows the thread.
    mark_menu: gio::Menu,
    /// The menu section holding Archive, Trash and Junk, whose wording
    /// follows the mailbox.
    filing_menu: gio::Menu,
    /// Everything that can be done to the conversations on screen. More
    /// Actions shows it, and so does a right click in the list.
    thread_menu: gio::Menu,
    /// Remind Me times, recomputed whenever a conversation opens.
    remind: gio::Menu,
    buttons: Buttons,
    /// The menu for one message. One popover serves the whole thread: the
    /// model changes with the message the menu was asked for.
    menu: gtk::PopoverMenu,
    /// Where that menu last opened, in the web view's coordinates, so a
    /// popover an item leads to comes up in the same place.
    menu_at: Cell<(i32, i32)>,
    /// The popover an item led to, such as the label list. Held so the one
    /// before it lets go of the view.
    menu_popover: RefCell<Option<gtk::Popover>>,
    filter: RefCell<Option<webkit::UserContentFilter>>,
    open: RefCell<Option<OpenThread>>,
    /// Counts the threads asked for, so a store read that answers after a
    /// later click can tell it lost.
    loading: Cell<u64>,
    scroll_to: RefCell<Option<String>>,
    compact: Cell<bool>,
    detached: Cell<bool>,
}

impl ConversationView {
    pub fn new(on_action: impl Fn(Action) + 'static) -> Rc<ConversationView> {
        let on_action: Rc<dyn Fn(Action)> = Rc::new(on_action);
        let content = webkit::UserContentManager::new();
        content.add_script(&webkit::UserScript::new(
            MENU_SCRIPT,
            webkit::UserContentInjectedFrames::TopFrame,
            webkit::UserScriptInjectionTime::End,
            &[],
            &[],
        ));
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
            .icon_name("io.github.c9dev.PenguinMail-symbolic")
            .title(gettext("No Conversation Selected"))
            .build();
        empty.add_css_class("dim-label");
        let banner = adw::Banner::builder()
            .title(gettext("Remote images are hidden to protect your privacy"))
            .button_label(gettext("Load Images"))
            .revealed(false)
            .build();
        let list_banner = adw::Banner::builder()
            .title(gettext("This message is from a mailing list"))
            .button_label(gettext("Unsubscribe"))
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
            (gettext("Archive"), "win.archive"),
            (gettext("Mark as Read"), "win.toggle-read"),
            (gettext("Flag"), "win.toggle-star"),
            (gettext("Mute"), "win.mute"),
            (gettext("Junk"), "win.junk"),
            (gettext("Move to Trash"), "win.trash"),
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
            .title(gettext("Several Conversations Selected"))
            .description(gettext(
                "Actions and shortcuts apply to all of them. Esc clears the selection.",
            ))
            .child(&bulk)
            .build();
        let card = {
            let on_action = Rc::clone(&on_action);
            EventCard::new(move |action| on_action(Action::Invitation(action)))
        };
        let seal = PgpCard::new();
        let translate = {
            let on_action = Rc::clone(&on_action);
            TranslationCard::new(move || on_action(Action::Translate))
        };
        let queued = QueuedCard::new();
        let web_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        web_box.append(&queued.widget);
        web_box.append(&list_banner);
        web_box.append(&banner);
        web_box.append(&seal.widget);
        web_box.append(&card.widget);
        web_box.append(&translate.widget);
        web_box.append(&webview);
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&web_box, Some("thread"));
        stack.add_named(&many, Some("many"));

        let button = |icon: &str, tip: String| {
            let button = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(&tip)
                .build();
            name_with_shortcut(&button, &tip);
            button
        };
        let buttons = Buttons {
            archive: button(
                "penguin-mail-archive-symbolic",
                gettext("Archive (E or Ctrl+Alt+A)"),
            ),
            trash: button("user-trash-symbolic", gettext("Move to Trash (Delete)")),
            junk: button("mail-mark-junk-symbolic", gettext("Junk (Ctrl+Shift+J)")),
            read: button(
                "mail-unread-symbolic",
                gettext("Mark as Unread (Ctrl+Shift+U)"),
            ),
            star: {
                let star = adw::SplitButton::builder()
                    .icon_name("penguin-mail-flag-outline-symbolic")
                    .tooltip_text(gettext("Flag (Ctrl+Shift+L)"))
                    .dropdown_tooltip(gettext("Flag Color"))
                    .popover(&flag_colors())
                    .build();
                name_with_shortcut(&star, &gettext("Flag (Ctrl+Shift+L)"));
                star
            },
            reply: button("mail-reply-sender-symbolic", gettext("Reply (Ctrl+R)")),
            reply_all: button(
                "mail-reply-all-symbolic",
                gettext("Reply All (Ctrl+Shift+R)"),
            ),
            forward: button("mail-forward-symbolic", gettext("Forward (Ctrl+Shift+F)")),
            edit: gtk::Button::builder()
                .label(gettext("Edit Draft"))
                .css_classes(["suggested-action"])
                .visible(false)
                .build(),
            more: {
                let more = gtk::MenuButton::builder()
                    .icon_name("view-more-symbolic")
                    .tooltip_text(gettext("More Actions"))
                    .visible(false)
                    .build();
                name(&more, &gettext("More Actions"));
                more
            },
        };
        let more = gio::Menu::new();
        // Only the Outbox turns these three on, and GTK leaves an item whose
        // action is off out of the menu rather than greying it.
        let waiting = gio::Menu::new();
        for (text, action) in [
            (gettext("Edit…"), "win.outbox-edit"),
            (gettext("Send Now"), "win.outbox-send"),
            (gettext("Delete"), "win.outbox-delete"),
        ] {
            let item = gio::MenuItem::new(Some(&text), Some(action));
            item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
            waiting.append_item(&item);
        }
        more.append_section(None, &waiting);
        let replies = gio::Menu::new();
        replies.append(Some(&gettext("Reply")), Some("win.reply"));
        replies.append(Some(&gettext("Reply All")), Some("win.reply-all"));
        replies.append(Some(&gettext("Forward")), Some("win.forward"));
        more.append_section(None, &replies);
        // set_folder words the last two for the mailbox on screen.
        let filing = gio::Menu::new();
        filing.append(Some(&gettext("Archive")), Some("win.archive"));
        filing.append(Some(&gettext("Move to Trash")), Some("win.trash"));
        filing.append(Some(&gettext("Junk")), Some("win.junk"));
        more.append_section(None, &filing);
        let marks = gio::Menu::new();
        marks.append(Some(&gettext("Flag or Unflag")), Some("win.toggle-star"));
        marks.append(
            Some(&gettext("Mark Read or Unread")),
            Some("win.toggle-read"),
        );
        marks.append(Some(&gettext("Mute")), Some("win.mute"));
        marks.append(Some(&gettext("Labels…")), Some("win.label"));
        let remind_menu = gio::Menu::new();
        marks.append_submenu(Some(&gettext("Remind Me")), &remind_menu);
        more.append_section(None, &marks);
        let views = gio::Menu::new();
        views.append(
            Some(&gettext("Open in New Window")),
            Some("win.open-window"),
        );
        views.append(Some(&gettext("Print…")), Some("win.print"));
        views.append(Some(&gettext("View Source")), Some("win.view-source"));
        views.append(Some(&gettext("Export…")), Some("win.export"));
        more.append_section(None, &views);
        let sender = gio::Menu::new();
        sender.append(Some(&gettext("Add Sender to VIPs")), Some("win.toggle-vip"));
        sender.append(Some(&gettext("Unsubscribe…")), Some("win.unsubscribe"));
        sender.append(
            Some(&gettext("Always Load Images…")),
            Some("win.always-load-images"),
        );
        sender.append(Some(&gettext("Block Sender…")), Some("win.block-sender"));
        let categories = gio::Menu::new();
        // The key is Gmail's own name for the category and stays as it is.
        for category in Category::ALL.iter().filter(|c| **c != Category::All) {
            let item = gio::MenuItem::new(Some(&category.name()), None);
            item.set_action_and_target_value(
                Some("win.categorize-sender"),
                Some(&category.key().to_variant()),
            );
            categories.append_item(&item);
        }
        sender.append_submenu(Some(&gettext("Categorize Sender")), &categories);
        more.append_section(None, &sender);
        buttons.more.set_menu_model(Some(&more));
        let sender_menu = sender.clone();
        let mark_menu = marks.clone();
        let filing_menu = filing.clone();
        let thread_menu = more.clone();
        let remind = remind_menu.clone();
        let header = adw::HeaderBar::builder()
            .title_widget(&gtk::Label::new(None))
            .build();
        let label_button = gtk::MenuButton::builder()
            .icon_name("penguin-mail-tag-symbolic")
            .tooltip_text(gettext("Labels (L)"))
            .build();
        name_with_shortcut(&label_button, &gettext("Labels (L)"));
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
        let menu_popover = gtk::PopoverMenu::from_model(None::<&gio::Menu>);
        menu_popover.set_has_arrow(false);
        menu_popover.set_halign(gtk::Align::Start);
        menu_popover.set_parent(&webview);
        let find = FindBar::new(&webview);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&find.widget);
        toolbar.set_content(Some(&stack));
        let page = adw::NavigationPage::builder()
            .title(gettext("Conversation"))
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
            translate,
            queued,
            find,
            find_closed: RefCell::new(Vec::new()),
            list_banner,
            sanitized: RefCell::new(HashMap::new()),
            sender_menu,
            mark_menu,
            filing_menu,
            thread_menu,
            remind,
            buttons,
            menu: menu_popover,
            menu_at: Cell::new((0, 0)),
            menu_popover: RefCell::new(None),
            filter: RefCell::new(None),
            open: RefCell::new(None),
            loading: Cell::new(0),
            scroll_to: RefCell::new(None),
            compact: Cell::new(false),
            detached: Cell::new(false),
        });

        view.set_buttons_shown(false);
        // A popover parented on a widget has to let go of it before the
        // widget goes, or GTK finalizes a widget that still has a parent.
        let weak = Rc::downgrade(&view);
        view.webview.connect_destroy(move |_| {
            let Some(view) = weak.upgrade() else { return };
            view.menu.unparent();
            if let Some(popover) = view.menu_popover.take() {
                popover.unparent();
            }
        });
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
            view.find.refresh();
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
        let weak = Rc::downgrade(&view);
        view.find.on_running(move |running| {
            let Some(view) = weak.upgrade() else { return };
            match running {
                true => *view.find_closed.borrow_mut() = view.open_every_message(),
                false => {
                    let closed = view.find_closed.take();
                    view.close_messages(&closed);
                }
            }
        });
        // Escape takes the bar down wherever the focus is in the
        // conversation, and before the window makes Escape its own. The
        // menu keys are caught here too, before WebKit's text field inside
        // the view can answer them with a menu of its own.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&view);
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(view) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let menu_key = key == gdk::Key::Menu
                || (key == gdk::Key::F10 && state.contains(gdk::ModifierType::SHIFT_MASK));
            if menu_key
                && view
                    .webview
                    .state_flags()
                    .contains(gtk::StateFlags::FOCUS_WITHIN)
            {
                run_script(&view.webview, MENU_KEY_SCRIPT);
                return glib::Propagation::Stop;
            }
            if key != gdk::Key::Escape || !view.find.is_open() {
                return glib::Propagation::Proceed;
            }
            view.find.close();
            glib::Propagation::Stop
        });
        view.page.add_controller(keys);
        view
    }

    /// Words the VIP menu item for whether the sender is one already.
    pub fn set_sender_vip(&self, vip: bool) {
        self.sender_menu.remove(0);
        self.sender_menu.insert(
            0,
            Some(&if vip {
                gettext("Remove Sender from VIPs")
            } else {
                gettext("Add Sender to VIPs")
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
            Some(&if muted {
                gettext("Unmute")
            } else {
                gettext("Mute")
            }),
            Some("win.mute"),
        );
    }

    /// Opens `model` as the menu for one message, at the point the reader
    /// asked from. The page measures in CSS pixels and the widget in its
    /// own, so the zoom level stands between the two.
    pub fn popup_message_menu(&self, model: &gio::Menu, x: f64, y: f64) {
        let zoom = self.webview.zoom_level();
        let at = ((x * zoom) as i32, (y * zoom) as i32);
        self.menu_at.set(at);
        self.menu.set_menu_model(Some(model));
        self.menu
            .set_pointing_to(Some(&gdk::Rectangle::new(at.0, at.1, 1, 1)));
        self.menu.popup();
    }

    /// Puts `popover` where the message menu was, for an item that leads
    /// to a popover of its own, such as the label list.
    pub fn popup_where_menu_was(&self, popover: &gtk::Popover) {
        if let Some(before) = self.menu_popover.replace(Some(popover.clone())) {
            before.unparent();
        }
        let (x, y) = self.menu_at.get();
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        popover.set_parent(&self.webview);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x, y, 1, 1)));
        popover.popup();
    }

    /// The screenshot hook's way in: asks for the menu of the message at
    /// `position`, counting from one, as a right click on it would. It
    /// goes through the page rather than around it, so a screenshot shows
    /// what a reader's own click shows.
    pub fn ask_message_menu(&self, position: usize) {
        run_script(
            &self.webview,
            &format!(
                "(function(){{var m=document.querySelectorAll('.message')[{}];if(!m)return;\
                 var b=m.getBoundingClientRect();\
                 m.dispatchEvent(new MouseEvent('contextmenu',{{bubbles:true,cancelable:true,\
                 clientX:b.left+140,clientY:b.top+14}}));}})()",
                position.saturating_sub(1)
            ),
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

    /// Whether this conversation is in a window of its own, which has no
    /// thread list and so no row selection to act on.
    pub fn detached(&self) -> bool {
        self.detached.get()
    }

    /// The window this conversation is in, which a dialog raised from it
    /// sits over.
    pub fn window(&self) -> Option<gtk::Window> {
        self.page.root().and_downcast::<gtk::Window>()
    }

    /// Closes the window a detached conversation lives in. The main
    /// window's conversation stays where it is.
    pub fn close_detached(&self) {
        if self.detached.get()
            && let Some(window) = self.window()
        {
            window.close();
        }
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
            Some(Folder::Trash) => ("edit-delete-symbolic", gettext("Delete Forever (Delete)")),
            _ => ("user-trash-symbolic", gettext("Move to Trash (Delete)")),
        };
        self.buttons.trash.set_icon_name(trash_icon);
        self.buttons.trash.set_tooltip_text(Some(&trash_tip));
        name_with_shortcut(&self.buttons.trash, &trash_tip);
        let (junk_icon, junk_tip) = match folder {
            Some(Folder::Junk) => (
                "mail-mark-notjunk-symbolic",
                gettext("Not Junk (Ctrl+Shift+J)"),
            ),
            _ => ("mail-mark-junk-symbolic", gettext("Junk (Ctrl+Shift+J)")),
        };
        self.buttons.junk.set_icon_name(junk_icon);
        self.buttons.junk.set_tooltip_text(Some(&junk_tip));
        name_with_shortcut(&self.buttons.junk, &junk_tip);
        let trash = match folder {
            Some(Folder::Trash) => gettext("Delete Forever"),
            _ => gettext("Move to Trash"),
        };
        self.many_trash.set_label(&trash);
        self.set_filing_word(1, &trash, "win.trash");
        let junk = match folder {
            Some(Folder::Junk) => gettext("Not Junk"),
            _ => gettext("Junk"),
        };
        self.many_junk.set_label(&junk);
        self.set_filing_word(2, &junk, "win.junk");
    }

    /// Says what the trash button and its menu item do in a mailbox where
    /// they do not move mail to the Trash. `tip` is the button's tooltip,
    /// with its key.
    pub fn set_trash_words(&self, word: &str, tip: &str) {
        self.many_trash.set_label(word);
        self.buttons.trash.set_tooltip_text(Some(tip));
        name_with_shortcut(&self.buttons.trash, tip);
        self.set_filing_word(1, word, "win.trash");
    }

    /// Words the item at `index` of the Archive, Trash and Junk section.
    fn set_filing_word(&self, index: i32, word: &str, action: &str) {
        self.filing_menu.remove(index);
        self.filing_menu.insert(index, Some(word), Some(action));
    }

    /// Everything that can be done to the conversations on screen, for a
    /// menu somewhere other than the header to show.
    pub fn thread_menu(&self) -> gio::MenuModel {
        self.thread_menu.clone().upcast()
    }

    pub fn showing_many(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some("many")
    }

    /// The page for a multiple selection. `any_unread`, `all_starred`, and
    /// `all_muted` decide what the read, star, and mute buttons do.
    pub fn show_many(
        &self,
        count: usize,
        threaded: bool,
        any_unread: bool,
        all_starred: bool,
        all_muted: bool,
    ) {
        self.many_read.set_label(&if any_unread {
            gettext("Mark as Read")
        } else {
            gettext("Mark as Unread")
        });
        self.many_star.set_label(&if all_starred {
            gettext("Unflag")
        } else {
            gettext("Flag")
        });
        self.many_mute.set_label(&if all_muted {
            gettext("Unmute")
        } else {
            gettext("Mute")
        });
        self.find.close();
        *self.open.borrow_mut() = None;
        let values = [("count", count.to_string())];
        let values: Vec<(&str, &str)> = values.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.many.set_title(&match threaded {
            true => fill_plural(
                "{count} Conversation Selected",
                "{count} Conversations Selected",
                count,
                &values,
            ),
            false => fill_plural(
                "{count} Message Selected",
                "{count} Messages Selected",
                count,
                &values,
            ),
        });
        self.stack.set_visible_child_name("many");
        self.banner.set_revealed(false);
        self.list_banner.set_revealed(false);
        self.show_invitation(None);
        self.seal.hide();
        self.translate.hide();
        self.queued.hide();
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

    /// Puts the find bar over the message and the cursor in it. Nothing
    /// happens when no message is on screen.
    pub fn open_find(&self) {
        if self.stack.visible_child_name().as_deref() == Some("thread") {
            self.find.open();
        }
    }

    /// Whether the focus is in this conversation: the message itself, or
    /// the find bar over it. Ctrl+F asks, so that the mailbox search
    /// keeps the key everywhere else.
    pub fn has_focus(&self) -> bool {
        let Some(window) = self.page.root().and_downcast::<gtk::Window>() else {
            return false;
        };
        GtkWindowExt::focus(&window)
            .is_some_and(|focus| focus.is_ancestor(self.page.upcast_ref::<gtk::Widget>()))
    }

    /// Clears the view and drops the thread on its way into it. Use this
    /// when the reader leaves the mail on screen for other mail, such as
    /// another mailbox or category: a thread clicked just before would
    /// otherwise land there once its store read answers.
    pub fn leave(&self) {
        self.stop_loading();
        self.clear();
    }

    pub fn clear(&self) {
        self.find.close();
        *self.open.borrow_mut() = None;
        self.stack.set_visible_child_name("empty");
        self.set_buttons_shown(false);
        self.banner.set_revealed(false);
        self.list_banner.set_revealed(false);
        self.show_invitation(None);
        self.seal.hide();
        self.translate.hide();
        self.queued.hide();
    }

    /// Puts an invitation above the message, or takes the card away when
    /// the message carries none.
    pub fn show_invitation(&self, showing: Option<Showing>) {
        match showing {
            Some(showing) => self.card.show(showing),
            None => self.card.hide(),
        }
    }

    /// What else the user has on while the event on the card runs. The
    /// card keeps it only while it still shows the invitation `uid` names.
    pub fn clashes(&self, uid: &str, busy: &[String]) {
        self.card.set_busy(uid, busy);
    }

    /// How the series behind the invitation `uid` runs, in words.
    pub fn series_known(&self, uid: &str, line: String) {
        self.card.set_series(uid, line);
    }

    /// Offers to add the account to GNOME Online Accounts on the card.
    pub fn offer_gnome(&self) {
        self.card.offer_gnome();
    }

    /// The answer the card shows for the invitation `uid`: the one that
    /// went, or the one from before an answer that did not.
    pub fn invitation_answered(&self, uid: &str, answer: Option<mailrs_domain::invitation::Answer>) {
        self.card.set_answer(uid, answer);
    }

    /// Where the last answer or proposal for the invitation `uid` went.
    pub fn invitation_went(&self, uid: &str, went: Option<String>) {
        self.card.set_went(uid, went);
    }

    /// Whether an answer on the card covers one occurrence or the series.
    pub fn invitation_scope(&self) -> mailrs_domain::invitation::Scope {
        self.card.scope()
    }

    /// Reads what the card shows. `None` means no invitation is on screen.
    pub fn with_invitation<R>(&self, f: impl FnOnce(&Showing) -> R) -> Option<R> {
        self.card.with_showing(f)
    }

    /// The message a translation applies to, with the prose the page
    /// draws for it: the newest open message whose body has arrived. The
    /// HTML is the cleaned copy, so the words come out of the markup the
    /// reader is actually looking at.
    pub fn open_prose(&self) -> Option<(String, Prose)> {
        let open = self.open.borrow();
        let open = open.as_ref()?;
        let meta = open.messages.iter().rev().find(|meta| {
            open.expanded.contains(&meta.id) && open.bodies.get(&meta.id).is_some_and(Result::is_ok)
        })?;
        let body = open.bodies.get(&meta.id)?.as_ref().ok()?;
        let clean = self.sanitized.borrow();
        let prose = match clean.get(&meta.id) {
            Some(cleaned) => Prose::read(Body::Html(&cleaned.html)),
            None => Prose::read(Body::Text(body.text.as_deref().unwrap_or(""))),
        };
        Some((meta.id.clone(), prose))
    }

    /// Whether `row` is what the view shows now.
    pub fn is_showing_row(&self, row: &mailrs_domain::ThreadSummary) -> bool {
        self.is_showing(&Target::from_row(row))
    }

    pub fn set_zoom(&self, zoom: f64) {
        self.webview.set_zoom_level(zoom);
    }

    /// Starts loading a thread into this view and returns its ticket. Two
    /// quick clicks can have their store reads answer out of order; only
    /// the thread holding the latest ticket may be shown.
    pub fn start_loading(&self) -> u64 {
        let ticket = self.loading.get() + 1;
        self.loading.set(ticket);
        ticket
    }

    /// Whether `ticket` is still the latest thread asked for.
    pub fn still_loading(&self, ticket: u64) -> bool {
        self.loading.get() == ticket
    }

    /// Drops the thread on its way into this view, so a store read that
    /// answers after the reader left its mailbox shows nothing.
    pub fn stop_loading(&self) {
        self.loading.set(self.loading.get() + 1);
    }

    /// Whether `target` is what the view shows now: the same account and
    /// thread, and the same one message when it shows one.
    pub fn is_showing(&self, target: &Target) -> bool {
        self.open
            .borrow()
            .as_ref()
            .is_some_and(|o| o.target() == *target)
    }

    /// Reads the open thread. `None` means no conversation is on screen.
    pub fn read<R>(&self, f: impl FnOnce(&OpenThread) -> R) -> Option<R> {
        self.open.borrow().as_ref().map(f)
    }

    /// Reads the open thread for something it may not hold, such as one
    /// message's body. `None` covers both: no conversation on screen, and
    /// nothing there to read.
    pub fn find<R>(&self, f: impl FnOnce(&OpenThread) -> Option<R>) -> Option<R> {
        self.open.borrow().as_ref().and_then(f)
    }

    /// The one way the thread changes. Each named change below goes
    /// through here and then redraws whatever its own change touched, so
    /// no caller has to know which of the fields the page is drawn from.
    fn change<R>(&self, f: impl FnOnce(&mut OpenThread) -> R) -> Option<R> {
        self.open.borrow_mut().as_mut().map(f)
    }

    /// The thread's messages as the store now has them. One that arrived
    /// unread opens, since the reader has not seen it. Gives back the ids
    /// whose bodies are still missing. Nothing is redrawn: those bodies
    /// are what the caller fetches next, and they bring a redraw with them.
    pub fn messages_arrived(&self, fresh: &[MessageMeta]) -> Vec<String> {
        self.change(|open| open.take_messages(fresh))
            .unwrap_or_default()
    }

    /// Replaces the messages after the store changed under the thread, and
    /// answers whether the ids differ from what was on screen. Nothing is
    /// redrawn: that answer is what decides between a redraw and a fetch.
    pub fn replace_messages(&self, fresh: Vec<MessageMeta>) -> bool {
        self.change(|open| open.replace_messages(fresh))
            .unwrap_or(false)
    }

    /// Bodies and the inline images that go in them, as they come back
    /// from Gmail, and the redraw that puts them on screen.
    pub fn bodies_arrived(
        &self,
        bodies: Vec<(String, Result<MessageBody, String>)>,
        images: HashMap<String, HashMap<String, String>>,
    ) {
        self.change(|open| open.take_bodies(bodies, images));
        self.render(false);
    }

    /// The pictures for the attachment rows, and the redraw that shows
    /// them.
    pub fn thumbnails_arrived(&self, found: HashMap<String, String>) {
        self.change(|open| open.thumbnails.extend(found));
        self.render(false);
    }

    /// The senders' photos, and the redraw that puts them in the message
    /// headers.
    pub fn set_photos(&self, photos: HashMap<String, String>) {
        self.change(|open| open.photos = photos);
        self.render(false);
    }

    /// What the outbox now says about the queued message on screen. Only
    /// the card changes; the message is the one the writer queued.
    pub fn unsent_changed(&self, unsent: Unsent) {
        self.change(|open| open.take_unsent(unsent));
        self.render_buttons();
    }

    /// The colour this thread is flagged in. The page says nothing about
    /// it, so only the header buttons are drawn again.
    pub fn set_flag_color(&self, color: Option<FlagColor>) {
        self.change(|open| open.flag_color = color);
        self.render_buttons();
    }

    /// Lets this thread load remote images, and draws it again without the
    /// filter that was blocking them.
    pub fn allow_images(&self) {
        self.change(|open| open.images_allowed = true);
        self.render(false);
    }

    /// Notes that the list has been unsubscribed from, which takes its
    /// banner down. The message itself does not change.
    pub fn mark_unsubscribed(&self) {
        self.change(|open| open.unsubscribed = true);
        self.render_buttons();
    }

    /// The thread's protected messages, claimed for one engine run. See
    /// [`OpenThread::take_protected`] for the once-per-message rule.
    pub fn take_protected(&self, installed: Installed) -> Vec<Claimed> {
        self.change(|open| open.take_protected(installed))
            .unwrap_or_default()
    }

    /// What the engine made of that message: the mark for the card, and,
    /// when it opened one, the body and the files that were inside. Those
    /// go no further than this window, since Gmail holds the ciphertext
    /// and nothing else. The message on screen may now be the opened one,
    /// so the thread is drawn again. Returns whether the engine opened a
    /// body, since whatever was read from the ciphertext, such as an
    /// invitation or the language, has to be read again from it.
    pub fn engine_answered(&self, message_id: String, read: protection::Read) -> bool {
        let opened = self
            .change(|open| open.take_engine_answer(message_id, read))
            .unwrap_or(false);
        self.render(false);
        opened
    }

    /// One message's translation, the card that says where it came from,
    /// and the redraw that puts the translated words in the page.
    pub fn translated(&self, message_id: String, translation: Translation) {
        let (from, cut) = (translation.from, translation.cut);
        let kept = self.change(|open| {
            open.translations.insert(message_id, translation);
        });
        if kept.is_none() {
            return;
        }
        self.translate.done(from, cut, true);
        self.render(false);
    }

    /// Turns the message over: the translation, or what arrived, whichever
    /// is not on screen. Both are kept, so this costs no second request.
    /// `false` when the message has no translation to turn.
    pub fn turn_translation(&self, message_id: &str) -> bool {
        let turned = self
            .change(|open| open.turn_translation(message_id))
            .flatten();
        let Some((from, cut, shown)) = turned else {
            return false;
        };
        self.translate.done(from, cut, shown);
        self.render(false);
        true
    }

    /// Shows a thread. `scroll` jumps to the first expanded message.
    pub fn show(&self, thread: OpenThread, scroll: bool) {
        // Before the thread changes, so the old search stops colouring
        // the new message and the old messages close again.
        self.find.close();
        // The card belongs to the thread that is leaving.
        self.translate.hide();
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
            let mark = body_mark(html, images);
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
            .map(|meta| {
                // A message showing its translation draws the translated
                // body and the translated HTML. What arrived stays where
                // it was, for the way back.
                let showing = open
                    .translations
                    .get(&meta.id)
                    .filter(|translation| translation.shown);
                MessageView {
                    meta,
                    body: match (showing, open.bodies.get(&meta.id)) {
                        (Some(translation), _) => BodyState::Loaded(&translation.body),
                        (None, None) => BodyState::Loading,
                        (None, Some(Ok(body))) => BodyState::Loaded(body),
                        (None, Some(Err(reason))) => BodyState::Failed(reason),
                    },
                    expanded: open.expanded.contains(&meta.id),
                    inline_images: open.inline_images.get(&meta.id).unwrap_or(&empty),
                    thumbnails: &open.thumbnails,
                    sanitized: match showing {
                        Some(translation) => translation.clean.as_deref(),
                        None => clean.get(&meta.id).map(|body| body.html.as_str()),
                    },
                }
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
        match open.card() {
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
        custom.append(Some(&gettext("Choose a Time…")), Some("win.remind-custom"));
        self.remind.append_section(None, &custom);
    }

    fn update_buttons(&self, open: &OpenThread) {
        // A queued message is not in Gmail yet, so the mail buttons have
        // nothing to act on. The card above it carries what does.
        if let Some(unsent) = &open.queued {
            self.set_buttons_shown(false);
            self.list_banner.set_revealed(false);
            self.queued.show(unsent);
            return;
        }
        self.queued.hide();
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
        let said = match starred {
            true => gettext("Unflag (Ctrl+Shift+L)"),
            false => gettext("Flag (Ctrl+Shift+L)"),
        };
        star.set_tooltip_text(Some(&said));
        name_with_shortcut(star, &said);
        self.set_muted(open.muted());
        let unread = open.unread();
        self.buttons.read.set_icon_name(if unread {
            "mail-read-symbolic"
        } else {
            "mail-unread-symbolic"
        });
        let said = match unread {
            true => gettext("Mark as Read (U)"),
            false => gettext("Mark as Unread (U)"),
        };
        self.buttons.read.set_tooltip_text(Some(&said));
        name_with_shortcut(&self.buttons.read, &said);
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
        } else if let Some(rest) = uri.strip_prefix("mailrs:menu/") {
            let mut parts = rest.split('/');
            if let (Some(id), Some(x), Some(y)) = (parts.next(), parts.next(), parts.next())
                && id == script_safe(id)
                && let (Ok(x), Ok(y)) = (x.parse(), y.parse())
            {
                actions(Action::MessageMenu {
                    message_id: id.to_string(),
                    x,
                    y,
                });
            }
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
        let expanded = self.change(|open| {
            if !open.expanded.remove(id) {
                open.expanded.insert(id.to_string());
            }
            open.expanded.contains(id)
        });
        if let Some(expanded) = expanded {
            self.show_message(id, expanded);
        }
    }

    /// Opens every message of the thread and answers the ones that were
    /// closed, so the find bar can close them again. The stylesheet hides
    /// a closed message's body, and WebKit finds nothing in it.
    fn open_every_message(&self) -> Vec<String> {
        let closed = self
            .change(|open| {
                let closed: Vec<String> = open
                    .messages
                    .iter()
                    .map(|m| m.id.clone())
                    .filter(|id| !open.expanded.contains(id))
                    .collect();
                open.expanded.extend(closed.iter().cloned());
                closed
            })
            .unwrap_or_default();
        for id in &closed {
            self.show_message(id, true);
        }
        closed
    }

    /// Closes the messages the find bar opened.
    fn close_messages(&self, ids: &[String]) {
        self.change(|open| {
            for id in ids {
                open.expanded.remove(id);
            }
        });
        for id in ids {
            self.show_message(id, false);
        }
    }

    /// Opens or closes one message in the page itself. Redrawing would do
    /// it too, and would throw away the find highlight and the place the
    /// reader had scrolled to.
    /// Opens or closes one message. The page's own stylesheet grows and
    /// shrinks the fold: its row goes from no height to the content's,
    /// which needs no measuring here and follows a body that grows later,
    /// as a picture loading does. A second click turns the movement
    /// around from wherever it had reached.
    fn show_message(&self, id: &str, expanded: bool) {
        let id = script_safe(id);
        let (add, remove) = match expanded {
            true => ("expanded", "collapsed"),
            false => ("collapsed", "expanded"),
        };
        run_script(
            &self.webview,
            &format!(
                "(function(){{var m=document.getElementById('m-{id}');\
                   if(m){{m.classList.add('{add}');m.classList.remove('{remove}');}}}})()"
            ),
        );
    }
}

/// The flag button's menu: seven colours in a row, then Clear Flag.
fn flag_colors() -> gtk::Popover {
    let row = gtk::Box::builder().spacing(2).build();
    for color in FlagColor::ALL {
        let tip = fill(
            &gettext("{color} (Ctrl+Alt+{number})"),
            &[
                ("color", &color.name()),
                ("number", &(color_index(color) + 1).to_string()),
            ],
        );
        let button = gtk::Button::builder()
            .icon_name("penguin-mail-flag-symbolic")
            .tooltip_text(&tip)
            .action_name("win.flag-color")
            .action_target(&color.as_str().to_variant())
            .css_classes(["flat", "flag-swatch", &format!("flag-{}", color.as_str())])
            .build();
        name_with_shortcut(&button, &tip);
        row.append(&button);
    }
    let clear = gtk::Button::builder()
        .label(gettext("Clear Flag"))
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

/// One number standing for the HTML and the inline images a cleaned body
/// was made from, so the cleaned copy is thrown away as soon as either
/// changes. It reads the whole body rather than its length, because two
/// bodies of the same length are still two bodies: opening an encrypted
/// message puts a different body under the same message id, and the
/// reader would otherwise go on looking at the cleaned ciphertext.
fn body_mark(html: &str, images: &HashMap<String, String>) -> u64 {
    let mut whole = DefaultHasher::new();
    html.hash(&mut whole);
    // A HashMap hands its entries back in whatever order it likes, so each
    // one is hashed on its own and the results mixed with xor, which
    // answers the same whichever order they come in.
    let mixed = images.iter().fold(0, |mixed, (cid, uri)| {
        let mut each = DefaultHasher::new();
        cid.hash(&mut each);
        uri.hash(&mut each);
        mixed ^ each.finish()
    });
    mixed.hash(&mut whole);
    whole.finish()
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

#[cfg(test)]
mod tests {
    use super::body_mark;
    use std::collections::HashMap;

    fn images(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(cid, uri)| (cid.to_string(), uri.to_string()))
            .collect()
    }

    #[test]
    fn a_body_that_did_not_change_keeps_its_cleaned_copy() {
        let pictures = images(&[("cid1", "data:image/png;base64,AAAA")]);
        assert_eq!(
            body_mark("<p>Hello</p>", &pictures),
            body_mark("<p>Hello</p>", &pictures)
        );
    }

    #[test]
    fn two_bodies_of_the_same_length_are_two_bodies() {
        let pictures = images(&[("cid1", "data:image/png;base64,AAAA")]);
        assert_ne!(
            body_mark("<p>Hello</p>", &pictures),
            body_mark("<p>Howdy</p>", &pictures)
        );
    }

    #[test]
    fn an_image_that_changed_is_a_new_body() {
        assert_ne!(
            body_mark(
                "<p>Hello</p>",
                &images(&[("cid1", "data:image/png;base64,AAAA")])
            ),
            body_mark(
                "<p>Hello</p>",
                &images(&[("cid1", "data:image/png;base64,BBBB")])
            )
        );
        assert_ne!(
            body_mark(
                "<p>Hello</p>",
                &images(&[("cid1", "data:image/png;base64,AAAA")])
            ),
            body_mark(
                "<p>Hello</p>",
                &images(&[("cid2", "data:image/png;base64,AAAA")])
            )
        );
        assert_ne!(
            body_mark("<p>Hello</p>", &images(&[])),
            body_mark(
                "<p>Hello</p>",
                &images(&[("cid1", "data:image/png;base64,AAAA")])
            )
        );
    }

    #[test]
    fn the_order_the_images_arrived_in_says_nothing() {
        let one = images(&[("cid1", "first"), ("cid2", "second")]);
        let other = images(&[("cid2", "second"), ("cid1", "first")]);
        assert_eq!(
            body_mark("<p>Hello</p>", &one),
            body_mark("<p>Hello</p>", &other)
        );
    }
}
