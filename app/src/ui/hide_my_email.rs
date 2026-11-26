//! The Hide My Email dialog: the plus addresses made so far and the
//! masked addresses of accounts whose server makes them, with a page to
//! make a new one. The work happens in the app's Hide My Email methods.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::{Account, AccountId, Provider};
use mailrs_sync::hidden::HiddenAddress;
use mailrs_sync::masked::{MaskedAddress, MaskedState};
use mailrs_sync::{Offers, Permitted};

use crate::app::App;
use crate::permission::Permission;
use crate::ui::confirm::{Tone, confirm};
use crate::ui::permission;
use mailrs_domain::translate::{fill, gettext, with_reason};

/// What the list says a hidden address is for.
fn about() -> String {
    gettext(
        "Gmail delivers mail sent to these addresses to your inbox. Each one shows \
         which site shared your address, and you can turn one off when the spam \
         starts. Your real address is part of each one, so anyone who removes the tag \
         can still reach you, and your replies come from your main address.",
    )
}

/// What the list says when masked addresses sit beside plus addresses.
fn about_masked() -> String {
    gettext(
        "Addresses that hide your own. Each one shows which site you gave it to, \
         and you can turn one off when the spam starts.",
    )
}

/// The accounts that can take a hidden address: a server that makes
/// masked addresses, or rules on the server, one that labels a plus
/// address's mail and one that sends it to the Trash once it is turned
/// off ([`crate::offered::hides_addresses`]).
fn with_rules(accounts: Vec<Account>, offers: impl Fn(AccountId) -> Offers) -> Vec<Account> {
    accounts
        .into_iter()
        .filter(|a| crate::offered::hides_addresses(offers(a.id)))
        .collect()
}

/// `address` with a break allowed before its `@`, so a narrow row wraps it
/// there rather than in the middle of a word. The break is no part of the
/// address, so whatever shows it is named with the plain address for
/// screen readers.
fn breakable(address: &str) -> String {
    address.replacen('@', "\u{200b}@", 1)
}

/// A masked address's subtitle: the site, the note, the day it was made,
/// then what its state means.
fn masked_details(masked: &MaskedAddress) -> String {
    let mut details = vec![masked.site.clone()];
    if !masked.note.is_empty() {
        details.push(masked.note.clone());
    }
    details.extend(masked.created.map(created_on));
    match masked.state {
        MaskedState::Disabled => details.push(gettext("Off: mail to it is dropped")),
        MaskedState::Pending => details.push(gettext("Waiting for its first message")),
        MaskedState::Enabled | MaskedState::Deleted => {}
    }
    details.retain(|d| !d.is_empty());
    details.join(" · ")
}

struct Dialog {
    app: Rc<App>,
    accounts: Vec<Account>,
    /// The accounts in `accounts` whose server makes masked addresses.
    masked: Vec<Account>,
    /// The account new addresses belong to unless the user picks another.
    preselect: Option<AccountId>,
    nav: adw::NavigationView,
    home: adw::NavigationPage,
    stack: gtk::Stack,
    list: adw::PreferencesGroup,
    /// Rows in `list` now, removed on each reload.
    shown: RefCell<Vec<gtk::Widget>>,
    /// Counts reloads, so masked addresses read for an earlier one add no
    /// rows to a newer list.
    reloads: Cell<u64>,
    toasts: adw::ToastOverlay,
    grant: Box<dyn Fn(String)>,
    dialog: adw::Dialog,
}

/// Shows the Hide My Email addresses. New ones go to the accounts whose
/// server `offers` masked addresses, or rules and labels. `grant` runs with an account address when Gmail
/// wants the settings permission first.
pub fn present(
    app: &Rc<App>,
    parent: &impl IsA<gtk::Widget>,
    preselect: Option<AccountId>,
    offers: impl Fn(AccountId) -> Offers,
    grant: impl Fn(String) + 'static,
) {
    // Each page of the stack is as tall as it needs, so the dialog fits
    // the list rather than the tallest page.
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .vhomogeneous(false)
        .build();
    let accounts = with_rules(app.mail_accounts(), &offers);
    let masked: Vec<Account> = accounts
        .iter()
        .filter(|a| offers(a.id).masked_email)
        .cloned()
        .collect();
    let list = adw::PreferencesGroup::builder()
        .description(if masked.is_empty() {
            about()
        } else {
            about_masked()
        })
        .build();
    stack.add_named(&fitted(&list), Some("list"));
    let add = gtk::Button::builder()
        .icon_name("penguin-mail-list-add-symbolic")
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
        .content_width(CONTENT_WIDTH)
        .child(&toasts)
        .build();

    let this = Rc::new(Dialog {
        app: Rc::clone(app),
        accounts,
        masked,
        preselect,
        nav,
        home,
        stack,
        list,
        shown: RefCell::new(Vec::new()),
        reloads: Cell::new(0),
        toasts,
        grant: Box::new(grant),
        dialog: dialog.clone(),
    });
    let weak = Rc::downgrade(&this);
    this.nav.connect_visible_page_notify(move |_| {
        if let Some(this) = weak.upgrade() {
            this.fit();
        }
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
    this.reload();
    dialog.present(Some(parent));
    if this.app.hidden_addresses().is_empty() && this.masked.is_empty() {
        this.show_form();
    }
}

fn created_on(ts: mailrs_domain::EpochMillis) -> String {
    crate::format::local(ts)
        .map(|when| {
            fill(
                &gettext("Created {date}"),
                &[(
                    "date",
                    &crate::format::day_month(when.date_naive(), chrono::Local::now().date_naive()),
                )],
            )
            // One line holds the whole date, however the subtitle wraps.
            .replace(' ', "\u{a0}")
        })
        .unwrap_or_default()
}

/// The dialog's width, and its height while a page other than the list
/// shows.
const CONTENT_WIDTH: i32 = 560;
const FULL_HEIGHT: i32 = 640;

/// `group` in a page as tall as its content, which scrolls once the
/// dialog reaches the window's height. A preferences page always takes
/// the height it is given, so the list would end above an empty area.
fn fitted(group: &adw::PreferencesGroup) -> gtk::ScrolledWindow {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    column.append(group);
    let clamp = adw::Clamp::builder().child(&column).build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&clamp)
        .build()
}

fn copy(widget: &impl IsA<gtk::Widget>, text: &str) {
    widget.as_ref().clipboard().set_text(text);
}

/// A flat icon button for a row's end, with a tooltip and a spoken name.
fn icon_button(icon: &str, tip: String, spoken: String) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tip)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    crate::ui::name(&button, &spoken);
    button
}

impl Dialog {
    fn toast(&self, text: &str) {
        self.toasts.add_toast(crate::ui::toast(text));
    }

    /// Handles a failed Gmail call with a toast naming what it was doing.
    /// `said` is the whole sentence, with `{reason}` where the error goes.
    fn failed(&self, said: &str, err: &anyhow::Error) {
        self.toast(&with_reason(said, err, &[]));
    }

    /// Sizes the dialog: the list as tall as its rows, which the dialog
    /// keeps within the window and scrolls past that, and the pages pushed
    /// over it at the full height. The dialog measures its content once
    /// when it opens, so each change of the list asks again.
    fn fit(&self) {
        let on_list = self
            .nav
            .visible_page()
            .is_some_and(|page| page == self.home);
        let height = match self.dialog.child() {
            Some(content) if on_list => {
                content.measure(gtk::Orientation::Vertical, CONTENT_WIDTH).1
            }
            _ => FULL_HEIGHT,
        };
        self.dialog.set_content_height(height);
    }

    /// Adds `row` to the list until the next reload.
    fn show(&self, row: &impl IsA<gtk::Widget>) {
        self.list.add(row);
        self.shown.borrow_mut().push(row.clone().upcast());
    }

    fn show_empty(&self) {
        let row = adw::ActionRow::builder()
            .title(gettext("No addresses yet"))
            .subtitle(gettext("Create one with the + button."))
            .build();
        self.show(&row);
    }

    /// Lists the plus addresses, then reads each masked account's
    /// addresses from its server and lists those after them.
    fn reload(self: &Rc<Self>) {
        for row in self.shown.borrow_mut().drain(..) {
            self.list.remove(&row);
        }
        let reload = self.reloads.get() + 1;
        self.reloads.set(reload);
        let addresses = self.app.hidden_addresses();
        for hidden in addresses.iter().rev() {
            let row = self.row(hidden);
            self.show(&row);
        }
        self.stack.set_visible_child_name("list");
        if self.masked.is_empty() {
            if addresses.is_empty() {
                self.show_empty();
            }
            self.fit();
            return;
        }
        let loading = adw::ActionRow::builder()
            .title(gettext("Loading masked addresses…"))
            .build();
        loading.add_prefix(
            &adw::Spinner::builder()
                .width_request(16)
                .height_request(16)
                .build(),
        );
        self.show(&loading);
        self.fit();
        let (this, listed) = (Rc::clone(self), addresses.len());
        glib::spawn_future_local(async move {
            let mut read = Vec::with_capacity(this.masked.len());
            for account in &this.masked {
                read.push((account, this.app.masked_addresses(account.id).await));
            }
            if this.reloads.get() != reload {
                return;
            }
            this.list.remove(&loading);
            this.shown
                .borrow_mut()
                .retain(|row| row != loading.upcast_ref::<gtk::Widget>());
            let mut rows = listed;
            for (account, addresses) in read {
                match addresses {
                    Ok(addresses) => {
                        for masked in &addresses {
                            let row = this.masked_row(account, masked);
                            this.show(&row);
                        }
                        rows += addresses.len();
                    }
                    Err(err) => {
                        let row = this.unread_row(&err);
                        this.show(&row);
                        rows += 1;
                    }
                }
            }
            if rows == 0 {
                this.show_empty();
            }
            this.fit();
        });
    }

    /// The row in place of an account's masked addresses when its server
    /// would not list them, with a button to ask again.
    fn unread_row(self: &Rc<Self>, err: &anyhow::Error) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&with_reason(
                &gettext("Could not read the masked addresses: {reason}"),
                err,
                &[],
            )))
            .build();
        row.add_prefix(
            &gtk::Image::builder()
                .icon_name("penguin-mail-dialog-warning-symbolic")
                .css_classes(["warning"])
                .build(),
        );
        let again = gtk::Button::builder()
            .icon_name("penguin-mail-view-refresh-symbolic")
            .tooltip_text(gettext("Try Again"))
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        crate::ui::name(&again, &gettext("Try Again"));
        let weak = Rc::downgrade(self);
        again.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.reload();
            }
        });
        row.add_suffix(&again);
        row
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
            .title(glib::markup_escape_text(&breakable(&hidden.address)))
            .subtitle(glib::markup_escape_text(&details.join(" · ")))
            .build();
        crate::ui::name(&row, &hidden.address);
        if !hidden.active {
            row.add_css_class("dim-label");
        }
        let copy_button = icon_button(
            "penguin-mail-edit-copy-symbolic",
            gettext("Copy Address"),
            fill(&gettext("Copy {address}"), &[("address", &hidden.address)]),
        );
        let address = hidden.address.clone();
        let weak = Rc::downgrade(self);
        let copied = crate::ui::copied::Copied::new(&copy_button);
        copy_button.connect_clicked(move |b| {
            copy(b, &address);
            copied.show();
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
                        this.failed(&gettext("Could not change the address: {reason}"), &err)
                    }
                }
                this.reload();
            });
            glib::Propagation::Stop
        });
        row.add_suffix(&switch);

        let delete = icon_button(
            "penguin-mail-user-trash-symbolic",
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

    /// A masked address of `account`, with the same buttons as a plus
    /// address. Its server keeps the state, so each change goes there.
    fn masked_row(self: &Rc<Self>, account: &Account, masked: &MaskedAddress) -> adw::ActionRow {
        let off = masked.state == MaskedState::Disabled;
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&breakable(&masked.email)))
            .subtitle(glib::markup_escape_text(&masked_details(masked)))
            .build();
        crate::ui::name(&row, &masked.email);
        if off {
            row.add_css_class("dim-label");
        }
        let copy_button = icon_button(
            "penguin-mail-edit-copy-symbolic",
            gettext("Copy Address"),
            fill(&gettext("Copy {address}"), &[("address", &masked.email)]),
        );
        let address = masked.email.clone();
        let weak = Rc::downgrade(self);
        copy_button.connect_clicked(move |b| {
            copy(b, &address);
            if let Some(this) = weak.upgrade() {
                this.toast(&gettext("Address copied"));
            }
        });
        row.add_suffix(&copy_button);

        let switch = gtk::Switch::builder()
            .active(!off)
            .valign(gtk::Align::Center)
            .tooltip_text(gettext("Receive Mail"))
            .build();
        crate::ui::name(
            &switch,
            &fill(
                &gettext("Receive mail at {address}"),
                &[("address", &masked.email)],
            ),
        );
        let (account_id, id) = (account.id, masked.id.clone());
        let weak = Rc::downgrade(self);
        switch.connect_state_set(move |switch, active| {
            let Some(this) = weak.upgrade() else {
                return glib::Propagation::Stop;
            };
            switch.set_sensitive(false);
            let id = id.clone();
            let state = if active {
                MaskedState::Enabled
            } else {
                MaskedState::Disabled
            };
            glib::spawn_future_local(async move {
                match this.app.set_masked_state(account_id, &id, state).await {
                    Ok(()) if active => {
                        this.toast(&gettext("Mail to this address reaches you again"))
                    }
                    Ok(()) => this.toast(&gettext("Mail to this address is now dropped")),
                    Err(err) => {
                        this.failed(&gettext("Could not change the address: {reason}"), &err)
                    }
                }
                this.reload();
            });
            glib::Propagation::Stop
        });
        row.add_suffix(&switch);

        let delete = icon_button(
            "penguin-mail-user-trash-symbolic",
            gettext("Delete Address"),
            fill(&gettext("Delete {address}"), &[("address", &masked.email)]),
        );
        let (address, id) = (masked.email.clone(), masked.id.clone());
        let weak = Rc::downgrade(self);
        delete.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.confirm_masked_delete(account_id, id.clone(), &address);
            }
        });
        row.add_suffix(&delete);
        row
    }

    fn confirm_masked_delete(self: &Rc<Self>, account_id: AccountId, id: String, address: &str) {
        let question = confirm(
            &gettext("Delete Address?"),
            &fill(
                &gettext("{address} stops working, and mail sent to it is dropped."),
                &[("address", address)],
            ),
            &gettext("Delete"),
            Tone::Destructive,
        );
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if !question.ask(&this.dialog).await {
                return;
            }
            match this
                .app
                .set_masked_state(account_id, &id, MaskedState::Deleted)
                .await
            {
                Ok(()) => this.toast(&gettext("Address deleted")),
                Err(err) => this.failed(&gettext("Could not delete the address: {reason}"), &err),
            }
            this.reload();
        });
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
                Err(err) => this.failed(&gettext("Could not delete the address: {reason}"), &err),
            }
            this.reload();
        });
    }

    fn ask_for_access(self: &Rc<Self>, account: &str) {
        self.nav.pop_to_page(&self.home);
        let provider = self
            .accounts
            .iter()
            .find(|a| a.email == account)
            .map_or(Provider::Gmail, |a| a.provider);
        let (weak, email) = (Rc::downgrade(self), account.to_string());
        let page = permission::page(
            &gettext("Allow Hide My Email"),
            Permission::Settings,
            account,
            provider,
            move || {
                if let Some(this) = weak.upgrade() {
                    this.dialog.close();
                    (this.grant)(email.clone());
                }
            },
        );
        if let Some(old) = self.stack.child_by_name("access") {
            self.stack.remove(&old);
        }
        self.stack.add_named(&page, Some("access"));
        self.stack.set_visible_child_name("access");
        self.fit();
    }

    /// The Create Address page.
    fn show_form(self: &Rc<Self>) {
        if self.accounts.is_empty() {
            return self.toast(&gettext("Add an account first"));
        }
        let group = adw::PreferencesGroup::new();
        let emails: Vec<&str> = self.accounts.iter().map(|a| a.email.as_str()).collect();
        let account = adw::ComboRow::builder()
            .title(gettext("Account"))
            .model(&gtk::StringList::new(&emails))
            .visible(self.accounts.len() > 1)
            .build();
        super::combo_value::widen_value(&account);
        if let Some(at) = self
            .preselect
            .and_then(|id| self.accounts.iter().position(|a| a.id == id))
        {
            account.set_selected(at as u32);
        }
        let site = adw::EntryRow::builder().title(gettext("Website")).build();
        let note = adw::EntryRow::builder()
            .title(gettext("Where Did You Use It?"))
            .build();
        group.add(&account);
        group.add(&site);
        group.add(&note);
        // A masked account's server makes the address for a site; a plus
        // address takes only the note.
        let fit = {
            let (weak, group, site) = (Rc::downgrade(self), group.clone(), site.clone());
            move |selected: u32| {
                let Some(this) = weak.upgrade() else { return };
                let masked = this.makes_masked(selected);
                group.set_description(Some(&if masked {
                    gettext(
                        "Your provider makes a new address that delivers to you. Give it \
                         to one site only.",
                    )
                } else {
                    gettext(
                        "You get a new address that ends in your own, such as \
                         name+kite.fern482@gmail.com. Give it to one site only.",
                    )
                }));
                site.set_visible(masked);
            }
        };
        fit(account.selected());
        account.connect_selected_notify(move |row| fit(row.selected()));
        let page = adw::PreferencesPage::new();
        page.add(&group);
        let create = gtk::Button::builder()
            .label(gettext("Create"))
            .css_classes(["suggested-action"])
            .build();
        // The back button is the way out, as the mockup draws it.
        let header = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .build();
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
        if site.is_visible() {
            site.grab_focus();
        } else {
            note.grab_focus();
        }
        let next = note.clone();
        site.connect_entry_activated(move |_| {
            next.grab_focus();
        });

        let (weak, entry, site_entry) = (Rc::downgrade(self), note.clone(), site.clone());
        let run = move |button: &gtk::Button| {
            let Some(this) = weak.upgrade() else { return };
            let selected = account.selected();
            let Some(chosen) = this.accounts.get(selected as usize).cloned() else {
                return;
            };
            let text = entry.text().to_string();
            button.set_sensitive(false);
            let button = button.clone();
            if this.makes_masked(selected) {
                let site = site_entry.text().to_string();
                glib::spawn_future_local(async move {
                    match this
                        .app
                        .create_masked_address(chosen.id, &site, &text)
                        .await
                    {
                        Ok(masked) => {
                            this.reload();
                            this.show_created(
                                &masked.email,
                                &gettext("It is on your clipboard. Mail sent to it arrives in your inbox."),
                            );
                        }
                        Err(err) => {
                            button.set_sensitive(true);
                            this.failed(&gettext("Could not create the address: {reason}"), &err);
                        }
                    }
                });
                return;
            }
            glib::spawn_future_local(async move {
                match this.app.create_hidden_address(chosen.id, &text).await {
                    Ok(Permitted::Done(hidden)) => {
                        this.reload();
                        this.show_created(
                            &hidden.address,
                            &gettext(
                                "It is on your clipboard. Mail sent to it gets the Hide My Email \
                                 label in Gmail.",
                            ),
                        );
                    }
                    Ok(Permitted::NeedsPermission) => {
                        button.set_sensitive(true);
                        this.ask_for_access(&chosen.email);
                    }
                    Err(err) => {
                        button.set_sensitive(true);
                        this.failed(&gettext("Could not create the address: {reason}"), &err);
                    }
                }
            });
        };
        let press = run.clone();
        let button = create.clone();
        note.connect_entry_activated(move |_| press(&button));
        create.connect_clicked(run);
    }

    /// Whether the account at `selected` in the form's list makes masked
    /// addresses.
    fn makes_masked(&self, selected: u32) -> bool {
        self.accounts
            .get(selected as usize)
            .is_some_and(|chosen| self.masked.iter().any(|a| a.id == chosen.id))
    }

    /// The new address, copied to the clipboard, in place of the form, with
    /// `said` under it saying where its mail goes.
    fn show_created(self: &Rc<Self>, address: &str, said: &str) {
        copy(&self.dialog, address);
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
        let shown = gtk::Label::builder()
            .label(breakable(address))
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .justify(gtk::Justification::Center)
            .css_classes(["title-4", "monospace"])
            .build();
        crate::ui::name(&shown, address);
        content.append(&shown);
        let copy_button = gtk::Button::builder()
            .label(gettext("Copy"))
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .build();
        let (address, weak) = (address.to_string(), Rc::downgrade(self));
        let copied = crate::ui::copied::Copied::new(&copy_button);
        copy_button.connect_clicked(move |b| {
            copy(b, &address);
            copied.show();
            if let Some(this) = weak.upgrade() {
                this.toast(&gettext("Address copied"));
            }
        });
        content.append(&copy_button);
        content.append(
            &gtk::Label::builder()
                .label(said)
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

#[cfg(test)]
mod tests {
    use mailrs_domain::{Account, AccountState, Provider};
    use mailrs_sync::Offers;

    use mailrs_sync::masked::{MaskedAddress, MaskedState};

    use super::{breakable, masked_details, with_rules};

    fn account(id: i64, email: &str) -> Account {
        Account {
            id,
            email: email.into(),
            state: AccountState::Ok,
            provider: Provider::Gmail,
            provider_name: None,
        }
    }

    #[test]
    fn new_addresses_go_only_to_accounts_with_rules() {
        let offers = |id| match id {
            2 => Offers {
                rules: false,
                ..Offers::EVERYTHING
            },
            _ => Offers::EVERYTHING,
        };
        let accounts = vec![account(1, "a@gmail.com"), account(2, "b@example.com")];
        let kept: Vec<String> = with_rules(accounts, offers)
            .into_iter()
            .map(|a| a.email)
            .collect();
        assert_eq!(kept, ["a@gmail.com"]);
    }

    #[test]
    fn an_account_with_masked_email_alone_takes_new_addresses() {
        let offers = |id| match id {
            2 => Offers {
                rules: false,
                labels: false,
                masked_email: true,
                ..Offers::EVERYTHING
            },
            _ => Offers {
                rules: false,
                ..Offers::EVERYTHING
            },
        };
        let accounts = vec![
            account(1, "a@example.com"),
            account(2, "dana@fastmail.example"),
        ];
        let kept: Vec<String> = with_rules(accounts, offers)
            .into_iter()
            .map(|a| a.email)
            .collect();
        assert_eq!(kept, ["dana@fastmail.example"]);
    }

    fn masked(state: MaskedState, note: &str) -> MaskedAddress {
        MaskedAddress {
            id: "ME1".into(),
            email: "moss.heron@fastmail.example".into(),
            state,
            site: "railhop.example".into(),
            note: note.into(),
            created: None,
            last_mail: None,
        }
    }

    #[test]
    fn a_masked_row_names_the_site_then_the_note() {
        assert_eq!(
            masked_details(&masked(MaskedState::Enabled, "Rail pass")),
            "railhop.example · Rail pass"
        );
    }

    #[test]
    fn a_masked_row_skips_an_empty_note() {
        assert_eq!(
            masked_details(&masked(MaskedState::Enabled, "")),
            "railhop.example"
        );
    }

    #[test]
    fn a_disabled_masked_row_says_its_mail_is_dropped() {
        assert!(
            masked_details(&masked(MaskedState::Disabled, ""))
                .ends_with("Off: mail to it is dropped")
        );
    }

    #[test]
    fn a_pending_masked_row_says_it_waits_for_mail() {
        assert!(
            masked_details(&masked(MaskedState::Pending, ""))
                .ends_with("Waiting for its first message")
        );
    }

    #[test]
    fn a_long_address_may_break_only_before_its_domain() {
        assert_eq!(
            breakable("tern.quill@fastmail.example"),
            "tern.quill\u{200b}@fastmail.example"
        );
    }
}
