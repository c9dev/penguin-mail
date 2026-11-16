//! A separate address book window, with the selected contact beside its list.

use crate::app::App;
use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::translate::{fill, gettext};
use mailrs_store::address_book::{self, Contact};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

mod editor;
mod photo;

pub struct Window {
    pub window: adw::Window,
    app: Weak<App>,
    list: gtk::ListBox,
    detail: gtk::Box,
    search: gtk::SearchEntry,
    account: gtk::DropDown,
    group: gtk::DropDown,
    groups: RefCell<Vec<String>>,
    owners: Vec<mailrs_domain::Account>,
    contacts: RefCell<Vec<Contact>>,
    shown: RefCell<Vec<Contact>>,
    toast: adw::ToastOverlay,
    refreshing: Cell<bool>,
    generation: Cell<u64>,
    search_mail: Box<dyn Fn(String)>,
}

impl Window {
    pub fn new(
        app: &Rc<App>,
        parent: &adw::Window,
        search_mail: impl Fn(String) + 'static,
    ) -> Rc<Self> {
        let window = adw::Window::builder()
            .title(gettext("Contacts"))
            .default_width(980)
            .default_height(700)
            .build();
        window.set_application(parent.application().as_ref());
        let child = window.downgrade();
        parent.connect_destroy(move |_| {
            if let Some(window) = child.upgrade() {
                window.close();
            }
        });
        let owners = app
            .accounts()
            .into_iter()
            .filter(|a| app.core.demo || app.settings().reads_contacts(&a.email))
            .collect::<Vec<_>>();
        let mut names = vec![gettext("All Accounts")];
        names.extend(owners.iter().map(|a| a.email.clone()));
        let account =
            gtk::DropDown::from_strings(&names.iter().map(String::as_str).collect::<Vec<_>>());
        super::name(&account, &gettext("Address Book"));
        let group = gtk::DropDown::from_strings(&[&gettext("All Groups")]);
        super::name(&group, &gettext("Group"));
        let search = gtk::SearchEntry::builder()
            .placeholder_text(gettext("Search Contacts"))
            .build();
        super::name(&search, &gettext("Search Contacts"));
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(["navigation-sidebar"])
            .build();
        super::name(&list, &gettext("Contacts"));
        let empty = gtk::Label::new(Some(&gettext("No Matching Contacts")));
        empty.set_margin_top(24);
        list.set_placeholder(Some(&empty));
        let left = gtk::Box::new(gtk::Orientation::Vertical, 8);
        left.set_width_request(260);
        for widget in [
            search.clone().upcast::<gtk::Widget>(),
            account.clone().upcast(),
            group.clone().upcast(),
        ] {
            left.append(&widget);
        }
        left.append(
            &gtk::ScrolledWindow::builder()
                .child(&list)
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build(),
        );
        let detail = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        let split = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(&left)
            .end_child(
                &gtk::ScrolledWindow::builder()
                    .child(&detail)
                    .hexpand(true)
                    .hscrollbar_policy(gtk::PolicyType::Never)
                    .build(),
            )
            .position(300)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .build();
        super::name(&split, &gettext("Contact List Width"));
        let header = adw::HeaderBar::new();
        let add = gtk::Button::builder()
            .icon_name("contact-new-symbolic")
            .tooltip_text(gettext("New Contact"))
            .build();
        super::name(&add, &gettext("New Contact"));
        let refresh = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text(gettext("Refresh Contacts"))
            .build();
        super::name(&refresh, &gettext("Refresh Contacts"));
        header.pack_start(&add);
        header.pack_end(&refresh);
        let view = adw::ToolbarView::builder().content(&split).build();
        view.add_top_bar(&header);
        let toast = adw::ToastOverlay::new();
        toast.set_child(Some(&view));
        window.set_content(Some(&toast));
        let this = Rc::new(Self {
            window,
            app: Rc::downgrade(app),
            list,
            detail,
            search,
            account,
            group,
            groups: RefCell::default(),
            owners,
            contacts: RefCell::default(),
            shown: RefCell::default(),
            toast,
            refreshing: Cell::new(false),
            generation: Cell::new(0),
            search_mail: Box::new(search_mail),
        });
        let weak = Rc::downgrade(&this);
        this.search.connect_search_changed(move |_| {
            if let Some(w) = weak.upgrade() {
                w.filter();
            }
        });
        let weak = Rc::downgrade(&this);
        this.account.connect_selected_notify(move |_| {
            if let Some(w) = weak.upgrade() {
                w.filter();
            }
        });
        let weak = Rc::downgrade(&this);
        this.group.connect_selected_notify(move |_| {
            if let Some(w) = weak.upgrade() {
                w.filter();
            }
        });
        let weak = Rc::downgrade(&this);
        this.list.connect_row_selected(move |_, row| {
            if let Some(w) = weak.upgrade() {
                let contact = row.and_then(|r| w.shown.borrow().get(r.index() as usize).cloned());
                w.show(contact);
            }
        });
        let weak = Rc::downgrade(&this);
        add.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                editor::present(&w, None);
            }
        });
        let weak = Rc::downgrade(&this);
        refresh.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.refresh();
            }
        });
        this.reload();
        this.window.present();
        this.refresh();
        this
    }

    fn say(&self, message: &str) {
        self.toast.add_toast(adw::Toast::new(message));
    }

    fn reload(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = app.core.read(address_book::list).await;
            let Some(this) = weak.upgrade().filter(|w| w.generation.get() == generation) else {
                return;
            };
            match result {
                Ok(mut contacts) => {
                    contacts.retain(|c| {
                        this.owners.iter().any(|a| {
                            a.id == c.account_id
                                && (app.core.demo || app.settings().reads_contacts(&a.email))
                        })
                    });
                    let mut groups: Vec<_> = contacts
                        .iter()
                        .flat_map(|c| c.details.groups.clone())
                        .collect();
                    groups.sort();
                    groups.dedup();
                    let mut names = vec![gettext("All Groups")];
                    names.extend(groups.clone());
                    *this.groups.borrow_mut() = groups;
                    *this.contacts.borrow_mut() = contacts;
                    this.group.set_model(Some(&gtk::StringList::new(
                        &names.iter().map(String::as_str).collect::<Vec<_>>(),
                    )));
                    this.filter();
                }
                Err(err) => this.say(&fill(
                    &gettext("Could not load contacts: {reason}"),
                    &[("reason", &err.to_string())],
                )),
            }
        });
    }

    fn filter(self: &Rc<Self>) {
        let owner = self
            .account
            .selected()
            .checked_sub(1)
            .and_then(|i| self.owners.get(i as usize))
            .map(|a| a.id);
        let group = self
            .group
            .selected()
            .checked_sub(1)
            .and_then(|i| self.groups.borrow().get(i as usize).cloned());
        let query = self.search.text().to_lowercase();
        let shown: Vec<_> = self
            .contacts
            .borrow()
            .iter()
            .filter(|c| {
                owner.is_none_or(|id| c.account_id == id)
                    && group.as_ref().is_none_or(|g| c.details.groups.contains(g))
                    && matches(c, &query)
            })
            .cloned()
            .collect();
        let selected = self.list.selected_row().and_then(|r| {
            self.shown
                .borrow()
                .get(r.index() as usize)
                .map(|c| (c.account_id, c.resource.clone()))
        });
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        *self.shown.borrow_mut() = shown.clone();
        for contact in shown {
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .title(contact.display())
                .subtitle(contact.email().unwrap_or_default())
                .build();
            self.list.append(&row);
        }
        let index = self
            .shown
            .borrow()
            .iter()
            .position(|c| {
                selected
                    .as_ref()
                    .is_some_and(|(id, resource)| c.account_id == *id && c.resource == *resource)
            })
            .unwrap_or(0);
        self.list
            .select_row(self.list.row_at_index(index as i32).as_ref());
        if self.shown.borrow().is_empty() {
            self.show(None);
        }
    }

    fn show(self: &Rc<Self>, contact: Option<Contact>) {
        while let Some(child) = self.detail.first_child() {
            self.detail.remove(&child);
        }
        let Some(contact) = contact else {
            self.detail.append(
                &adw::StatusPage::builder()
                    .title(gettext("Select a Contact"))
                    .icon_name("x-office-address-book-symbolic")
                    .description(gettext(
                        "Choose a contact or add someone to an address book.",
                    ))
                    .build(),
            );
            return;
        };
        let avatar = adw::Avatar::builder()
            .size(128)
            .text(contact.display())
            .show_initials(true)
            .halign(gtk::Align::Center)
            .build();
        if let Some(app) = self.app.upgrade() {
            photo::load(&avatar, &contact, &app);
        }
        let view_photo = gtk::Button::builder()
            .child(&avatar)
            .halign(gtk::Align::Center)
            .css_classes(["flat"])
            .sensitive(contact.photo_file.is_some())
            .build();
        super::name(&view_photo, &gettext("View Photo"));
        let weak = Rc::downgrade(self);
        let selected = contact.clone();
        view_photo.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade()
                && let Some(app) = w.app.upgrade()
            {
                photo::view(&w.window, &selected, &app);
            }
        });
        self.detail.append(&view_photo);
        self.detail.append(
            &gtk::Label::builder()
                .label(contact.display())
                .selectable(true)
                .wrap(true)
                .css_classes(["title-1"])
                .build(),
        );
        if let Some(owner) = self.owners.iter().find(|a| a.id == contact.account_id) {
            self.detail.append(&gtk::Label::new(Some(&owner.email)));
        }
        let actions = gtk::Box::builder()
            .spacing(8)
            .halign(gtk::Align::Center)
            .build();
        let edit = gtk::Button::with_label(&gettext("Edit…"));
        actions.append(&edit);
        let weak = Rc::downgrade(self);
        let selected = contact.clone();
        edit.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                editor::present(&w, Some(selected.clone()));
            }
        });
        if self
            .app
            .upgrade()
            .is_some_and(|a| a.core.demo || a.core.contacts().edits_details(contact.account_id))
        {
            let delete = gtk::Button::with_label(&gettext("Delete"));
            delete.add_css_class("destructive-action");
            actions.append(&delete);
            let weak = Rc::downgrade(self);
            let selected = contact.clone();
            delete.connect_clicked(move |_| {
                if let Some(w) = weak.upgrade() {
                    w.delete(selected.clone());
                }
            });
        }
        self.detail.append(&actions);
        let emails = adw::PreferencesGroup::builder()
            .title(gettext("Email"))
            .build();
        emails.set_visible(!contact.emails.is_empty());
        for email in &contact.emails {
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .title(email)
                .build();
            let write = icon("mail-message-new-symbolic", &gettext("New Message"));
            let weak_app = self.app.clone();
            let address = email.clone();
            let id = contact.account_id;
            write.connect_clicked(move |_| {
                if let Some(app) = weak_app.upgrade() {
                    app.new_message(Some(id), &address);
                }
            });
            row.add_suffix(&write);
            let search = icon(
                "system-search-symbolic",
                &gettext("Mail to or from This Address"),
            );
            let weak = Rc::downgrade(self);
            let address = email.clone();
            search.connect_clicked(move |_| {
                if let Some(w) = weak.upgrade() {
                    (w.search_mail)(address.clone());
                }
            });
            row.add_suffix(&search);
            row.add_suffix(&copy(email));
            emails.add(&row);
        }
        self.detail.append(&emails);
        let phones = adw::PreferencesGroup::builder()
            .title(gettext("Phone"))
            .build();
        let numbers = if contact.details.phones.is_empty() {
            contact.phone.iter().cloned().collect()
        } else {
            contact.details.phones.clone()
        };
        phones.set_visible(!numbers.is_empty());
        for number in numbers {
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .title(&number)
                .build();
            for (scheme, label, image) in [
                ("tel", gettext("Call"), "call-start-symbolic"),
                ("sms", gettext("Send SMS"), "mail-message-new-symbolic"),
            ] {
                let button = icon(image, &label);
                let weak = Rc::downgrade(self);
                let uri = phone_uri(scheme, &number);
                button.connect_clicked(move |_| {
                    if let Some(w) = weak.upgrade() {
                        let weak = Rc::downgrade(&w);
                        gtk::UriLauncher::new(&uri).launch(
                            Some(&w.window),
                            gio::Cancellable::NONE,
                            move |result| {
                                if let Err(err) = result
                                    && let Some(w) = weak.upgrade()
                                {
                                    w.say(&err.to_string());
                                }
                            },
                        );
                    }
                });
                row.add_suffix(&button);
            }
            row.add_suffix(&copy(&number));
            phones.add(&row);
        }
        self.detail.append(&phones);
        let details = adw::PreferencesGroup::builder()
            .title(gettext("Details"))
            .build();
        let d = &contact.details;
        for (label, value) in [
            (
                gettext("Organization"),
                contact.organization.clone().unwrap_or_default(),
            ),
            (
                gettext("Birthday"),
                mailrs_domain::contact::birthday_label(
                    &d.birthday,
                    chrono::Local::now().date_naive(),
                ),
            ),
            (gettext("Groups"), d.groups.join(", ")),
            (gettext("Postal Addresses"), d.addresses.join("\n\n")),
            (gettext("Notes"), d.notes.clone()),
        ] {
            if !value.is_empty() {
                details.add(
                    &adw::ActionRow::builder()
                        .use_markup(false)
                        .title(label)
                        .subtitle(value)
                        .build(),
                );
            }
        }
        for field in d.dates.iter().chain(&d.custom) {
            details.add(
                &adw::ActionRow::builder()
                    .use_markup(false)
                    .title(&field.label)
                    .subtitle(&field.value)
                    .build(),
            );
        }
        self.detail.append(&details);
        if !d.urls.is_empty() {
            let websites = adw::PreferencesGroup::builder()
                .title(gettext("Websites"))
                .build();
            for address in &d.urls {
                if let Some(uri) = website_uri(address) {
                    let label = gtk::Label::builder()
                        .label(address)
                        .xalign(0.0)
                        .wrap(true)
                        .wrap_mode(gtk::pango::WrapMode::WordChar)
                        .build();
                    let link = gtk::LinkButton::builder().uri(uri).child(&label).build();
                    super::name(&link, address);
                    websites.add(&link);
                } else {
                    websites.add(
                        &adw::ActionRow::builder()
                            .use_markup(false)
                            .title(address)
                            .title_lines(0)
                            .build(),
                    );
                }
            }
            self.detail.append(&websites);
        }
    }

    fn refresh(self: &Rc<Self>) {
        if self.refreshing.replace(true) {
            return;
        }
        let Some(app) = self.app.upgrade() else {
            self.refreshing.set(false);
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            if !app.core.demo {
                for account in app
                    .accounts()
                    .into_iter()
                    .filter(|a| app.settings().reads_contacts(&a.email))
                {
                    let book = app.core.contacts();
                    let result = app
                        .core
                        .call(async move { book.refresh(account.id).await })
                        .await;
                    if let Some(w) = weak.upgrade() {
                        match result {
                            Ok(mailrs_sync::Permitted::Done(_)) => {}
                            Ok(mailrs_sync::Permitted::NeedsPermission) => w.say(&gettext(
                                "Allow contacts access in Preferences, then try again.",
                            )),
                            Err(err) => w.say(&err.to_string()),
                        }
                    }
                }
            }
            app.contacts_added();
            if let Some(w) = weak.upgrade() {
                w.refreshing.set(false);
                w.reload();
            }
        });
    }

    fn delete(self: &Rc<Self>, contact: Contact) {
        self.search.grab_focus();
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Delete Contact?"))
            .body(fill(
                &gettext("Delete {name} from its account? This cannot be undone."),
                &[("name", contact.display())],
            ))
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("Cancel")),
            ("delete", &gettext("Delete")),
        ]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.choose(
            Some(&self.window),
            gio::Cancellable::NONE,
            move |response| {
                if response != "delete" {
                    return;
                }
                let Some(w) = weak.upgrade() else {
                    return;
                };
                let Some(app) = w.app.upgrade() else {
                    return;
                };
                glib::spawn_future_local(async move {
                    let result = if app.core.demo {
                        app.core
                            .write(move |c| {
                                address_book::forget(c, contact.account_id, &[contact.resource])
                            })
                            .await
                    } else {
                        let book = app.core.contacts();
                        app.core
                            .call(async move {
                                book.delete(
                                    contact.account_id,
                                    contact.resource,
                                    (!contact.details.revision.is_empty())
                                        .then_some(contact.details.revision),
                                )
                                .await
                            })
                            .await
                    };
                    if let Some(w) = weak.upgrade() {
                        match result {
                            Ok(()) => {
                                app.contacts_added();
                                w.reload();
                            }
                            Err(err) => w.say(&err.to_string()),
                        }
                    }
                });
            },
        );
    }
}

fn icon(image: &str, label: &str) -> gtk::Button {
    let b = gtk::Button::builder()
        .icon_name(image)
        .tooltip_text(label)
        .valign(gtk::Align::Center)
        .build();
    super::name(&b, label);
    b
}
fn copy(text: &str) -> gtk::Button {
    let b = icon("edit-copy-symbolic", &gettext("Copy"));
    let text = text.to_string();
    b.connect_clicked(move |b| b.clipboard().set_text(&text));
    b
}
fn website_uri(address: &str) -> Option<String> {
    let address = address.trim();
    let url = url::Url::parse(address)
        .or_else(|_| url::Url::parse(&format!("https://{address}")))
        .ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.to_string())
}

fn phone_uri(scheme: &str, number: &str) -> String {
    format!(
        "{scheme}:{}",
        glib::uri_escape_string(number, Some("+"), false)
    )
}
fn matches(contact: &Contact, query: &str) -> bool {
    let text = format!(
        "{} {} {} {} {}",
        contact.display(),
        contact.emails.join(" "),
        contact.details.phones.join(" "),
        contact.phone.as_deref().unwrap_or_default(),
        contact.organization.as_deref().unwrap_or_default()
    )
    .to_lowercase();
    query.split_whitespace().all(|word| text.contains(word))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contact_websites_open_web_addresses_only() {
        assert_eq!(
            website_uri("https://example.org/a?q=1&b=2").as_deref(),
            Some("https://example.org/a?q=1&b=2")
        );
        assert_eq!(
            website_uri("http://example.org").as_deref(),
            Some("http://example.org/")
        );
        assert_eq!(
            website_uri(" www.example.org ").as_deref(),
            Some("https://www.example.org/")
        );
        for value in [
            "",
            "not a website",
            "file:///tmp/file",
            "javascript:alert(1)",
            "mailto:demo@example.org",
            "https://name:password@example.org",
        ] {
            assert_eq!(website_uri(value), None, "{value}");
        }
    }

    #[test]
    fn search_and_phone_links_keep_the_whole_value() {
        let contact = Contact {
            name: Some("Mara Okafor".into()),
            emails: vec!["mara@example.org".into()],
            phone: Some("+351 210000000".into()),
            ..Default::default()
        };
        assert!(matches(&contact, "mara 2100"));
        assert!(!matches(&contact, "theo"));
        assert_eq!(phone_uri("tel", "+1#2?3"), "tel:+1%232%3F3");
    }
}
