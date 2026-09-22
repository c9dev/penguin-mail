//! A conversation in a window of its own, opened by double-clicking a row,
//! and the message source viewer.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::{Folder, ThreadSummary};
use mailrs_sync::Mailbox;

use super::MainWindow;
use crate::compose::ReplyKind;
use crate::ui::conversation::{Action, ConversationView};
use mailrs_domain::Category;
use mailrs_domain::translate::{fill, gettext};

/// A window action, given the main window and the conversation's view.
type ViewAction = Box<dyn Fn(&Rc<MainWindow>, &Rc<ConversationView>)>;
/// Builds the conversation action a menu entry stands for.
type MakeAction = fn() -> Action;

impl MainWindow {
    /// Opens the conversation on screen in its own window.
    pub(super) fn open_current_in_window(self: &Rc<Self>) {
        let summary = self.conversation.read(|o| ThreadSummary {
            account_id: o.account_id,
            id: o.thread_id.clone(),
            message_id: o.only_message.clone(),
            subject: o.subject.clone(),
            ..ThreadSummary::default()
        });
        match summary {
            Some(summary) => self.open_in_window(summary),
            None => self.toast(&gettext("Open a conversation first")),
        }
    }

    pub(super) fn open_in_window(self: &Rc<Self>, summary: ThreadSummary) {
        let holder: Rc<RefCell<Weak<ConversationView>>> = Rc::new(RefCell::new(Weak::new()));
        let (win, slot) = (Rc::downgrade(self), Rc::clone(&holder));
        let view = ConversationView::new(move |action| {
            let (Some(win), Some(view)) = (win.upgrade(), slot.borrow().upgrade()) else {
                return;
            };
            win.act_from(&view, action);
        });
        *holder.borrow_mut() = Rc::downgrade(&view);
        view.set_detached();
        // The window keeps the mailbox it was opened from, so its buttons
        // and what they do stay put when the main window moves on.
        let mailbox = self.mailbox.borrow().clone();
        view.set_folder(mailbox.folder());
        self.detached
            .borrow_mut()
            .push((Rc::downgrade(&view), mailbox));
        if let Some(filter) = self.app.upgrade().and_then(|app| app.filter()) {
            view.set_filter(filter);
        }
        view.set_zoom(self.settings().text_size.zoom());
        let window = adw::Window::builder()
            .title(if summary.subject.is_empty() {
                gettext("Conversation")
            } else {
                summary.subject.clone()
            })
            .default_width(820)
            .default_height(760)
            .content(&view.page)
            .build();
        self.install_window_actions(&window, &view);
        // The view lives as long as its window.
        let keep = Rc::clone(&view);
        window.connect_destroy(move |_| {
            keep.stop_rendering();
        });
        window.present();
        self.load_into(view, summary);
    }

    /// Every conversation on screen: the main window's, and one for each
    /// window of its own. Windows that have closed drop out here.
    pub(super) fn views(&self) -> Vec<Rc<ConversationView>> {
        let mut views = vec![Rc::clone(&self.conversation)];
        self.detached
            .borrow_mut()
            .retain(|(held, _)| match held.upgrade() {
                Some(view) => {
                    views.push(view);
                    true
                }
                None => false,
            });
        views
    }

    /// Tells the main window's conversation which folder its mail is in,
    /// so the trash and junk buttons say what they do. A conversation in a
    /// window of its own keeps the folder it was opened from.
    pub(super) fn set_folder(self: &Rc<Self>, folder: Option<Folder>) {
        self.conversation.set_folder(folder);
    }

    /// The mailbox `view`'s conversation was opened from: the main
    /// window's for its own view, and the one each separate window was
    /// opened from for the rest.
    pub(super) fn mailbox_of(&self, view: &Rc<ConversationView>) -> Mailbox {
        self.detached
            .borrow()
            .iter()
            .find(|(held, _)| held.upgrade().is_some_and(|held| Rc::ptr_eq(&held, view)))
            .map_or_else(|| self.mailbox.borrow().clone(), |(_, mailbox)| mailbox.clone())
    }

    /// The `win.*` actions a separate window's menus and keys use.
    fn install_window_actions(self: &Rc<Self>, window: &adw::Window, view: &Rc<ConversationView>) {
        let group = gio::SimpleActionGroup::new();
        let add = |name: &str, run: ViewAction| {
            let action = gio::SimpleAction::new(name, None);
            let (win, view) = (Rc::downgrade(self), Rc::downgrade(view));
            action.connect_activate(move |_, _| {
                if let (Some(win), Some(view)) = (win.upgrade(), view.upgrade()) {
                    run(&win, &view);
                }
            });
            group.add_action(&action);
        };
        let entries: [(&str, MakeAction); 8] = [
            ("reply", || Action::Reply(ReplyKind::Reply)),
            ("reply-all", || Action::Reply(ReplyKind::ReplyAll)),
            ("forward", || Action::Reply(ReplyKind::Forward)),
            ("archive", || Action::Archive),
            ("trash", || Action::Trash),
            ("junk", || Action::Junk),
            ("toggle-star", || Action::ToggleStar),
            ("toggle-read", || Action::ToggleRead),
        ];
        for (name, make) in entries {
            add(name, Box::new(move |win, view| win.act_from(view, make())));
        }
        add("find", Box::new(|_, view| view.open_find()));
        add("print", Box::new(|_, view| view.print()));
        add("view-source", Box::new(|win, view| win.view_source(view)));
        add(
            "export",
            Box::new(|win, view| win.export_conversation(view)),
        );
        add(
            "unsubscribe",
            Box::new(|win, view| win.unsubscribe_from(Rc::clone(view))),
        );
        add(
            "block-sender",
            Box::new(|win, view| win.block_sender_from(Rc::clone(view))),
        );
        add(
            "always-load-images",
            Box::new(|win, view| win.always_load_images(&Rc::clone(view))),
        );
        let categorize = gio::SimpleAction::new("categorize-sender", Some(glib::VariantTy::STRING));
        let (win, target) = (Rc::downgrade(self), Rc::downgrade(view));
        categorize.connect_activate(move |_, parameter| {
            let category = parameter
                .and_then(|p| p.get::<String>())
                .and_then(|k| Category::from_key(&k));
            if let (Some(win), Some(view), Some(category)) =
                (win.upgrade(), target.upgrade(), category)
            {
                win.categorize_sender_from(view, category);
            }
        });
        group.add_action(&categorize);
        window.insert_action_group("win", Some(&group));

        let shortcuts = gtk::ShortcutController::new();
        for (trigger, action) in [
            ("<Control>r", "win.reply"),
            ("<Control><Shift>r", "win.reply-all"),
            ("<Control><Shift>f", "win.forward"),
            ("<Control><Alt>a", "win.archive"),
            ("Delete", "win.trash"),
            ("<Control><Shift>j", "win.junk"),
            ("<Control><Shift>l", "win.toggle-star"),
            ("<Control><Shift>u", "win.toggle-read"),
            ("<Control>f", "win.find"),
            ("<Control>p", "win.print"),
            ("<Control><Alt>u", "win.view-source"),
            ("<Control>w", "window.close"),
            ("Escape", "window.close"),
        ] {
            shortcuts.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(gtk::NamedAction::new(action)),
            ));
        }
        window.add_controller(shortcuts);
    }

    /// Shows the newest message in `view` as it arrived, headers and all.
    pub(super) fn view_source(self: &Rc<Self>, view: &ConversationView) {
        let found = view.find(|o| {
            let message = match &o.only_message {
                Some(id) => o.messages.iter().find(|m| &m.id == id),
                None => o.messages.last(),
            }?;
            Some((o.account_id, message.id.clone(), message.subject.clone()))
        });
        let Some((account_id, message_id, subject)) = found else {
            return;
        };
        let Some(sync) = self.core.account(account_id) else {
            return self.toast(&gettext("That account is not connected"));
        };
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            match this
                .core
                .call(async move { sync.raw_message(&message_id).await })
                .await
            {
                Ok(raw) => show_source(&this.window, &subject, raw),
                Err(err) => this.toast(&fill(
                    &gettext("Could not load the source: {reason}"),
                    &[("reason", &err.to_string())],
                )),
            }
        });
    }
}

fn show_source(parent: &adw::Window, subject: &str, raw: Vec<u8>) {
    let text = String::from_utf8_lossy(&raw).replace("\r\n", "\n");
    let view = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::Char)
        .top_margin(12)
        .bottom_margin(12)
        .left_margin(14)
        .right_margin(14)
        .build();
    view.buffer().set_text(&text);
    crate::ui::name(&view, &gettext("Message Source"));
    let save = gtk::Button::builder()
        .label(gettext("Save As…"))
        .tooltip_text(gettext("Save as an .eml file"))
        .build();
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new(&gettext("Message Source"), subject))
        .build();
    header.pack_start(&save);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(
        &gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .build(),
    ));
    let window = adw::Window::builder()
        .title(gettext("Message Source"))
        .default_width(760)
        .default_height(640)
        .transient_for(parent)
        .content(&toolbar)
        .build();
    let name: String = subject
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .to_string();
    let file_name = format!("{}.eml", if name.is_empty() { "message" } else { &name });
    let owner = window.clone();
    save.connect_clicked(move |_| {
        let dialog = gtk::FileDialog::builder()
            .title(gettext("Save Message"))
            .initial_name(&file_name)
            .build();
        let (owner, raw) = (owner.clone(), raw.clone());
        glib::spawn_future_local(async move {
            if let Ok(file) = dialog.save_future(Some(&owner)).await
                && let Some(path) = file.path()
                && let Err(err) = std::fs::write(&path, &raw)
            {
                tracing::warn!(error = %err, "could not save the message source");
            }
        });
    });
    let keys = gtk::ShortcutController::new();
    keys.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("Escape"),
        Some(gtk::NamedAction::new("window.close")),
    ));
    window.add_controller(keys);
    window.present();
}
