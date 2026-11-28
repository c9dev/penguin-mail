//! The person pane: a person, a correspondent, a pair to merge or link.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_domain::{Account, AccountId};
use mailrs_store::address_book::{self, Contact, Link};

use super::people::Person;
use super::{Contacts, List, Pane, RowKind, UndoRun, ops, parts};
use crate::format::account_label;
use crate::settings::Change;

fn short(account: AccountId) -> String {
    account_label(account).short
}

fn date_of(ts: i64) -> String {
    let today = chrono::Local::now().date_naive();
    crate::format::local(ts)
        .map(|d| crate::format::short_date(d.date_naive(), today))
        .unwrap_or_default()
}

/// A column that scrolls under a fixed header.
fn scrolled(body: &gtk::Box) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(body)
        .build()
}

pub(super) fn body_box() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["cts-pbody"])
        .build()
}

impl Contacts {
    /// A photo avatar for `contact`, with initials until the file reads.
    pub(super) fn photo_avatar(
        &self,
        name: &str,
        size: i32,
        contact: Option<&Contact>,
    ) -> gtk::Widget {
        let holder = gtk::Box::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        holder.append(&parts::avatar(name, size, None));
        if let (Some(app), Some(file), Some(contact)) = (
            self.app.upgrade(),
            contact.and_then(|c| c.photo_file.clone()),
            contact,
        ) {
            // A photo opens large, from a click or the keyboard.
            let button = gtk::Button::builder()
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .css_classes(["cts-go", "cts-photo"])
                .build();
            let (shown, app_weak) = (contact.clone(), Rc::downgrade(&app));
            button.connect_clicked(move |b| {
                if let Some(app) = app_weak.upgrade() {
                    super::photo::view(b.upcast_ref(), &shown, &app);
                }
            });
            button.set_cursor_from_name(Some("pointer"));
            super::super::name(&button, &gettext("View Photo"));
            button.set_child(Some(&holder));
            let path = app.core.contacts().photo_dir().join(file);
            let (weak, name) = (holder.downgrade(), name.to_string());
            glib::spawn_future_local(async move {
                if let Some(texture) = super::photo::read(&path).await
                    && let Some(holder) = weak.upgrade()
                {
                    while let Some(child) = holder.first_child() {
                        holder.remove(&child);
                    }
                    holder.append(&parts::avatar(&name, size, Some(&texture)));
                }
            });
            return button.upcast();
        }
        holder.upcast()
    }

    /// Draws the person pane for what is selected. A redraw of the same
    /// pane, after a switch or a button in it, keeps the keyboard there.
    pub(super) fn show_pane(self: &Rc<Self>) {
        let holder = self.pane.upcast_ref::<gtk::Widget>();
        let at = parts::focus_path(holder);
        self.show_pane_now();
        parts::refocus(holder, at);
    }

    fn show_pane_now(self: &Rc<Self>) {
        while let Some(child) = self.pane.first_child() {
            self.pane.remove(&child);
        }
        *self.new_message.borrow_mut() = None;
        // A new pane fades in. The two classes name the same fade, so
        // swapping them starts it again. GTK runs no animation when the
        // user has turned animations off.
        let (on, off) = if self.pane.has_css_class("cts-fade-a") {
            ("cts-fade-b", "cts-fade-a")
        } else {
            ("cts-fade-a", "cts-fade-b")
        };
        self.pane.remove_css_class(off);
        self.pane.add_css_class(on);
        let (pane, kind, list) = {
            let state = self.state.borrow();
            let kind = state
                .selected
                .as_ref()
                .and_then(|id| state.rows.iter().find(|r| &r.id == id))
                .map(|r| (r.kind, r.id.clone()));
            (
                state.pane.clone().unwrap_or(Pane::View),
                kind,
                state.list.clone().unwrap_or(List::All),
            )
        };
        let list_only = match &pane {
            Pane::View => kind.is_none(),
            _ => false,
        };
        self.nav
            .set_max_sidebar_width(if list_only { 10_000.0 } else { 320.0 });
        self.nav
            .set_sidebar_width_fraction(if list_only { 1.0 } else { 0.3 });
        self.nav.set_min_sidebar_width(320.0);
        self.pane.set_visible(!list_only);
        match pane {
            Pane::Edit(contact) => self.edit_pane(*contact),
            Pane::New => self.new_pane(),
            Pane::View => match kind {
                None => {}
                Some((RowKind::Person(i), id)) => self.person_pane(i, &id, &list),
                Some((RowKind::Merge(i), id)) => self.merge_pane(i, &id),
                Some((RowKind::Pair(i), id)) => self.link_pane(i, &id),
                Some((RowKind::Mail(i), _)) => self.from_pane(i),
            },
        }
    }

    /// The ident header every person view starts with: the back button,
    /// an avatar, name and address, chips, and the actions below.
    fn ident(
        self: &Rc<Self>,
        who: &gtk::Widget,
        name: &str,
        sub: &str,
        chips: Option<gtk::Box>,
        actions: &gtk::Box,
    ) -> gtk::Box {
        let header = gtk::Box::builder()
            .css_classes(["cts-ident"])
            .spacing(4)
            .build();
        let back = parts::icon_button("back", &gettext("Back"), &["back"]);
        back.set_valign(gtk::Align::Start);
        self.nav
            .bind_property("collapsed", &back, "visible")
            .sync_create()
            .build();
        let weak = Rc::downgrade(self);
        back.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.nav.set_show_content(false);
            }
        });
        header.append(&back);
        let copy = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .build();
        let who_row = gtk::Box::builder()
            .spacing(16)
            .css_classes(["cts-who"])
            .build();
        who_row.append(who);
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        let h2 = parts::label(name, &["cts-who-name"]);
        h2.set_selectable(true);
        text.append(&h2);
        if !sub.is_empty() {
            text.append(&parts::label(sub, &["cts-who-sub"]));
        }
        if let Some(chips) = chips {
            chips.add_css_class("cts-who-chips");
            text.append(&chips);
        }
        who_row.append(&text);
        copy.append(&who_row);
        copy.append(actions);
        header.append(&copy);
        header
    }

    fn actions_box() -> gtk::Box {
        gtk::Box::builder()
            .spacing(8)
            .css_classes(["cts-actions"])
            .build()
    }

    fn person_pane(self: &Rc<Self>, index: usize, id: &str, list: &List) {
        let (person, contacts, links, images) = {
            let state = self.state.borrow();
            (
                state.people[index].clone(),
                state.data.contacts.clone(),
                state.data.links.clone(),
                state.data.images.clone(),
            )
        };
        let shown = &contacts[person.shown];
        let emails = person.emails(&contacts);
        let linked = person_link(&person, &contacts, &links);
        let vip = self.is_vip(&person);
        let read_only = person
            .members
            .iter()
            .all(|&i| contacts[i].details.read_only);

        // Actions
        let actions = Self::actions_box();
        let weak = Rc::downgrade(self);
        let vip_button = parts::button(
            &if vip {
                gettext("Remove from VIPs")
            } else {
                gettext("Add to VIPs")
            },
            &[],
        );
        vip_button.set_tooltip_text(Some(&fill(
            &if vip {
                gettext("{name} is in VIPs. Remove them from that list.")
            } else {
                gettext("Add {name} to VIPs")
            },
            &[("name", &person.name)],
        )));
        {
            let (weak, email, name) = (weak.clone(), person.email.clone(), person.name.clone());
            let all_emails = emails.clone();
            vip_button.connect_clicked(move |_| {
                let Some(this) = weak.upgrade() else { return };
                if let Some(app) = this.app.upgrade() {
                    let settings = app.settings();
                    // Remove every address of the person; add the first.
                    if all_emails.iter().any(|e| settings.is_vip(e)) {
                        for e in all_emails.iter().filter(|e| settings.is_vip(e)) {
                            app.change_settings(Change::ToggleVip {
                                email: e.clone(),
                                name: name.clone(),
                            });
                        }
                    } else {
                        app.change_settings(Change::ToggleVip {
                            email: email.clone(),
                            name: name.clone(),
                        });
                    }
                }
                this.rebuild();
            });
        }
        actions.append(&vip_button);
        let their = parts::button(&gettext("All Their Mail"), &[]);
        their.set_tooltip_text(Some(&gettext("Every conversation with this person")));
        {
            let (weak, mails) = (weak.clone(), emails.clone());
            their.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    (this.hooks.search_mail)(mails.clone());
                }
            });
        }
        actions.append(&their);
        // The record Edit opens: the one whose name the row shows, or the
        // first of an account the person picks in the editor.
        let editable: Vec<Contact> = editable_members(&person, &contacts, &links)
            .into_iter()
            .map(|i| contacts[i].clone())
            .collect();
        if !read_only && let Some(first) = editable.first().cloned() {
            let edit = parts::button(&gettext("Edit"), &[]);
            let weak = weak.clone();
            edit.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.state.borrow_mut().pane = Some(Pane::Edit(Box::new(first.clone())));
                    this.show_pane();
                }
            });
            actions.append(&edit);
        }
        let write = parts::button(&gettext("New Message"), &["yes"]);
        write.set_tooltip_text(Some(&gettext("New Message (C)")));
        write.set_child(Some(
            &adw::ButtonContent::builder()
                .icon_name("penguin-mail-ct-mail-symbolic")
                .label(gettext("New Message"))
                .build(),
        ));
        super::super::name(&write, &gettext("New Message"));
        {
            let (weak, to, account) = (weak.clone(), person.email.clone(), shown.account_id);
            write.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade()
                    && let Some(app) = this.app.upgrade()
                {
                    app.new_message(Some(account), &to);
                }
            });
        }
        actions.append(&write);
        *self.new_message.borrow_mut() = Some(write);

        let chips = (!person.accounts.is_empty()).then(|| parts::chips(&person.accounts, 6));
        let who = self.photo_avatar(&person.name, 72, Some(shown));
        let header = self.ident(&who, &person.name, &person.email, chips, &actions);
        self.pane.append(&header);

        // Body
        let body = body_box();
        if let Some(words) = person_hint(&person, &contacts, list, linked.is_some()) {
            body.append(&parts::hint(&words));
        }
        let by_account = linked.is_some();
        body.append(&parts::sec(&gettext("Reach")));
        let mut lines: Vec<gtk::Widget> = Vec::new();
        let mut orgs: Vec<String> = Vec::new();
        for &i in &person.members {
            if let Some(o) = contacts[i]
                .organization
                .as_deref()
                .filter(|o| !o.is_empty())
                && !orgs.iter().any(|x| x == o)
            {
                orgs.push(o.to_string());
            }
        }
        for org in orgs {
            lines
                .push(parts::line(Some("building"), &org, &gettext("Organization"), None).upcast());
        }
        let mut first_email = true;
        for &i in &person.members {
            for (n, email) in contacts[i].emails.iter().enumerate() {
                if lines.iter().any(|_| false) {
                    continue;
                }
                let sub = if by_account {
                    short(contacts[i].account_id)
                } else if first_email && n == 0 {
                    gettext("Email")
                } else {
                    gettext("Other email")
                };
                // An address two accounts hold shows once.
                if emails_seen(&lines, email) {
                    continue;
                }
                lines.push(email_line(email, &sub));
                first_email = false;
            }
        }
        let mut phones: Vec<String> = Vec::new();
        for &i in &person.members {
            let c = &contacts[i];
            let list = if c.details.phones.is_empty() {
                c.phone.iter().cloned().collect()
            } else {
                c.details.phones.clone()
            };
            for p in list {
                if !phones.contains(&p) {
                    phones.push(p);
                }
            }
        }
        for phone in phones {
            lines.push(parts::line(Some("phone"), &phone, &gettext("Phone"), None).upcast());
        }
        let mut sites: Vec<String> = Vec::new();
        let mut places: Vec<String> = Vec::new();
        for &i in &person.members {
            for u in &contacts[i].details.urls {
                if !sites.contains(u) {
                    sites.push(u.clone());
                }
            }
            for a in &contacts[i].details.addresses {
                let text = a
                    .iter()
                    .filter(|p| !p.is_empty())
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() && !places.contains(&text) {
                    places.push(text);
                }
            }
        }
        for site in sites {
            lines.push(website_line(&site));
        }
        for place in places {
            lines.push(parts::line(Some("pin"), &place, &gettext("Address"), None).upcast());
        }
        if !lines.is_empty() {
            body.append(&parts::group(&lines));
        }
        // What the card holds besides ways to reach them.
        let mut details: Vec<gtk::Widget> = Vec::new();
        for &i in &person.members {
            let d = &contacts[i].details;
            if !d.birthday.is_empty() {
                let words = mailrs_domain::contact::birthday_label(
                    &d.birthday,
                    chrono::Local::now().date_naive(),
                );
                details
                    .push(parts::line(Some("cake"), &words, &gettext("Birthday"), None).upcast());
            }
            for f in &d.dates {
                let shown = mailrs_domain::contact::date_label(&f.value);
                let line = parts::line(Some("star"), &shown, &humane(&f.label), None);
                line.set_tooltip_text(Some(&f.label));
                details.push(line.upcast());
            }
            for f in &d.custom {
                let line = parts::line(Some("note"), &f.value, &humane(&f.label), None);
                line.set_tooltip_text(Some(&f.label));
                details.push(line.upcast());
            }
        }
        let mut groups: Vec<String> = Vec::new();
        for &i in &person.members {
            for g in &contacts[i].details.groups {
                if !groups.contains(g) {
                    groups.push(g.clone());
                }
            }
        }
        if !groups.is_empty() {
            details.push(
                parts::line(Some("tag"), &groups.join(", "), &gettext("Groups"), None).upcast(),
            );
        }
        if !details.is_empty() {
            body.append(&parts::sec(&gettext("Details")));
            body.append(&parts::group(&details));
        }
        let notes: Vec<&str> = person
            .members
            .iter()
            .map(|&i| contacts[i].details.notes.as_str())
            .filter(|n| !n.is_empty())
            .collect();
        if !notes.is_empty() {
            body.append(&parts::sec(&gettext("Notes")));
            body.append(&parts::group(&[parts::line(
                Some("note"),
                &notes.join("\n\n"),
                "",
                None,
            )
            .upcast()]));
        }

        // Mail
        let mail = self.state.borrow().mail.get(id).cloned();
        match mail {
            None => self.load_mail(id.to_string(), emails.clone()),
            Some(found) => {
                let mut rows: Vec<gtk::Widget> = Vec::new();
                if let Some((account, thread, subject, date)) = found.last.clone() {
                    let sub = if found.last_mine {
                        fill(
                            &gettext("You wrote {date}. Waiting on a reply."),
                            &[("date", &date_of(date))],
                        )
                    } else {
                        fill(
                            &gettext("Last message, {date}"),
                            &[("date", &date_of(date))],
                        )
                    };
                    let weak = Rc::downgrade(self);
                    rows.push(
                        parts::go_line("mail", &subject, &sub, move || {
                            if let Some(this) = weak.upgrade() {
                                (this.hooks.open_thread)(account, thread.clone());
                            }
                        })
                        .upcast(),
                    );
                }
                if found.conversations > 1 {
                    let weak = Rc::downgrade(self);
                    let mails = emails.clone();
                    let sub = match found.last_wrote {
                        Some(at) => {
                            fill(&gettext("You last wrote {date}"), &[("date", &date_of(at))])
                        }
                        None => gettext("You have not written to them."),
                    };
                    rows.push(
                        parts::go_line(
                            "chat",
                            &fill_plural(
                                "{count} conversation",
                                "{count} conversations",
                                found.conversations as usize,
                                &[("count", &found.conversations.to_string())],
                            ),
                            &sub,
                            move || {
                                if let Some(this) = weak.upgrade() {
                                    (this.hooks.search_mail)(mails.clone());
                                }
                            },
                        )
                        .upcast(),
                    );
                }
                if !rows.is_empty() {
                    body.append(&parts::sec(&gettext("Mail")));
                    body.append(&parts::group(&rows));
                }
            }
        }
        // On this computer
        body.append(&parts::sec(&gettext("On this computer")));
        body.append(&self.local_group(id, &emails, &images));
        self.pane.append(&scrolled(&body));
    }

    fn load_mail(self: &Rc<Self>, id: String, emails: Vec<String>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let found = app
                .core
                .read(move |c| address_book::mail_with(c, &emails))
                .await
                .unwrap_or_default();
            let Some(this) = weak.upgrade() else { return };
            let current = this.state.borrow().selected.clone();
            this.state.borrow_mut().mail.insert(id.clone(), found);
            if current.as_deref() == Some(&id)
                && matches!(this.state.borrow().pane, Some(Pane::View) | None)
            {
                this.show_pane();
            }
        });
    }

    /// The OpenPGP key line and the remote images switch.
    fn local_group(
        self: &Rc<Self>,
        id: &str,
        emails: &[String],
        images: &[mailrs_store::image_senders::ImageSender],
    ) -> gtk::Box {
        let known = self.state.borrow().keys.get(id).copied();
        if known.is_none() {
            self.load_key(id.to_string(), emails.to_vec());
        }
        let about = if emails.len() == 1 {
            emails[0].clone()
        } else {
            String::new()
        };
        let sub = match (known.unwrap_or(false), emails.len() == 1) {
            (true, true) => fill(
                &gettext("A key for {email} is on this computer."),
                &[("email", &about)],
            ),
            (true, false) => gettext("A key for these addresses is on this computer."),
            (false, true) => fill(
                &gettext("No key for {email} is on this computer."),
                &[("email", &about)],
            ),
            (false, false) => gettext("No key for these addresses is on this computer."),
        };
        let key = parts::line(Some("key"), &gettext("OpenPGP key"), &sub, None);
        let allowed = !emails.is_empty()
            && emails
                .iter()
                .all(|e| crate::images::allowed(images, Some(e)));
        let images_line = parts::switch_line(
            "image",
            &gettext("Remote images"),
            &if allowed {
                gettext("Allowed for this sender.")
            } else {
                gettext("Blocked until you allow them.")
            },
            &if allowed {
                gettext("Remote images, allowed for this sender")
            } else {
                gettext("Remote images, blocked until you allow them")
            },
            allowed,
            {
                let (weak, emails) = (Rc::downgrade(self), emails.to_vec());
                move |on| {
                    if let Some(this) = weak.upgrade() {
                        this.set_images(&emails, on);
                    }
                }
            },
        );
        parts::group(&[key.upcast(), images_line.upcast()])
    }

    fn load_key(self: &Rc<Self>, id: String, emails: Vec<String>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let wanted = emails.clone();
            let held = app.core.gpg(move |pgp| pgp.keys_for(&wanted)).await;
            let has =
                held.is_ok_and(|keys| !keys.is_empty() && keys.iter().all(|k| k.key.is_some()));
            let Some(this) = weak.upgrade() else { return };
            this.state.borrow_mut().keys.insert(id.clone(), has);
            if this.state.borrow().selected.as_deref() == Some(&id)
                && matches!(this.state.borrow().pane, Some(Pane::View) | None)
            {
                this.show_pane();
            }
        });
    }

    fn set_images(self: &Rc<Self>, emails: &[String], on: bool) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let emails = emails.to_vec();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let now = chrono::Utc::now().timestamp_millis();
            let written = app
                .core
                .write(move |c| {
                    for e in &emails {
                        if on {
                            mailrs_store::image_senders::allow(c, e, false, now)?;
                        } else {
                            mailrs_store::image_senders::forget(c, e)?;
                        }
                    }
                    mailrs_store::image_senders::list(c)
                })
                .await;
            let Some(this) = weak.upgrade() else { return };
            match written {
                Ok(list) => {
                    this.state.borrow_mut().data.images = list;
                    app.tell_window(crate::ui::window::Notice::ImageSendersChanged);
                    this.show_pane();
                }
                Err(err) => this.say_error(&err),
            }
        });
    }

    // Delete

    /// Deletes `contact` from its account and offers Undo.
    pub(super) fn delete_contact(self: &Rc<Self>, contact: Contact, still: Option<String>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        let words = {
            let base = fill(
                &gettext("Deleted {name} from {account}."),
                &[
                    ("name", contact.display()),
                    ("account", &short(contact.account_id)),
                ],
            );
            match still {
                Some(other) => fill(
                    &gettext("{base} {other} still has this person."),
                    &[("base", &base), ("other", &other)],
                ),
                None => base,
            }
        };
        let shown = self.state.borrow().selected.clone();
        let links = self.state.borrow().data.links.clone();
        glib::spawn_future_local(async move {
            let gone = ops::gone(&app, contact.clone(), &links);
            let result = ops::delete(&app, contact).await;
            let Some(this) = weak.upgrade() else { return };
            match result {
                Ok(()) => {
                    app.contacts_added();
                    // The person may have opened someone else meanwhile.
                    let still_here = this.state.borrow().selected == shown;
                    if still_here {
                        let mut state = this.state.borrow_mut();
                        state.pane = Some(Pane::View);
                        state.selected = None;
                        drop(state);
                        this.nav.set_show_content(false);
                    }
                    this.reload();
                    this.tell(
                        &words,
                        Some(Box::new(move |this| this.restore_all(vec![gone]))),
                    );
                }
                Err(err) => this.say_error(&err),
            }
        });
    }

    // Merge

    fn merge_pane(self: &Rc<Self>, index: usize, id: &str) {
        let (merge, contacts) = {
            let state = self.state.borrow();
            (state.merges[index].clone(), state.data.contacts.clone())
        };
        let flipped = self.state.borrow().keep.get(id).copied();
        let (keep_i, drop_i) = match flipped {
            Some(k) if k == merge.drop => (merge.drop, merge.keep),
            _ => (merge.keep, merge.drop),
        };
        let (keep, drop) = (contacts[keep_i].clone(), contacts[drop_i].clone());
        let account = short(merge.account);
        let phead = gtk::Box::builder()
            .css_classes(["cts-phead"])
            .spacing(8)
            .build();
        phead.append(&parts::label(
            &fill(&gettext("Merge in {account}"), &[("account", &account)]),
            &["cts-h2"],
        ));
        phead.append(&gtk::Box::builder().hexpand(true).build());
        let weak = Rc::downgrade(self);
        let cancel = parts::button(&gettext("Cancel"), &[]);
        {
            let weak = weak.clone();
            cancel.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.show_list(List::All);
                }
            });
        }
        let delete = parts::button(
            &fill(
                &gettext("Delete {name} from {account}"),
                &[("name", drop.display()), ("account", &account)],
            ),
            &["dangerfill"],
        );
        {
            let (weak, keep, drop) = (weak.clone(), keep.clone(), drop.clone());
            delete.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.do_merge(keep.clone(), drop.clone());
                }
            });
        }
        phead.append(&cancel);
        phead.append(&delete);
        self.pane.append(&phead);
        let body = body_box();
        let decide = gtk::Box::builder()
            .spacing(24)
            .css_classes(["cts-decide"])
            .homogeneous(false)
            .build();
        let left = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .build();
        let shared = keep
            .emails
            .iter()
            .find(|e| drop.emails.iter().any(|d| d.eq_ignore_ascii_case(e)))
            .cloned()
            .unwrap_or_default();
        left.append(&parts::hint(&fill(
            &gettext("Both contacts use {email}. {account} keeps one contact and deletes the other. Other accounts are not changed."),
            &[("email", &shared), ("account", &account)],
        )));
        left.append(&parts::sec(&gettext("Which contact")));
        let mut head: Option<gtk::CheckButton> = None;
        let mut rows: Vec<gtk::Widget> = Vec::new();
        for (c, index) in [
            (&contacts[merge.keep], merge.keep),
            (&contacts[merge.drop], merge.drop),
        ] {
            let (row, check) =
                parts::choice(head.as_ref(), c.display(), &sub_of(c), index == keep_i);
            head.get_or_insert(check.clone());
            let (weak, id) = (weak.clone(), id.to_string());
            check.connect_toggled(move |c| {
                if c.is_active()
                    && let Some(this) = weak.upgrade()
                {
                    this.state.borrow_mut().keep.insert(id.clone(), index);
                    this.show_pane();
                }
            });
            rows.push(row.upcast());
        }
        left.append(&parts::group(&rows));
        decide.append(&left);
        let right = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .build();
        right.append(&parts::sec(&fill(
            &gettext("{account} keeps"),
            &[("account", &account)],
        )));
        let who = gtk::Box::builder()
            .spacing(16)
            .css_classes(["cts-who", "cts-keepwho"])
            .build();
        who.append(&self.photo_avatar(keep.display(), 56, Some(&keep)));
        who.append(&parts::label(keep.display(), &["cts-keep-name"]));
        let mut card: Vec<gtk::Widget> = vec![who.upcast()];
        for e in &keep.emails {
            card.push(parts::line(Some("mail"), e, &gettext("Email"), None).upcast());
        }
        if let Some(o) = keep.organization.as_deref().filter(|o| !o.is_empty()) {
            card.push(parts::line(Some("building"), o, &gettext("Organization"), None).upcast());
        }
        if let Some(p) = phone_of(&keep) {
            card.push(parts::line(Some("phone"), &p, &gettext("Phone"), None).upcast());
        }
        right.append(&parts::group(&card));
        decide.append(&right);
        body.append(&decide);
        self.pane.append(&scrolled(&body));
    }

    fn do_merge(self: &Rc<Self>, keep: Contact, drop: Contact) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let words = fill(
            &gettext("Deleted {name} from {account}. {kept} is the contact {account} keeps."),
            &[
                ("name", drop.display()),
                ("account", &short(drop.account_id)),
                ("kept", keep.display()),
            ],
        );
        let weak = Rc::downgrade(self);
        let shown = self.state.borrow().selected.clone();
        let links = self.state.borrow().data.links.clone();
        glib::spawn_future_local(async move {
            let gone = ops::gone(&app, drop.clone(), &links);
            let result = ops::delete(&app, drop.clone()).await;
            let Some(this) = weak.upgrade() else { return };
            match result {
                Ok(()) => {
                    app.contacts_added();
                    let mut state = this.state.borrow_mut();
                    // The person may have opened someone else meanwhile.
                    if state.selected == shown {
                        state.selected = None;
                        state.list = Some(List::Account(drop.account_id));
                    }
                    std::mem::drop(state);
                    this.reload();
                    this.tell(
                        &words,
                        Some(Box::new(move |this| this.restore_all(vec![gone]))),
                    );
                }
                Err(err) => this.say_error(&err),
            }
        });
    }

    // Link

    fn link_pane(self: &Rc<Self>, index: usize, id: &str) {
        let (pair, contacts) = {
            let state = self.state.borrow();
            (state.pairs[index].clone(), state.data.contacts.clone())
        };
        let (a, b) = (contacts[pair.a].clone(), contacts[pair.b].clone());
        let shown_a = self.state.borrow().shown_a.get(id).copied().unwrap_or(true);
        let shown = if shown_a { &a } else { &b };
        let weak = Rc::downgrade(self);
        let phead = gtk::Box::builder()
            .css_classes(["cts-phead"])
            .spacing(8)
            .build();
        phead.append(&parts::label(&gettext("Same person"), &["cts-h2"]));
        phead.append(&gtk::Box::builder().hexpand(true).build());
        let cancel = parts::button(&gettext("Cancel"), &[]);
        {
            let weak = weak.clone();
            cancel.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.show_list(List::All);
                }
            });
        }
        let link = parts::button(&gettext("Link"), &["yes"]);
        {
            let (weak, a, b) = (weak.clone(), a.clone(), b.clone());
            link.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.do_link(a.clone(), b.clone(), shown_a);
                }
            });
        }
        phead.append(&cancel);
        phead.append(&link);
        self.pane.append(&phead);
        let body = body_box();
        let decide = gtk::Box::builder()
            .spacing(24)
            .css_classes(["cts-decide"])
            .build();
        let left = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .build();
        left.append(&parts::hint(&fill(
            &gettext("{a} is in {account_a}. {b} is in {account_b}. The names differ, and the addresses differ. Linking keeps both contacts. All contacts shows one row."),
            &[
                ("a", a.display()),
                ("account_a", &short(a.account_id)),
                ("b", b.display()),
                ("account_b", &short(b.account_id)),
            ],
        )));
        left.append(&parts::sec(&gettext("The two contacts")));
        let two: Vec<gtk::Widget> = [&a, &b]
            .iter()
            .map(|c| {
                let row = parts::line(
                    None,
                    c.display(),
                    &format!("{}, {}", short(c.account_id), c.email().unwrap_or_default()),
                    None,
                );
                row.prepend(&parts::avatar(c.display(), 36, None));
                row.upcast()
            })
            .collect();
        left.append(&parts::group(&two));
        left.append(&parts::sec(&gettext("Name in All contacts")));
        let mut head: Option<gtk::CheckButton> = None;
        let mut rows: Vec<gtk::Widget> = Vec::new();
        for (c, is_a) in [(&a, true), (&b, false)] {
            let sub = format!("{}, {}", short(c.account_id), c.email().unwrap_or_default());
            let (row, check) = parts::choice(head.as_ref(), c.display(), &sub, is_a == shown_a);
            head.get_or_insert(check.clone());
            let (weak, id) = (weak.clone(), id.to_string());
            check.connect_toggled(move |c| {
                if c.is_active()
                    && let Some(this) = weak.upgrade()
                {
                    this.state.borrow_mut().shown_a.insert(id.clone(), is_a);
                    this.show_pane();
                }
            });
            rows.push(row.upcast());
        }
        left.append(&parts::group(&rows));
        decide.append(&left);
        let right = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .build();
        right.append(&parts::sec(&gettext("All contacts will show")));
        let who = gtk::Box::builder()
            .spacing(16)
            .css_classes(["cts-who", "cts-keepwho"])
            .build();
        who.append(&self.photo_avatar(shown.display(), 56, Some(shown)));
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .build();
        text.append(&parts::label(shown.display(), &["cts-keep-name"]));
        text.append(&parts::label(
            &gettext("One row, two accounts"),
            &["cts-who-sub"],
        ));
        let chips = parts::chips(&[a.account_id, b.account_id], 6);
        chips.add_css_class("cts-who-chips");
        text.append(&chips);
        who.append(&text);
        let mut card: Vec<gtk::Widget> = vec![who.upcast()];
        for c in [&a, &b] {
            for e in &c.emails {
                card.push(parts::line(Some("mail"), e, &short(c.account_id), None).upcast());
            }
        }
        right.append(&parts::group(&card));
        decide.append(&right);
        body.append(&decide);
        self.pane.append(&scrolled(&body));
    }

    fn do_link(self: &Rc<Self>, a: Contact, b: Contact, shown_a: bool) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let ends = (
            (a.account_id, a.resource.clone()),
            (b.account_id, b.resource.clone()),
        );
        let link = Link {
            a: ends.0.clone(),
            b: ends.1.clone(),
            shown_a,
        };
        let name = if shown_a { a.display() } else { b.display() }.to_string();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = app.core.write(move |c| address_book::link(c, &link)).await;
            let Some(this) = weak.upgrade() else { return };
            match result {
                Ok(()) => {
                    let (x, y) = ends;
                    this.state.borrow_mut().selected = None;
                    this.state.borrow_mut().list = Some(List::All);
                    this.reload();
                    let undo: UndoRun = Box::new(move |this| {
                        let Some(app) = this.app.upgrade() else {
                            return;
                        };
                        let (x, y) = (x, y);
                        let weak = Rc::downgrade(this);
                        glib::spawn_future_local(async move {
                            let _ = app
                                .core
                                .write(move |c| address_book::unlink(c, &x, &y))
                                .await;
                            if let Some(this) = weak.upgrade() {
                                this.state.borrow_mut().list = Some(List::SamePerson);
                                this.reload();
                            }
                        });
                    });
                    this.tell(
                        &fill(
                            &gettext(
                                "All contacts now shows one row for {name}. Nothing was deleted.",
                            ),
                            &[("name", &name)],
                        ),
                        Some(undo),
                    );
                }
                Err(err) => this.say_error(&err),
            }
        });
    }

    // From mail

    fn from_pane(self: &Rc<Self>, index: usize) {
        let (c, seen) = {
            let state = self.state.borrow();
            let c = state.data.correspondents[index].clone();
            let seen = state
                .data
                .seen
                .get(&c.email.to_lowercase())
                .copied()
                .unwrap_or(1);
            (c, seen)
        };
        let targets = self.accounts_with_contacts();
        let name = c
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| c.email.clone());
        let actions = Self::actions_box();
        let their = parts::button(&gettext("All Their Mail"), &[]);
        their.set_tooltip_text(Some(&gettext("Every conversation with this person")));
        let weak = Rc::downgrade(self);
        {
            let (weak, email) = (weak.clone(), c.email.clone());
            their.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    (this.hooks.search_mail)(vec![email.clone()]);
                }
            });
        }
        actions.append(&their);
        let chosen = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let save = parts::button(&gettext("Save to Contacts"), &["yes"]);
        save.set_sensitive(!targets.is_empty());
        {
            let (weak, name, email, targets, chosen) = (
                weak.clone(),
                name.clone(),
                c.email.clone(),
                targets.clone(),
                chosen.clone(),
            );
            save.connect_clicked(move |_| {
                if let (Some(this), Some(account)) = (weak.upgrade(), targets.get(chosen.get())) {
                    this.save_correspondent(account.clone(), name.clone(), email.clone());
                }
            });
        }
        actions.append(&save);
        let who = parts::avatar(&name, 72, None);
        let header = self.ident(&who, &name, &gettext("Not a contact"), None, &actions);
        self.pane.append(&header);
        let body = body_box();
        body.append(&parts::hint(&fill_plural(
            "From mail lists people the mailbox has seen who are not saved as contacts. Seen in {count} message. Saving writes a contact into the account you pick.",
            "From mail lists people the mailbox has seen who are not saved as contacts. Seen in {count} messages. Saving writes a contact into the account you pick.",
            seen as usize,
            &[("count", &seen.to_string())],
        )));
        let mail = self
            .state
            .borrow()
            .mail
            .get(&format!("c:{}", c.email.to_lowercase()))
            .cloned();
        match mail {
            None => self.load_mail(
                format!("c:{}", c.email.to_lowercase()),
                vec![c.email.clone()],
            ),
            Some(found) => {
                if let Some((account, thread, subject, date)) = found.last {
                    body.append(&parts::sec(&gettext("Latest message")));
                    let weak = Rc::downgrade(self);
                    body.append(&parts::group(&[parts::go_line(
                        "mail",
                        &subject,
                        &date_of(date),
                        move || {
                            if let Some(this) = weak.upgrade() {
                                (this.hooks.open_thread)(account, thread.clone());
                            }
                        },
                    )
                    .upcast()]));
                }
            }
        }
        body.append(&parts::sec(&gettext("Save to")));
        if targets.is_empty() {
            body.append(&parts::hint(&gettext(
                "Turn contacts on for an account first.",
            )));
        } else {
            let names: Vec<String> = targets.iter().map(|a| short(a.id)).collect();
            body.append(&parts::seg(&gettext("Save to"), &names, 0, {
                let chosen = chosen.clone();
                move |at| chosen.set(at)
            }));
        }
        body.append(&parts::sec(&gettext("Address")));
        body.append(&parts::group(&[parts::line(
            Some("mail"),
            &c.email,
            &gettext("From the messages"),
            None,
        )
        .upcast()]));
        self.pane.append(&scrolled(&body));
    }

    fn save_correspondent(self: &Rc<Self>, account: Account, name: String, email: String) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let fields = mailrs_gmail::ContactFields {
            name: Some(name.clone()),
            emails: Some(vec![email.clone()]),
            ..Default::default()
        };
        let demo = Contact {
            account_id: account.id,
            resource: format!("demo/{}", glib::uuid_string_random()),
            name: Some(name.clone()),
            emails: vec![email.clone()],
            ..Default::default()
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = ops::save(&app, account.id, None, fields, demo).await;
            let Some(this) = weak.upgrade() else { return };
            match result {
                Ok(made) => {
                    app.contacts_added();
                    let words = fill(
                        &gettext("Saved {name} to {account}."),
                        &[("name", &name), ("account", &short(account.id))],
                    );
                    this.reload();
                    // Undo deletes the card the save made, and only that one.
                    let undo: UndoRun = Box::new(move |this| {
                        let Some(app) = this.app.upgrade() else {
                            return;
                        };
                        let weak = Rc::downgrade(this);
                        glib::spawn_future_local(async move {
                            let result = ops::delete(&app, made).await;
                            app.contacts_added();
                            if let Some(this) = weak.upgrade() {
                                if let Err(err) = result {
                                    this.say_error(&err);
                                }
                                this.reload();
                            }
                        });
                    });
                    this.tell(&words, Some(undo));
                }
                Err(err) => this.say_error(&err),
            }
        });
    }
}

fn emails_seen(lines: &[gtk::Widget], email: &str) -> bool {
    lines
        .iter()
        .any(|l| l.widget_name() == email.to_lowercase())
}

fn email_line(email: &str, sub: &str) -> gtk::Widget {
    let line = parts::line(Some("mail"), email, sub, None);
    line.set_widget_name(&email.to_lowercase());
    line.upcast()
}

fn website_line(site: &str) -> gtk::Widget {
    let line = parts::line(Some("globe"), site, &gettext("Website"), None);
    let Some(uri) = super::website_uri(site) else {
        return line.upcast();
    };
    // A button, so the keyboard and screen readers reach the site too.
    let button = gtk::Button::builder()
        .child(&line)
        .css_classes(["cts-go"])
        .build();
    parts::mark_unselectable(line.upcast_ref());
    super::super::name(&button, &format!("{site}, {}", gettext("Website")));
    button.connect_clicked(move |b| {
        gtk::UriLauncher::new(&uri).launch(
            b.root().and_downcast_ref::<gtk::Window>(),
            gtk::gio::Cancellable::NONE,
            |_| {},
        );
    });
    button.set_cursor_from_name(Some("pointer"));
    button.upcast()
}

pub(super) fn phone_of(c: &Contact) -> Option<String> {
    c.details
        .phones
        .first()
        .cloned()
        .or_else(|| c.phone.clone())
}

fn sub_of(c: &Contact) -> String {
    let org = c.organization.as_deref().filter(|o| !o.is_empty());
    let phone = phone_of(c);
    match (org, phone) {
        (None, None) => gettext("No organization, no phone"),
        (org, phone) => [
            org.map(|o| fill(&gettext("Organization {value}"), &[("value", o)])),
            phone.map(|p| fill(&gettext("Phone {value}"), &[("value", &p)])),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n"),
    }
}

/// The link a person's members come from, if one joins two of them.
fn person_link<'a>(person: &Person, contacts: &[Contact], links: &'a [Link]) -> Option<&'a Link> {
    links.iter().find(|l| {
        let has = |k: &(AccountId, String)| {
            person
                .members
                .iter()
                .any(|&i| contacts[i].account_id == k.0 && contacts[i].resource == k.1)
        };
        has(&l.a) && has(&l.b)
    })
}

/// The contacts Edit may change: only the shown one of a linked person,
/// else the first of each account.
pub(super) fn editable_members(
    person: &Person,
    contacts: &[Contact],
    links: &[Link],
) -> Vec<usize> {
    if let Some(link) = person_link(person, contacts, links) {
        let key = if link.shown_a { &link.a } else { &link.b };
        return person
            .members
            .iter()
            .copied()
            .filter(|&i| contacts[i].account_id == key.0 && contacts[i].resource == key.1)
            .collect();
    }
    let mut seen: Vec<AccountId> = Vec::new();
    let mut out = Vec::new();
    for &i in &person.members {
        if !seen.contains(&contacts[i].account_id) {
            seen.push(contacts[i].account_id);
            out.push(i);
        }
    }
    out
}

fn person_hint(person: &Person, contacts: &[Contact], list: &List, linked: bool) -> Option<String> {
    let names = |ids: &[AccountId]| {
        let words: Vec<String> = ids.iter().map(|a| short(*a)).collect();
        let refs: Vec<&str> = words.iter().map(String::as_str).collect();
        crate::protection::joined(&refs)
    };
    if linked {
        // The shown contact keeps the name; the other one keeps its own.
        let a = &contacts[person.shown];
        let b = person
            .members
            .iter()
            .find(|&&i| i != person.shown)
            .map(|&i| &contacts[i])?;
        return Some(fill(
            &gettext(
                "All contacts uses the name {name}. {account_a} still has its contact, and {account_b} still has {other}.",
            ),
            &[
                ("name", &person.name),
                ("account_a", &short(a.account_id)),
                ("account_b", &short(b.account_id)),
                ("other", b.display()),
            ],
        ));
    }
    let many = person.accounts.len() > 1;
    match list {
        List::Account(id) => {
            let others: Vec<AccountId> = contacts
                .iter()
                .filter(|c| {
                    c.account_id != *id
                        && c.emails.iter().any(|e| {
                            person
                                .emails(contacts)
                                .iter()
                                .any(|p| p.eq_ignore_ascii_case(e))
                        })
                })
                .map(|c| c.account_id)
                .collect();
            if others.is_empty() {
                None
            } else {
                Some(fill(
                    &gettext(
                        "The chip is the account. {others} has its own contact for this person, and editing here changes {account}.",
                    ),
                    &[("others", &names(&others)), ("account", &short(*id))],
                ))
            }
        }
        List::Vips if many => Some(fill(
            &gettext(
                "VIPs are people you mark, so you can open them from this list. {name} is also stored in {accounts}.",
            ),
            &[
                ("name", &person.name),
                ("accounts", &names(&person.accounts)),
            ],
        )),
        List::Vips => Some(gettext(
            "VIPs are people you mark, so you can open them from this list.",
        )),
        _ if many => Some(fill(
            &gettext("One person, stored in {accounts}. Each account keeps its own contact."),
            &[("accounts", &names(&person.accounts))],
        )),
        _ => None,
    }
}

/// A card's own field name for people: `X-CUSTOMER-ID` reads "Customer ID".
/// Short all-capital words such as ID stay capitals; others take sentence case.
pub(super) fn humane(label: &str) -> String {
    // Only a raw property name is rewritten; a label someone typed stays.
    if label.chars().any(|c| c.is_lowercase() || c.is_whitespace()) {
        return label.to_string();
    }
    let name = label
        .strip_prefix("X-")
        .or_else(|| label.strip_prefix("x-"))
        .unwrap_or(label);
    let words: Vec<String> = name
        .split(['-', '_'])
        .filter(|w| !w.is_empty())
        .enumerate()
        .map(|(i, w)| {
            if w.len() <= 2 && w.chars().all(|c| c.is_ascii_uppercase()) {
                w.to_string()
            } else if i == 0 {
                let mut c = w.chars();
                c.next()
                    .map(|f| {
                        f.to_uppercase()
                            .chain(c.flat_map(char::to_lowercase))
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                w.to_lowercase()
            }
        })
        .collect();
    if words.is_empty() {
        label.to_string()
    } else {
        words.join(" ")
    }
}

#[cfg(test)]
mod humane_tests {
    use super::humane;

    #[test]
    fn raw_property_names_read_as_words_and_typed_labels_stay() {
        assert_eq!(humane("X-CUSTOMER-ID"), "Customer ID");
        assert_eq!(humane("X-ANNIVERSARY"), "Anniversary");
        assert_eq!(humane("SPOUSE_NAME"), "Spouse name");
        assert_eq!(humane("Customer ID"), "Customer ID");
        assert_eq!(humane("Badge"), "Badge");
    }
}
