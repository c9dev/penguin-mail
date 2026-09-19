//! Mailboxes: the unified views, then one section per account.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use mailrs_domain::{Account, AccountId, AccountState, Label, LabelKind};

use super::{Folder, Mailbox, UNIFIED, account_label_name, mailbox_icon, unified_name};
use crate::format::account_color_index;

struct Row {
    row: gtk::ListBoxRow,
    mailbox: Mailbox,
    count: gtk::Label,
}

struct Heading {
    row: gtk::ListBoxRow,
    account_id: AccountId,
    chevron: gtk::Image,
    count: gtk::Label,
}

pub struct Sidebar {
    pub page: adw::ToolbarView,
    pub header: adw::HeaderBar,
    pub add_account: gtk::Button,
    list: gtk::ListBox,
    scroller: gtk::ScrolledWindow,
    rows: RefCell<Vec<Row>>,
    headings: RefCell<Vec<Heading>>,
    /// Accounts whose sections the user expanded or collapsed.
    expanded: RefCell<HashMap<AccountId, bool>>,
    /// Whether sections start open. Unset means open only with one account.
    pub start_expanded: Cell<Option<bool>>,
    muted: Cell<bool>,
    /// Handles mail dropped on a mailbox; true when it was taken.
    on_drop: Rc<dyn Fn(Mailbox) -> bool>,
}

impl Sidebar {
    pub fn new(
        on_select: impl Fn(Mailbox) + 'static,
        on_drop: impl Fn(Mailbox) -> bool + 'static,
    ) -> Rc<Sidebar> {
        let list = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar"])
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        let header = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .title_widget(&adw::WindowTitle::new("Mailboxes", ""))
            .build();
        let add_account = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("list-add-symbolic")
                    .label("Add Account")
                    .build(),
            )
            .css_classes(["flat"])
            .halign(gtk::Align::Start)
            .margin_start(6)
            .margin_end(6)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        let page = adw::ToolbarView::new();
        page.add_top_bar(&header);
        page.set_content(Some(&scroller));
        page.add_bottom_bar(&add_account);

        let sidebar = Rc::new(Sidebar {
            page,
            header,
            add_account,
            list,
            scroller: scroller.clone(),
            rows: RefCell::new(Vec::new()),
            headings: RefCell::new(Vec::new()),
            expanded: RefCell::new(HashMap::new()),
            start_expanded: Cell::new(None),
            muted: Cell::new(false),
            on_drop: Rc::new(on_drop),
        });
        let weak = Rc::downgrade(&sidebar);
        sidebar.list.connect_row_selected(move |_, row| {
            let (Some(sidebar), Some(row)) = (weak.upgrade(), row) else {
                return;
            };
            if sidebar.muted.get() {
                return;
            }
            let chosen = sidebar
                .rows
                .borrow()
                .iter()
                .find(|r| &r.row == row)
                .map(|r| r.mailbox.clone());
            if let Some(mailbox) = chosen {
                on_select(mailbox);
            }
        });
        let weak = Rc::downgrade(&sidebar);
        sidebar.list.connect_row_activated(move |_, row| {
            let Some(sidebar) = weak.upgrade() else {
                return;
            };
            let account = sidebar
                .headings
                .borrow()
                .iter()
                .find(|h| &h.row == row)
                .map(|h| h.account_id);
            if let Some(account_id) = account {
                let open = !sidebar.is_expanded(account_id);
                sidebar.expanded.borrow_mut().insert(account_id, open);
                sidebar.apply_expansion();
            }
        });
        sidebar
    }

    fn is_expanded(&self, account_id: AccountId) -> bool {
        let default = self
            .start_expanded
            .get()
            .unwrap_or(self.headings.borrow().len() <= 1);
        self.expanded
            .borrow()
            .get(&account_id)
            .copied()
            .unwrap_or(default)
    }

    /// Shows or hides each account's rows. The selected row always stays visible.
    fn apply_expansion(&self) {
        let selected = self.list.selected_row();
        for heading in self.headings.borrow().iter() {
            let open = self.is_expanded(heading.account_id);
            heading.chevron.set_icon_name(Some(if open {
                "pan-down-symbolic"
            } else {
                "pan-end-symbolic"
            }));
            heading
                .count
                .set_visible(!open && heading.count.label() != "0");
            for row in self.rows.borrow().iter() {
                if row.mailbox.account() == Some(heading.account_id) {
                    row.row
                        .set_visible(open || selected.as_ref() == Some(&row.row));
                }
            }
        }
    }

    /// Rebuilds every row. `selected` is kept selected when it still exists.
    pub fn rebuild(&self, accounts: &[(Account, Vec<Label>)], selected: &Mailbox) {
        // Keep the scroll position; label changes rebuild every row.
        let scrolled = self.scroller.vadjustment().value();
        self.muted.set(true);
        self.list.remove_all();
        self.rows.borrow_mut().clear();
        self.headings.borrow_mut().clear();
        for label in UNIFIED {
            self.add_mailbox(
                Mailbox::Unified(label),
                unified_name(label),
                mailbox_icon(label),
                0,
            );
        }
        self.add_mailbox(Mailbox::Scheduled, "Send Later", "alarm-symbolic", 0);
        for folder in Folder::ALL {
            let mailbox = Mailbox::Folder {
                account_id: None,
                folder,
            };
            self.add_mailbox(mailbox, folder.name(), folder.icon(), 0);
        }
        for (account, labels) in accounts {
            let (row, chevron, count) = heading(account);
            self.list.append(&row);
            self.headings.borrow_mut().push(Heading {
                row,
                account_id: account.id,
                chevron,
                count,
            });
            for label in UNIFIED {
                let mailbox = Mailbox::Label {
                    account_id: account.id,
                    label_id: label.into(),
                    name: account_label_name(label).into(),
                };
                self.add_mailbox(mailbox, account_label_name(label), mailbox_icon(label), 1);
            }
            for folder in Folder::ALL {
                let mailbox = Mailbox::Folder {
                    account_id: Some(account.id),
                    folder,
                };
                self.add_mailbox(mailbox, folder.name(), folder.icon(), 1);
            }
            let mut user: Vec<&Label> = labels
                .iter()
                .filter(|l| l.kind == LabelKind::User)
                .collect();
            user.sort_by_key(|l| l.name.to_lowercase());
            for label in user {
                // Gmail nests labels with slashes: "Work/Clients" sits under "Work".
                let depth = 1 + label.name.matches('/').count() as u32;
                let leaf = label.name.rsplit('/').next().unwrap_or(&label.name);
                let mailbox = Mailbox::Label {
                    account_id: account.id,
                    label_id: label.id.clone(),
                    name: label.name.replace('/', " › "),
                };
                let row = self.add_mailbox(mailbox, leaf, "mailrs-tag-symbolic", depth);
                label_menu(&row, account.id, &label.id);
            }
        }
        self.select(selected);
        self.apply_expansion();
        self.muted.set(false);
        let adjustment = self.scroller.vadjustment();
        glib::idle_add_local_once(move || adjustment.set_value(scrolled));
    }

    /// Adds a mailbox row. `depth` indents it: 0 for the unified views, 1
    /// for an account's mailboxes, and one more per level of label nesting.
    fn add_mailbox(&self, mailbox: Mailbox, name: &str, icon: &str, depth: u32) -> gtk::ListBoxRow {
        let content = gtk::Box::builder()
            .spacing(12)
            .margin_start(18 * depth as i32)
            .css_classes(["mailbox-row"])
            .build();
        content.append(&gtk::Image::from_icon_name(icon));
        content.append(
            &gtk::Label::builder()
                .label(name)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::End)
                .build(),
        );
        let count = gtk::Label::builder()
            .css_classes(["count"])
            .visible(false)
            .build();
        content.append(&count);
        let row = gtk::ListBoxRow::builder()
            .child(&content)
            .visible(mailbox != Mailbox::Scheduled)
            .build();
        if takes_mail(&mailbox) {
            let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
            let (on_drop, dest) = (Rc::clone(&self.on_drop), mailbox.clone());
            target.connect_drop(move |_, value, _, _| {
                value.get::<String>().is_ok_and(|v| v == DRAG_MAIL) && on_drop(dest.clone())
            });
            row.add_controller(target);
        }
        self.list.append(&row);
        self.rows.borrow_mut().push(Row {
            row: row.clone(),
            mailbox,
            count,
        });
        row
    }

    /// Selects `mailbox` without reporting it as a user choice.
    pub fn select(&self, mailbox: &Mailbox) {
        let was_muted = self.muted.replace(true);
        let rows = self.rows.borrow();
        let row = rows.iter().find(|r| &r.mailbox == mailbox).or(rows.first());
        self.list.select_row(row.map(|r| &r.row));
        self.muted.set(was_muted);
    }

    /// Deselects everything, as when showing search results.
    pub fn clear_selection(&self) {
        let was_muted = self.muted.replace(true);
        self.list.unselect_all();
        self.muted.set(was_muted);
    }

    pub fn mailboxes(&self) -> Vec<Mailbox> {
        self.rows
            .borrow()
            .iter()
            .map(|r| r.mailbox.clone())
            .collect()
    }

    /// Unread counts for inboxes, totals for drafts and Send Later.
    pub fn set_counts(&self, counts: &HashMap<Mailbox, i64>) {
        let selected = self.list.selected_row();
        for row in self.rows.borrow().iter() {
            let count = counts.get(&row.mailbox).copied().unwrap_or(0);
            if row.mailbox == Mailbox::Scheduled {
                // Send Later appears only while something waits in it.
                row.row
                    .set_visible(count > 0 || selected.as_ref() == Some(&row.row));
            }
            let is_drafts = matches!(&row.mailbox, Mailbox::Unified("DRAFT") | Mailbox::Scheduled)
                || matches!(&row.mailbox, Mailbox::Label { label_id, .. } if label_id == "DRAFT");
            let shown = count > 0 && (row.mailbox.counts_unread() || is_drafts);
            row.count.set_visible(shown);
            row.count.set_label(&count.to_string());
            if row.mailbox.counts_unread() {
                row.count.add_css_class("unread");
            } else {
                row.count.remove_css_class("unread");
            }
        }
        for heading in self.headings.borrow().iter() {
            let inbox = Mailbox::Label {
                account_id: heading.account_id,
                label_id: "INBOX".into(),
                name: "Inbox".into(),
            };
            heading
                .count
                .set_label(&counts.get(&inbox).copied().unwrap_or(0).to_string());
        }
        self.apply_expansion();
    }
}

/// What a dragged set of list rows carries. The rows themselves stay with
/// the thread list.
pub const DRAG_MAIL: &str = "mailrs-mail";

/// Mailboxes mail can be moved into.
fn takes_mail(mailbox: &Mailbox) -> bool {
    match mailbox {
        Mailbox::Unified(label) => matches!(*label, "INBOX" | "STARRED"),
        Mailbox::Label { label_id, .. } => !matches!(label_id.as_str(), "SENT" | "DRAFT"),
        Mailbox::Folder { .. } => true,
        Mailbox::Search { .. } | Mailbox::Scheduled => false,
    }
}

/// Rename and Delete on a right click or long press of a label row.
fn label_menu(row: &gtk::ListBoxRow, account_id: AccountId, label_id: &str) {
    let menu = gio::Menu::new();
    let target = (account_id, label_id.to_string()).to_variant();
    for (text, action) in [
        ("Rename…", "win.label-rename"),
        ("Delete…", "win.label-delete"),
    ] {
        let item = gio::MenuItem::new(Some(text), None);
        item.set_action_and_target_value(Some(action), Some(&target));
        menu.append_item(&item);
    }
    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    popover.set_parent(row);
    let show = {
        let popover = popover.clone();
        move |x: f64, y: f64| {
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

fn heading(account: &Account) -> (gtk::ListBoxRow, gtk::Image, gtk::Label) {
    let content = gtk::Box::builder()
        .spacing(8)
        .css_classes(["sidebar-heading"])
        .build();
    let chevron = gtk::Image::builder()
        .icon_name("pan-down-symbolic")
        .css_classes(["dim-label"])
        .build();
    content.append(&chevron);
    let dot = gtk::Box::builder()
        .valign(gtk::Align::Center)
        .css_classes([
            "account-dot",
            &format!("account-{}", account_color_index(account.id)),
        ])
        .build();
    content.append(&dot);
    content.append(
        &gtk::Label::builder()
            .label(&account.email)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::Middle)
            .css_classes(["email"])
            .tooltip_text(&account.email)
            .build(),
    );
    let status = match account.state {
        AccountState::NeedsReauth => {
            Some(("dialog-warning-symbolic", "Sign in again to keep syncing"))
        }
        AccountState::Offline => Some(("network-offline-symbolic", "Offline")),
        AccountState::BackingOff => Some((
            "network-offline-symbolic",
            "Gmail is not responding; retrying",
        )),
        AccountState::Bootstrapping => Some(("mail-send-receive-symbolic", "Downloading mail")),
        AccountState::Ok => None,
    };
    let count = gtk::Label::builder()
        .css_classes(["count", "unread"])
        .visible(false)
        .build();
    content.append(&count);
    if let Some((icon, tip)) = status {
        let image = gtk::Image::builder()
            .icon_name(icon)
            .tooltip_text(tip)
            .build();
        if account.state == AccountState::NeedsReauth {
            image.add_css_class("warning");
        } else {
            image.add_css_class("dim-label");
        }
        content.append(&image);
    }
    let menu = gio::Menu::new();
    let item = |label: &str, action: &str| {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(Some(action), Some(&account.id.to_variant()));
        item
    };
    menu.append_item(&item("Check for Mail", "win.account-check"));
    let settings = gio::Menu::new();
    settings.append_item(&item("Automatic Reply…", "win.account-vacation"));
    settings.append_item(&item("Signature…", "win.account-signature"));
    settings.append_item(&item("New Label…", "win.account-new-label"));
    menu.append_section(None, &settings);
    let access = gio::Menu::new();
    access.append_item(&item("Sign In Again…", "win.account-reconnect"));
    menu.append_section(None, &access);
    let danger = gio::Menu::new();
    danger.append_item(&item("Remove Account…", "win.account-remove"));
    menu.append_section(None, &danger);
    content.append(
        &gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&menu)
            .css_classes(["flat", "circular"])
            .valign(gtk::Align::Center)
            .tooltip_text("Account options")
            .build(),
    );
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .selectable(false)
        .activatable(true)
        .build();
    row.update_property(&[gtk::accessible::Property::Label(&format!(
        "{}, show or hide mailboxes",
        account.email
    ))]);
    (row, chevron, count)
}
