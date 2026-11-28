//! The editor in the person pane. It names the account it will change,
//! and writes nothing until Save. Edits are sparse so opening a card does
//! not rewrite its unrecognized fields.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::contact::{Changes, Details, Field};
use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_gmail::ContactFields;
use mailrs_store::address_book::Contact;

use super::person::{body_box, editable_members};
use super::{Contacts, List, Pane, parts, people};
use crate::format::account_label;

/// A repeated field: one entry box per value, with Add and a remove cross.
struct Rows {
    group: gtk::Box,
    rows: RefCell<Vec<(gtk::Box, Vec<gtk::Entry>)>>,
    /// The label of each entry of a row, and the label of the first row
    /// when it differs from the others.
    labels: Vec<String>,
    first: Option<String>,
    dirty: Rc<dyn Fn()>,
    /// The add line under the group.
    add: gtk::Button,
    holder: gtk::Box,
    /// "Add phone": the hint an empty field shows.
    hint: String,
}

impl Rows {
    fn new(
        add_label: &str,
        first: Option<String>,
        labels: Vec<String>,
        values: Vec<Vec<String>>,
        at_least: usize,
        dirty: Rc<dyn Fn()>,
    ) -> Rc<Self> {
        let group = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["cts-group"])
            .build();
        group.set_overflow(gtk::Overflow::Hidden);
        let add = gtk::Button::builder()
            .child(&{
                let row = gtk::Box::builder().spacing(6).build();
                row.append(&parts::icon("plus"));
                row.append(&gtk::Label::new(Some(add_label)));
                row
            })
            .halign(gtk::Align::Start)
            .css_classes(["cts-addline"])
            .build();
        super::super::name(&add, add_label);
        let holder = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        holder.append(&group);
        holder.append(&add);
        let rows = Rc::new(Self {
            group,
            rows: RefCell::default(),
            labels,
            first,
            dirty,
            add,
            holder,
            hint: add_label.to_string(),
        });
        let count = values.len().max(at_least);
        for at in 0..count {
            rows.push(values.get(at).cloned().unwrap_or_default(), false);
        }
        let weak = Rc::downgrade(&rows);
        rows.add.connect_clicked(move |_| {
            if let Some(rows) = weak.upgrade() {
                rows.push(Vec::new(), true);
            }
        });
        rows.group.set_visible(count > 0);
        rows
    }

    fn push(self: &Rc<Self>, values: Vec<String>, user: bool) {
        let at = self.rows.borrow().len();
        let shown_labels: Vec<String> = self
            .labels
            .iter()
            .enumerate()
            .map(|(i, l)| match (&self.first, at, i) {
                (Some(first), 0, 0) => first.clone(),
                _ => l.clone(),
            })
            .collect();
        let row = gtk::Box::builder().css_classes(["cts-entryrow"]).build();
        let fields = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .build();
        let mut entries = Vec::new();
        for (i, label) in shown_labels.iter().enumerate() {
            let (cell, entry) = parts::entry(
                label,
                values.get(i).map_or("", String::as_str),
                if i == 0 { &self.hint } else { "" },
            );
            let dirty = self.dirty.clone();
            entry.connect_changed(move |_| dirty());
            if i > 0 {
                cell.add_css_class("after");
            }
            fields.append(&cell);
            entries.push(entry);
        }
        row.append(&fields);
        let remove = parts::icon_button("x", &gettext("Remove Field"), &[]);
        remove.set_valign(gtk::Align::Center);
        remove.set_margin_end(8);
        // An empty field has nothing to remove: the button shows with text.
        let filled = {
            let entries = entries.clone();
            move || entries.iter().any(|e| !e.text().is_empty())
        };
        remove.set_visible(filled());
        for entry in &entries {
            let (remove, filled) = (remove.downgrade(), filled.clone());
            entry.connect_changed(move |_| {
                if let Some(remove) = remove.upgrade() {
                    remove.set_visible(filled());
                }
            });
        }
        row.append(&remove);
        if at > 0 {
            row.add_css_class("after");
        }
        let (weak, weak_row) = (Rc::downgrade(self), row.downgrade());
        remove.connect_clicked(move |_| {
            if let (Some(rows), Some(row)) = (weak.upgrade(), weak_row.upgrade()) {
                rows.group.remove(&row);
                rows.rows.borrow_mut().retain(|(r, _)| r != &row);
                rows.group.set_visible(!rows.rows.borrow().is_empty());
                (rows.dirty)();
            }
        });
        self.group.append(&row);
        self.group.set_visible(true);
        self.rows.borrow_mut().push((row, entries));
        if user {
            (self.dirty)();
            if let Some((_, e)) = self.rows.borrow().last()
                && let Some(first) = e.first()
            {
                first.grab_focus();
            }
        }
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
    name: gtk::Entry,
    organization: gtk::Entry,
    birthday: gtk::Entry,
    notes: gtk::TextBuffer,
    emails: Rc<Rows>,
    phones: Rc<Rows>,
    addresses: Rc<Rows>,
    urls: Rc<Rows>,
    groups: Rc<Rows>,
    dates: Rc<Rows>,
    custom: Rc<Rows>,
    /// `Some(empty)` removes the photo, `Some(bytes)` replaces it.
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
                    .map(|r| std::array::from_fn(|i| r.get(i).cloned().unwrap_or_default()))
                    .collect(),
                urls: self.urls.strings(),
                groups: self.groups.strings(),
                dates: self.dates.fields(),
                custom: self.custom.fields(),
                ..Default::default()
            },
        )
    }

    fn changes(&self, original: &Contact) -> ContactFields {
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
            photo: self.photo.borrow().clone(),
            details: Changes {
                birthday: changed(before.birthday.clone(), d.birthday),
                notes: changed(before.notes.clone(), d.notes),
                addresses: changed(before.addresses.clone(), d.addresses),
                urls: changed(before.urls.clone(), d.urls),
                groups: changed(before.groups.clone(), d.groups),
                dates: changed(before.dates.clone(), d.dates),
                custom: changed(before.custom.clone(), d.custom),
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

/// The edit and new-contact forms share these widgets.
struct Built {
    form: Rc<Form>,
    body: gtk::Box,
    photo_box: gtk::Box,
}

fn text_group(
    label: &str,
    value: &str,
    placeholder: &str,
    dirty: &Rc<dyn Fn()>,
) -> (gtk::Box, gtk::Entry) {
    let (cell, entry) = parts::entry(label, value, placeholder);
    let d = dirty.clone();
    entry.connect_changed(move |_| d());
    (parts::group(&[cell.upcast()]), entry)
}

fn build(this: &Rc<Contacts>, original: &Contact, dirty: Rc<dyn Fn()>, new: bool) -> Built {
    let phones_before = if original.details.phones.is_empty() {
        original.phone.iter().cloned().collect::<Vec<_>>()
    } else {
        original.details.phones.clone()
    };
    let d = &original.details;
    let (name_cell, name) = parts::entry(
        &gettext("Name"),
        original.name.as_deref().unwrap_or_default(),
        &if new {
            gettext("Full name")
        } else {
            String::new()
        },
    );
    let (org_cell, organization) = parts::entry(
        &gettext("Organization"),
        original.organization.as_deref().unwrap_or_default(),
        &gettext("Add organization"),
    );
    for e in [&name, &organization] {
        let d = dirty.clone();
        e.connect_changed(move |_| d());
    }
    org_cell.add_css_class("after");
    let emails = Rows::new(
        &gettext("Add email"),
        Some(gettext("Email")),
        vec![gettext("Other email")],
        original.emails.iter().map(|e| vec![e.clone()]).collect(),
        usize::from(new),
        dirty.clone(),
    );
    if new {
        for (_, entries) in emails.rows.borrow().iter() {
            entries[0].set_placeholder_text(Some(&gettext("name@example.com")));
        }
    }
    let phones = Rows::new(
        &gettext("Add phone"),
        None,
        vec![gettext("Phone")],
        phones_before.iter().map(|p| vec![p.clone()]).collect(),
        usize::from(new),
        dirty.clone(),
    );
    let addresses = Rows::new(
        &gettext("Add postal address"),
        None,
        vec![
            gettext("PO box"),
            gettext("Extended address"),
            gettext("Street"),
            gettext("City"),
            gettext("Region"),
            gettext("Postal code"),
            gettext("Country"),
        ],
        d.addresses.iter().map(|a| a.to_vec()).collect(),
        0,
        dirty.clone(),
    );
    let urls = Rows::new(
        &gettext("Add website"),
        None,
        vec![gettext("Website")],
        d.urls.iter().map(|u| vec![u.clone()]).collect(),
        0,
        dirty.clone(),
    );
    let groups = Rows::new(
        &gettext("Add group"),
        None,
        vec![gettext("Group")],
        d.groups.iter().map(|g| vec![g.clone()]).collect(),
        0,
        dirty.clone(),
    );
    let dates = Rows::new(
        &gettext("Add date"),
        None,
        vec![gettext("Label"), gettext("Date")],
        d.dates
            .iter()
            .map(|f| vec![f.label.clone(), f.value.clone()])
            .collect(),
        0,
        dirty.clone(),
    );
    let custom = Rows::new(
        &gettext("Add custom field"),
        None,
        vec![gettext("Label"), gettext("Value")],
        d.custom
            .iter()
            .map(|f| vec![f.label.clone(), f.value.clone()])
            .collect(),
        0,
        dirty.clone(),
    );
    let (birthday_group, birthday) = text_group(
        &gettext("Birthday"),
        &d.birthday,
        &gettext("YYYY-MM-DD"),
        &dirty,
    );
    let notes = gtk::TextBuffer::new(None);
    notes.set_text(&d.notes);
    {
        let dirty = dirty.clone();
        notes.connect_changed(move |_| dirty());
    }
    let notes_view = gtk::TextView::builder()
        .buffer(&notes)
        .wrap_mode(gtk::WrapMode::WordChar)
        .accepts_tab(false)
        .css_classes(["cts-notes"])
        .build();
    super::super::name(&notes_view, &gettext("Notes"));
    let photo = Rc::new(RefCell::new(None));

    let body = body_box();
    let photo_box = gtk::Box::builder()
        .spacing(12)
        .css_classes(["cts-photo"])
        .build();
    body.append(&photo_box);
    body.append(&parts::sec(&gettext("Details")));
    let details = [name_cell.upcast::<gtk::Widget>(), org_cell.upcast()];
    // Name, organization, addresses and numbers are one group, as the
    // mockup draws them; each kind keeps its own Add line below it.
    let first_group = parts::group(&[details[0].clone(), details[1].clone()]);
    for rows in [&emails, &phones] {
        rows.group.remove_css_class("cts-group");
        rows.group.add_css_class("cts-rowsbox");
        rows.holder.remove(&rows.group);
        rows.holder.remove(&rows.add);
        rows.group.add_css_class("after");
        first_group.append(&rows.group);
    }
    body.append(&first_group);
    for rows in [&emails, &phones] {
        body.append(&rows.add);
    }
    for (title, rows) in [
        (gettext("Postal addresses"), &addresses),
        (gettext("Websites"), &urls),
        (gettext("Groups"), &groups),
        (gettext("Other dates"), &dates),
        (gettext("Custom fields"), &custom),
    ] {
        body.append(&parts::sec(&title));
        body.append(&rows.holder);
    }
    body.append(&parts::sec(&gettext("Birthday")));
    body.append(&birthday_group);
    body.append(&parts::sec(&gettext("Notes")));
    body.append(&parts::group(&[notes_view.upcast()]));
    let _ = this;
    Built {
        form: Rc::new(Form {
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
            photo,
        }),
        body,
        photo_box,
    }
}

impl Contacts {
    pub(super) fn edit_pane(self: &Rc<Self>, contact: Contact) {
        let (person, contacts, links, list) = {
            let state = self.state.borrow();
            let key = (contact.account_id, contact.resource.clone());
            let person = state
                .people
                .iter()
                .find(|p| {
                    p.members.iter().any(|&i| {
                        (
                            state.data.contacts[i].account_id,
                            state.data.contacts[i].resource.clone(),
                        ) == key
                    })
                })
                .cloned();
            (
                person,
                state.data.contacts.clone(),
                state.data.links.clone(),
                state.list.clone().unwrap_or(List::All),
            )
        };
        let others: Vec<Contact> = person
            .as_ref()
            .map(|p| {
                editable_members(p, &contacts, &links)
                    .into_iter()
                    .map(|i| contacts[i].clone())
                    .collect()
            })
            .unwrap_or_default();
        let account = account_label(contact.account_id).short;
        let hint = edit_hint(&contact, &contacts, &links, &person, &account, &list);
        self.form_pane(Some(contact), others, hint);
    }

    pub(super) fn new_pane(self: &Rc<Self>) {
        self.form_pane(None, Vec::new(), gettext("A new contact is saved into one account. The other accounts do not get a copy. Add a name or an email. Save stays off until one of those is filled in."));
    }

    /// The editor, for `contact` or, with none, a new contact.
    fn form_pane(self: &Rc<Self>, contact: Option<Contact>, others: Vec<Contact>, hint: String) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let new = contact.is_none();
        let targets = self.accounts_with_contacts();
        let original = contact.clone().unwrap_or_default();
        let chosen = Rc::new(Cell::new(
            targets
                .iter()
                .position(|a| a.id == original.account_id)
                .unwrap_or_else(|| match self.current_list() {
                    List::Account(id) => targets.iter().position(|a| a.id == id).unwrap_or(0),
                    _ => 0,
                }),
        ));
        let unsaved = parts::label(&gettext("Unsaved changes"), &["cts-dirty"]);
        unsaved.set_visible(false);
        unsaved.set_wrap(false);
        let save = parts::button(&gettext("Save"), &["yes"]);
        let error = parts::label("", &["cts-error"]);
        error.set_visible(false);
        // Save waits for a first change on a saved contact; a new one waits
        // for a name or address instead, below.
        save.set_sensitive(new);
        self.form_dirty.set(false);
        let dirty: Rc<dyn Fn()> = {
            let (unsaved, save, this) =
                (unsaved.downgrade(), save.downgrade(), Rc::downgrade(self));
            Rc::new(move || {
                if let Some(this) = this.upgrade() {
                    this.form_dirty.set(true);
                }
                if let Some(u) = unsaved.upgrade() {
                    u.set_visible(true);
                }
                if let Some(s) = save.upgrade()
                    && !new
                {
                    s.set_sensitive(true);
                }
            })
        };
        let built = build(self, &original, dirty.clone(), new);
        let form = built.form.clone();
        // Header
        let phead = gtk::Box::builder()
            .css_classes(["cts-phead"])
            .spacing(8)
            .build();
        let back = parts::icon_button("back", &gettext("Back"), &["back"]);
        self.nav
            .bind_property("collapsed", &back, "visible")
            .sync_create()
            .build();
        let weak = Rc::downgrade(self);
        {
            let weak = weak.clone();
            back.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.nav.set_show_content(false);
                }
            });
        }
        phead.append(&back);
        phead.append(&parts::label(
            &if new {
                gettext("New contact")
            } else {
                gettext("Edit contact")
            },
            &["cts-h2"],
        ));
        phead.append(&gtk::Box::builder().hexpand(true).build());
        let cancel = parts::button(&gettext("Cancel"), &[]);
        {
            let weak = weak.clone();
            cancel.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.leave_form(|this| {
                        this.state.borrow_mut().pane = Some(Pane::View);
                        this.show_pane();
                    });
                }
            });
        }
        phead.append(&cancel);
        phead.append(&unsaved);
        phead.append(&save);
        self.pane.append(&phead);
        self.pane.append(&error);

        // Save stays off for a new contact until it has a name or address.
        if new {
            save.set_sensitive(false);
            super::super::name(&save, &gettext("Save"));
            save.update_property(&[gtk::accessible::Property::Description(&hint)]);
            let (save, f, can) = (save.downgrade(), form.clone(), !targets.is_empty());
            let update = move || {
                if let Some(save) = save.upgrade() {
                    save.set_sensitive(
                        can && (!f.name.text().trim().is_empty() || !f.emails.strings().is_empty()),
                    );
                }
            };
            let u = update.clone();
            form.name.connect_changed(move |_| u());
            for (_, entries) in form.emails.rows.borrow().iter() {
                let u = update.clone();
                entries[0].connect_changed(move |_| u());
            }
            let u = update.clone();
            form.emails.add.connect_clicked(move |_| u());
        }

        let body = built.body.clone();
        // Hint, account choice
        let top = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        top.append(&parts::hint(&hint));
        if new {
            if targets.is_empty() {
                top.append(&parts::hint(&gettext(
                    "Turn contacts on for an account first.",
                )));
                save.set_sensitive(false);
            } else {
                top.append(&parts::sec(&gettext("Save to")));
                let names: Vec<String> =
                    targets.iter().map(|a| account_label(a.id).short).collect();
                let chosen = chosen.clone();
                top.append(&parts::seg(
                    &gettext("Save to"),
                    &names,
                    chosen.get(),
                    move |at| chosen.set(at),
                ));
            }
        } else if others.len() > 1 {
            let names: Vec<String> = others
                .iter()
                .map(|c| account_label(c.account_id).short)
                .collect();
            let at = others
                .iter()
                .position(|c| {
                    c.resource == original.resource && c.account_id == original.account_id
                })
                .unwrap_or(0);
            let (weak, others) = (weak.clone(), others.clone());
            let seg_ref: Rc<glib::WeakRef<gtk::Box>> = Rc::default();
            let held = seg_ref.clone();
            let seg = parts::seg(&gettext("Which account"), &names, at, move |to| {
                let (Some(this), Some(next)) = (weak.upgrade(), others.get(to).cloned()) else {
                    return;
                };
                if to == at {
                    return;
                }
                let held = held.clone();
                this.leave_form_or(
                    move |this| {
                        this.state.borrow_mut().pane = Some(Pane::Edit(Box::new(next)));
                        this.show_pane();
                    },
                    // Kept: the seg goes back to the account being edited.
                    move |_| {
                        let toggle = held.upgrade().and_then(|seg| {
                            std::iter::successors(seg.first_child(), |w| w.next_sibling()).nth(at)
                        });
                        if let Some(toggle) = toggle.and_downcast::<gtk::ToggleButton>() {
                            toggle.set_active(true);
                        }
                    },
                );
            });
            seg_ref.set(Some(&seg));
            top.append(&seg);
        }
        body.prepend(&top);

        // Photo
        let avatar_holder = gtk::Box::builder().build();
        avatar_holder.append(&self.photo_avatar(
            &original.name.clone().unwrap_or_default(),
            56,
            contact.as_ref(),
        ));
        built.photo_box.append(&avatar_holder);
        let photo_label = |has: bool| {
            if has {
                gettext("Replace Photo")
            } else {
                gettext("Add Photo")
            }
        };
        let replace = parts::button(&photo_label(original.photo_file.is_some()), &[]);
        let remove = parts::button(&gettext("Remove Photo"), &[]);
        replace.set_valign(gtk::Align::Center);
        remove.set_valign(gtk::Align::Center);
        avatar_holder.set_valign(gtk::Align::Center);
        remove.set_visible(original.photo_file.is_some());
        {
            let (form, holder, remove, dirty) = (
                form.clone(),
                avatar_holder.clone(),
                remove.clone(),
                dirty.clone(),
            );
            let name = original.name.clone().unwrap_or_default();
            replace.connect_clicked(move |b| {
                let button = b.clone();
                let (form, holder, remove, dirty, name) = (
                    form.clone(),
                    holder.clone(),
                    remove.clone(),
                    dirty.clone(),
                    name.clone(),
                );
                super::photo::choose(b.upcast_ref(), move |bytes, texture| {
                    *form.photo.borrow_mut() = Some(bytes);
                    while let Some(c) = holder.first_child() {
                        holder.remove(&c);
                    }
                    holder.append(&parts::avatar(&name, 56, Some(&texture)));
                    button.set_label(&photo_label(true));
                    remove.set_visible(true);
                    dirty();
                });
            });
        }
        {
            let (form, holder, replace, dirty) = (
                form.clone(),
                avatar_holder.clone(),
                replace.clone(),
                dirty.clone(),
            );
            let name = original.name.clone().unwrap_or_default();
            remove.connect_clicked(move |b| {
                replace.set_label(&photo_label(false));
                *form.photo.borrow_mut() = Some(Vec::new());
                while let Some(c) = holder.first_child() {
                    holder.remove(&c);
                }
                holder.append(&parts::avatar(&name, 56, None));
                b.set_visible(false);
                dirty();
            });
        }
        built.photo_box.append(&replace);
        built.photo_box.append(&remove);

        // Remove
        if let Some(c) = contact.clone().filter(|c| !c.details.read_only) {
            body.append(&parts::sec(&gettext("Remove")));
            let delete = gtk::Button::builder()
                .label(fill(
                    &gettext("Delete {name} from {account}"),
                    &[
                        ("name", c.display()),
                        ("account", &account_label(c.account_id).short),
                    ],
                ))
                .halign(gtk::Align::Fill)
                .css_classes(["cts-line", "cts-danger"])
                .build();
            if let Some(l) = delete.child().and_downcast::<gtk::Label>() {
                l.set_xalign(0.0);
            }
            let weak = weak.clone();
            let still = others
                .iter()
                .find(|o| o.account_id != c.account_id)
                .map(|o| account_label(o.account_id).short);
            delete.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.delete_contact(c.clone(), still.clone());
                }
            });
            body.append(&parts::group(&[delete.upcast()]));
        }
        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&body)
            .build();
        self.pane.append(&scroll);

        // Save
        let weak_this = Rc::downgrade(self);
        let (form, error_label, targets, chosen, original) = (
            form.clone(),
            error.clone(),
            targets.clone(),
            chosen.clone(),
            original.clone(),
        );
        save.connect_clicked(move |b| {
            let Some(this) = weak_this.upgrade() else {
                return;
            };
            let Some(app) = this.app.upgrade() else {
                return;
            };
            let Some(owner) = targets.get(chosen.get()).cloned() else {
                return;
            };
            let fail = |text: &str| {
                error_label.set_label(text);
                error_label.set_visible(true);
            };
            let (name, org, emails, details) = form.values();
            if name.is_empty() && emails.is_empty() {
                return fail(&gettext("Enter a name or an email address."));
            }
            if emails
                .iter()
                .any(|e| !e.contains('@') || e.chars().any(char::is_whitespace))
            {
                return fail(&gettext("Enter a valid email address."));
            }
            if !app.core.demo && !app.settings().reads_contacts(&owner.email) {
                return fail(&gettext("Contacts access for this account was turned off."));
            }
            let mut fields = form.changes(&original);
            if new && fields.name.is_none() {
                fields.name = Some(name.clone());
            }
            if fields.details.groups.is_some() {
                fields.expected_groups = (!new).then(|| original.details.groups.clone());
            }
            if !new && fields.is_empty() {
                this.state.borrow_mut().pane = Some(Pane::View);
                this.show_pane();
                return;
            }
            let demo = Contact {
                account_id: owner.id,
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
            b.set_sensitive(false);
            error_label.set_visible(false);
            let (weak, error_label, b) = (Rc::downgrade(&this), error_label.clone(), b.clone());
            let old = (!new).then(|| original.clone());
            glib::spawn_future_local(async move {
                let result = super::ops::save(&app, owner.id, old, fields, demo).await;
                let Some(this) = weak.upgrade() else { return };
                b.set_sensitive(true);
                match result {
                    Ok(_) => {
                        app.contacts_added();
                        this.state.borrow_mut().pane = Some(Pane::View);
                        this.reload();
                    }
                    Err(err) => {
                        error_label.set_label(&fill(
                            &gettext("Could not save the contact: {reason}"),
                            &[("reason", &err.to_string())],
                        ));
                        error_label.set_visible(true);
                    }
                }
            });
        });
        let _ = app;
    }
}

/// The words above the form, naming the account it will change.
fn edit_hint(
    contact: &Contact,
    contacts: &[Contact],
    links: &[mailrs_store::address_book::Link],
    person: &Option<people::Person>,
    account: &str,
    list: &List,
) -> String {
    let linked = links.iter().find(|l| {
        (l.a == (contact.account_id, contact.resource.clone()))
            || (l.b == (contact.account_id, contact.resource.clone()))
    });
    let other_of = |l: &mailrs_store::address_book::Link| {
        let key = if l.a == (contact.account_id, contact.resource.clone()) {
            &l.b
        } else {
            &l.a
        };
        contacts
            .iter()
            .find(|c| (c.account_id, c.resource.clone()) == *key)
            .cloned()
    };
    if let Some(link) = linked
        && let Some(other) = other_of(link)
    {
        let shown_here = (link.shown_a && link.a == (contact.account_id, contact.resource.clone()))
            || (!link.shown_a && link.b == (contact.account_id, contact.resource.clone()));
        let other_account = account_label(other.account_id).short;
        return if shown_here && !matches!(list, List::Account(_)) {
            fill(
                &gettext(
                    "Editing the {account} contact. The All contacts row uses this name. {other_account} still has {other}, and linking does not change that record. Nothing is written until you press Save.",
                ),
                &[
                    ("account", account),
                    ("other_account", &other_account),
                    ("other", other.display()),
                ],
            )
        } else {
            fill(
                &gettext(
                    "Editing the {account} contact for {name}. {other_account} keeps {other}. Nothing is written until you press Save.",
                ),
                &[
                    ("account", account),
                    ("name", contact.display()),
                    ("other_account", &other_account),
                    ("other", other.display()),
                ],
            )
        };
    }
    let across: Vec<String> = person
        .as_ref()
        .map(|p| {
            p.accounts
                .iter()
                .filter(|a| **a != contact.account_id)
                .map(|a| account_label(*a).short)
                .collect()
        })
        .unwrap_or_default();
    if across.is_empty() {
        fill(
            &gettext("Editing the {account} contact. Nothing is written until you press Save."),
            &[("account", account)],
        )
    } else {
        let refs: Vec<&str> = across.iter().map(String::as_str).collect();
        fill_plural(
            "Editing the {account} contact. {others} keeps its own. Nothing is written until you press Save.",
            "Editing the {account} contact. {others} keep their own. Nothing is written until you press Save.",
            across.len(),
            &[
                ("account", account),
                ("others", &crate::protection::joined(&refs)),
            ],
        )
    }
}
