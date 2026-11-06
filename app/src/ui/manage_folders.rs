//! Manage Folders: every folder on an IMAP account's server, each with a
//! check box for whether the person subscribes to it. The sidebar shows
//! the subscribed ones alone, as Thunderbird does.
//!
//! An account can hold hundreds of folders, so the list is one
//! `gtk::ListView` whose rows are made as they scroll into view, with no
//! menu of their own: per-row menus were what made the sidebar heavy.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::Account;
use mailrs_domain::translate::{fill, gettext};
use mailrs_sync::services::imap::FolderSubscription;

use crate::core::Core;

/// What a row's check box can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// The person subscribes or not, and can change it.
    Toggle(bool),
    /// A role folder, which shows whatever its subscription.
    AlwaysShown,
    /// A parent that holds no mail, listed for the folders under it.
    None,
}

/// One row of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The server's name, which SUBSCRIBE takes.
    pub id: String,
    /// The full path, for a screen reader and for a search's results.
    pub path: String,
    /// What the row shows: the last part of the path in the tree, the
    /// whole path in a search, where rows lose their place in the tree.
    pub label: String,
    pub depth: usize,
    pub check: Check,
}

/// The rows for `folders`: the tree, Inbox first and then by name, or
/// with a `query`, the folders whose path holds it, flat.
pub fn lines(folders: &[FolderSubscription], query: &str) -> Vec<Line> {
    let mut sorted: Vec<&FolderSubscription> = folders.iter().collect();
    sorted.sort_by_key(|f| (!f.id.eq_ignore_ascii_case("INBOX"), f.name.to_lowercase()));
    let query = query.trim().to_lowercase();
    sorted
        .into_iter()
        .filter(|f| query.is_empty() || f.name.to_lowercase().contains(&query))
        .map(|f| {
            let (label, depth) = match query.is_empty() {
                true => (
                    f.name.rsplit('/').next().unwrap_or(&f.name).to_string(),
                    f.name.matches('/').count(),
                ),
                false => (f.name.clone(), 0),
            };
            let check = if f.parent_only {
                Check::None
            } else if f.always_shown {
                Check::AlwaysShown
            } else {
                Check::Toggle(f.subscribed)
            };
            Line {
                id: f.id.clone(),
                path: f.name.clone(),
                label,
                depth,
                check,
            }
        })
        .collect()
}

/// Turns the subscription to `id` on or off in `folders`, as the server
/// now has it.
pub fn set(folders: &mut [FolderSubscription], id: &str, on: bool) {
    if let Some(folder) = folders.iter_mut().find(|f| f.id == id) {
        folder.subscribed = on;
    }
}

/// How far each level of the tree is indented, in pixels.
const INDENT: i32 = 18;

struct Dialog {
    core: Rc<Core>,
    account: Account,
    folders: std::cell::RefCell<Vec<FolderSubscription>>,
    store: gio::ListStore,
    search: gtk::SearchEntry,
    stack: gtk::Stack,
    toasts: adw::ToastOverlay,
}

/// Opens Manage Folders for `account` over `parent`, and reads the
/// folders from the server.
pub fn present(core: &Rc<Core>, parent: &impl IsA<gtk::Widget>, account: Account) {
    let search = gtk::SearchEntry::builder()
        .placeholder_text(gettext("Search folders"))
        .hexpand(true)
        .build();
    crate::ui::name(&search, &gettext("Search folders"));
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let factory = gtk::SignalListItemFactory::new();
    let view = gtk::ListView::builder()
        .model(&gtk::NoSelection::new(Some(store.clone())))
        .factory(&factory)
        .css_classes(["navigation-sidebar"])
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&view)
        .build();
    let loading = adw::Spinner::new();
    let failed = adw::StatusPage::builder()
        .icon_name("dialog-warning-symbolic")
        .title(gettext("Could Not List the Folders"))
        .build();
    let empty = adw::StatusPage::builder()
        .icon_name("system-search-symbolic")
        .title(gettext("No Matching Folders"))
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&loading, Some("loading"));
    stack.add_named(&scroller, Some("list"));
    stack.add_named(&failed, Some("failed"));
    stack.add_named(&empty, Some("empty"));
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.set_margin_bottom(12);
    content.append(
        &gtk::Label::builder()
            .label(gettext(
                "The sidebar shows the folders you subscribe to. Inbox and the folders mail is filed into always show.",
            ))
            .wrap(true)
            .xalign(0.0)
            .css_classes(["dim-label"])
            .build(),
    );
    content.append(&search);
    content.append(&stack);
    let header = adw::HeaderBar::new();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&toolbar));
    let dialog = adw::Dialog::builder()
        .title(fill(
            &gettext("Folders of {account}"),
            &[("account", &account.email)],
        ))
        .content_width(420)
        .content_height(560)
        .child(&toasts)
        .build();
    let this = Rc::new(Dialog {
        core: Rc::clone(core),
        account,
        folders: Default::default(),
        store,
        search: search.clone(),
        stack,
        toasts,
    });

    let weak = Rc::downgrade(&this);
    factory.connect_setup(move |_, item| {
        let item = item
            .downcast_ref::<gtk::ListItem>()
            .expect("list items are ListItems");
        item.set_activatable(false);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let check = gtk::CheckButton::new();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        row.append(&check);
        row.append(&label);
        item.set_child(Some(&row));
        let (weak, item) = (weak.clone(), item.downgrade());
        check.connect_toggled(move |check| {
            let (Some(this), Some(item)) = (weak.upgrade(), item.upgrade()) else {
                return;
            };
            let Some(line) = line_of(&item) else { return };
            // Binding sets the box to the line's state; only a change by
            // the person differs from it.
            if line.check != Check::Toggle(!check.is_active()) {
                return;
            }
            this.toggle(line.id.clone(), check.is_active());
        });
    });
    factory.connect_bind(|_, item| {
        let item = item
            .downcast_ref::<gtk::ListItem>()
            .expect("list items are ListItems");
        let (Some(line), Some(row)) = (line_of(item), item.child()) else {
            return;
        };
        let row = row.downcast::<gtk::Box>().expect("the row is a box");
        let check = row
            .first_child()
            .and_downcast::<gtk::CheckButton>()
            .expect("the row starts with its check box");
        let label = row
            .last_child()
            .and_downcast::<gtk::Label>()
            .expect("the row ends with its label");
        row.set_margin_start(line.depth as i32 * INDENT);
        label.set_label(&line.label);
        let (active, sensitive, visible) = match line.check {
            Check::Toggle(on) => (on, true, true),
            Check::AlwaysShown => (true, false, true),
            Check::None => (false, false, false),
        };
        check.set_active(active);
        check.set_sensitive(sensitive);
        // A parent that holds no mail keeps the box's room, so the names
        // under it line up, but there is nothing to tick.
        check.set_opacity(if visible { 1.0 } else { 0.0 });
        check.set_can_focus(visible);
        let name = match line.check {
            Check::AlwaysShown => fill(
                &gettext("{folder}, always shown"),
                &[("folder", &line.path)],
            ),
            _ => line.path.clone(),
        };
        crate::ui::name(&check, &name);
    });

    let weak = Rc::downgrade(&this);
    search.connect_search_changed(move |_| {
        if let Some(this) = weak.upgrade() {
            this.show();
        }
    });
    // The dialog owns the state behind its boxes until it closes.
    let keep = std::cell::RefCell::new(Some(Rc::clone(&this)));
    dialog.connect_closed(move |_| {
        keep.borrow_mut().take();
    });
    dialog.present(Some(parent));
    this.load(failed);
}

fn line_of(item: &gtk::ListItem) -> Option<Line> {
    let object = item.item().and_downcast::<glib::BoxedAnyObject>()?;
    let line = object.borrow::<Line>().clone();
    Some(line)
}

impl Dialog {
    fn load(self: &Rc<Self>, failed: adw::StatusPage) {
        self.stack.set_visible_child_name("loading");
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let id = this.account.id;
            let Some(sync) = this.core.account(id) else {
                failed.set_description(Some(&gettext("This account is not syncing yet")));
                return this.stack.set_visible_child_name("failed");
            };
            match this
                .core
                .call_for(id, async move { sync.folder_subscriptions().await })
                .await
            {
                Ok(folders) => {
                    *this.folders.borrow_mut() = folders;
                    this.show();
                }
                Err(err) => {
                    failed.set_description(Some(&err.to_string()));
                    this.stack.set_visible_child_name("failed");
                }
            }
        });
    }

    /// Fills the list from the folders and the search.
    fn show(&self) {
        let lines = lines(&self.folders.borrow(), &self.search.text());
        let items: Vec<glib::BoxedAnyObject> =
            lines.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.store.splice(0, self.store.n_items(), &items);
        self.stack
            .set_visible_child_name(match self.store.n_items() {
                0 => "empty",
                _ => "list",
            });
    }

    /// Sends the change to the server. The box shows it at once; a
    /// refusal puts it back and says why.
    fn toggle(self: &Rc<Self>, id: String, on: bool) {
        let Some(sync) = self.core.account(self.account.id) else {
            self.refresh_line(&id);
            return;
        };
        set(&mut self.folders.borrow_mut(), &id, on);
        self.refresh_line(&id);
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let asked = id.clone();
            let done = this
                .core
                .call_for(this.account.id, async move {
                    sync.set_folder_subscribed(&asked, on).await
                })
                .await;
            if let Err(err) = done {
                set(&mut this.folders.borrow_mut(), &id, !on);
                this.refresh_line(&id);
                let said = fill(
                    &gettext("Could not change the subscription: {reason}"),
                    &[("reason", &err.to_string())],
                );
                this.toasts.add_toast(crate::ui::toast(&said));
            }
        });
    }

    /// Puts the folder's line in the list again, so its row binds anew.
    fn refresh_line(&self, id: &str) {
        let fresh = lines(&self.folders.borrow(), &self.search.text());
        let Some(line) = fresh.into_iter().find(|l| l.id == id) else {
            return;
        };
        for position in 0..self.store.n_items() {
            let Some(object) = self
                .store
                .item(position)
                .and_downcast::<glib::BoxedAnyObject>()
            else {
                continue;
            };
            if object.borrow::<Line>().id == id {
                self.store
                    .splice(position, 1, &[glib::BoxedAnyObject::new(line)]);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str, subscribed: bool) -> FolderSubscription {
        FolderSubscription {
            id: name.into(),
            name: name.into(),
            subscribed,
            always_shown: false,
            parent_only: false,
        }
    }

    #[test]
    fn the_tree_puts_inbox_first_and_nests_by_path() {
        let mut sent = folder("Sent", false);
        sent.always_shown = true;
        let mut lists = folder("Lists", false);
        lists.parent_only = true;
        let folders = [
            folder("lists/zed", true),
            lists,
            folder("Lists/Rust", true),
            sent,
            folder("INBOX", true),
        ];

        let lines = lines(&folders, "");

        let shown: Vec<_> = lines
            .iter()
            .map(|l| (l.label.as_str(), l.depth, l.check))
            .collect();
        assert_eq!(
            shown,
            [
                ("INBOX", 0, Check::Toggle(true)),
                ("Lists", 0, Check::None),
                ("Rust", 1, Check::Toggle(true)),
                ("zed", 1, Check::Toggle(true)),
                ("Sent", 0, Check::AlwaysShown),
            ]
        );
    }

    #[test]
    fn a_search_lists_matching_folders_flat_by_their_path() {
        let folders = [
            folder("Archive/2019", false),
            folder("Projects/Archive", true),
            folder("Projects/Plans", true),
        ];

        let lines = lines(&folders, "  archive ");

        let shown: Vec<_> = lines.iter().map(|l| (l.label.as_str(), l.depth)).collect();
        assert_eq!(shown, [("Archive/2019", 0), ("Projects/Archive", 0)]);
    }

    #[test]
    fn a_change_lands_on_its_folder_alone() {
        let mut folders = [folder("Old", false), folder("New", false)];
        set(&mut folders, "Old", true);
        assert!(folders[0].subscribed && !folders[1].subscribed);
    }
}
