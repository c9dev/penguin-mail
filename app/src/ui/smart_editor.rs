//! The dialog that creates or edits a smart mailbox.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use mailrs_domain::smart::{Condition, Field, MailboxChoice, SmartMailbox, UnreadCount};
use mailrs_domain::translate::gettext;

struct ConditionRow {
    row: adw::ActionRow,
    field: gtk::DropDown,
    value: gtk::Entry,
    _choices: Rc<Vec<(MailboxChoice, gtk::CheckButton)>>,
}

/// Shows the editor. `accounts` lists addresses for the scope menu;
/// `on_save` receives the finished mailbox.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    existing: Option<SmartMailbox>,
    accounts: Vec<String>,
    mailboxes: Vec<(String, Vec<String>)>,
    on_save: impl Fn(SmartMailbox) + 'static,
) {
    let editing = existing.is_some();
    let mailbox = existing.unwrap_or_else(|| SmartMailbox {
        unread_count: Default::default(),
        id: format!("smart-{}", mailrs_gmail::random_token(6)),
        name: String::new(),
        account: None,
        match_all: true,
        conditions: vec![Condition {
            field: Field::From,
            value: String::new(),
        }],
    });

    let name = adw::EntryRow::builder().title(gettext("Name")).build();
    name.set_text(&mailbox.name);
    let mut scopes = vec![gettext("All Accounts")];
    scopes.extend(accounts.iter().cloned());
    let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
    let scope = adw::ComboRow::builder()
        .title(gettext("Accounts"))
        .model(&gtk::StringList::new(&scope_refs))
        .selected(
            mailbox
                .account
                .as_ref()
                .and_then(|a| accounts.iter().position(|e| e.eq_ignore_ascii_case(a)))
                .map_or(0, |i| i as u32 + 1),
        )
        .build();
    let matching = adw::ComboRow::builder()
        .title(gettext("Match"))
        .model(&gtk::StringList::new(&[
            &gettext("All of the conditions"),
            &gettext("Any of the conditions"),
        ]))
        .selected(if mailbox.match_all { 0 } else { 1 })
        .build();
    let unread = adw::ComboRow::builder()
        .title(gettext("Unread count"))
        .model(&gtk::StringList::new(&[
            &gettext("Do not show"),
            &gettext("Fast, approximate"),
            &gettext("Slow, exact"),
        ]))
        .selected(match mailbox.unread_count {
            UnreadCount::Hidden => 0,
            UnreadCount::Local => 1,
            UnreadCount::Server => 2,
        })
        .build();
    let explain = |row: &adw::ComboRow| {
        row.set_subtitle(&match row.selected() {
            1 => gettext("Counts downloaded unread messages. Older mail may be missing."),
            2 => gettext("Counts unread messages on the server in the background. Some servers or conditions are unsupported."),
            _ => gettext("No unread count beside this smart mailbox."),
        });
    };
    explain(&unread);
    unread.connect_selected_notify(explain);
    let about = adw::PreferencesGroup::new();
    about.add(&name);
    about.add(&scope);
    about.add(&matching);
    about.add(&unread);

    let conditions = adw::PreferencesGroup::builder()
        .title(gettext("Conditions"))
        .build();
    let add = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name("list-add-symbolic")
                .label(gettext("Add Condition"))
                .build(),
        )
        .css_classes(["flat"])
        .build();
    conditions.set_header_suffix(Some(&add));
    let rows: Rc<RefCell<Vec<ConditionRow>>> = Rc::new(RefCell::new(Vec::new()));
    let roles = vec![
        ("role:inbox".to_string(), gettext("Inbox")),
        ("role:sent".to_string(), gettext("Sent")),
        ("role:drafts".to_string(), gettext("Drafts")),
        ("role:archive".to_string(), gettext("Archive")),
        ("role:junk".to_string(), gettext("Junk")),
        ("role:trash".to_string(), gettext("Trash")),
        ("role:all".to_string(), gettext("All Mail")),
    ];
    let mut choices = Vec::new();
    for (account, folders) in mailboxes {
        for (key, title) in &roles {
            choices.push((
                MailboxChoice::InAccount {
                    account: account.clone(),
                    mailbox: key.clone(),
                },
                title.clone(),
            ));
        }
        for folder in folders {
            let choice = MailboxChoice::InAccount {
                account: account.clone(),
                mailbox: format!("label:{folder}"),
            };
            if !choices.iter().any(|(known, _)| known == &choice) {
                choices.push((choice, folder));
            }
        }
    }
    let add_row = {
        let (conditions, rows) = (conditions.clone(), Rc::clone(&rows));
        let choices = choices.clone();
        Rc::new(move |condition: &Condition| {
            let labels: Vec<String> = Field::ALL.iter().map(|f| f.label()).collect();
            let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
            let field = gtk::DropDown::from_strings(&labels);
            field.set_valign(gtk::Align::Center);
            field.set_selected(
                Field::ALL
                    .iter()
                    .position(|f| *f == condition.field)
                    .unwrap_or(0) as u32,
            );
            let value = gtk::Entry::builder()
                .text(&condition.value)
                .valign(gtk::Align::Center)
                .hexpand(true)
                .sensitive(condition.field.takes_value())
                .visible(condition.field != Field::InMailbox)
                .build();
            let mut choices = choices.clone();
            let selected = if condition.field == Field::InMailbox {
                MailboxChoice::parse(&condition.value)
            } else {
                Vec::new()
            };
            for id in &selected {
                if !choices.iter().any(|(known, _)| known == id) {
                    let title = roles
                        .iter()
                        .find(|(key, _)| key == id.mailbox())
                        .map(|(_, title)| title.clone())
                        .unwrap_or_else(|| {
                            id.mailbox()
                                .strip_prefix("label:")
                                .unwrap_or(id.mailbox())
                                .to_string()
                        });
                    choices.push((id.clone(), title));
                }
            }
            let options = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(6)
                .margin_top(12)
                .margin_bottom(12)
                .margin_start(12)
                .margin_end(12)
                .build();
            let mut group = None;
            let checks: Rc<Vec<(MailboxChoice, gtk::CheckButton)>> = Rc::new(
                choices
                    .into_iter()
                    .map(|(id, title)| {
                        let account = id
                            .account()
                            .map(str::to_string)
                            .unwrap_or_else(|| gettext("All Accounts"));
                        if group.as_ref() != Some(&account) {
                            let heading = gtk::Label::builder()
                                .label(&account)
                                .xalign(0.0)
                                .margin_top(8)
                                .css_classes(["heading"])
                                .build();
                            options.append(&heading);
                            group = Some(account.clone());
                        }
                        let check = gtk::CheckButton::with_label(&title);
                        crate::ui::name(&check, &format!("{title} ({account})"));
                        check.set_tooltip_text(Some(&account));
                        check.set_active(selected.contains(&id));
                        options.append(&check);
                        (id, check)
                    })
                    .collect(),
            );
            let popover = gtk::Popover::builder()
                .child(
                    &gtk::ScrolledWindow::builder()
                        .min_content_width(320)
                        .max_content_width(480)
                        .propagate_natural_width(true)
                        .min_content_height(280)
                        .max_content_height(360)
                        .propagate_natural_height(true)
                        .child(&options)
                        .build(),
                )
                .build();
            let picker = gtk::MenuButton::builder()
                .label(gettext("Choose Mailboxes"))
                .popover(&popover)
                .build();
            picker.set_visible(condition.field == Field::InMailbox);
            crate::ui::name(&picker, &gettext("Choose Mailboxes"));
            for (_, check) in checks.iter() {
                let weak = Rc::downgrade(&checks);
                let chosen = value.clone();
                check.connect_toggled(move |_| {
                    let Some(checks) = weak.upgrade() else { return };
                    let ids: Vec<&MailboxChoice> = checks
                        .iter()
                        .filter(|(_, check)| check.is_active())
                        .map(|(id, _)| id)
                        .collect();
                    chosen.set_text(&serde_json::to_string(&ids).unwrap());
                });
            }
            let entry = value.clone();
            let button = picker.clone();
            let checkboxes = Rc::clone(&checks);
            let previous = Cell::new(condition.field);
            field.connect_selected_notify(move |field| {
                let chosen = Field::ALL[field.selected() as usize];
                entry.set_sensitive(chosen.takes_value());
                entry.set_visible(chosen != Field::InMailbox);
                button.set_visible(chosen == Field::InMailbox);
                if chosen == Field::InMailbox {
                    for (_, check) in checkboxes.iter() {
                        check.set_active(false);
                    }
                    entry.set_text("[]");
                } else if previous.get() == Field::InMailbox {
                    entry.set_text("");
                }
                previous.set(chosen);
            });
            let remove = gtk::Button::builder()
                .icon_name("list-remove-symbolic")
                .tooltip_text(gettext("Remove Condition"))
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            crate::ui::name(&field, &gettext("Condition"));
            crate::ui::name(&value, &gettext("Matches"));
            crate::ui::name(&remove, &gettext("Remove Condition"));
            let row = adw::ActionRow::new();
            row.add_prefix(&field);
            row.add_suffix(&value);
            row.add_suffix(&picker);
            row.add_suffix(&remove);
            conditions.add(&row);
            let (group, all, gone) = (conditions.clone(), Rc::clone(&rows), row.clone());
            remove.connect_clicked(move |_| {
                group.remove(&gone);
                all.borrow_mut().retain(|r| r.row != gone);
            });
            rows.borrow_mut().push(ConditionRow {
                row,
                field,
                value,
                _choices: checks,
            });
        })
    };
    for condition in &mailbox.conditions {
        add_row(condition);
    }
    let adder = Rc::clone(&add_row);
    add.connect_clicked(move |_| {
        adder(&Condition {
            field: Field::Subject,
            value: String::new(),
        })
    });

    let page = adw::PreferencesPage::new();
    page.add(&about);
    page.add(&conditions);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&page));
    let save = gtk::Button::builder()
        .label(if editing {
            gettext("Save")
        } else {
            gettext("Create")
        })
        .css_classes(["suggested-action"])
        .build();
    let cancel = gtk::Button::with_label(&gettext("Cancel"));
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&save);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toasts));
    let dialog = adw::Dialog::builder()
        .title(if editing {
            gettext("Edit Smart Mailbox")
        } else {
            gettext("New Smart Mailbox")
        })
        .content_width(620)
        .content_height(660)
        .child(&toolbar)
        .build();
    let closer = dialog.clone();
    cancel.connect_clicked(move |_| {
        closer.close();
    });
    let closer = dialog.clone();
    save.connect_clicked(move |_| {
        let result = SmartMailbox {
            unread_count: match unread.selected() {
                1 => UnreadCount::Local,
                2 => UnreadCount::Server,
                _ => UnreadCount::Hidden,
            },
            id: mailbox.id.clone(),
            name: name.text().trim().to_string(),
            account: (scope.selected() > 0)
                .then(|| accounts.get(scope.selected() as usize - 1).cloned())
                .flatten(),
            match_all: matching.selected() == 0,
            conditions: rows
                .borrow()
                .iter()
                .map(|r| Condition {
                    field: Field::ALL[r.field.selected() as usize],
                    value: r.value.text().trim().to_string(),
                })
                .collect(),
        };
        if result.name.is_empty() {
            toasts.add_toast(crate::ui::toast(&gettext("Give the mailbox a name")));
            return;
        }
        if result.query().is_none() {
            toasts.add_toast(crate::ui::toast(&gettext("Add a condition with a value")));
            return;
        }
        closer.close();
        on_save(result);
    });
    dialog.present(Some(parent));
}
