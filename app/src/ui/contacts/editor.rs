//! Edits are sparse so opening a card does not rewrite its unrecognized fields.
use super::{Window, photo};
use adw::prelude::*;
use gtk::glib;
use mailrs_domain::contact::{Changes, Details, Field};
use mailrs_domain::translate::{fill, gettext};
use mailrs_gmail::ContactFields;
use mailrs_store::address_book::{self, Contact};
use mailrs_sync::Permitted;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

// A row's first value is its label only for dates and custom fields.
struct Rows {
    group: adw::PreferencesGroup,
    rows: RefCell<Vec<(adw::ActionRow, Vec<gtk::Entry>)>>,
    labels: Vec<String>,
}
impl Rows {
    fn new(title: &str, labels: Vec<String>, values: Vec<Vec<String>>) -> Rc<Self> {
        let group = adw::PreferencesGroup::builder().title(title).build();
        let add = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .valign(gtk::Align::Center)
            .build();
        super::super::name(&add, &fill(&gettext("Add {field}"), &[("field", title)]));
        group.set_header_suffix(Some(&add));
        let rows = Rc::new(Self {
            group,
            rows: RefCell::default(),
            labels,
        });
        for value in values {
            rows.add(value);
        }
        let weak = Rc::downgrade(&rows);
        add.connect_clicked(move |_| {
            if let Some(rows) = weak.upgrade() {
                rows.add(Vec::new());
            }
        });
        rows
    }
    fn add(self: &Rc<Self>, values: Vec<String>) {
        let row = adw::ActionRow::new();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .hexpand(true)
            .margin_top(8)
            .margin_bottom(8)
            .build();
        let entries: Vec<_> = self
            .labels
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let entry = gtk::Entry::builder()
                    .placeholder_text(label)
                    .text(values.get(i).map(String::as_str).unwrap_or_default())
                    .hexpand(true)
                    .build();
                super::super::name(&entry, label);
                content.append(&entry);
                entry
            })
            .collect();
        row.add_prefix(&content);
        let remove = gtk::Button::builder()
            .icon_name("list-remove-symbolic")
            .valign(gtk::Align::Center)
            .build();
        super::super::name(&remove, &gettext("Remove Field"));
        row.add_suffix(&remove);
        let weak = Rc::downgrade(self);
        let weak_row = row.downgrade();
        remove.connect_clicked(move |_| {
            if let (Some(rows), Some(row)) = (weak.upgrade(), weak_row.upgrade()) {
                rows.group.remove(&row);
                rows.rows.borrow_mut().retain(|(r, _)| r != &row);
            }
        });
        self.group.add(&row);
        self.rows.borrow_mut().push((row, entries));
    }
    fn values(&self) -> Vec<Vec<String>> {
        self.rows
            .borrow()
            .iter()
            .map(|(_, entries)| {
                entries
                    .iter()
                    .map(|e| e.text().trim().to_string())
                    .collect::<Vec<_>>()
            })
            .filter(|r| r.iter().any(|s| !s.is_empty()))
            .collect()
    }
    fn strings(&self) -> Vec<String> {
        self.values().into_iter().map(|r| r[0].clone()).collect()
    }
    fn fields(&self) -> Vec<Field> {
        self.values()
            .into_iter()
            .map(|r| Field {
                label: r[0].clone(),
                value: r[1].clone(),
            })
            .collect()
    }
}
struct Form {
    name: adw::EntryRow,
    organization: adw::EntryRow,
    birthday: adw::EntryRow,
    notes: gtk::TextBuffer,
    emails: Rc<Rows>,
    phones: Rc<Rows>,
    addresses: Rc<Rows>,
    urls: Rc<Rows>,
    groups: Rc<Rows>,
    dates: Rc<Rows>,
    custom: Rc<Rows>,
    photo: Rc<RefCell<Option<Vec<u8>>>>,
}
impl Form {
    fn values(&self) -> (String, String, Vec<String>, Details) {
        (
            self.name.text().trim().to_string(),
            self.organization.text().trim().to_string(),
            self.emails.strings(),
            Details {
                phones: self.phones.strings(),
                birthday: self.birthday.text().trim().to_string(),
                notes: self
                    .notes
                    .text(&self.notes.start_iter(), &self.notes.end_iter(), true)
                    .to_string(),
                addresses: self
                    .addresses
                    .values()
                    .into_iter()
                    .map(|r| r.join("\n"))
                    .collect(),
                urls: self.urls.strings(),
                groups: self.groups.strings(),
                dates: self.dates.fields(),
                custom: self.custom.fields(),
                ..Default::default()
            },
        )
    }
    fn changes(&self, original: &Contact, rich: bool) -> ContactFields {
        let (name, org, emails, d) = self.values();
        let before = &original.details;
        let phones = if before.phones.is_empty() {
            original.phone.iter().cloned().collect()
        } else {
            before.phones.clone()
        };
        ContactFields {
            name: changed(original.name.clone().unwrap_or_default(), name),
            organization: changed(original.organization.clone().unwrap_or_default(), org),
            emails: changed(original.emails.clone(), emails),
            phones: changed(phones, d.phones),
            photo: rich.then(|| self.photo.borrow().clone()).flatten(),
            details: if rich {
                Changes {
                    birthday: changed(before.birthday.clone(), d.birthday),
                    notes: changed(before.notes.clone(), d.notes),
                    addresses: changed(before.addresses.clone(), d.addresses),
                    urls: changed(before.urls.clone(), d.urls),
                    groups: changed(before.groups.clone(), d.groups),
                    dates: changed(before.dates.clone(), d.dates),
                    custom: changed(before.custom.clone(), d.custom),
                }
            } else {
                Changes::default()
            },
            expected_revision: (!before.revision.is_empty()).then(|| before.revision.clone()),
            expected_groups: None,
            creation_key: None,
            address_book: None,
        }
    }
}
fn changed<T: PartialEq>(old: T, new: T) -> Option<T> {
    (old != new).then_some(new)
}

pub(super) fn present(window: &Rc<Window>, contact: Option<Contact>) {
    let Some(app) = window.app.upgrade() else {
        return;
    };
    let owners: Vec<_> = window
        .owners
        .iter()
        .filter(|a| app.core.demo || app.settings().reads_contacts(&a.email))
        .cloned()
        .collect();
    if owners.is_empty() {
        window.say(&gettext(
            "Enable contacts for an account in Preferences first.",
        ));
        return;
    }
    if contact
        .as_ref()
        .is_some_and(|c| !owners.iter().any(|a| a.id == c.account_id))
    {
        window.say(&gettext(
            "Enable contacts for this account in Preferences first.",
        ));
        return;
    }
    let original = contact.clone().unwrap_or_default();
    let dialog = adw::Dialog::builder()
        .title(if contact.is_some() {
            gettext("Edit Contact")
        } else {
            gettext("New Contact")
        })
        .content_width(600)
        .content_height(720)
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(20)
        .margin_start(20)
        .margin_end(20)
        .margin_top(16)
        .margin_bottom(24)
        .build();
    let account = adw::ComboRow::builder()
        .title(gettext("Account"))
        .model(&gtk::StringList::new(
            &owners.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(),
        ))
        .build();
    account.set_selected(
        owners
            .iter()
            .position(|a| a.id == original.account_id)
            .unwrap_or(0) as u32,
    );
    account.set_sensitive(contact.is_none());
    let identity = adw::PreferencesGroup::new();
    identity.add(&account);
    content.append(&identity);
    let avatar = adw::Avatar::builder()
        .size(128)
        .show_initials(true)
        .text(original.display())
        .halign(gtk::Align::Center)
        .build();
    photo::load(&avatar, &original, &app);
    content.append(&avatar);
    let photo_buttons = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::Center)
        .build();
    let choose = gtk::Button::with_label(&gettext("Change Photo…"));
    let remove = gtk::Button::with_label(&gettext("Remove Photo"));
    photo_buttons.append(&choose);
    photo_buttons.append(&remove);
    content.append(&photo_buttons);
    let image = Rc::new(RefCell::new(None));
    let weak = dialog.downgrade();
    let (selected, preview) = (image.clone(), avatar.downgrade());
    choose.connect_clicked(move |_| {
        if let Some(dialog) = weak.upgrade() {
            let (selected, preview) = (selected.clone(), preview.clone());
            photo::choose(&dialog, move |bytes, texture| {
                *selected.borrow_mut() = Some(bytes);
                if let Some(a) = preview.upgrade() {
                    a.set_custom_image(Some(&texture));
                }
            });
        }
    });
    let (selected, preview) = (image.clone(), avatar.downgrade());
    remove.connect_clicked(move |_| {
        *selected.borrow_mut() = Some(Vec::new());
        if let Some(a) = preview.upgrade() {
            a.set_custom_image(gtk::gdk::Texture::NONE);
        }
    });
    let basic = adw::PreferencesGroup::new();
    let name = entry(
        &basic,
        &gettext("Name"),
        original.name.as_deref().unwrap_or_default(),
    );
    let organization = entry(
        &basic,
        &gettext("Organization"),
        original.organization.as_deref().unwrap_or_default(),
    );
    content.append(&basic);
    let strings = |title: &str, values: &[String]| {
        Rows::new(
            title,
            vec![title.to_string()],
            values.iter().map(|s| vec![s.clone()]).collect(),
        )
    };
    let emails = strings(&gettext("Email"), &original.emails);
    let numbers = if original.details.phones.is_empty() {
        original.phone.iter().cloned().collect()
    } else {
        original.details.phones.clone()
    };
    let phones = strings(&gettext("Phone"), &numbers);
    content.append(&emails.group);
    content.append(&phones.group);
    let rich_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(20)
        .build();
    content.append(&rich_box);
    let dates_group = adw::PreferencesGroup::builder()
        .description(gettext(
            "Use YYYY-MM-DD for dates, or --MM-DD when the year is unknown.",
        ))
        .build();
    let birthday = entry(
        &dates_group,
        &gettext("Birthday"),
        &original.details.birthday,
    );
    rich_box.append(&dates_group);
    let groups = strings(&gettext("Groups"), &original.details.groups);
    let urls = strings(&gettext("Websites"), &original.details.urls);
    let addresses = Rows::new(
        &gettext("Postal Addresses"),
        [
            gettext("PO Box"),
            gettext("Extended Address"),
            gettext("Street"),
            gettext("City"),
            gettext("Region"),
            gettext("Postal Code"),
            gettext("Country"),
        ]
        .to_vec(),
        original
            .details
            .addresses
            .iter()
            .map(|s| s.split('\n').map(str::to_string).collect())
            .collect(),
    );
    let fields = |title: &str, values: &[Field]| {
        Rows::new(
            title,
            vec![gettext("Label"), gettext("Value")],
            values
                .iter()
                .map(|s| vec![s.label.clone(), s.value.clone()])
                .collect(),
        )
    };
    let dates = fields(&gettext("Other Dates"), &original.details.dates);
    let custom = fields(&gettext("Custom Fields"), &original.details.custom);
    custom.group.set_description(Some(&gettext(
        "Use a vCard field name beginning with X-, such as X-CUSTOMER-ID.",
    )));
    for group in [&groups, &urls, &addresses, &dates, &custom] {
        rich_box.append(&group.group);
    }
    let notes = gtk::TextBuffer::new(None);
    notes.set_text(&original.details.notes);
    let notes_view = gtk::TextView::builder()
        .buffer(&notes)
        .wrap_mode(gtk::WrapMode::WordChar)
        .height_request(120)
        .build();
    super::super::name(&notes_view, &gettext("Notes"));
    rich_box.append(&gtk::Label::new(Some(&gettext("Notes"))));
    rich_box.append(&notes_view);
    let capabilities = gtk::Label::builder().label(gettext("This account supports editing names, email addresses, phone numbers, and organizations. Full contact editing requires CardDAV.")).wrap(true).build();
    content.append(&capabilities);
    let update_capabilities = {
        let owners = owners.clone();
        let weak_app = Rc::downgrade(&app);
        let (rich_box, photo_buttons, capabilities) = (
            rich_box.downgrade(),
            photo_buttons.downgrade(),
            capabilities.downgrade(),
        );
        move |account: &adw::ComboRow| {
            if let Some(app) = weak_app.upgrade() {
                let rich = owners
                    .get(account.selected() as usize)
                    .is_some_and(|a| app.core.demo || app.core.contacts().edits_details(a.id));
                if let Some(b) = rich_box.upgrade() {
                    b.set_visible(rich);
                }
                if let Some(b) = photo_buttons.upgrade() {
                    b.set_visible(rich);
                }
                if let Some(b) = capabilities.upgrade() {
                    b.set_visible(!rich);
                }
            }
        }
    };
    update_capabilities(&account);
    account.connect_selected_notify(update_capabilities);
    let form = Rc::new(Form {
        name,
        organization,
        birthday,
        notes,
        emails,
        phones,
        addresses,
        urls,
        groups,
        dates,
        custom,
        photo: image,
    });
    let save = gtk::Button::with_label(&gettext("Save"));
    save.add_css_class("suggested-action");
    let header = adw::HeaderBar::new();
    header.pack_end(&save);
    let error = gtk::Label::builder()
        .wrap(true)
        .selectable(true)
        .css_classes(["error"])
        .visible(false)
        .build();
    error.set_margin_start(12);
    error.set_margin_end(12);
    let view = adw::ToolbarView::builder()
        .content(
            &gtk::ScrolledWindow::builder()
                .child(&content)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build(),
        )
        .build();
    view.add_top_bar(&header);
    view.add_top_bar(&error);
    dialog.set_child(Some(&view));
    let saving = Rc::new(Cell::new(false));
    let weak = dialog.downgrade();
    let weak_window = Rc::downgrade(window);
    // Compare with what the controls initially showed, so trimming in a field
    // does not turn an untouched server value into an edit.
    let mut initial = original.clone();
    let (name, organization, emails, mut details) = form.values();
    details.revision = original.details.revision.clone();
    initial.name = Some(name);
    initial.organization = Some(organization);
    initial.emails = emails;
    initial.details = details;
    let (saved_form, saved_original, saved_flag) = (form.clone(), initial, saving.clone());
    let books = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
    let book_row = adw::ComboRow::builder()
        .title(gettext("Address Book"))
        .visible(false)
        .build();
    identity.add(&book_row);
    let loading = Rc::new(Cell::new(false));
    if contact.is_none() {
        let load = {
            let (owners, books, loading, row, save, error, app) = (
                owners.clone(),
                books.clone(),
                loading.clone(),
                book_row.downgrade(),
                save.downgrade(),
                error.downgrade(),
                Rc::downgrade(&app),
            );
            move |account: &adw::ComboRow| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                let Some(owner) = owners.get(account.selected() as usize) else {
                    return;
                };
                let id = owner.id;
                books.borrow_mut().clear();
                let rich = !app.core.demo && app.core.contacts().edits_details(id);
                if let Some(row) = row.upgrade() {
                    row.set_visible(rich);
                    row.set_model(None::<&gtk::StringList>);
                }
                loading.set(rich);
                if let Some(save) = save.upgrade() {
                    save.set_sensitive(!rich);
                }
                if !rich {
                    return;
                }
                let (books, loading, row, save, error, selected, selected_index) = (
                    books.clone(),
                    loading.clone(),
                    row.clone(),
                    save.clone(),
                    error.clone(),
                    account.downgrade(),
                    account.selected(),
                );
                glib::spawn_future_local(async move {
                    let service = app.core.contacts();
                    let answer = app
                        .core
                        .call(async move { service.address_books(id).await })
                        .await;
                    if !selected
                        .upgrade()
                        .is_some_and(|a| a.selected() == selected_index)
                    {
                        return;
                    }
                    loading.set(false);
                    match answer {
                        Ok(found) if !found.is_empty() => {
                            if let Some(row) = row.upgrade() {
                                row.set_model(Some(&gtk::StringList::new(
                                    &found
                                        .iter()
                                        .map(|(_, name)| name.as_str())
                                        .collect::<Vec<_>>(),
                                )));
                                row.set_selected(0);
                            }
                            *books.borrow_mut() = found;
                            if let Some(save) = save.upgrade() {
                                save.set_sensitive(true);
                            }
                        }
                        result => {
                            if let Some(error) = error.upgrade() {
                                error.set_text(&match result {
                                    Err(err) => err.to_string(),
                                    _ => gettext("This account has no address books."),
                                });
                                error.set_visible(true);
                            }
                        }
                    }
                });
            }
        };
        load(&account);
        account.connect_selected_notify(load);
    }
    let creation_key = glib::uuid_string_random().to_string();
    save.connect_clicked(move |save| {
        if saved_flag.get() || loading.get() {
            return;
        }
        let Some(window) = weak_window.upgrade() else {
            return;
        };
        let Some(app) = window.app.upgrade() else {
            return;
        };
        let Some(owner) = owners.get(account.selected() as usize) else {
            return;
        };
        if !app.core.demo && !app.settings().reads_contacts(&owner.email) {
            error.set_text(&gettext("Contacts access for this account was turned off."));
            error.set_visible(true);
            return;
        }
        let rich = app.core.demo || app.core.contacts().edits_details(owner.id);
        let mut fields = saved_form.changes(&saved_original, rich);
        if fields.details.groups.is_some() {
            fields.expected_groups = contact.as_ref().map(|c| c.details.groups.clone());
        }
        if contact.is_none() {
            fields.creation_key = Some(creation_key.clone());
            fields.address_book = books
                .borrow()
                .get(book_row.selected() as usize)
                .map(|(id, _)| id.clone());
            if !app.core.demo && rich && fields.address_book.is_none() {
                error.set_text(&gettext("Choose an address book first."));
                error.set_visible(true);
                return;
            }
        }
        if contact.is_none() && fields.name.is_none() {
            fields.name = Some(saved_form.name.text().trim().to_string());
        }
        if saved_form.name.text().trim().is_empty() && saved_form.emails.strings().is_empty() {
            error.set_text(&gettext("Enter a name or an email address."));
            error.set_visible(true);
            return;
        }
        if saved_form
            .emails
            .strings()
            .iter()
            .any(|email| !email.contains('@') || email.chars().any(char::is_whitespace))
        {
            error.set_text(&gettext("Enter a valid email address."));
            error.set_visible(true);
            return;
        }
        if contact.is_some() && fields.is_empty() {
            if let Some(dialog) = weak.upgrade() {
                dialog.force_close();
            }
            return;
        }
        let id = owner.id;
        let original = saved_original.clone();
        let new = contact.is_none();
        let (name, org, emails, details) = saved_form.values();
        let mut demo_contact = Contact {
            account_id: id,
            resource: if new {
                format!("demo/{}", glib::uuid_string_random())
            } else {
                original.resource.clone()
            },
            name: Some(name),
            organization: Some(org),
            emails,
            phone: details.phones.first().cloned(),
            details,
            ..original.clone()
        };
        saved_flag.set(true);
        save.set_sensitive(false);
        account.set_sensitive(false);
        content.set_sensitive(false);
        if let Some(d) = weak.upgrade() {
            d.set_can_close(false);
        }
        let (weak, weak_window, flag, save, error, content, account) = (
            weak.clone(),
            weak_window.clone(),
            saved_flag.clone(),
            save.downgrade(),
            error.downgrade(),
            content.downgrade(),
            account.downgrade(),
        );
        glib::spawn_future_local(async move {
            let result: anyhow::Result<()> = if app.core.demo {
                async {
                    if let Some(bytes) = fields.photo {
                        demo_contact.photo_file = None;
                        demo_contact.photo_url = None;
                        if !bytes.is_empty() {
                            let file = format!("demo-{}.png", glib::uuid_string_random());
                            let directory = app.core.contacts().photo_dir().to_owned();
                            let path = directory.join(&file);
                            app.core
                                .call(async move {
                                    std::fs::create_dir_all(directory)?;
                                    std::fs::write(path, bytes)
                                })
                                .await?;
                            demo_contact.photo_url = Some(file.clone());
                            demo_contact.photo_file = Some(file);
                        }
                    }
                    app.core
                        .write(move |c| address_book::save(c, &[demo_contact]))
                        .await?;
                    Ok(())
                }
                .await
            } else {
                let book = app.core.contacts();
                app.core
                    .call(async move {
                        if new {
                            book.create(id, &fields, true).await
                        } else {
                            book.update(id, &original.resource, &fields, true).await
                        }
                    })
                    .await
                    .and_then(|answer| match answer {
                        Permitted::Done(_) => Ok(()),
                        Permitted::NeedsPermission => Err(anyhow::anyhow!(gettext(
                            "Grant contacts permission in Preferences, then try again."
                        ))),
                    })
            };
            flag.set(false);
            let Some(dialog) = weak.upgrade() else {
                return;
            };
            dialog.set_can_close(false);
            match result {
                Ok(()) => {
                    app.contacts_added();
                    if let Some(w) = weak_window.upgrade() {
                        w.reload();
                    }
                    dialog.force_close();
                }
                Err(err) => {
                    if let Some(e) = error.upgrade() {
                        e.set_text(&fill(
                            &gettext("Could not save contact: {reason}"),
                            &[("reason", &err.to_string())],
                        ));
                        e.set_visible(true);
                    }
                    if let Some(s) = save.upgrade() {
                        s.set_sensitive(true);
                    }
                    if let Some(c) = content.upgrade() {
                        c.set_sensitive(true);
                    }
                    if new && let Some(a) = account.upgrade() {
                        a.set_sensitive(true);
                    }
                }
            }
        });
    });
    // Refuse an accidental close once any field changed, including a photo.
    dialog.set_can_close(false);
    let original_values = form.values();
    dialog.connect_close_attempt(move |dialog| {
        if saving.get() {
            return;
        }
        if form.values() == original_values && form.photo.borrow().is_none() {
            dialog.force_close();
            return;
        }
        let confirm = adw::AlertDialog::builder()
            .heading(gettext("Discard Contact Changes?"))
            .body(gettext("Your changes have not been saved to the account."))
            .build();
        confirm.add_responses(&[
            ("cancel", &gettext("Keep Editing")),
            ("discard", &gettext("Discard")),
        ]);
        confirm.set_default_response(Some("cancel"));
        confirm.set_close_response("cancel");
        confirm.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        let weak = dialog.downgrade();
        confirm.connect_response(None, move |_, response| {
            if response == "discard"
                && let Some(d) = weak.upgrade()
            {
                d.force_close();
            }
        });
        confirm.present(Some(dialog));
    });
    // The selected card is rebuilt after saving. Return focus to a widget
    // that survives that rebuild when the dialog closes.
    window.search.grab_focus();
    dialog.present(Some(&window.window));
}
fn entry(group: &adw::PreferencesGroup, title: &str, value: &str) -> adw::EntryRow {
    let row = adw::EntryRow::builder().title(title).text(value).build();
    group.add(&row);
    row
}
