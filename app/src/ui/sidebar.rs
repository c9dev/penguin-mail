//! Mailboxes: the unified views, then one section per account.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_domain::{
    Account, AccountId, AccountState, FlagColor, Folder, Label, LabelKind,
};
use mailrs_sync::Offers;

use super::{FolderLook, LABEL_COLORS, Mailbox, Standard, describe, label_color_name};
use crate::format::{PALETTE, account_color_index, palette_name};
use crate::offered::Filing;

struct Row {
    row: gtk::ListBoxRow,
    mailbox: Mailbox,
    /// The mailbox's name, kept so the spoken name can be built again
    /// whenever the count beside it changes.
    name: String,
    count: gtk::Label,
}

struct Heading {
    row: gtk::ListBoxRow,
    account_id: AccountId,
    /// What the heading calls the account: its name, or its address.
    name: String,
    chevron: gtk::Image,
    count: gtk::Label,
    /// The row's own Rules, Hide My Email and Automatic Reply actions,
    /// gated again by [`Sidebar::regate`] when the account starts.
    actions: gio::SimpleActionGroup,
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
        // An open account's mailboxes run straight into the next heading,
        // so that heading gets space above it. Closed accounts stay flush,
        // on the same pitch as the mailboxes.
        let mut after_open = false;
        for heading in self.headings.borrow().iter() {
            let open = self.is_expanded(heading.account_id);
            if after_open {
                heading.row.add_css_class("after-open");
            } else {
                heading.row.remove_css_class("after-open");
            }
            after_open = open;
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

    /// Sets each account heading's own Rules, Hide My Email and Automatic
    /// Reply actions from what `offers` says now, without rebuilding
    /// anything else. `read_accounts` calls this every time, even while a
    /// search on screen skips the rest of a rebuild, so an account that
    /// starts mid-search does not leave its menu gated at "everything".
    pub fn regate(&self, offers: impl Fn(AccountId) -> Offers) {
        for heading in self.headings.borrow().iter() {
            for (name, enabled) in crate::offered::account_menu_actions(offers(heading.account_id))
            {
                if let Some(action) = heading
                    .actions
                    .lookup_action(name)
                    .and_downcast::<gio::SimpleAction>()
                {
                    action.set_enabled(enabled);
                }
            }
        }
    }

    /// Rebuilds every row. `selected` is kept selected when it still exists.
    /// Rebuilds every row. `vips` lists VIPs by address and name. `offers`
    /// says what each account offers, which words its menu.
    pub fn rebuild(
        &self,
        accounts: &[(Account, Vec<Label>)],
        extras: &Extras,
        selected: &Mailbox,
        offers: impl Fn(AccountId) -> Offers,
    ) {
        let vips = &extras.vips;
        let mut label_rules = String::new();
        // Keep the scroll position; label changes rebuild every row.
        let scrolled = self.scroller.vadjustment().value();
        self.muted.set(true);
        self.list.remove_all();
        self.rows.borrow_mut().clear();
        self.headings.borrow_mut().clear();
        for which in Standard::ALL {
            self.add_mailbox(
                Mailbox::Unified(which),
                &which.unified_name(),
                which.icon(),
                0,
            );
            if which == Standard::Inbox && !vips.is_empty() {
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
            if which == Standard::Flagged {
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
            "penguin-mail-outbox-symbolic",
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
        if !accounts.is_empty() {
            // The rows above list every account at once. This says what
            // the ones below are, rather than leaving a reader to work it
            // out from the addresses.
            self.list.append(&section_title(&gettext("Accounts")));
        }
        for (account, labels) in accounts {
            let shown = extras.names.get(&account.id);
            let account_offers = offers(account.id);
            let (row, chevron, count, actions) = heading(account, shown, account_offers);
            self.list.append(&row);
            self.headings.borrow_mut().push(Heading {
                row,
                account_id: account.id,
                name: shown.unwrap_or(&account.email).clone(),
                chevron,
                count,
                actions,
            });
            for which in Standard::ALL {
                let mailbox = Mailbox::Standard {
                    account_id: account.id,
                    which,
                };
                self.add_mailbox(mailbox, &which.name(), which.icon(), 1);
            }
            for folder in Folder::ALL {
                let mailbox = Mailbox::Folder {
                    account_id: Some(account.id),
                    folder,
                };
                self.add_mailbox(mailbox, &folder.name(), folder.icon(), 1);
            }
            // Gmail nests labels with slashes, and an IMAP server's folder
            // names reach the store with slashes too: "Work/Clients" sits
            // under "Work".
            for entry in label_rows(labels) {
                let label = entry.label;
                let mailbox = Mailbox::Label {
                    account_id: account.id,
                    label_id: label.id.clone(),
                    name: label.name.replace('/', " › "),
                };
                if !entry.opens {
                    self.add_group(mailbox, entry.leaf, entry.depth);
                    continue;
                }
                let row =
                    self.add_mailbox(mailbox, entry.leaf, label_icon(account_offers), entry.depth);
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
        self.add_row(mailbox, name, icon, depth, true)
    }

    /// Adds a row for a group, a server folder that holds only other
    /// folders. It indents and closes with its account like a mailbox, so
    /// the folders under it nest, but nothing selects or opens it and it
    /// takes no dropped mail.
    fn add_group(&self, mailbox: Mailbox, name: &str, depth: u32) {
        let row = self.add_row(mailbox, name, "folder-symbolic", depth, false);
        row.set_tooltip_text(Some(&gettext("Holds folders, not mail")));
    }

    /// Adds a row. `opens` is false for a row that only groups others.
    fn add_row(
        &self,
        mailbox: Mailbox,
        name: &str,
        icon: &str,
        depth: u32,
        opens: bool,
    ) -> gtk::ListBoxRow {
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
            .selectable(opens)
            .activatable(opens)
            .build();
        if opens && takes_mail(&mailbox) {
            let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
            let (on_drop, dest) = (Rc::clone(&self.on_drop), mailbox.clone());
            target.connect_drop(move |_, value, _, _| {
                value.get::<String>().is_ok_and(|v| v == DRAG_MAIL) && on_drop(dest.clone())
            });
            row.add_controller(target);
        }
        super::name(&row, name);
        self.list.append(&row);
        self.rows.borrow_mut().push(Row {
            row: row.clone(),
            mailbox,
            name: name.to_string(),
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
                // Send Later, Remind Me, Follow Up and each flag colour appear
                // only while in use.
                row.row
                    .set_visible(count > 0 || selected.as_ref() == Some(&row.row));
            }
            let is_drafts = row.mailbox.standard() == Some(Standard::Drafts)
                || matches!(
                    &row.mailbox,
                    Mailbox::Scheduled
                        | Mailbox::Outbox
                        | Mailbox::Reminders
                        | Mailbox::FollowUp
                        | Mailbox::Flag(_)
                );
            let shown = count > 0 && (row.mailbox.counts_unread() || is_drafts);
            row.count.set_visible(shown);
            row.count.set_label(&count.to_string());
            super::name(
                &row.row,
                &mailbox_row_name(
                    &row.name,
                    if shown { count } else { 0 },
                    row.mailbox.counts_unread(),
                ),
            );
            if row.mailbox.counts_unread() {
                row.count.add_css_class("unread");
            } else {
                row.count.remove_css_class("unread");
            }
        }
        for heading in self.headings.borrow().iter() {
            let inbox = Mailbox::Standard {
                account_id: heading.account_id,
                which: Standard::Inbox,
            };
            let unread = counts.get(&inbox).copied().unwrap_or(0);
            heading.count.set_label(&unread.to_string());
            describe(
                &heading.row,
                &heading_row_name(&heading.name, unread),
                &gettext("Show or hide this account's mailboxes"),
            );
        }
        self.apply_expansion();
    }
}

/// What a dragged set of list rows carries. The rows themselves stay with
/// the thread list.
pub const DRAG_MAIL: &str = "mailrs-mail";

/// What a mailbox row says out loud. The badge at its end is a bare
/// number on screen, so the name takes it in and says what it counts. A
/// row whose badge is hidden says only its name.
fn mailbox_row_name(mailbox: &str, count: i64, unread: bool) -> String {
    if count <= 0 {
        return mailbox.to_string();
    }
    let number = count.to_string();
    let values = [("mailbox", mailbox), ("count", number.as_str())];
    match unread {
        true => fill_plural(
            "{mailbox}, {count} unread message",
            "{mailbox}, {count} unread messages",
            count as usize,
            &values,
        ),
        false => fill_plural(
            "{mailbox}, {count} message",
            "{mailbox}, {count} messages",
            count as usize,
            &values,
        ),
    }
}

/// What an account heading says out loud: the account, and the unread
/// mail behind it while the section is closed.
fn heading_row_name(account: &str, unread: i64) -> String {
    if unread <= 0 {
        return account.to_string();
    }
    let number = unread.to_string();
    fill_plural(
        "{account}, {count} unread message",
        "{account}, {count} unread messages",
        unread as usize,
        &[("account", account), ("count", number.as_str())],
    )
}

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
    let row = gtk::ListBoxRow::builder()
        .child(
            &gtk::Label::builder()
                .label(text)
                .xalign(0.0)
                .css_classes(["sidebar-section", "dim-label", "caption-heading"])
                .build(),
        )
        .selectable(false)
        .activatable(false)
        .build();
    // The list puts the label inside a row of its own, and the row is
    // what a screen reader reaches, so the words have to be on it too or
    // the section announces as nothing.
    row.set_accessible_role(gtk::AccessibleRole::RowHeader);
    crate::ui::name(&row, text);
    row
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
        Mailbox::Scheduled | Mailbox::Reminders | Mailbox::FollowUp | Mailbox::Flag(_)
    )
}

/// The icon for a folder row a person can open: the tag Gmail's labels
/// wear, since mail there can carry several at once, or the plain folder
/// icon the sidebar gives a group once the account keeps mail in one
/// place at a time.
fn label_icon(offers: Offers) -> &'static str {
    if offers.labels {
        "penguin-mail-tag-symbolic"
    } else {
        "folder-symbolic"
    }
}

/// Mailboxes mail can be moved into.
fn takes_mail(mailbox: &Mailbox) -> bool {
    match mailbox {
        Mailbox::Unified(which) | Mailbox::Standard { which, .. } => {
            matches!(which, Standard::Inbox | Standard::Flagged | Standard::Muted)
        }
        Mailbox::Label { .. } => true,
        Mailbox::Folder { .. } => true,
        Mailbox::Flag(_) => true,
        Mailbox::Search { .. }
        | Mailbox::Scheduled
        | Mailbox::Outbox
        | Mailbox::Reminders
        | Mailbox::FollowUp
        | Mailbox::Vips { .. }
        | Mailbox::Set { .. }
        | Mailbox::Smart(_) => false,
    }
}

/// One of an account's own labels or folders as the sidebar lists it.
#[derive(Debug, PartialEq, Eq)]
struct LabelRow<'a> {
    label: &'a Label,
    /// The part of the name after the last slash.
    leaf: &'a str,
    /// 1 at the top, one more for each slash in the name.
    depth: u32,
    /// False for a group, which holds only other folders.
    opens: bool,
}

/// An account's labels and folders by name, ignoring case, with the
/// groups that hold folders, so "Work/Clients" sits under "Work" even
/// where the server keeps no mail in "Work".
fn label_rows(labels: &[Label]) -> Vec<LabelRow<'_>> {
    let mut rows: Vec<LabelRow<'_>> = labels
        .iter()
        .filter(|l| matches!(l.kind, LabelKind::User | LabelKind::Group))
        .map(|label| LabelRow {
            label,
            leaf: label.name.rsplit('/').next().unwrap_or(&label.name),
            depth: 1 + label.name.matches('/').count() as u32,
            opens: label.kind == LabelKind::User,
        })
        .collect();
    rows.sort_by_key(|row| row.label.name.to_lowercase());
    rows
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
    super::name_menu_items(&popover);
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

/// The settings section of an account's menu, as words and actions. Every
/// setting is listed; the ones a server may lack are the account's own
/// actions under the `account` prefix, which `heading` turns off from
/// what the account `offers`, and their items hide while they are off.
fn account_settings(offers: Offers) -> Vec<(String, &'static str)> {
    vec![
        (gettext("Automatic Reply…"), "account.vacation"),
        (gettext("Signature…"), "win.account-signature"),
        (gettext("Rules…"), "account.rules"),
        (gettext("Hide My Email…"), "account.hide-my-email"),
        (Filing::of([offers]).new_item(), "win.account-new-label"),
    ]
}

fn heading(
    account: &Account,
    name: Option<&String>,
    offers: Offers,
) -> (gtk::ListBoxRow, gtk::Image, gtk::Label, gio::SimpleActionGroup) {
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
    let status = status_of(account);
    let count = gtk::Label::builder()
        .css_classes(["count", "unread"])
        .visible(false)
        .build();
    content.append(&count);
    if let Some((icon, tip)) = status {
        let image = gtk::Image::builder()
            .icon_name(icon)
            .tooltip_text(&tip)
            .build();
        super::name(&image, &tip);
        if matches!(
            account.state,
            AccountState::NeedsReauth | AccountState::Stopped
        ) {
            image.add_css_class("warning");
        } else {
            image.add_css_class("dim-label");
        }
        content.append(&image);
    }
    let menu = gio::Menu::new();
    let item = |label: &str, action: &str| {
        let item = gio::MenuItem::new(Some(label), None);
        if action.starts_with("account.") {
            // The row's own action knows its account, so it takes no
            // target, and its item hides while the account lacks it.
            item.set_action_and_target_value(Some(action), None);
            item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
        } else {
            item.set_action_and_target_value(Some(action), Some(&account.id.to_variant()));
        }
        item
    };
    menu.append_item(&item(&gettext("Check for Mail"), "win.account-check"));
    let settings = gio::Menu::new();
    for (label, action) in account_settings(offers) {
        settings.append_item(&item(&label, action));
    }
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
    let options = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .menu_model(&menu)
        .css_classes(["flat", "circular"])
        .valign(gtk::Align::Center)
        .tooltip_text(gettext("Account options"))
        .build();
    super::name(
        &options,
        &fill(
            &gettext("Options for {account}"),
            &[("account", name.unwrap_or(&account.email))],
        ),
    );
    super::name_menu_items_of(&options);
    content.append(&options);
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .css_classes(["account-heading"])
        .selectable(false)
        .activatable(true)
        .build();
    // One `win.` action serves every account's menu, so it cannot be off
    // for one account. The row holds this account's own Rules, Hide My
    // Email and Automatic Reply, each off when the account lacks it;
    // `Sidebar::regate` sets them again once the account's offers change,
    // whether or not the row itself gets rebuilt.
    let own = gio::SimpleActionGroup::new();
    for (name, enabled) in crate::offered::account_menu_actions(offers) {
        let action = gio::SimpleAction::new(name, None);
        action.set_enabled(enabled);
        let (weak, account_id) = (row.downgrade(), account.id);
        action.connect_activate(move |_, _| {
            let Some(row) = weak.upgrade() else {
                return;
            };
            let target = format!("win.account-{name}");
            if let Err(err) = row.activate_action(&target, Some(&account_id.to_variant())) {
                tracing::warn!(error = %err, action = %target, "could not open an account setting");
            }
        });
        own.add_action(&action);
    }
    row.insert_action_group("account", Some(&own));
    describe(
        &row,
        &heading_row_name(name.unwrap_or(&account.email), 0),
        &gettext("Show or hide this account's mailboxes"),
    );
    (row, chevron, count, own)
}

/// The icon and the words beside an account's name for its state, or
/// nothing while it syncs as it should.
fn status_of(account: &Account) -> Option<(&'static str, String)> {
    match account.state {
        AccountState::NeedsReauth => Some((
            "dialog-warning-symbolic",
            gettext("Sign in again to keep syncing"),
        )),
        AccountState::Offline => Some(("network-offline-symbolic", gettext("Offline"))),
        AccountState::BackingOff => Some((
            "network-offline-symbolic",
            fill(
                &gettext("{provider} is not responding; retrying"),
                &[("provider", &mailrs_discover::resolved_provider_name(account.provider_name()))],
            ),
        )),
        AccountState::Bootstrapping => {
            Some(("mail-send-receive-symbolic", gettext("Downloading mail")))
        }
        AccountState::Stopped => Some((
            "dialog-warning-symbolic",
            gettext("Syncing stopped after an error; restart Penguin Mail to try again"),
        )),
        AccountState::Ok => None,
    }
}

#[cfg(test)]
mod tests {
    use mailrs_domain::{Account, AccountState, Provider};

    use super::status_of;
    use super::{
        Label, LabelKind, LabelRow, Mailbox, Standard, heading_row_name, label_icon, label_rows,
        mailbox_row_name, takes_mail,
    };

    use super::{Offers, account_settings};

    #[test]
    fn a_label_account_opens_a_folder_row_under_a_tag() {
        assert_eq!(
            label_icon(Offers { labels: true, ..Offers::EVERYTHING }),
            "penguin-mail-tag-symbolic"
        );
    }

    #[test]
    fn a_folder_account_opens_a_folder_row_under_a_folder() {
        assert_eq!(
            label_icon(Offers { labels: false, ..Offers::EVERYTHING }),
            "folder-symbolic"
        );
    }

    #[test]
    fn an_account_that_backs_off_names_who_is_not_answering() {
        let gmail = Account {
            id: 1,
            email: "me@gmail.com".into(),
            state: AccountState::BackingOff,
            provider: Provider::Gmail,
            provider_name: None,
        };
        let fastmail = Account {
            provider: Provider::Imap,
            provider_name: Some("Fastmail".into()),
            ..gmail.clone()
        };
        let by_domain = Account {
            provider_name: Some("fastmail.com".into()),
            ..fastmail.clone()
        };
        let said = |account: &Account| status_of(account).map(|(_, said)| said);
        assert_eq!(said(&gmail).as_deref(), Some("Gmail is not responding; retrying"));
        assert_eq!(said(&fastmail).as_deref(), Some("Fastmail is not responding; retrying"));
        assert_eq!(
            said(&by_domain).as_deref(),
            Some("Fastmail is not responding; retrying"),
            "an account saved under its domain still shows its real provider"
        );
    }

    #[test]
    fn an_account_that_syncs_shows_no_state() {
        let fine = Account {
            id: 1,
            email: "me@gmail.com".into(),
            state: AccountState::Ok,
            provider: Provider::Gmail,
            provider_name: None,
        };
        assert_eq!(status_of(&fine), None);
    }

    fn actions(offers: Offers) -> Vec<&'static str> {
        account_settings(offers).into_iter().map(|(_, action)| action).collect()
    }

    #[test]
    fn a_gmail_account_menu_keeps_every_setting() {
        assert_eq!(
            actions(Offers::EVERYTHING),
            [
                "account.vacation",
                "win.account-signature",
                "account.rules",
                "account.hide-my-email",
                "win.account-new-label",
            ]
        );
    }

    #[test]
    fn an_account_menu_lists_every_setting_and_its_own_actions_hide_what_it_lacks() {
        let bare = Offers {
            rules: false,
            auto_reply: false,
            ..Offers::EVERYTHING
        };
        // The items stay in the model; the account's own actions are off,
        // and an item bound to an action that is off hides.
        assert_eq!(actions(bare), actions(Offers::EVERYTHING));
    }

    #[test]
    fn a_mailbox_row_reads_its_badge_as_part_of_the_row() {
        assert_eq!(
            mailbox_row_name("Inbox", 12, true),
            "Inbox, 12 unread messages"
        );
        assert_eq!(
            mailbox_row_name("Inbox", 1, true),
            "Inbox, 1 unread message"
        );
        assert_eq!(mailbox_row_name("Drafts", 3, false), "Drafts, 3 messages");
        assert_eq!(mailbox_row_name("Drafts", 0, false), "Drafts");
    }

    #[test]
    fn an_account_heading_reads_the_mail_behind_a_closed_section() {
        assert_eq!(heading_row_name("ann@example.com", 0), "ann@example.com");
        assert_eq!(heading_row_name("Work", 1), "Work, 1 unread message");
        assert_eq!(heading_row_name("Work", 4), "Work, 4 unread messages");
    }

    #[test]
    fn inbox_flagged_and_muted_take_dropped_mail() {
        for which in [Standard::Inbox, Standard::Flagged, Standard::Muted] {
            assert!(takes_mail(&Mailbox::Unified(which)), "{which:?}");
            assert!(takes_mail(&Mailbox::Standard { account_id: 1, which }), "{which:?}");
        }
        for which in [Standard::Sent, Standard::Drafts] {
            assert!(!takes_mail(&Mailbox::Unified(which)), "{which:?}");
            assert!(!takes_mail(&Mailbox::Standard { account_id: 1, which }), "{which:?}");
        }
    }

    #[test]
    fn a_group_nests_its_folders_but_opens_nothing() {
        let label = |name: &str, kind| Label {
            account_id: 1,
            id: name.to_string(),
            name: name.to_string(),
            kind,
            color: None,
        };
        let labels = [
            label("Work/Clients", LabelKind::User),
            label("INBOX", LabelKind::System),
            label("Work", LabelKind::Group),
            label("receipts", LabelKind::User),
        ];
        let rows: Vec<(&str, u32, bool)> = label_rows(&labels)
            .iter()
            .map(|row: &LabelRow<'_>| (row.leaf, row.depth, row.opens))
            .collect();
        assert_eq!(
            rows,
            [("receipts", 1, true), ("Work", 1, false), ("Clients", 2, true)]
        );
    }
}
