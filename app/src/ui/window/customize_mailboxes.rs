//! The person's visible mailboxes and favorites, independent of mail sync.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, glib};
use mailrs_domain::translate::{fill, gettext};

use super::MainWindow;
use crate::settings::{Change, FavoriteMailbox, SidebarLayout};
use crate::ui::Standard;
use crate::ui::sidebar::sections::{LAYOUT, Place};
use crate::ui::sidebar::tree::{self, label_rows};
use crate::ui::sidebar::{MailboxChoice, mailbox_choices};
use mailrs_domain::Folder;

fn save(win: &MainWindow, layout: SidebarLayout) {
    if let Some(app) = win.app.upgrade() {
        app.change_settings(Change::Sidebar(layout));
    }
}

fn set_visible(win: &MainWindow, key: &str, shown: bool) {
    let mut layout = win.settings().sidebar;
    layout.hidden.retain(|item| item != key);
    layout.shown.retain(|item| item != key);
    if shown {
        layout.shown.push(key.into());
    } else {
        layout.hidden.push(key.into());
    }
    save(win, layout);
}

pub(super) fn reorder_siblings(mut ids: Vec<String>, siblings: &[String]) -> Vec<String> {
    for id in siblings {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    let slots: Vec<usize> = ids
        .iter()
        .enumerate()
        .filter_map(|(i, id)| siblings.contains(id).then_some(i))
        .collect();
    for (slot, id) in slots.into_iter().zip(siblings) {
        ids[slot] = id.clone();
    }
    ids
}

fn move_folder(
    win: &MainWindow,
    account: mailrs_domain::AccountId,
    email: &str,
    label_id: &str,
    step: i32,
    list: &gtk::ListBox,
    widgets: &HashMap<String, adw::ActionRow>,
) {
    let root = format!("account:{}", email.to_ascii_lowercase());
    let labels = win.labels_of(account);
    let mut layout = win.settings().sidebar;
    let positions = layout
        .label_order
        .get(&root)
        .map(|ids| {
            ids.iter()
                .enumerate()
                .map(|(i, id)| (id.clone(), i as i64))
                .collect()
        })
        .unwrap_or_else(|| win.label_order(account));
    let rows = label_rows(&labels, &positions);
    let Some(placement) = tree::step(&rows, label_id, step) else {
        return;
    };
    let ids = reorder_siblings(
        rows.iter().map(|row| row.label.id.clone()).collect(),
        &placement.order,
    );
    layout.label_order.insert(root, ids.clone());
    save(win, layout);
    let positions: HashMap<String, i64> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), i as i64))
        .collect();
    let ordered = label_rows(&labels, &positions);
    let start = widgets.values().map(|row| row.index()).min().unwrap_or(0);
    for (offset, entry) in ordered.iter().enumerate() {
        if let Some(row) = widgets.get(&entry.label.id) {
            list.remove(row);
            list.insert(row, start + offset as i32);
        }
    }
}

fn control(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(label)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    crate::ui::name(&button, label);
    button
}

fn row(title: &str) -> (adw::ActionRow, gtk::CheckButton, gtk::Button, gtk::Button) {
    let item = crate::ui::plain_row().build();
    item.set_title(title);
    let check = gtk::CheckButton::new();
    crate::ui::name(
        &check,
        &fill(&gettext("Show {mailbox}"), &[("mailbox", title)]),
    );
    item.add_prefix(&check);
    let up = control(
        "penguin-mail-move-up-symbolic",
        &fill(&gettext("Move {button} Up"), &[("button", title)]),
    );
    let down = control(
        "penguin-mail-move-down-symbolic",
        &fill(&gettext("Move {button} Down"), &[("button", title)]),
    );
    item.add_suffix(&up);
    item.add_suffix(&down);
    (item, check, up, down)
}

struct FavoriteRow {
    choice: MailboxChoice,
    row: adw::ActionRow,
    check: gtk::Button,
    available: adw::ActionRow,
    add: gtk::Button,
    name: gtk::Entry,
    up: gtk::Button,
    down: gtk::Button,
}

struct Editor {
    win: Weak<super::MainWindow>,
    list: gtk::ListBox,
    favorites: Vec<FavoriteRow>,
    changing: Cell<bool>,
    dragging: RefCell<Option<String>>,
}

type AccountBlocks = Rc<RefCell<Vec<(String, Vec<adw::ActionRow>)>>>;

fn favorite_neighbor(
    favorites: &[FavoriteMailbox],
    visible: &[&str],
    key: &str,
    step: isize,
) -> Option<usize> {
    let indices: Vec<usize> = favorites
        .iter()
        .enumerate()
        .filter_map(|(index, favorite)| visible.contains(&favorite.key.as_str()).then_some(index))
        .collect();
    let at = indices
        .iter()
        .position(|index| favorites[*index].key == key)?;
    indices.get(at.checked_add_signed(step)?).copied()
}

fn place_favorite(
    favorites: &mut Vec<FavoriteMailbox>,
    moved: &str,
    target: &str,
    after: bool,
) -> bool {
    if moved == target || !favorites.iter().any(|favorite| favorite.key == target) {
        return false;
    }
    let Some(from) = favorites.iter().position(|favorite| favorite.key == moved) else {
        return false;
    };
    let favorite = favorites.remove(from);
    let at = favorites
        .iter()
        .position(|favorite| favorite.key == target)
        .unwrap();
    favorites.insert(at + usize::from(after), favorite);
    true
}

impl Editor {
    fn layout(&self) -> Option<SidebarLayout> {
        self.win.upgrade().map(|win| win.settings().sidebar)
    }

    fn save(&self, layout: SidebarLayout) {
        if let Some(win) = self.win.upgrade() {
            save(&win, layout);
        }
        self.refresh();
    }

    fn refresh(&self) {
        let Some(layout) = self.layout() else { return };
        self.changing.set(true);
        let favorites = layout.favorites();
        let visible: Vec<&str> = self
            .favorites
            .iter()
            .map(|row| row.choice.key.as_str())
            .collect();
        let mut keys: Vec<String> = favorites
            .iter()
            .map(|favorite| favorite.key.clone())
            .collect();
        for row in &self.favorites {
            if !keys.contains(&row.choice.key) {
                keys.push(row.choice.key.clone());
            }
        }
        keys.retain(|key| self.favorites.iter().any(|row| row.choice.key == *key));
        for (index, key) in keys.iter().enumerate() {
            let Some(row) = self.favorites.iter().find(|row| row.choice.key == *key) else {
                continue;
            };
            if row.row.index() != index as i32 {
                self.list.remove(&row.row);
                self.list.insert(&row.row, index as i32);
            }
            let chosen = favorites.iter().find(|favorite| favorite.key == *key);
            row.row.set_visible(chosen.is_some());
            row.available.set_visible(chosen.is_none());
            row.name.set_visible(chosen.is_some());
            row.name
                .set_text(chosen.map_or("", |favorite| favorite.name.as_str()));
            row.up
                .set_sensitive(favorite_neighbor(&favorites, &visible, key, -1).is_some());
            row.down
                .set_sensitive(favorite_neighbor(&favorites, &visible, key, 1).is_some());
        }
        self.changing.set(false);
    }
}

impl MainWindow {
    pub(super) fn customize_mailboxes(self: &Rc<Self>) {
        let accounts = self.mail_accounts();
        let data: Vec<_> = accounts
            .iter()
            .map(|account| (account.clone(), self.labels_of(account.id)))
            .collect();
        let settings = self.settings();
        let vips: Vec<_> = settings
            .vips
            .iter()
            .map(|(email, name)| (email.clone(), name.clone()))
            .collect();
        let choices = mailbox_choices(&data, &settings.smart_mailboxes, &vips);
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        let section = |title: String, key: &'static str| {
            let page = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(24)
                .margin_top(18)
                .margin_bottom(18)
                .margin_start(18)
                .margin_end(18)
                .build();
            let header = gtk::CheckButton::with_label(&title);
            header.set_active(settings.sidebar.visible(key, true));
            header.add_css_class("heading");
            page.append(&header);
            let weak = Rc::downgrade(self);
            header.connect_toggled(move |check| {
                if let Some(win) = weak.upgrade() {
                    set_visible(&win, key, check.is_active());
                }
            });
            let list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .css_classes(["boxed-list"])
                .build();
            page.append(&list);
            let scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&page)
                .build();
            stack.add_titled(&scroll, Some(key), &title);
            (page, list)
        };
        let (favorite_page, favorite_list) = section(gettext("Favorites"), "section:favorites");
        favorite_page.remove(&favorite_list);
        let available_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        let columns = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        columns.set_vexpand(true);
        for (title, list) in [
            (gettext("Available Mailboxes"), &available_list),
            (gettext("Favorites"), &favorite_list),
        ] {
            let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
            column.set_hexpand(true);
            let heading = gtk::Label::builder()
                .label(title)
                .xalign(0.0)
                .css_classes(["heading"])
                .build();
            column.append(&heading);
            let scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .min_content_height(380)
                .child(list)
                .build();
            column.append(&scroll);
            columns.append(&column);
        }
        favorite_page.append(&columns);
        let favorites: Vec<_> = choices
            .iter()
            .map(|choice| {
                let row = crate::ui::plain_row().build();
                row.set_title(&choice.name);
                row.set_title_lines(1);
                let available = crate::ui::plain_row().build();
                available.set_title(&choice.name);
                available.set_title_lines(1);
                let add = control("go-next-symbolic", &gettext("Add to Favorites"));
                available.add_suffix(&add);
                available_list.append(&available);
                let check = control("go-previous-symbolic", &gettext("Remove from Favorites"));
                row.add_prefix(&check);
                let up = control(
                    "penguin-mail-move-up-symbolic",
                    &fill(&gettext("Move {button} Up"), &[("button", &choice.name)]),
                );
                let down = control(
                    "penguin-mail-move-down-symbolic",
                    &fill(&gettext("Move {button} Down"), &[("button", &choice.name)]),
                );
                row.add_suffix(&up);
                row.add_suffix(&down);
                if let Some(account) = choice
                    .key
                    .strip_prefix("account:")
                    .and_then(|rest| rest.split(':').next())
                {
                    row.set_subtitle(account);
                    available.set_subtitle(account);
                }
                let name = gtk::Entry::builder()
                    .placeholder_text(gettext("Favorite name"))
                    .valign(gtk::Align::Center)
                    .width_chars(10)
                    .max_width_chars(14)
                    .build();
                crate::ui::name(
                    &name,
                    &fill(
                        &gettext("Name for {mailbox} in Favorites"),
                        &[("mailbox", &choice.name)],
                    ),
                );
                row.add_suffix(&name);
                favorite_list.append(&row);
                FavoriteRow {
                    choice: choice.clone(),
                    row,
                    check,
                    available,
                    add,
                    name,
                    up,
                    down,
                }
            })
            .collect();
        let editor = Rc::new(Editor {
            win: Rc::downgrade(self),
            list: favorite_list,
            favorites,
            changing: Cell::new(false),
            dragging: RefCell::new(None),
        });
        editor.refresh();
        for item in &editor.favorites {
            let key = item.choice.key.clone();
            let weak = Rc::downgrade(&editor);
            item.add.connect_clicked(move |_| {
                let Some(editor) = weak.upgrade() else { return };
                let Some(mut layout) = editor.layout() else {
                    return;
                };
                let mut favorites = layout.favorites();
                if !favorites.iter().any(|favorite| favorite.key == key) {
                    favorites.push(FavoriteMailbox {
                        key: key.clone(),
                        name: String::new(),
                    });
                    layout.favorites = Some(favorites);
                    editor.save(layout);
                }
            });
            let handle = gtk::Image::from_icon_name("list-drag-handle-symbolic");
            item.row.add_prefix(&handle);
            let source = gtk::DragSource::new();
            source.set_actions(gdk::DragAction::MOVE);
            source.connect_prepare(|_, _, _| {
                Some(gdk::ContentProvider::for_value(
                    &"penguin-favorite".to_value(),
                ))
            });
            let weak = Rc::downgrade(&editor);
            let key = item.choice.key.clone();
            source.connect_drag_begin(move |_, _| {
                if let Some(editor) = weak.upgrade() {
                    *editor.dragging.borrow_mut() = Some(key.clone());
                }
            });
            let weak = Rc::downgrade(&editor);
            source.connect_drag_end(move |_, _, _| {
                if let Some(editor) = weak.upgrade() {
                    editor.dragging.borrow_mut().take();
                }
            });
            handle.add_controller(source);
            let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
            let weak = Rc::downgrade(&editor);
            let row = item.row.downgrade();
            let key = item.choice.key.clone();
            target.connect_drop(move |_, value, _, y| {
                if value.get::<String>().ok().as_deref() != Some("penguin-favorite") {
                    return false;
                }
                let (Some(editor), Some(row)) = (weak.upgrade(), row.upgrade()) else {
                    return false;
                };
                let Some(moved) = editor.dragging.borrow().clone() else {
                    return false;
                };
                let Some(mut layout) = editor.layout() else {
                    return false;
                };
                let mut favorites = layout.favorites();
                if !place_favorite(
                    &mut favorites,
                    &moved,
                    &key,
                    y > f64::from(row.height()) / 2.0,
                ) {
                    return false;
                }
                layout.favorites = Some(favorites);
                editor.save(layout);
                true
            });
            item.row.add_controller(target);
            let key = item.choice.key.clone();
            let weak = Rc::downgrade(&editor);
            item.check.connect_clicked(move |_| {
                let Some(editor) = weak.upgrade() else { return };
                if editor.changing.get() {
                    return;
                }
                let Some(mut layout) = editor.layout() else {
                    return;
                };
                let mut favorites = layout.favorites();
                favorites.retain(|favorite| favorite.key != key);

                layout.favorites = Some(favorites);
                editor.save(layout);
            });
            let key = item.choice.key.clone();
            let weak = Rc::downgrade(&editor);
            item.name.connect_changed(move |entry| {
                let Some(editor) = weak.upgrade() else { return };
                if editor.changing.get() {
                    return;
                }
                let Some(mut layout) = editor.layout() else {
                    return;
                };
                let mut favorites = layout.favorites();
                if let Some(favorite) = favorites.iter_mut().find(|favorite| favorite.key == key) {
                    favorite.name = entry.text().trim().to_string();
                    layout.favorites = Some(favorites);
                    if let Some(win) = editor.win.upgrade() {
                        save(&win, layout);
                    }
                }
            });
            for (button, step) in [(&item.up, -1isize), (&item.down, 1)] {
                let key = item.choice.key.clone();
                let weak = Rc::downgrade(&editor);
                button.connect_clicked(move |_| {
                    let Some(editor) = weak.upgrade() else { return };
                    let Some(mut layout) = editor.layout() else {
                        return;
                    };
                    let mut favorites = layout.favorites();
                    let visible: Vec<&str> = editor
                        .favorites
                        .iter()
                        .map(|row| row.choice.key.as_str())
                        .collect();
                    if let Some(index) = favorites.iter().position(|favorite| favorite.key == key)
                        && let Some(next) = favorite_neighbor(&favorites, &visible, &key, step)
                    {
                        favorites.swap(index, next);
                        layout.favorites = Some(favorites);
                        editor.save(layout);
                    }
                });
            }
        }
        let (_, mailbox_list) = section(gettext("Mailboxes"), "section:mailboxes");
        let mut places: Vec<Place> = [
            Place::Unified(Standard::Inbox),
            Place::Unified(Standard::Flagged),
            Place::FollowUp,
        ]
        .into_iter()
        .chain(LAYOUT[1].1.iter().copied())
        .collect();
        places.sort_by_key(|place| {
            settings
                .sidebar
                .mailboxes
                .iter()
                .position(|key| key == &place.key())
        });
        for place in places.iter().copied() {
            let key = place.key();
            let Some(choice) = choices.iter().find(|choice| choice.key == key) else {
                continue;
            };
            let (row, check, up, down) = row(&choice.name);
            let default = !matches!(
                place,
                Place::Unified(Standard::Inbox | Standard::Flagged) | Place::FollowUp
            );
            check.set_active(settings.sidebar.visible(&key, default));
            mailbox_list.append(&row);
            let weak = Rc::downgrade(self);
            check.connect_toggled(move |check| {
                if let Some(win) = weak.upgrade() {
                    let mut layout = win.settings().sidebar;
                    layout.hidden.retain(|item| item != &key);
                    layout.shown.retain(|item| item != &key);
                    let target = if check.is_active() {
                        &mut layout.shown
                    } else {
                        &mut layout.hidden
                    };
                    target.push(key.clone());
                    save(&win, layout);
                }
            });
            for (button, step) in [(&up, -1isize), (&down, 1)] {
                let weak = Rc::downgrade(self);
                let list = mailbox_list.clone();
                let row = row.clone();
                let defaults: Vec<String> = places.iter().map(|place| place.key()).collect();
                button.connect_clicked(move |_| {
                    let Some(win) = weak.upgrade() else { return };
                    let Some(next) = (row.index() as usize).checked_add_signed(step) else {
                        return;
                    };
                    if next >= defaults.len() {
                        return;
                    }
                    let mut layout = win.settings().sidebar;
                    let mut ordered: Vec<String> = layout
                        .mailboxes
                        .into_iter()
                        .filter(|key| defaults.contains(key))
                        .collect();
                    for key in &defaults {
                        if !ordered.contains(key) {
                            ordered.push(key.clone());
                        }
                    }
                    let index = row.index() as usize;
                    ordered.swap(index, next);
                    layout.mailboxes = ordered;
                    list.remove(&row);
                    list.insert(&row, next as i32);
                    save(&win, layout);
                });
            }
        }
        if !settings.smart_mailboxes.is_empty() {
            let (_, smart_list) = section(gettext("Smart Mailboxes"), "section:smart");
            for smart in &settings.smart_mailboxes {
                let key = format!("smart:{}", smart.id);
                let (item, check, up, down) = row(&smart.name);
                check.set_active(settings.sidebar.visible(&key, true));
                smart_list.append(&item);
                let weak = Rc::downgrade(self);
                check.connect_toggled(move |check| {
                    if let Some(win) = weak.upgrade() {
                        set_visible(&win, &key, check.is_active());
                    }
                });
                for (button, step) in [(&up, -1), (&down, 1)] {
                    let weak = Rc::downgrade(self);
                    let list = smart_list.clone();
                    let item = item.clone();
                    let id = smart.id.clone();
                    let count = settings.smart_mailboxes.len();
                    button.connect_clicked(move |_| {
                        let Some(win) = weak.upgrade() else { return };
                        let Some(next) = (item.index() as usize).checked_add_signed(step) else {
                            return;
                        };
                        if next < count {
                            if let Some(app) = win.app.upgrade() {
                                app.change_settings(Change::MoveSmartMailbox {
                                    id: id.clone(),
                                    step,
                                });
                            }
                            list.remove(&item);
                            list.insert(&item, next as i32);
                        }
                    });
                }
            }
        }
        let (_, accounts_list) = section(gettext("Accounts"), "section:accounts");
        let blocks: AccountBlocks = Rc::new(RefCell::new(Vec::new()));
        for account in &accounts {
            let root = format!("account:{}", account.email.to_ascii_lowercase());
            let title = settings
                .account_names
                .get(&account.email)
                .cloned()
                .unwrap_or_else(|| account.email.clone());
            let (account_row, check, up, down) = row(&title);
            check.set_active(settings.sidebar.visible(&root, true));
            accounts_list.append(&account_row);
            let mut block = vec![account_row.clone()];
            let weak = Rc::downgrade(self);
            let key = root.clone();
            check.connect_toggled(move |check| {
                if let Some(win) = weak.upgrade() {
                    set_visible(&win, &key, check.is_active());
                }
            });
            for (button, step) in [(&up, -1isize), (&down, 1)] {
                let weak = Rc::downgrade(self);
                let account = account.clone();
                let list = accounts_list.clone();
                let blocks = Rc::clone(&blocks);
                let root = root.clone();
                button.connect_clicked(move |_| {
                    let Some(win) = weak.upgrade() else { return };
                    let mut blocks = blocks.borrow_mut();
                    let Some(index) = blocks.iter().position(|(key, _)| key == &root) else {
                        return;
                    };
                    let Some(next) = index.checked_add_signed(step) else {
                        return;
                    };
                    if next >= blocks.len() {
                        return;
                    }
                    let mut moving = blocks[index].1.clone();
                    moving.sort_by_key(|row| row.index());
                    let at = if step > 0 {
                        moving[0].index() + blocks[next].1.len() as i32
                    } else {
                        blocks[next].1[0].index()
                    };
                    for row in &moving {
                        list.remove(row);
                    }
                    for (offset, row) in moving.iter().enumerate() {
                        list.insert(row, at + offset as i32);
                    }
                    blocks.swap(index, next);
                    win.move_account(account.clone(), step);
                });
            }
            let mut roots: Vec<_> = Standard::ALL
                .into_iter()
                .map(|which| format!("{root}:{}", which.key()))
                .chain(
                    Folder::ALL
                        .into_iter()
                        .map(|folder| format!("{root}:{folder:?}")),
                )
                .collect();
            if let Some(order) = settings.sidebar.account_roots.get(&root) {
                roots.sort_by_key(|key| order.iter().position(|item| item == key));
            }
            let root_rows: Rc<RefCell<Vec<(String, adw::ActionRow)>>> =
                Rc::new(RefCell::new(Vec::new()));
            for key in roots {
                let Some(choice) = choices.iter().find(|choice| choice.key == key) else {
                    continue;
                };
                let (item, check, up, down) = row(&choice.name);
                item.set_margin_start(20);
                check.set_active(settings.sidebar.visible(&key, true));
                accounts_list.append(&item);
                block.push(item.clone());
                root_rows.borrow_mut().push((key.clone(), item.clone()));
                let weak = Rc::downgrade(self);
                let visible_key = key.clone();
                check.connect_toggled(move |check| {
                    if let Some(win) = weak.upgrade() {
                        set_visible(&win, &visible_key, check.is_active());
                    }
                });
                for (button, step) in [(&up, -1isize), (&down, 1)] {
                    let weak = Rc::downgrade(self);
                    let list = accounts_list.clone();
                    let item = item.clone();
                    let root_rows = Rc::clone(&root_rows);
                    let root = root.clone();
                    button.connect_clicked(move |_| {
                        let Some(win) = weak.upgrade() else { return };
                        let mut rows = root_rows.borrow_mut();
                        let Some(index) = rows.iter().position(|(_, row)| row == &item) else {
                            return;
                        };
                        let Some(next) = index.checked_add_signed(step) else {
                            return;
                        };
                        if next >= rows.len() {
                            return;
                        }
                        let at = rows[next].1.index();
                        list.remove(&item);
                        list.insert(&item, at);
                        rows.swap(index, next);
                        let mut layout = win.settings().sidebar;
                        layout.account_roots.insert(
                            root.clone(),
                            rows.iter().map(|(key, _)| key.clone()).collect(),
                        );
                        save(&win, layout);
                    });
                }
            }
            let labels = self.labels_of(account.id);
            let positions = settings
                .sidebar
                .label_order
                .get(&root)
                .map(|ids| {
                    ids.iter()
                        .enumerate()
                        .map(|(i, id)| (id.clone(), i as i64))
                        .collect()
                })
                .unwrap_or_else(|| self.label_order(account.id));
            let label_widgets: Rc<RefCell<HashMap<String, adw::ActionRow>>> =
                Rc::new(RefCell::new(HashMap::new()));
            for entry in label_rows(&labels, &positions) {
                let label = entry.label;
                let key = format!("{root}:label:{}", label.id);
                let (item, check, up, down) = row(entry.leaf);
                item.set_margin_start(20 + 20 * entry.depth as i32);
                check.set_active(settings.sidebar.visible(&key, true));
                accounts_list.append(&item);
                block.push(item.clone());
                label_widgets.borrow_mut().insert(label.id.clone(), item);
                let weak = Rc::downgrade(self);
                check.connect_toggled(move |check| {
                    if let Some(win) = weak.upgrade() {
                        set_visible(&win, &key, check.is_active());
                    }
                });
                for (button, step) in [(&up, -1i32), (&down, 1)] {
                    let weak = Rc::downgrade(self);
                    let list = accounts_list.clone();
                    let widgets = Rc::clone(&label_widgets);
                    let id = label.id.clone();
                    let email = account.email.clone();
                    let account_id = account.id;
                    button.connect_clicked(move |_| {
                        if let Some(win) = weak.upgrade() {
                            move_folder(
                                &win,
                                account_id,
                                &email,
                                &id,
                                step,
                                &list,
                                &widgets.borrow(),
                            );
                        }
                    });
                }
            }
            for label in tree::tag_rows(&labels) {
                let key = format!("{root}:label:{}", label.id);
                let (item, check, up, down) = row(&label.name);
                item.set_margin_start(40);
                up.set_visible(false);
                down.set_visible(false);
                check.set_active(settings.sidebar.visible(&key, true));
                accounts_list.append(&item);
                block.push(item);
                let weak = Rc::downgrade(self);
                check.connect_toggled(move |check| {
                    if let Some(win) = weak.upgrade() {
                        set_visible(&win, &key, check.is_active());
                    }
                });
            }
            blocks.borrow_mut().push((root, block));
        }
        let reset = gtk::Button::with_label(&gettext("Restore Defaults"));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.append(&stack);
        reset.set_halign(gtk::Align::Center);
        reset.set_margin_bottom(12);
        content.append(&reset);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        let tabs = gtk::StackSwitcher::builder()
            .stack(&stack)
            .halign(gtk::Align::Center)
            .build();
        toolbar.add_top_bar(&tabs);
        toolbar.set_content(Some(&content));
        let dialog = adw::Dialog::builder()
            .title(gettext("Customize Mailboxes"))
            .content_width(1000)
            .content_height(640)
            .child(&toolbar)
            .build();
        let weak = Rc::downgrade(self);
        let closing = dialog.clone();
        reset.connect_clicked(move |_| {
            if let Some(win) = weak.upgrade() {
                save(&win, SidebarLayout::default());
                closing.close();
            }
        });
        dialog.connect_closed(move |_| {
            let _ = &editor;
        });
        dialog.present(Some(&self.window));
    }
}

#[cfg(test)]
mod tests {
    use super::{FavoriteMailbox, favorite_neighbor, place_favorite, reorder_siblings};

    fn favorites() -> Vec<FavoriteMailbox> {
        ["inbox", "vips", "flagged", "follow-up"]
            .map(|key| FavoriteMailbox {
                key: key.into(),
                name: format!("Name for {key}"),
            })
            .to_vec()
    }

    #[test]
    fn arrows_skip_unavailable_favorites() {
        let favorites = favorites();
        let visible = ["inbox", "flagged", "follow-up"];
        assert_eq!(
            favorite_neighbor(&favorites, &visible, "flagged", -1),
            Some(0)
        );
        assert_eq!(favorite_neighbor(&favorites, &visible, "inbox", 1), Some(2));
        assert_eq!(favorite_neighbor(&favorites, &visible, "inbox", -1), None);
        assert_eq!(
            favorite_neighbor(&favorites, &visible, "follow-up", 1),
            None
        );
    }

    #[test]
    fn dragging_places_a_favorite_before_or_after_its_target_and_keeps_its_name() {
        let mut favorites = favorites();
        assert!(place_favorite(&mut favorites, "inbox", "follow-up", true));
        assert_eq!(favorites.last().unwrap().name, "Name for inbox");
        assert!(place_favorite(&mut favorites, "inbox", "vips", false));
        assert_eq!(favorites.first().unwrap().key, "inbox");
        let before = favorites.clone();
        assert!(!place_favorite(&mut favorites, "inbox", "inbox", true));
        assert!(!place_favorite(&mut favorites, "missing", "inbox", true));
        assert!(!place_favorite(&mut favorites, "inbox", "missing", true));
        assert_eq!(favorites, before);
    }

    #[test]
    fn moving_a_new_folder_keeps_existing_sibling_positions() {
        let ids = ["work", "work/client", "home"].map(str::to_string).to_vec();
        let siblings = ["new", "home", "work"].map(str::to_string);
        assert_eq!(
            reorder_siblings(ids, &siblings),
            ["new", "work/client", "home", "work"]
        );
    }

    #[test]
    fn moving_a_parent_keeps_child_slots_in_the_tree() {
        let ids = ["work", "work/client", "home", "home/travel"]
            .map(str::to_string)
            .to_vec();
        let siblings = ["home", "work"].map(str::to_string);
        assert_eq!(
            reorder_siblings(ids, &siblings),
            ["home", "work/client", "work", "home/travel"]
        );
    }
}
