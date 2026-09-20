//! The dialog that creates or edits a smart mailbox.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use mailrs_domain::smart::{Condition, Field, SmartMailbox};
use mailrs_domain::translate::gettext;

struct ConditionRow {
    row: adw::ActionRow,
    field: gtk::DropDown,
    value: gtk::Entry,
}

/// Shows the editor. `accounts` lists addresses for the scope menu;
/// `on_save` receives the finished mailbox.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    existing: Option<SmartMailbox>,
    accounts: Vec<String>,
    on_save: impl Fn(SmartMailbox) + 'static,
) {
    let editing = existing.is_some();
    let mailbox = existing.unwrap_or_else(|| SmartMailbox {
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
    let about = adw::PreferencesGroup::new();
    about.add(&name);
    about.add(&scope);
    about.add(&matching);

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
    let add_row = {
        let (conditions, rows) = (conditions.clone(), Rc::clone(&rows));
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
                .build();
            let entry = value.clone();
            field.connect_selected_notify(move |field| {
                let chosen = Field::ALL[field.selected() as usize];
                entry.set_sensitive(chosen.takes_value());
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
            row.add_suffix(&remove);
            conditions.add(&row);
            let (group, all, gone) = (conditions.clone(), Rc::clone(&rows), row.clone());
            remove.connect_clicked(move |_| {
                group.remove(&gone);
                all.borrow_mut().retain(|r| r.row != gone);
            });
            rows.borrow_mut().push(ConditionRow { row, field, value });
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
        .content_height(560)
        .child(&toolbar)
        .build();
    let closer = dialog.clone();
    cancel.connect_clicked(move |_| {
        closer.close();
    });
    let closer = dialog.clone();
    save.connect_clicked(move |_| {
        let result = SmartMailbox {
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
            toasts.add_toast(adw::Toast::new(&gettext("Give the mailbox a name")));
            return;
        }
        if result.query().is_none() {
            toasts.add_toast(adw::Toast::new(&gettext("Add a condition with a value")));
            return;
        }
        closer.close();
        on_save(result);
    });
    dialog.present(Some(parent));
}
