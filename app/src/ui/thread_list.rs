//! The middle pane: threads of the current mailbox, or search results.
//! One page of rows loads at a time; scrolling near the end asks for more.
//! Every row is held once, behind an `Rc`, and shared with the list model.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use mailrs_domain::{AccountId, ThreadSummary};

use super::sidebar::DRAG_MAIL;
use super::thread_row::{Avatar, ThreadRow};
use crate::diff::splice;
use mailrs_domain::translate::gettext;

/// What is selected in the list.
pub enum Picked {
    None,
    One(ThreadSummary),
    Many(Vec<ThreadSummary>),
}

type Key = (AccountId, String, Option<String>);

/// A row shared between the list model and the rows this pane keeps.
pub type Row = Rc<ThreadSummary>;

fn key(row: &ThreadSummary) -> Key {
    (row.account_id, row.id.clone(), row.message_id.clone())
}

pub struct ThreadList {
    pub page: adw::NavigationPage,
    pub sidebar_button: gtk::ToggleButton,
    pub search_button: gtk::ToggleButton,
    /// Shows or hides the assistant pane.
    pub assistant_button: gtk::ToggleButton,
    pub banner: adw::Banner,
    pub search_entry: gtk::SearchEntry,
    search_bar: gtk::SearchBar,
    title: adw::WindowTitle,
    stack: gtk::Stack,
    empty: adw::StatusPage,
    store: gio::ListStore,
    selection: gtk::MultiSelection,
    view: gtk::ListView,
    scroller: gtk::ScrolledWindow,
    rows: Rc<RefCell<Vec<Row>>>,
    /// The rows of the drag in progress.
    dragged: Rc<RefCell<Vec<ThreadSummary>>>,
    show_accounts: Rc<Cell<bool>>,
    /// Lower-case VIP addresses; their rows get a star.
    vips: Rc<RefCell<HashSet<String>>>,
    /// Contact photos by lower-case sender address, already decoded. Empty
    /// while contacts are off, and then rows show no face at all.
    photos: Rc<RefCell<HashMap<String, gdk::Texture>>>,
    muted: Cell<bool>,
}

impl ThreadList {
    pub fn new(
        on_select: impl Fn(Picked) + 'static,
        on_search: impl Fn(String) + 'static,
    ) -> Rc<ThreadList> {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::MultiSelection::new(Some(store.clone()));
        let show_accounts = Rc::new(Cell::new(true));
        let rows: Rc<RefCell<Vec<Row>>> = Rc::new(RefCell::new(Vec::new()));
        let dragged: Rc<RefCell<Vec<ThreadSummary>>> = Rc::new(RefCell::new(Vec::new()));
        let factory = gtk::SignalListItemFactory::new();
        let (all, drag_rows, picked) = (Rc::clone(&rows), Rc::clone(&dragged), selection.clone());
        factory.connect_setup(move |_, item| {
            let item = item
                .downcast_ref::<gtk::ListItem>()
                .expect("list items are ListItems");
            let row = ThreadRow::default();
            context_menu(&row, item, &picked);
            // Dragging a selected row takes the whole selection along.
            let source = gtk::DragSource::new();
            source.set_actions(gdk::DragAction::MOVE);
            let counted = Rc::clone(&drag_rows);
            let (list_item, all, taken_rows, picked) = (
                item.clone(),
                Rc::clone(&all),
                Rc::clone(&drag_rows),
                picked.clone(),
            );
            source.connect_prepare(move |_, _, _| {
                let position = list_item.position();
                let rows = all.borrow();
                let copy = |row: &Row| (**row).clone();
                let taken: Vec<ThreadSummary> = if picked.is_selected(position) {
                    (0..rows.len() as u32)
                        .filter(|p| picked.is_selected(*p))
                        .filter_map(|p| rows.get(p as usize).map(copy))
                        .collect()
                } else {
                    rows.get(position as usize).map(copy).into_iter().collect()
                };
                if taken.is_empty() {
                    return None;
                }
                *taken_rows.borrow_mut() = taken;
                Some(gdk::ContentProvider::for_value(&DRAG_MAIL.to_value()))
            });
            source.connect_drag_begin(move |source, drag| {
                let count = counted.borrow().len();
                let label = gtk::Label::builder()
                    .label(count.to_string())
                    .css_classes(["drag-badge"])
                    .build();
                let icon = gtk::DragIcon::for_drag(drag);
                icon.set_child(Some(&label));
                source.set_icon(None::<&gdk::Paintable>, 0, 0);
            });
            row.add_controller(source);
            item.set_child(Some(&row));
        });
        let shown = Rc::clone(&show_accounts);
        let vips: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));
        let starred_people = Rc::clone(&vips);
        let photos: Rc<RefCell<HashMap<String, gdk::Texture>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let faces = Rc::clone(&photos);
        factory.connect_bind(move |_, item| {
            let item = item
                .downcast_ref::<gtk::ListItem>()
                .expect("list items are ListItems");
            let (Some(row), Some(object)) = (
                item.child().and_downcast::<ThreadRow>(),
                item.item().and_downcast::<glib::BoxedAnyObject>(),
            ) else {
                return;
            };
            let thread = object.borrow::<Row>();
            let sender = thread.from_email.to_lowercase();
            let vip = starred_people.borrow().contains(&sender);
            let faces = faces.borrow();
            let avatar = if faces.is_empty() {
                Avatar::Hidden
            } else {
                match faces.get(&sender) {
                    Some(photo) => Avatar::Photo(photo),
                    None => Avatar::Initials,
                }
            };
            row.bind(&thread, shown.get(), vip, avatar);
        });
        let view = gtk::ListView::builder()
            .model(&selection)
            .factory(&factory)
            .css_classes(["navigation-sidebar", "thread-list"])
            .single_click_activate(false)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&view)
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name("penguin-mail-inbox-symbolic")
            .title(gettext("No Mail"))
            .build();
        empty.add_css_class("compact");
        let spinner = adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        stack.add_named(&scroller, Some("list"));
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&spinner, Some("loading"));

        let search_entry = gtk::SearchEntry::builder()
            // The two search terms are Gmail's own and stay in English.
            .placeholder_text(gettext("Search mail, e.g. from:ann has:attachment"))
            .hexpand(true)
            .build();
        let search_bar = gtk::SearchBar::builder()
            .child(
                &adw::Clamp::builder()
                    .maximum_size(520)
                    .child(&search_entry)
                    .build(),
            )
            .show_close_button(false)
            .build();
        search_bar.connect_entry(&search_entry);

        let title = adw::WindowTitle::new(&gettext("All Inboxes"), "");
        let sidebar_button = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text(gettext("Show Mailboxes"))
            .visible(false)
            .build();
        let compose_button = gtk::Button::builder()
            .icon_name("mail-message-new-symbolic")
            .tooltip_text(gettext("New Message (C)"))
            .action_name("win.compose")
            .build();
        let search_button = gtk::ToggleButton::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text(gettext("Search (/)"))
            .build();
        search_button
            .bind_property("active", &search_bar, "search-mode-enabled")
            .bidirectional()
            .sync_create()
            .build();
        let assistant_button = gtk::ToggleButton::builder()
            .icon_name("penguin-mail-sparkle-symbolic")
            .tooltip_text(gettext("Assistant (Ctrl+J)"))
            .build();
        // The tooltips carry the shortcut for the eye; the spoken names
        // leave it out, since a reader hears the whole toolbar in turn.
        super::name(&search_entry, &gettext("Search mail"));
        super::name(&sidebar_button, &gettext("Show Mailboxes"));
        super::name(&compose_button, &gettext("New Message"));
        super::name(&search_button, &gettext("Search"));
        super::name(&assistant_button, &gettext("Assistant"));
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_start(&sidebar_button);
        header.pack_end(&assistant_button);
        header.pack_end(&compose_button);
        header.pack_end(&search_button);

        let banner = adw::Banner::builder().revealed(false).build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&search_bar);
        toolbar.add_top_bar(&banner);
        toolbar.set_content(Some(&stack));
        let page = adw::NavigationPage::builder()
            .title(gettext("Mail"))
            .tag("list")
            .child(&toolbar)
            .build();

        let list = Rc::new(ThreadList {
            page,
            sidebar_button,
            search_button,
            assistant_button,
            banner,
            search_entry,
            search_bar,
            title,
            stack,
            empty,
            store,
            selection,
            view,
            scroller,
            rows,
            dragged,
            show_accounts,
            vips,
            photos,
            muted: Cell::new(false),
        });
        let weak = Rc::downgrade(&list);
        list.selection.connect_selection_changed(move |_, _, _| {
            let Some(list) = weak.upgrade() else { return };
            if !list.muted.get() {
                on_select(list.picked());
            }
        });
        list.search_entry.connect_activate(move |entry| {
            let query = entry.text().trim().to_string();
            if !query.is_empty() {
                on_search(query);
            }
        });
        list
    }

    pub fn set_title(&self, title: &str, subtitle: &str) {
        self.title.set_title(title);
        self.title.set_subtitle(subtitle);
        self.page.set_title(title);
    }

    pub fn set_show_accounts(&self, show: bool) {
        if self.show_accounts.replace(show) != show {
            // Rebind every row so account dots appear or disappear.
            self.rebind();
        }
    }

    /// Draws every row again, for example after account colours change.
    pub fn rebind(&self) {
        let rows = self.rows.borrow().clone();
        self.replace_all(&rows);
    }

    /// The contact photos rows may show, by lower-case address. An empty
    /// map takes the whole avatar column away, which is how the list looks
    /// while contacts are off.
    pub fn set_photos(&self, files: &HashMap<String, std::path::PathBuf>) {
        let mut photos = self.photos.borrow_mut();
        let before = photos.len();
        photos.retain(|email, _| files.contains_key(email));
        for (email, file) in files {
            if photos.contains_key(email) {
                continue;
            }
            match gdk::Texture::from_filename(file) {
                Ok(texture) => {
                    photos.insert(email.clone(), texture);
                }
                Err(err) => tracing::debug!(error = %err, "could not read a contact photo"),
            }
        }
        let changed = before != photos.len();
        drop(photos);
        if changed {
            self.rebind();
        }
    }

    /// Marks rows from these addresses as VIP mail.
    pub fn set_vips(&self, vips: HashSet<String>) {
        if *self.vips.borrow() != vips {
            *self.vips.borrow_mut() = vips;
            self.rebind();
        }
    }

    /// How many rows are loaded. The mailbox may hold more.
    pub fn loaded(&self) -> usize {
        self.rows.borrow().len()
    }

    /// Runs `more` when the view scrolls within a screenful of the end.
    pub fn connect_more(&self, more: impl Fn() + 'static) {
        self.scroller
            .vadjustment()
            .connect_value_changed(move |adjustment| {
                let seen = adjustment.value() + adjustment.page_size();
                if adjustment.upper() - seen < adjustment.page_size() {
                    more();
                }
            });
    }

    pub fn show_loading(&self) {
        self.stack.set_visible_child_name("loading");
    }

    /// Updates the list with one splice, keeping the selected rows selected
    /// when they are still present.
    pub fn set_rows(&self, rows: Vec<Row>, empty_title: &str, empty_icon: &str) {
        let keys: Vec<Key> = self.selected_rows().iter().map(key).collect();
        self.muted.set(true);
        let old = self.rows.replace(rows);
        {
            let new = self.rows.borrow();
            if let Some(change) = splice(&old, &new) {
                let added: Vec<glib::BoxedAnyObject> = new[change.added]
                    .iter()
                    .map(|row| glib::BoxedAnyObject::new(Rc::clone(row)))
                    .collect();
                self.store.splice(change.position, change.removed, &added);
            }
        }
        self.reselect(&keys);
        self.muted.set(false);
        self.empty.set_title(empty_title);
        self.empty.set_icon_name(Some(empty_icon));
        self.stack
            .set_visible_child_name(if self.rows.borrow().is_empty() {
                "empty"
            } else {
                "list"
            });
    }

    /// Adds a page of older rows at the end, leaving the selection alone.
    pub fn append(&self, rows: Vec<Row>) {
        if rows.is_empty() {
            return;
        }
        self.muted.set(true);
        let added: Vec<glib::BoxedAnyObject> = rows
            .iter()
            .map(|row| glib::BoxedAnyObject::new(Rc::clone(row)))
            .collect();
        let at = self.store.n_items();
        self.rows.borrow_mut().extend(rows);
        self.store.splice(at, 0, &added);
        self.muted.set(false);
        self.stack.set_visible_child_name("list");
    }

    /// Puts fresh rows in place of the ones belonging to `changed` threads.
    /// Rows that came back keep the list in date order; the rest drop out.
    pub fn replace_threads(&self, changed: &[(AccountId, String)], fresh: Vec<ThreadSummary>) {
        let touched = |row: &ThreadSummary| {
            changed
                .iter()
                .any(|(account_id, id)| *account_id == row.account_id && *id == row.id)
        };
        let mut rows: Vec<Row> = self
            .rows
            .borrow()
            .iter()
            .filter(|row| !touched(row))
            .map(Rc::clone)
            .collect();
        // Past the last loaded row the list is incomplete, so a row that
        // sorts below it waits for the next page instead of jumping in.
        let floor = rows.last().map(Rc::clone);
        for row in fresh {
            let row = Rc::new(row);
            if floor.as_ref().is_some_and(|last| order(&row) < order(last)) {
                continue;
            }
            let at = rows.partition_point(|held| order(held) > order(&row));
            rows.insert(at, row);
        }
        let title = self.empty.title().to_string();
        let icon = self
            .empty
            .icon_name()
            .map(|i| i.to_string())
            .unwrap_or_default();
        self.set_rows(rows, &title, &icon);
    }

    fn replace_all(&self, rows: &[Row]) {
        let keys: Vec<Key> = self.selected_rows().iter().map(key).collect();
        self.muted.set(true);
        let objects: Vec<glib::BoxedAnyObject> = rows
            .iter()
            .map(|row| glib::BoxedAnyObject::new(Rc::clone(row)))
            .collect();
        self.store.splice(0, self.store.n_items(), &objects);
        self.reselect(&keys);
        self.muted.set(false);
    }

    /// Selects exactly the rows whose keys are in `keys`.
    fn reselect(&self, keys: &[Key]) {
        self.selection.unselect_all();
        let rows = self.rows.borrow();
        for (position, row) in rows.iter().enumerate() {
            if keys.contains(&key(row)) {
                self.selection.select_item(position as u32, false);
            }
        }
    }

    fn positions(&self) -> Vec<usize> {
        (0..self.store.n_items())
            .filter(|i| self.selection.is_selected(*i))
            .map(|i| i as usize)
            .collect()
    }

    pub fn picked(&self) -> Picked {
        let rows = self.rows.borrow();
        let mut picked: Vec<ThreadSummary> = self
            .positions()
            .into_iter()
            .filter_map(|p| rows.get(p).map(|row| (**row).clone()))
            .collect();
        match picked.len() {
            0 => Picked::None,
            1 => Picked::One(picked.remove(0)),
            _ => Picked::Many(picked),
        }
    }

    pub fn selected_rows(&self) -> Vec<ThreadSummary> {
        let rows = self.rows.borrow();
        self.positions()
            .into_iter()
            .filter_map(|p| rows.get(p).map(|row| (**row).clone()))
            .collect()
    }

    /// Selects a row alone: the thread's first row, or the given message's row.
    pub fn select(&self, account_id: AccountId, thread_id: &str, message_id: Option<&str>) {
        let position = self.rows.borrow().iter().position(|t| {
            t.account_id == account_id
                && t.id == thread_id
                && message_id.is_none_or(|m| t.message_id.as_deref() == Some(m))
        });
        if let Some(position) = position {
            self.selection.select_item(position as u32, true);
            self.view
                .scroll_to(position as u32, gtk::ListScrollFlags::FOCUS, None);
        }
    }

    pub fn unselect(&self) {
        self.muted.set(true);
        self.selection.unselect_all();
        self.muted.set(false);
    }

    pub fn select_all(&self) {
        self.selection.select_all();
    }

    /// Moves to the row `delta` away from the last selected one and opens it.
    pub fn step(&self, delta: i32) {
        let count = self.rows.borrow().len() as i64;
        if count == 0 {
            return;
        }
        let next = match self.positions().last() {
            None => 0,
            Some(&current) => (current as i64 + delta as i64).clamp(0, count - 1),
        };
        self.selection.select_item(next as u32, true);
        self.view
            .scroll_to(next as u32, gtk::ListScrollFlags::FOCUS, None);
    }

    /// The row to open once the selected rows leave the list: the first
    /// unselected row after them, else the last one before them.
    pub fn neighbour_of_selected(&self) -> Option<ThreadSummary> {
        let rows = self.rows.borrow();
        let positions = self.positions();
        let (Some(&first), Some(&last)) = (positions.first(), positions.last()) else {
            return None;
        };
        rows.iter()
            .enumerate()
            .skip(last + 1)
            .find(|(p, _)| !positions.contains(p))
            .or_else(|| rows.iter().enumerate().take(first).next_back())
            .map(|(_, row)| (**row).clone())
    }

    /// Keeps only the rows `keep` accepts, leaving the rest selected as they were.
    pub fn retain(&self, keep: impl Fn(&ThreadSummary) -> bool) {
        let rows: Vec<Row> = self
            .rows
            .borrow()
            .iter()
            .filter(|r| keep(r))
            .map(Rc::clone)
            .collect();
        let title = self.empty.title().to_string();
        let icon = self
            .empty
            .icon_name()
            .map(|i| i.to_string())
            .unwrap_or_default();
        self.set_rows(rows, &title, &icon);
    }

    /// Runs `open` with a row that was double-clicked or activated with Enter.
    pub fn connect_open(&self, open: impl Fn(ThreadSummary) + 'static) {
        let rows = Rc::clone(&self.rows);
        self.view.connect_activate(move |_, position| {
            let row = rows
                .borrow()
                .get(position as usize)
                .map(|row| (**row).clone());
            if let Some(row) = row {
                open(row);
            }
        });
    }

    /// The rows of the last drag.
    pub fn dragged(&self) -> Vec<ThreadSummary> {
        self.dragged.borrow().clone()
    }

    pub fn open_search(&self) {
        self.search_bar.set_search_mode(true);
        self.search_entry.grab_focus();
    }

    pub fn close_search(&self) {
        self.search_bar.set_search_mode(false);
    }

    pub fn search_open(&self) -> bool {
        self.search_bar.is_search_mode()
    }
}

/// Puts a menu under the pointer on a right click or long press of `row`.
/// A click outside the selection takes the row it landed on first, so the
/// menu acts on what the pointer is over rather than on whatever was
/// selected before.
fn context_menu(row: &ThreadRow, item: &gtk::ListItem, selection: &gtk::MultiSelection) {
    let menu = gio::Menu::new();
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
    menu.append_section(None, &waiting);
    menu.append(Some(&gettext("Export…")), Some("win.export"));
    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    popover.set_parent(row);
    let show = {
        let (popover, item, selection) = (popover.clone(), item.clone(), selection.clone());
        move |x: f64, y: f64| {
            let position = item.position();
            if position != gtk::INVALID_LIST_POSITION && !selection.is_selected(position) {
                selection.select_item(position, true);
            }
            popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.popup();
        }
    };
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    let open = show.clone();
    click.connect_pressed(move |_, _, x, y| open(x, y));
    row.add_controller(click);
    let press = gtk::GestureLongPress::new();
    press.connect_pressed(move |_, x, y| show(x, y));
    row.add_controller(press);
    row.connect_destroy(move |_| popover.unparent());
}

/// Where a row sorts in the list: newest first, ties broken as the store
/// breaks them.
fn order(row: &ThreadSummary) -> (i64, i64, &str) {
    (row.last_message_at, -row.account_id, row.id.as_str())
}
