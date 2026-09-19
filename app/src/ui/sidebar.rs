//! Mailboxes: the unified views, then one section per account.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, pango};
use mailrs_domain::{Account, AccountId, AccountState, Label, LabelKind};

use super::{Mailbox, UNIFIED, account_label_name, mailbox_icon, unified_name};
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
    rows: RefCell<Vec<Row>>,
    headings: RefCell<Vec<Heading>>,
    /// Accounts whose sections the user expanded or collapsed.
    expanded: RefCell<HashMap<AccountId, bool>>,
    /// Whether sections start open. Unset means open only with one account.
    pub start_expanded: Cell<Option<bool>>,
    muted: Cell<bool>,
}

impl Sidebar {
    pub fn new(on_select: impl Fn(Mailbox) + 'static) -> Rc<Sidebar> {
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
            rows: RefCell::new(Vec::new()),
            headings: RefCell::new(Vec::new()),
            expanded: RefCell::new(HashMap::new()),
            start_expanded: Cell::new(None),
            muted: Cell::new(false),
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
                self.add_mailbox(mailbox, leaf, "mailrs-tag-symbolic", depth);
            }
        }
        self.select(selected);
        self.apply_expansion();
        self.muted.set(false);
    }

    /// Adds a mailbox row. `depth` indents it: 0 for the unified views, 1
    /// for an account's mailboxes, and one more per level of label nesting.
    fn add_mailbox(&self, mailbox: Mailbox, name: &str, icon: &str, depth: u32) {
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
        let row = gtk::ListBoxRow::builder().child(&content).build();
        self.list.append(&row);
        self.rows.borrow_mut().push(Row {
            row,
            mailbox,
            count,
        });
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

    /// Unread counts for inboxes, totals for drafts.
    pub fn set_counts(&self, counts: &HashMap<Mailbox, i64>) {
        for row in self.rows.borrow().iter() {
            let count = counts.get(&row.mailbox).copied().unwrap_or(0);
            let is_drafts = matches!(&row.mailbox, Mailbox::Unified("DRAFT"))
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
    menu.append_item(&item("Sign In Again…", "win.account-reconnect"));
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
