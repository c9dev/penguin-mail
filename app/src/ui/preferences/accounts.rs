//! Account defaults and the shared order of account headers and pickers.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, glib};
use mailrs_domain::Account;
use mailrs_domain::translate::{fill, gettext};

use crate::app::App;
use crate::settings::{AccountChoice, AccountUse, Change};

pub(super) struct Defaults {
    pub group: adw::PreferencesGroup,
    app: Weak<App>,
    rows: Vec<AccountPicker>,
}

struct AccountPicker {
    row: adw::ComboRow,
    task: Option<AccountUse>,
    values: Rc<RefCell<Vec<AccountChoice>>>,
    updating: Rc<Cell<bool>>,
}

impl Defaults {
    pub fn new(app: &Rc<App>) -> Rc<Self> {
        let group = adw::PreferencesGroup::builder()
            .title(gettext("Defaults"))
            .build();
        let general = AccountPicker::new(app, None);
        group.add(&general.row);
        let all = adw::SwitchRow::builder()
            .title(gettext("Use Default Account for All Tasks"))
            .active(app.settings().account_defaults.use_for_all)
            .sensitive(!app.mail_accounts().is_empty())
            .build();
        group.add(&all);
        let tasks = crate::ui::disclosure::row(
            adw::ExpanderRow::builder()
                .title(gettext("Defaults by Task"))
                .build(),
        );
        let mut rows = vec![general];
        for task in AccountUse::ALL {
            let picker = AccountPicker::new(app, Some(task));
            tasks.add_row(&picker.row);
            rows.push(picker);
        }
        all.bind_property("active", &tasks, "sensitive")
            .invert_boolean()
            .sync_create()
            .build();
        let weak = Rc::downgrade(app);
        all.connect_active_notify(move |row| {
            if let Some(app) = weak.upgrade() {
                let mut defaults = app.settings().account_defaults;
                defaults.use_for_all = row.is_active();
                app.change_settings(Change::AccountDefaults(defaults));
            }
        });
        group.add(&tasks);
        let this = Rc::new(Self {
            group,
            app: Rc::downgrade(app),
            rows,
        });
        this.refresh();
        this
    }

    /// Changes model order while retaining each saved account, including a removed one.
    pub fn refresh(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let accounts = app.accounts();
        for picker in &self.rows {
            picker.refresh(&app, &accounts);
        }
    }
}

impl AccountPicker {
    fn new(app: &Rc<App>, task: Option<AccountUse>) -> Self {
        let row = adw::ComboRow::builder()
            .title(task.map_or_else(|| gettext("Default Account"), task_title))
            .title_lines(0)
            .build();
        if task.is_some() {
            super::dialog::explain(
                &row,
                &gettext(
                    "Automatic follows this task's context. Use Default Account follows the choice above.",
                ),
            );
        }
        crate::ui::combo_value::widen_value(&row);
        let values = Rc::new(RefCell::new(Vec::<AccountChoice>::new()));
        let updating = Rc::new(Cell::new(false));
        let (weak, choices, busy) = (Rc::downgrade(app), Rc::clone(&values), Rc::clone(&updating));
        row.connect_selected_notify(move |row| {
            if busy.get() {
                return;
            }
            let (Some(app), Some(choice)) = (
                weak.upgrade(),
                choices.borrow().get(row.selected() as usize).cloned(),
            ) else {
                return;
            };
            match task {
                Some(task) => {
                    let mut defaults = app.settings().account_defaults;
                    defaults.set(task, choice);
                    app.change_settings(Change::AccountDefaults(defaults));
                }
                None => {
                    app.change_settings(Change::DefaultAccount(match choice {
                        AccountChoice::Account(email) => Some(email),
                        _ => None,
                    }));
                }
            }
        });
        Self {
            row,
            task,
            values,
            updating,
        }
    }

    fn refresh(&self, app: &App, accounts: &[Account]) {
        let accounts = crate::settings::accounts_for(self.task, accounts);
        let settings = app.settings();
        let saved = self.task.map_or_else(
            || {
                settings
                    .default_account
                    .clone()
                    .map_or(AccountChoice::Automatic, AccountChoice::Account)
            },
            |task| settings.account_defaults.choice(task).clone(),
        );
        let mut values = vec![AccountChoice::Automatic];
        let mut labels = vec![
            self.task
                .map_or_else(|| gettext("Automatic"), automatic_label),
        ];
        if self.task.is_some() {
            values.push(AccountChoice::Default);
            labels.push(gettext("Use Default Account"));
        }
        for account in &accounts {
            values.push(AccountChoice::Account(account.email.clone()));
            labels.push(account.email.clone());
        }
        let same = |value: &AccountChoice| match (value, &saved) {
            (AccountChoice::Account(a), AccountChoice::Account(b)) => a.eq_ignore_ascii_case(b),
            _ => *value == saved,
        };
        let selected = values.iter().position(same).unwrap_or_else(|| {
            if let AccountChoice::Account(email) = &saved {
                values.push(saved.clone());
                labels.push(fill(
                    &gettext("{account} (Not Available)"),
                    &[("account", email)],
                ));
                values.len() - 1
            } else {
                0
            }
        });
        self.updating.set(true);
        *self.values.borrow_mut() = values;
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        self.row.set_model(Some(&gtk::StringList::new(&labels)));
        self.row.set_selected(selected as u32);
        self.row.set_sensitive(!accounts.is_empty());
        self.updating.set(false);
    }
}

fn task_title(task: AccountUse) -> String {
    match task {
        AccountUse::NewMessages => gettext("New Messages"),
        AccountUse::Replies => gettext("Replies"),
        AccountUse::Forwards => gettext("Forwards"),
        AccountUse::Calendar => gettext("Calendar Events"),
        AccountUse::Assistant => gettext("Assistant Actions"),
        AccountUse::Startup => gettext("At Startup"),
    }
}

fn automatic_label(task: AccountUse) -> String {
    match task {
        AccountUse::NewMessages => gettext("Account on Screen, Then First"),
        AccountUse::Replies => gettext("Account That Received the Message"),
        AccountUse::Forwards => gettext("Account of the Original Message"),
        AccountUse::Calendar => gettext("Last Calendar Account Used"),
        AccountUse::Assistant => gettext("Same as New Messages"),
        AccountUse::Startup => gettext("All Inboxes"),
    }
}

const DRAG_ACCOUNT: &str = "penguin-mail-preference-account";

struct Entry {
    email: String,
    row: glib::WeakRef<adw::ExpanderRow>,
    up: glib::WeakRef<gtk::Button>,
    down: glib::WeakRef<gtk::Button>,
}

pub(super) struct Order {
    app: Weak<App>,
    group: glib::WeakRef<adw::PreferencesGroup>,
    entries: RefCell<Vec<Entry>>,
    dragging: RefCell<Option<String>>,
    defaults: Rc<Defaults>,
}

impl Order {
    pub fn new(app: &Rc<App>, group: &adw::PreferencesGroup, defaults: Rc<Defaults>) -> Rc<Self> {
        Rc::new(Self {
            app: Rc::downgrade(app),
            group: group.downgrade(),
            entries: RefCell::new(Vec::new()),
            dragging: RefCell::new(None),
            defaults,
        })
    }

    pub fn add(self: &Rc<Self>, row: &adw::ExpanderRow, email: &str) {
        let handle = gtk::Box::builder().valign(gtk::Align::Center).build();
        handle.set_cursor_from_name(Some("grab"));
        handle.append(&gtk::Image::from_icon_name("penguin-mail-list-drag-handle-symbolic"));
        handle.set_tooltip_text(Some(&gettext("Drag to Reorder Accounts")));
        row.add_prefix(&handle);
        let button = |icon: &str, text: &str| {
            let button = gtk::Button::builder()
                .icon_name(icon)
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .tooltip_text(text)
                .build();
            crate::ui::name(&button, text);
            button
        };
        let up = button(
            "penguin-mail-move-up-symbolic",
            &fill(&gettext("Move {account} Up"), &[("account", email)]),
        );
        let down = button(
            "penguin-mail-move-down-symbolic",
            &fill(&gettext("Move {account} Down"), &[("account", email)]),
        );
        row.add_suffix(&up);
        row.add_suffix(&down);
        self.entries.borrow_mut().push(Entry {
            email: email.to_string(),
            row: row.downgrade(),
            up: up.downgrade(),
            down: down.downgrade(),
        });
        for (button, step) in [(&up, -1), (&down, 1)] {
            let (order, email) = (Rc::clone(self), email.to_string());
            button.connect_clicked(move |_| order.step(&email, step));
        }
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::MOVE);
        source.connect_prepare(|_, _, _| {
            Some(gdk::ContentProvider::for_value(&DRAG_ACCOUNT.to_value()))
        });
        let (order, dragged_email) = (Rc::clone(self), email.to_string());
        source.connect_drag_begin(move |_, _| {
            *order.dragging.borrow_mut() = Some(dragged_email.clone())
        });
        let order = Rc::clone(self);
        source.connect_drag_end(move |_, _, _| {
            order.dragging.borrow_mut().take();
        });
        handle.add_controller(source);
        let header = header_of(row.upcast_ref()).unwrap_or_else(|| row.clone().upcast());
        header.add_css_class("account-reorder-target");
        let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
        let order = Rc::clone(self);
        target.connect_enter(move |target, _, _| {
            if order.dragging.borrow().is_some() {
                gdk::DragAction::MOVE
            } else {
                target.reject();
                gdk::DragAction::empty()
            }
        });
        let weak = header.downgrade();
        target.connect_motion(move |_, _, y| {
            if let Some(header) = weak.upgrade() {
                mark_drop(&header, Some(y >= f64::from(header.height()) / 2.0));
            }
            gdk::DragAction::MOVE
        });
        let weak = header.downgrade();
        target.connect_leave(move |_| {
            if let Some(header) = weak.upgrade() {
                mark_drop(&header, None);
            }
        });
        let (order, email, weak) = (Rc::clone(self), email.to_string(), header.downgrade());
        target.connect_drop(move |_, value, _, y| {
            let Some(header) = weak.upgrade() else {
                return false;
            };
            mark_drop(&header, None);
            if value.get::<String>().ok().as_deref() != Some(DRAG_ACCOUNT) {
                return false;
            }
            let Some(moved) = order.dragging.borrow().clone() else {
                return false;
            };
            order.drop_on(&moved, &email, y >= f64::from(header.height()) / 2.0)
        });
        header.add_controller(target);
    }

    /// The accounts the page lists, which leave out a calendar account,
    /// so a step moves past a row the person can see.
    fn emails(&self, app: &App) -> Vec<String> {
        app.mail_accounts().into_iter().map(|a| a.email).collect()
    }

    fn step(&self, email: &str, step: isize) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        app.change_settings(Change::MoveAccount {
            emails: self.emails(&app),
            email: email.to_string(),
            step,
        });
        self.refresh();
    }

    fn drop_on(&self, email: &str, target: &str, after: bool) -> bool {
        let Some(app) = self.app.upgrade() else {
            return false;
        };
        let emails = self.emails(&app);
        if email.eq_ignore_ascii_case(target)
            || !emails.iter().any(|e| e.eq_ignore_ascii_case(email))
        {
            return false;
        }
        app.change_settings(Change::ReorderAccount {
            emails,
            email: email.to_string(),
            target: target.to_string(),
            after,
        });
        self.refresh();
        true
    }

    /// Reparents the same rows so edits, expansion and search targets survive a move.
    pub fn refresh(&self) {
        let (Some(app), Some(group)) = (self.app.upgrade(), self.group.upgrade()) else {
            return;
        };
        let settings = app.settings();
        let focus = group.root().and_then(|root| root.focus());
        let mut entries = self.entries.borrow_mut();
        entries.sort_by_key(|entry| settings.account_rank(&entry.email));
        let rows: Vec<_> = entries
            .iter()
            .filter_map(|entry| entry.row.upgrade())
            .collect();
        for row in &rows {
            group.remove(row);
        }
        for row in &rows {
            group.add(row);
        }
        for (index, entry) in entries.iter().enumerate() {
            if let Some(up) = entry.up.upgrade() {
                up.set_sensitive(index > 0);
            }
            if let Some(down) = entry.down.upgrade() {
                down.set_sensitive(index + 1 < entries.len());
            }
        }
        drop(entries);
        self.defaults.refresh();
        if let Some(focus) = focus {
            if focus.is_sensitive() {
                focus.grab_focus();
            } else if let Some(entry) = self.entries.borrow().iter().find(|entry| {
                entry
                    .row
                    .upgrade()
                    .is_some_and(|row| focus.is_ancestor(&row))
            }) {
                for button in [&entry.up, &entry.down] {
                    if let Some(button) = button.upgrade().filter(|button| button.is_sensitive()) {
                        button.grab_focus();
                        break;
                    }
                }
            }
        }
    }
}

fn header_of(root: &gtk::Widget) -> Option<gtk::Widget> {
    if root.has_css_class("header") {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(header) = header_of(&widget) {
            return Some(header);
        }
        child = widget.next_sibling();
    }
    None
}

fn mark_drop(header: &gtk::Widget, after: Option<bool>) {
    for (class, active) in [
        ("drop-before", after == Some(false)),
        ("drop-after", after == Some(true)),
    ] {
        if active {
            header.add_css_class(class);
        } else {
            header.remove_css_class(class);
        }
    }
}
