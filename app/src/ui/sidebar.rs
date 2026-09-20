//! Mailboxes: the unified views, then one section per account.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{
    Account, AccountId, AccountState, FlagColor, Folder, Label, LabelKind, system_label,
};

use super::{
    FolderLook, LABEL_COLORS, Mailbox, UNIFIED, account_label_name, label_color_name, mailbox_icon,
    unified_name,
};
use crate::format::{PALETTE, account_color_index, palette_name};

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
    /// Colours for label icons, rewritten on each rebuild.
    label_css: gtk::CssProvider,
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
            .title_widget(&adw::WindowTitle::new(&gettext("Mailboxes"), ""))
            .build();
        let add_account = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("list-add-symbolic")
                    .label(gettext("Add Account"))
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
            label_css: {
                let css = gtk::CssProvider::new();
                if let Some(display) = gdk::Display::default() {
                    gtk::style_context_add_provider_for_display(
                        &display,
                        &css,
                        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                    );
                }
                css
            },
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
    /// Rebuilds every row. `vips` lists VIPs by address and name.
    pub fn rebuild(&self, accounts: &[(Account, Vec<Label>)], extras: &Extras, selected: &Mailbox) {
        let vips = &extras.vips;
        let mut label_rules = String::new();
        // Keep the scroll position; label changes rebuild every row.
        let scrolled = self.scroller.vadjustment().value();
        self.muted.set(true);
        self.list.remove_all();
        self.rows.borrow_mut().clear();
        self.headings.borrow_mut().clear();
        for label in UNIFIED {
            self.add_mailbox(
                Mailbox::Unified(label),
                &unified_name(label),
                mailbox_icon(label),
                0,
            );
            if label == system_label::INBOX && !vips.is_empty() {
                let everyone = Mailbox::Vips {
                    emails: vips.iter().map(|(e, _)| e.clone()).collect(),
                    name: gettext("VIPs"),
                };
                self.add_mailbox(everyone, &gettext("VIPs"), "starred-symbolic", 0);
                for (email, name) in vips {
                    let person = Mailbox::Vips {
                        emails: vec![email.clone()],
                        name: name.clone(),
                    };
                    let row = self.add_mailbox(person, name, "avatar-default-symbolic", 1);
                    let menu = gio::Menu::new();
                    let item = gio::MenuItem::new(Some(&gettext("Remove from VIPs")), None);
                    item.set_action_and_target_value(
                        Some("win.vip-remove"),
                        Some(&email.to_variant()),
                    );
                    menu.append_item(&item);
                    context_menu(&row, &menu);
                }
            }
            if label == system_label::STARRED {
                // One row per flag colour in use, as Apple Mail shows them.
                for color in FlagColor::ALL {
                    let row = self.add_mailbox(
                        Mailbox::Flag(color),
                        &color.name(),
                        "penguin-mail-flag-symbolic",
                        1,
                    );
                    if let Some(icon) = row.child().and_then(|c| c.first_child()) {
                        icon.add_css_class(&format!("flag-{}", color.as_str()));
                    }
                }
            }
        }
        self.add_mailbox(
            Mailbox::Outbox,
            &gettext("Outbox"),
            "mail-outbox-symbolic",
            0,
        );
        self.add_mailbox(
            Mailbox::Scheduled,
            &gettext("Send Later"),
            "mail-send-symbolic",
            0,
        );
        self.add_mailbox(
            Mailbox::Reminders,
            &gettext("Remind Me"),
            "alarm-symbolic",
            0,
        );
        self.add_mailbox(
            Mailbox::FollowUp,
            &gettext("Follow Up"),
            "mail-reply-sender-symbolic",
            0,
        );
        for folder in Folder::ALL {
            let mailbox = Mailbox::Folder {
                account_id: None,
                folder,
            };
            self.add_mailbox(mailbox, &folder.name(), folder.icon(), 0);
        }
        if !extras.smart.is_empty() {
            self.list
                .append(&section_title(&gettext("Smart Mailboxes")));
            for smart in &extras.smart {
                let mailbox = Mailbox::Smart(smart.clone());
                let row = self.add_mailbox(mailbox, &smart.name, "folder-saved-search-symbolic", 0);
                smart_menu(&row, &smart.id);
            }
        }
        for (account, labels) in accounts {
            let (row, chevron, count) = heading(account, extras.names.get(&account.id));
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
                    name: account_label_name(label),
                };
                self.add_mailbox(mailbox, &account_label_name(label), mailbox_icon(label), 1);
            }
            for folder in Folder::ALL {
                let mailbox = Mailbox::Folder {
                    account_id: Some(account.id),
                    folder,
                };
                self.add_mailbox(mailbox, &folder.name(), folder.icon(), 1);
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
                let row = self.add_mailbox(mailbox, leaf, "penguin-mail-tag-symbolic", depth);
                if let Some(color) = label.color.as_deref().and_then(css_hex)
                    && let Some(icon) = row.child().and_then(|c| c.first_child())
                {
                    let class = format!("label-color-{color}");
                    label_rules.push_str(&format!(".{class} {{ color: #{color}; }}\n"));
                    icon.add_css_class(&class);
                }
                label_menu(&row, account.id, &label.id);
            }
        }
        self.label_css.load_from_string(&label_rules);
        self.select(selected);
        self.apply_expansion();
        self.muted.set(false);
        // The new rows get their height, and selecting a row scrolls to it,
        // a moment later; put the view back once that has happened.
        let adjustment = self.scroller.vadjustment();
        glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
            adjustment.set_value(scrolled);
        });
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
            .visible(!hidden_until_used(&mailbox))
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
            if hidden_until_used(&row.mailbox) {
                // The Outbox, Send Later and each flag colour appear
                // only while in use.
                row.row
                    .set_visible(count > 0 || selected.as_ref() == Some(&row.row));
            }
            let is_drafts = matches!(
                &row.mailbox,
                Mailbox::Unified(system_label::DRAFT)
                    | Mailbox::Scheduled
                    | Mailbox::Outbox
                    | Mailbox::Reminders
                    | Mailbox::FollowUp
                    | Mailbox::Flag(_)
            ) || matches!(&row.mailbox, Mailbox::Label { label_id, .. } if label_id == system_label::DRAFT);
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
                label_id: system_label::INBOX.into(),
                name: gettext("Inbox"),
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

/// What the sidebar shows besides accounts and their labels.
#[derive(Debug, Clone, Default)]
pub struct Extras {
    /// VIPs as address and name.
    pub vips: Vec<(String, String)>,
    /// Smart mailboxes in the user's order.
    pub smart: Vec<mailrs_domain::SmartMailbox>,
    /// Names shown instead of account addresses.
    pub names: HashMap<AccountId, String>,
}

/// A small heading between sections. It cannot be selected.
fn section_title(text: &str) -> gtk::ListBoxRow {
    gtk::ListBoxRow::builder()
        .child(
            &gtk::Label::builder()
                .label(text)
                .xalign(0.0)
                .css_classes(["sidebar-section", "dim-label", "caption-heading"])
                .build(),
        )
        .selectable(false)
        .activatable(false)
        .build()
}

/// Edit, move, and delete on a right click or long press of a smart mailbox.
fn smart_menu(row: &gtk::ListBoxRow, id: &str) {
    let menu = gio::Menu::new();
    let target = id.to_variant();
    let item = |text: &str, action: &str| {
        let item = gio::MenuItem::new(Some(text), None);
        item.set_action_and_target_value(Some(action), Some(&target));
        item
    };
    menu.append_item(&item(&gettext("Edit…"), "win.smart-edit"));
    let order = gio::Menu::new();
    order.append_item(&item(&gettext("Move Up"), "win.smart-up"));
    order.append_item(&item(&gettext("Move Down"), "win.smart-down"));
    menu.append_section(None, &order);
    let danger = gio::Menu::new();
    danger.append_item(&item(&gettext("Delete…"), "win.smart-delete"));
    menu.append_section(None, &danger);
    context_menu(row, &menu);
}

/// `#rrggbb` as six lower-case hex digits, safe inside a CSS class name.
fn css_hex(color: &str) -> Option<String> {
    let hex = color.trim().trim_start_matches('#').to_lowercase();
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(hex)
}

/// Rows that show only while they hold something.
fn hidden_until_used(mailbox: &Mailbox) -> bool {
    matches!(
        mailbox,
        Mailbox::Outbox
            | Mailbox::Scheduled
            | Mailbox::Reminders
            | Mailbox::FollowUp
            | Mailbox::Flag(_)
    )
}

/// Mailboxes mail can be moved into.
fn takes_mail(mailbox: &Mailbox) -> bool {
    match mailbox {
        Mailbox::Unified(label) => matches!(*label, system_label::INBOX | system_label::STARRED),
        Mailbox::Label { label_id, .. } => {
            !matches!(label_id.as_str(), system_label::SENT | system_label::DRAFT)
        }
        Mailbox::Folder { .. } => true,
        Mailbox::Flag(_) => true,
        Mailbox::Search { .. }
        | Mailbox::Scheduled
        | Mailbox::Outbox
        | Mailbox::Reminders
        | Mailbox::FollowUp
        | Mailbox::Vips { .. }
        | Mailbox::Smart(_) => false,
    }
}

/// Rename and Delete on a right click or long press of a label row.
fn label_menu(row: &gtk::ListBoxRow, account_id: AccountId, label_id: &str) {
    let menu = gio::Menu::new();
    let target = (account_id, label_id.to_string()).to_variant();
    let item = |text: &str, action: &str| {
        let item = gio::MenuItem::new(Some(text), None);
        item.set_action_and_target_value(Some(action), Some(&target));
        item
    };
    menu.append_item(&item(&gettext("Rename…"), "win.label-rename"));
    let colors = gio::Menu::new();
    for index in 0..LABEL_COLORS.len() {
        let entry = gio::MenuItem::new(Some(&label_color_name(index)), None);
        entry.set_action_and_target_value(
            Some("win.label-color"),
            Some(&(account_id, label_id.to_string(), index as i32).to_variant()),
        );
        colors.append_item(&entry);
    }
    menu.append_submenu(Some(&gettext("Color")), &colors);
    menu.append_item(&item(&gettext("Delete…"), "win.label-delete"));
    context_menu(row, &menu);
}

/// Opens `menu` at the pointer on a right click or long press of `row`.
fn context_menu(row: &gtk::ListBoxRow, menu: &gio::Menu) {
    let popover = gtk::PopoverMenu::from_model(Some(menu));
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

fn heading(account: &Account, name: Option<&String>) -> (gtk::ListBoxRow, gtk::Image, gtk::Label) {
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
            .label(name.map_or(account.email.as_str(), String::as_str))
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::Middle)
            .css_classes(["email"])
            .tooltip_text(&account.email)
            .build(),
    );
    let status = match account.state {
        AccountState::NeedsReauth => Some((
            "dialog-warning-symbolic",
            gettext("Sign in again to keep syncing"),
        )),
        AccountState::Offline => Some(("network-offline-symbolic", gettext("Offline"))),
        AccountState::BackingOff => Some((
            "network-offline-symbolic",
            gettext("Gmail is not responding; retrying"),
        )),
        AccountState::Bootstrapping => {
            Some(("mail-send-receive-symbolic", gettext("Downloading mail")))
        }
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
    menu.append_item(&item(&gettext("Check for Mail"), "win.account-check"));
    let settings = gio::Menu::new();
    settings.append_item(&item(&gettext("Automatic Reply…"), "win.account-vacation"));
    settings.append_item(&item(&gettext("Signature…"), "win.account-signature"));
    settings.append_item(&item(&gettext("Rules…"), "win.account-rules"));
    settings.append_item(&item(
        &gettext("Hide My Email…"),
        "win.account-hide-my-email",
    ));
    settings.append_item(&item(&gettext("New Label…"), "win.account-new-label"));
    menu.append_section(None, &settings);
    let look = gio::Menu::new();
    look.append_item(&item(&gettext("Rename…"), "win.account-rename"));
    let colors = gio::Menu::new();
    for index in 0..PALETTE.len() {
        let entry = gio::MenuItem::new(Some(&palette_name(index)), None);
        entry.set_action_and_target_value(
            Some("win.account-color"),
            Some(&(account.id, index as i32).to_variant()),
        );
        colors.append_item(&entry);
    }
    look.append_submenu(Some(&gettext("Color")), &colors);
    look.append_item(&item(&gettext("Move Up"), "win.account-up"));
    look.append_item(&item(&gettext("Move Down"), "win.account-down"));
    menu.append_section(None, &look);
    let access = gio::Menu::new();
    access.append_item(&item(&gettext("Sign In Again…"), "win.account-reconnect"));
    menu.append_section(None, &access);
    let danger = gio::Menu::new();
    danger.append_item(&item(&gettext("Remove Account…"), "win.account-remove"));
    menu.append_section(None, &danger);
    content.append(
        &gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&menu)
            .css_classes(["flat", "circular"])
            .valign(gtk::Align::Center)
            .tooltip_text(gettext("Account options"))
            .build(),
    );
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .selectable(false)
        .activatable(true)
        .build();
    let described = fill(
        &gettext("{account}, show or hide mailboxes"),
        &[("account", &account.email)],
    );
    row.update_property(&[gtk::accessible::Property::Label(&described)]);
    (row, chevron, count)
}
