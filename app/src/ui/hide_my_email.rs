//! The Hide My Email dialog: the plus addresses made so far, with a page to
//! make a new one. The work happens in the app's Hide My Email methods.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::{Account, AccountId};
use mailrs_sync::Permitted;

use crate::app::App;
use crate::hide_my_email::HiddenAddress;
use crate::ui::confirm::{Tone, confirm};
use mailrs_domain::translate::{fill, gettext};

/// What the list says a hidden address is for.
fn about() -> String {
    gettext(
        "Gmail delivers mail sent to these addresses to your inbox. Each one shows \
         which site shared your address, and you can turn one off when the spam \
         starts. Your real address is part of each one, so anyone who removes the tag \
         can still reach you, and your replies come from your main address.",
    )
}

struct Dialog {
    app: Rc<App>,
    accounts: Vec<Account>,
    /// The account new addresses belong to unless the user picks another.
    preselect: Option<AccountId>,
    nav: adw::NavigationView,
    home: adw::NavigationPage,
    stack: gtk::Stack,
    list: adw::PreferencesGroup,
    /// Rows in `list` now, removed on each reload.
    shown: RefCell<Vec<gtk::Widget>>,
    toasts: adw::ToastOverlay,
    grant: Box<dyn Fn(String)>,
    dialog: adw::Dialog,
}

/// Shows the Hide My Email addresses. `grant` runs with an account address
/// when Gmail wants the settings permission first.
pub fn present(
    app: &Rc<App>,
    parent: &impl IsA<gtk::Widget>,
    preselect: Option<AccountId>,
    grant: impl Fn(String) + 'static,
) {
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    let list = adw::PreferencesGroup::builder()
        .description(about())
        .build();
    let page = adw::PreferencesPage::new();
    page.add(&list);
    stack.add_named(&page, Some("list"));
    let add = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text(gettext("Create Address"))
        .build();
    crate::ui::name(&add, &gettext("Create Address"));
    let header = adw::HeaderBar::new();
    header.pack_start(&add);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));
    let home = adw::NavigationPage::builder()
        .title(gettext("Hide My Email"))
        .tag("addresses")
        .child(&toolbar)
        .build();
    let nav = adw::NavigationView::new();
    nav.add(&home);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&nav));
    let dialog = adw::Dialog::builder()
        .content_width(560)
        .content_height(640)
        .child(&toasts)
        .build();
    let this = Rc::new(Dialog {
        app: Rc::clone(app),
        accounts: app.accounts(),
        preselect,
        nav,
        home,
        stack,
        list,
        shown: RefCell::new(Vec::new()),
        toasts,
        grant: Box::new(grant),
        dialog: dialog.clone(),
    });
    let weak = Rc::downgrade(&this);
    add.connect_clicked(move |_| {
        if let Some(this) = weak.upgrade() {
            this.show_form();
        }
    });
    // The dialog owns the state behind its buttons until it closes.
    let keep = RefCell::new(Some(Rc::clone(&this)));
    dialog.connect_closed(move |_| {
        keep.borrow_mut().take();
    });
    dialog.present(Some(parent));
    this.reload();
    if this.app.hidden_addresses().is_empty() {
        this.show_form();
    }
}

fn created_on(ts: mailrs_domain::EpochMillis) -> String {
    crate::format::local(ts)
        .map(|when| {
            fill(
                &gettext("Created {date}"),
                &[("date", &when.format(&gettext("%-d %b %Y")).to_string())],
            )
        })
        .unwrap_or_default()
}

fn copy(widget: &impl IsA<gtk::Widget>, text: &str) {
    widget.as_ref().clipboard().set_text(text);
}

impl Dialog {
    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    /// Handles a failed Gmail call with a toast naming what it was doing.
    /// `said` is the whole sentence, with `{reason}` where the error goes.
    fn failed(&self, err: &anyhow::Error, said: &str) {
        self.toast(&fill(said, &[("reason", &err.to_string())]));
    }

    fn reload(self: &Rc<Self>) {
        for row in self.shown.borrow_mut().drain(..) {
            self.list.remove(&row);
        }
        let addresses = self.app.hidden_addresses();
        if addresses.is_empty() {
            let row = adw::ActionRow::builder()
                .title(gettext("No addresses yet"))
                .subtitle(gettext("Create one with the + button."))
                .build();
            self.list.add(&row);
            self.shown.borrow_mut().push(row.upcast());
        }
        for hidden in addresses.iter().rev() {
            let row = self.row(hidden);
            self.list.add(&row);
            self.shown.borrow_mut().push(row.upcast());
        }
        self.stack.set_visible_child_name("list");
    }

    fn row(self: &Rc<Self>, hidden: &HiddenAddress) -> adw::ActionRow {
        let mut details = Vec::new();
        if !hidden.note.is_empty() {
            details.push(hidden.note.clone());
        }
        details.push(created_on(hidden.created));
        if !hidden.active {
            details.push(gettext("Off: mail goes to the Trash"));
        }
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&hidden.address))
            .subtitle(glib::markup_escape_text(&details.join(" · ")))
            .title_selectable(true)
            .build();
        if !hidden.active {
            row.add_css_class("dim-label");
        }
        let button = |icon: &str, tip: String, spoken: String| {
            let button = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tip)
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            crate::ui::name(&button, &spoken);
            button
        };
        let copy_button = button(
            "edit-copy-symbolic",
            gettext("Copy Address"),
            fill(&gettext("Copy {address}"), &[("address", &hidden.address)]),
        );
        let address = hidden.address.clone();
        let weak = Rc::downgrade(self);
        copy_button.connect_clicked(move |b| {
            copy(b, &address);
            if let Some(this) = weak.upgrade() {
                this.toast(&gettext("Address copied"));
            }
        });
        row.add_suffix(&copy_button);

        let switch = gtk::Switch::builder()
            .active(hidden.active)
            .valign(gtk::Align::Center)
            .tooltip_text(gettext("Receive Mail"))
            .build();
        crate::ui::name(
            &switch,
            &fill(
                &gettext("Receive mail at {address}"),
                &[("address", &hidden.address)],
            ),
        );
        let (address, account) = (hidden.address.clone(), hidden.account.clone());
        let weak = Rc::downgrade(self);
        switch.connect_state_set(move |switch, active| {
            let Some(this) = weak.upgrade() else {
                return glib::Propagation::Stop;
            };
            switch.set_sensitive(false);
            let (address, account) = (address.clone(), account.clone());
            glib::spawn_future_local(async move {
                match this.app.set_hidden_address_active(&address, active).await {
                    Ok(Permitted::Done(())) if active => {
                        this.toast(&gettext("Mail to this address reaches you again"))
                    }
                    Ok(Permitted::Done(())) => {
                        this.toast(&gettext("Mail to this address now goes to the Trash"))
                    }
                    Ok(Permitted::NeedsPermission) => this.ask_for_access(&account),
                    Err(err) => {
                        this.failed(&err, &gettext("Could not change the address: {reason}"))
                    }
                }
                this.reload();
            });
            glib::Propagation::Stop
        });
        row.add_suffix(&switch);

        let delete = button(
            "user-trash-symbolic",
            gettext("Delete Address"),
            fill(
                &gettext("Delete {address}"),
                &[("address", &hidden.address)],
            ),
        );
        let (address, account) = (hidden.address.clone(), hidden.account.clone());
        let weak = Rc::downgrade(self);
        delete.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.confirm_delete(address.clone(), account.clone());
            }
        });
        row.add_suffix(&delete);
        row
    }

    fn confirm_delete(self: &Rc<Self>, address: String, account: String) {
        let question = confirm(
            &gettext("Delete Address?"),
            &fill(
                &gettext(
                    "Mail sent to {address} arrives in your inbox again, without the \
                     Hide My Email label. To stop that mail, turn the address off instead.",
                ),
                &[("address", &address)],
            ),
            &gettext("Delete"),
            Tone::Destructive,
        );
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if !question.ask(&this.dialog).await {
                return;
            }
            match this.app.delete_hidden_address(&address).await {
                Ok(Permitted::Done(())) => this.toast(&gettext("Address deleted")),
                Ok(Permitted::NeedsPermission) => this.ask_for_access(&account),
                Err(err) => this.failed(&err, &gettext("Could not delete the address: {reason}")),
            }
            this.reload();
        });
    }

    fn ask_for_access(self: &Rc<Self>, account: &str) {
        self.nav.pop_to_page(&self.home);
        let page = adw::StatusPage::builder()
            .icon_name("mail-send-symbolic")
            .title(gettext("Allow Hide My Email"))
            .description(fill(
                &gettext(
                    "Penguin Mail needs permission to change Gmail settings for \
                     {account}. Google asks you to confirm in your browser.",
                ),
                &[("account", account)],
            ))
            .build();
        let button = gtk::Button::builder()
            .label(gettext("Grant Access"))
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        let (weak, account) = (Rc::downgrade(self), account.to_string());
        button.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.dialog.close();
                (this.grant)(account.clone());
            }
        });
        page.set_child(Some(&button));
        if let Some(old) = self.stack.child_by_name("access") {
            self.stack.remove(&old);
        }
        self.stack.add_named(&page, Some("access"));
        self.stack.set_visible_child_name("access");
    }

    /// The Create Address page.
    fn show_form(self: &Rc<Self>) {
        if self.accounts.is_empty() {
            return self.toast(&gettext("Add an account first"));
        }
        let group = adw::PreferencesGroup::builder()
            .description(gettext(
                "You get a new address that ends in your own, such as \
                 name+kite.fern482@gmail.com. Give it to one site only.",
            ))
            .build();
        let emails: Vec<&str> = self.accounts.iter().map(|a| a.email.as_str()).collect();
        let account = adw::ComboRow::builder()
            .title(gettext("Account"))
            .model(&gtk::StringList::new(&emails))
            .visible(self.accounts.len() > 1)
            .build();
        if let Some(at) = self
            .preselect
            .and_then(|id| self.accounts.iter().position(|a| a.id == id))
        {
            account.set_selected(at as u32);
        }
        let note = adw::EntryRow::builder()
            .title(gettext("Where Did You Use It?"))
            .build();
        group.add(&account);
        group.add(&note);
        let page = adw::PreferencesPage::new();
        page.add(&group);
        let create = gtk::Button::builder()
            .label(gettext("Create"))
            .css_classes(["suggested-action"])
            .build();
        let header = adw::HeaderBar::new();
        header.pack_end(&create);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&page));
        self.nav.push(
            &adw::NavigationPage::builder()
                .title(gettext("Create Address"))
                .tag("create")
                .child(&toolbar)
                .build(),
        );
        note.grab_focus();

        let (weak, entry) = (Rc::downgrade(self), note.clone());
        let run = move |button: &gtk::Button| {
            let Some(this) = weak.upgrade() else { return };
            let Some(chosen) = this.accounts.get(account.selected() as usize).cloned() else {
                return;
            };
            let text = entry.text().to_string();
            button.set_sensitive(false);
            let button = button.clone();
            glib::spawn_future_local(async move {
                match this.app.create_hidden_address(chosen.id, &text).await {
                    Ok(Permitted::Done(hidden)) => {
                        this.reload();
                        this.show_created(&hidden);
                    }
                    Ok(Permitted::NeedsPermission) => {
                        button.set_sensitive(true);
                        this.ask_for_access(&chosen.email);
                    }
                    Err(err) => {
                        button.set_sensitive(true);
                        this.failed(&err, &gettext("Could not create the address: {reason}"));
                    }
                }
            });
        };
        let press = run.clone();
        let button = create.clone();
        note.connect_entry_activated(move |_| press(&button));
        create.connect_clicked(run);
    }

    /// The new address, copied to the clipboard, in place of the form.
    fn show_created(self: &Rc<Self>, hidden: &HiddenAddress) {
        copy(&self.dialog, &hidden.address);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .valign(gtk::Align::Center)
            .margin_start(24)
            .margin_end(24)
            .margin_top(24)
            .margin_bottom(24)
            .build();
        content.append(
            &gtk::Label::builder()
                .label(gettext("Your New Address"))
                .css_classes(["title-2"])
                .build(),
        );
        content.append(
            &gtk::Label::builder()
                .label(&hidden.address)
                .selectable(true)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .justify(gtk::Justification::Center)
                .css_classes(["title-4", "monospace"])
                .build(),
        );
        let copy_button = gtk::Button::builder()
            .label(gettext("Copy"))
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .build();
        let (address, weak) = (hidden.address.clone(), Rc::downgrade(self));
        copy_button.connect_clicked(move |b| {
            copy(b, &address);
            if let Some(this) = weak.upgrade() {
                this.toast(&gettext("Address copied"));
            }
        });
        content.append(&copy_button);
        content.append(
            &gtk::Label::builder()
                .label(gettext(
                    "It is on your clipboard. Mail sent to it gets the Hide My Email \
                     label in Gmail.",
                ))
                .wrap(true)
                .justify(gtk::Justification::Center)
                .css_classes(["dim-label"])
                .build(),
        );
        let done = gtk::Button::builder()
            .label(gettext("Done"))
            .css_classes(["suggested-action"])
            .build();
        let weak = Rc::downgrade(self);
        done.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.nav.pop_to_page(&this.home);
            }
        });
        let header = adw::HeaderBar::builder().show_back_button(false).build();
        header.pack_end(&done);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&content));
        let page = adw::NavigationPage::builder()
            .title(gettext("Address Created"))
            .tag("created")
            .child(&toolbar)
            .build();
        self.nav.replace(&[self.home.clone(), page]);
    }
}
