//! The middle pane: threads of the current mailbox, or search results.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::{AccountId, ThreadSummary};

use super::thread_row::ThreadRow;
use crate::diff::splice;

pub struct ThreadList {
    pub page: adw::NavigationPage,
    pub sidebar_button: gtk::ToggleButton,
    pub search_button: gtk::ToggleButton,
    pub banner: adw::Banner,
    pub search_entry: gtk::SearchEntry,
    search_bar: gtk::SearchBar,
    title: adw::WindowTitle,
    stack: gtk::Stack,
    empty: adw::StatusPage,
    store: gio::ListStore,
    selection: gtk::SingleSelection,
    view: gtk::ListView,
    rows: RefCell<Vec<ThreadSummary>>,
    show_accounts: Rc<Cell<bool>>,
    muted: Cell<bool>,
}

impl ThreadList {
    pub fn new(
        on_select: impl Fn(ThreadSummary) + 'static,
        on_search: impl Fn(String) + 'static,
    ) -> Rc<ThreadList> {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::SingleSelection::builder()
            .model(&store)
            .autoselect(false)
            .can_unselect(true)
            .build();
        let show_accounts = Rc::new(Cell::new(true));
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item
                .downcast_ref::<gtk::ListItem>()
                .expect("list items are ListItems");
            item.set_child(Some(&ThreadRow::default()));
        });
        let shown = Rc::clone(&show_accounts);
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
            row.bind(&object.borrow::<ThreadSummary>(), shown.get());
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
            .icon_name("mailrs-inbox-symbolic")
            .title("No Mail")
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
            .placeholder_text("Search mail, e.g. from:ann has:attachment")
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

        let title = adw::WindowTitle::new("All Inboxes", "");
        let sidebar_button = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Show Mailboxes")
            .visible(false)
            .build();
        let compose_button = gtk::Button::builder()
            .icon_name("mail-message-new-symbolic")
            .tooltip_text("New Message (C)")
            .action_name("win.compose")
            .build();
        let search_button = gtk::ToggleButton::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text("Search (/)")
            .build();
        search_button
            .bind_property("active", &search_bar, "search-mode-enabled")
            .bidirectional()
            .sync_create()
            .build();
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_start(&sidebar_button);
        header.pack_end(&compose_button);
        header.pack_end(&search_button);

        let banner = adw::Banner::builder().revealed(false).build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&search_bar);
        toolbar.add_top_bar(&banner);
        toolbar.set_content(Some(&stack));
        let page = adw::NavigationPage::builder()
            .title("Mail")
            .tag("list")
            .child(&toolbar)
            .build();

        let list = Rc::new(ThreadList {
            page,
            sidebar_button,
            search_button,
            banner,
            search_entry,
            search_bar,
            title,
            stack,
            empty,
            store,
            selection,
            view,
            rows: RefCell::new(Vec::new()),
            show_accounts,
            muted: Cell::new(false),
        });
        let weak = Rc::downgrade(&list);
        list.selection.connect_selected_notify(move |selection| {
            let Some(list) = weak.upgrade() else { return };
            if list.muted.get() {
                return;
            }
            let chosen = list
                .rows
                .borrow()
                .get(selection.selected() as usize)
                .cloned();
            if let Some(thread) = chosen {
                on_select(thread);
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
            let rows = self.rows.borrow().clone();
            self.replace_all(&rows);
        }
    }

    pub fn show_loading(&self) {
        self.stack.set_visible_child_name("loading");
    }

    /// Updates the list with one splice, keeping the selected thread
    /// selected when it is still present.
    pub fn set_rows(&self, rows: Vec<ThreadSummary>, empty_title: &str, empty_icon: &str) {
        let selected = self.selected().map(|t| (t.account_id, t.id));
        self.muted.set(true);
        let old = self.rows.replace(rows);
        let new = self.rows.borrow();
        if let Some(change) = splice(&old, &new) {
            let added: Vec<glib::BoxedAnyObject> = new[change.added]
                .iter()
                .cloned()
                .map(glib::BoxedAnyObject::new)
                .collect();
            self.store.splice(change.position, change.removed, &added);
        }
        let position = selected.and_then(|(account, id)| {
            new.iter()
                .position(|t| t.account_id == account && t.id == id)
        });
        self.selection
            .set_selected(position.map_or(gtk::INVALID_LIST_POSITION, |p| p as u32));
        drop(new);
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

    fn replace_all(&self, rows: &[ThreadSummary]) {
        let selected = self.selection.selected();
        self.muted.set(true);
        let objects: Vec<glib::BoxedAnyObject> = rows
            .iter()
            .cloned()
            .map(glib::BoxedAnyObject::new)
            .collect();
        self.store.splice(0, self.store.n_items(), &objects);
        self.selection.set_selected(selected);
        self.muted.set(false);
    }

    pub fn selected(&self) -> Option<ThreadSummary> {
        self.rows
            .borrow()
            .get(self.selection.selected() as usize)
            .cloned()
    }

    pub fn select(&self, account_id: AccountId, thread_id: &str) {
        let position = self
            .rows
            .borrow()
            .iter()
            .position(|t| t.account_id == account_id && t.id == thread_id);
        if let Some(position) = position {
            self.selection.set_selected(position as u32);
            self.view
                .scroll_to(position as u32, gtk::ListScrollFlags::FOCUS, None);
        }
    }

    pub fn unselect(&self) {
        self.muted.set(true);
        self.selection.set_selected(gtk::INVALID_LIST_POSITION);
        self.muted.set(false);
    }

    /// Moves the selection by `delta` rows and opens the thread there.
    pub fn step(&self, delta: i32) {
        let count = self.rows.borrow().len() as i64;
        if count == 0 {
            return;
        }
        let current = self.selection.selected();
        let next = if current == gtk::INVALID_LIST_POSITION {
            0
        } else {
            (current as i64 + delta as i64).clamp(0, count - 1)
        };
        self.selection.set_selected(next as u32);
        self.view
            .scroll_to(next as u32, gtk::ListScrollFlags::FOCUS, None);
    }

    /// The thread that should open after the selected one leaves the list.
    pub fn neighbour_of_selected(&self) -> Option<ThreadSummary> {
        let rows = self.rows.borrow();
        let current = self.selection.selected() as usize;
        rows.get(current + 1)
            .or_else(|| current.checked_sub(1).and_then(|p| rows.get(p)))
            .cloned()
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
