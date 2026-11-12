//! Manage Folders: every folder on an IMAP account's server, each with a
//! check box for whether the person subscribes to it. The sidebar shows
//! the subscribed ones alone, as Thunderbird does.
//!
//! An account can hold hundreds of folders, so the list is one
//! `gtk::ListView` whose rows are made as they scroll into view, with no
//! menu of their own: per-row menus were what made the sidebar heavy.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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
    /// Other folders sit under it, so it can open and close. Never in a
    /// search, whose rows are flat.
    pub children: bool,
    /// Its children show.
    pub expanded: bool,
}

/// The paths above `name`, nearest last: `a` and `a/b` for `a/b/c`.
fn ancestors(name: &str) -> impl Iterator<Item = &str> {
    name.match_indices('/').map(move |(at, _)| &name[..at])
}

/// The rows for `folders`: the tree, Inbox first and then by name, each
/// folder showing while every folder above it is in `expanded`; or with
/// a `query`, every folder whose path holds it, flat.
pub fn lines(folders: &[FolderSubscription], query: &str, expanded: &HashSet<String>) -> Vec<Line> {
    let names: HashSet<&str> = folders.iter().map(|f| f.name.as_str()).collect();
    let parents: HashSet<&str> = folders.iter().flat_map(|f| ancestors(&f.name)).collect();
    let mut sorted: Vec<&FolderSubscription> = folders.iter().collect();
    // By each part of the path, so a subtree stays together: as one
    // string, `a-b` would sort between `a` and `a/b`.
    sorted.sort_by_cached_key(|f| {
        let parts: Vec<String> = f.name.split('/').map(str::to_lowercase).collect();
        (!f.id.eq_ignore_ascii_case("INBOX"), parts)
    });
    let query = query.trim().to_lowercase();
    sorted
        .into_iter()
        .filter(|f| match query.is_empty() {
            true => ancestors(&f.name)
                .filter(|above| names.contains(above))
                .all(|above| expanded.contains(above)),
            false => f.name.to_lowercase().contains(&query),
        })
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
            let children = query.is_empty() && parents.contains(f.name.as_str());
            Line {
                id: f.id.clone(),
                path: f.name.clone(),
                label,
                depth,
                check,
                children,
                expanded: children && expanded.contains(&f.name),
            }
        })
        .collect()
}

/// The folders open when the dialog starts: every one above a folder the
/// person subscribes to, so the list opens on what they chose.
pub fn opened_for(folders: &[FolderSubscription]) -> HashSet<String> {
    folders
        .iter()
        .filter(|f| f.subscribed && !f.always_shown)
        .flat_map(|f| ancestors(&f.name).map(str::to_string))
        .collect()
}

/// Turns the subscription to `id` on or off in `folders`, as the server
/// now has it.
pub fn set(folders: &mut [FolderSubscription], id: &str, on: bool) {
    if let Some(folder) = folders.iter_mut().find(|f| f.id == id) {
        folder.subscribed = on;
    }
}

/// Subscription changes on their way to the server, one folder at a
/// time. The client spreads requests over several connections, so two
/// changes to one folder sent at once could arrive in either order: tick
/// then untick could leave the server subscribed under an unticked box.
/// Each folder has at most one request out; a change made meanwhile is
/// kept, and goes once that request ends if it still differs from what
/// the server has.
#[derive(Default)]
pub struct Sends {
    /// Each folder with a request out, and the newest change made since.
    out: HashMap<String, Option<bool>>,
}

impl Sends {
    /// The person set `id` to `on`: the value to send now, or `None`
    /// while a request for it is out, which then keeps `on` for later.
    pub fn want(&mut self, id: &str, on: bool) -> Option<bool> {
        match self.out.get_mut(id) {
            Some(next) => {
                *next = Some(on);
                None
            }
            None => {
                self.out.insert(id.to_string(), None);
                Some(on)
            }
        }
    }

    /// The request for `id` ended and the server now has `server`: the
    /// value to send next, or `None` when nothing differs and the folder
    /// is settled.
    pub fn done(&mut self, id: &str, server: bool) -> Option<bool> {
        match self.out.get_mut(id).and_then(Option::take) {
            Some(next) if next != server => Some(next),
            _ => {
                self.out.remove(id);
                None
            }
        }
    }
}

/// How far each level of the tree is indented, in pixels.
const INDENT: i32 = 18;

struct Dialog {
    core: Rc<Core>,
    account: Account,
    folders: RefCell<Vec<FolderSubscription>>,
    /// The paths whose children show.
    expanded: RefCell<HashSet<String>>,
    store: gio::ListStore,
    search: gtk::SearchEntry,
    stack: gtk::Stack,
    toasts: adw::ToastOverlay,
    sends: RefCell<Sends>,
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
        expanded: Default::default(),
        store,
        search: search.clone(),
        stack,
        toasts,
        sends: Default::default(),
    });

    let weak = Rc::downgrade(&this);
    factory.connect_setup(move |_, item| {
        let item = item
            .downcast_ref::<gtk::ListItem>()
            .expect("list items are ListItems");
        item.set_activatable(false);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let arrow = gtk::Button::builder()
            .css_classes(["flat", "circular"])
            .valign(gtk::Align::Center)
            .build();
        let check = gtk::CheckButton::new();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        row.append(&arrow);
        row.append(&check);
        row.append(&label);
        item.set_child(Some(&row));
        {
            let (weak, item) = (weak.clone(), item.downgrade());
            arrow.connect_clicked(move |_| {
                let (Some(this), Some(item)) = (weak.upgrade(), item.upgrade()) else {
                    return;
                };
                if let Some(line) = line_of(&item).filter(|l| l.children) {
                    this.flip(&line.path, item.position());
                }
            });
        }
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
        let arrow = row
            .first_child()
            .and_downcast::<gtk::Button>()
            .expect("the row starts with its arrow");
        let check = arrow
            .next_sibling()
            .and_downcast::<gtk::CheckButton>()
            .expect("the arrow is followed by the check box");
        let label = row
            .last_child()
            .and_downcast::<gtk::Label>()
            .expect("the row ends with its label");
        row.set_margin_start(line.depth as i32 * INDENT);
        label.set_label(&line.label);
        // Grayed, as Thunderbird draws a folder that holds no mail. Rows
        // are reused as the list scrolls, so the class comes off too.
        match line.check {
            Check::None => label.add_css_class("dim-label"),
            _ => label.remove_css_class("dim-label"),
        }
        // A folder with nothing under it keeps the arrow's room, so the
        // names line up, but there is nothing to open.
        arrow.set_opacity(if line.children { 1.0 } else { 0.0 });
        arrow.set_can_focus(line.children);
        arrow.set_can_target(line.children);
        arrow.set_icon_name(if line.expanded {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        });
        if line.children {
            let said = match line.expanded {
                true => gettext("Hide the folders in {folder}"),
                false => gettext("Show the folders in {folder}"),
            };
            crate::ui::name(&arrow, &fill(&said, &[("folder", &line.path)]));
            arrow.update_state(&[gtk::accessible::State::Expanded(Some(line.expanded))]);
        }
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
                    *this.expanded.borrow_mut() = opened_for(&folders);
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
        let lines = self.lines();
        let items: Vec<glib::BoxedAnyObject> =
            lines.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.store.splice(0, self.store.n_items(), &items);
        self.stack
            .set_visible_child_name(match self.store.n_items() {
                0 => "empty",
                _ => "list",
            });
    }

    fn lines(&self) -> Vec<Line> {
        lines(
            &self.folders.borrow(),
            &self.search.text(),
            &self.expanded.borrow(),
        )
    }

    /// Opens or closes the folder at `path`, on the row at `position`.
    /// Only the rows under it change, so the rest keep their place and the
    /// focus stays on the arrow.
    fn flip(&self, path: &str, position: u32) {
        {
            let mut expanded = self.expanded.borrow_mut();
            if !expanded.remove(path) {
                expanded.insert(path.to_string());
            }
        }
        let depth_at = |position: u32| {
            self.store
                .item(position)
                .and_downcast::<glib::BoxedAnyObject>()
                .map(|object| object.borrow::<Line>().depth)
        };
        let Some(depth) = depth_at(position) else {
            return self.show();
        };
        let mut shown_under = 0;
        while depth_at(position + 1 + shown_under).is_some_and(|d| d > depth) {
            shown_under += 1;
        }
        let fresh = self.lines();
        let Some(at) = fresh.iter().position(|l| l.path == path) else {
            return self.show();
        };
        let under: Vec<glib::BoxedAnyObject> = fresh[at + 1..]
            .iter()
            .take_while(|l| l.depth > depth)
            .cloned()
            .map(glib::BoxedAnyObject::new)
            .collect();
        let row = glib::BoxedAnyObject::new(fresh[at].clone());
        self.store
            .splice(position, 1 + shown_under, &[&[row], &under[..]].concat());
    }

    /// Sends the change to the server. The box shows it at once; a
    /// refusal puts it back and says why.
    fn toggle(self: &Rc<Self>, id: String, on: bool) {
        set(&mut self.folders.borrow_mut(), &id, on);
        self.refresh_line(&id);
        let now = self.sends.borrow_mut().want(&id, on);
        if let Some(on) = now {
            self.send(id, on);
        }
    }

    /// Sends one change, then the next one made meanwhile, if any; see
    /// [`Sends`]. Once the folder settles, its box shows what the server
    /// has, which after a refusal is the old value.
    fn send(self: &Rc<Self>, id: String, on: bool) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let done = match this.core.account(this.account.id) {
                Some(sync) => {
                    let asked = id.clone();
                    this.core
                        .call_for(this.account.id, async move {
                            sync.set_folder_subscribed(&asked, on).await
                        })
                        .await
                }
                None => Err(anyhow::anyhow!(gettext("This account is not syncing yet"))),
            };
            let server = match done {
                Ok(()) => on,
                Err(err) => {
                    let said = fill(
                        &gettext("Could not change the subscription: {reason}"),
                        &[("reason", &err.to_string())],
                    );
                    this.toasts.add_toast(crate::ui::toast(&said));
                    !on
                }
            };
            let next = this.sends.borrow_mut().done(&id, server);
            match next {
                Some(next) => this.send(id, next),
                None => {
                    set(&mut this.folders.borrow_mut(), &id, server);
                    this.refresh_line(&id);
                }
            }
        });
    }

    /// Puts the folder's line in the list again, so its row binds anew.
    fn refresh_line(&self, id: &str) {
        let fresh = self.lines();
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

        let open = opened_for(&folders);
        let lines = lines(&folders, "", &open);

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

        let lines = lines(&folders, "  archive ", &HashSet::new());

        let shown: Vec<_> = lines.iter().map(|l| (l.label.as_str(), l.depth)).collect();
        assert_eq!(shown, [("Archive/2019", 0), ("Projects/Archive", 0)]);
    }

    #[test]
    fn a_closed_folder_hides_its_subtree_and_a_subtree_stays_together() {
        let mut shared = folder("shared", false);
        shared.parent_only = true;
        let folders = [
            shared,
            folder("shared/lists", false),
            folder("shared/lists/dev", false),
            folder("shared/lists-old", false),
            folder("INBOX", true),
        ];

        let closed = lines(&folders, "", &HashSet::new());
        let shown: Vec<_> = closed
            .iter()
            .map(|l| (l.path.as_str(), l.children, l.expanded))
            .collect();
        assert_eq!(shown, [("INBOX", false, false), ("shared", true, false)]);

        let open = HashSet::from(["shared".to_string(), "shared/lists".to_string()]);
        let paths: Vec<_> = lines(&folders, "", &open)
            .into_iter()
            .map(|l| l.path)
            .collect();
        assert_eq!(
            paths,
            [
                "INBOX",
                "shared",
                "shared/lists",
                "shared/lists/dev",
                "shared/lists-old"
            ]
        );
    }

    #[test]
    fn the_dialog_opens_on_the_folders_subscribed_to() {
        let folders = [
            folder("a/b/c", true),
            folder("a/b", false),
            folder("a", false),
            folder("x/y", false),
        ];
        assert_eq!(
            opened_for(&folders),
            HashSet::from(["a".to_string(), "a/b".to_string()])
        );
    }

    /// Tick, then untick while the first request is still out: the
    /// untick waits for it, then goes, so the server ends unsubscribed.
    #[test]
    fn a_change_made_while_one_is_out_goes_after_it() {
        let mut sends = Sends::default();
        assert_eq!(sends.want("Old", true), Some(true));
        assert_eq!(sends.want("Old", false), None, "the first is still out");
        assert_eq!(sends.done("Old", true), Some(false));
        assert_eq!(sends.done("Old", false), None);
        assert_eq!(sends.want("Old", true), Some(true), "settled, so it sends");
    }

    #[test]
    fn changes_that_come_back_to_what_was_sent_send_nothing_more() {
        let mut sends = Sends::default();
        assert_eq!(sends.want("Old", true), Some(true));
        assert_eq!(sends.want("Old", false), None);
        assert_eq!(sends.want("Old", true), None);
        assert_eq!(sends.done("Old", true), None);
    }

    /// A refused change leaves the server as it was. A change made
    /// meanwhile that asks for that same value has nothing left to send.
    #[test]
    fn a_refused_change_settles_on_what_the_server_has() {
        let mut sends = Sends::default();
        assert_eq!(sends.want("Old", true), Some(true));
        assert_eq!(sends.want("Old", false), None);
        assert_eq!(sends.done("Old", false), None);
        assert_eq!(sends.want("Old", true), Some(true));
        assert_eq!(sends.want("Old", true), None);
        assert_eq!(
            sends.done("Old", false),
            Some(true),
            "the newest wish differs"
        );
    }

    #[test]
    fn folders_do_not_wait_for_each_other() {
        let mut sends = Sends::default();
        assert_eq!(sends.want("Old", true), Some(true));
        assert_eq!(sends.want("New", true), Some(true));
    }

    #[test]
    fn a_change_lands_on_its_folder_alone() {
        let mut folders = [folder("Old", false), folder("New", false)];
        set(&mut folders, "Old", true);
        assert!(folders[0].subscribed && !folders[1].subscribed);
    }
}
