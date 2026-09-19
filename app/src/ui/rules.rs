//! The Rules dialog: Gmail filters for one account, listed in plain words,
//! with a form to add one.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::{Account, Filter, Label, LabelKind};
use mailrs_sync::Permitted;

use crate::core::Core;
use crate::rules::{RuleForm, describe_action, describe_criteria};

struct Rules {
    core: Rc<Core>,
    account: Account,
    labels: Vec<Label>,
    nav: adw::NavigationView,
    stack: gtk::Stack,
    list: adw::PreferencesGroup,
    /// Rows in `list` now, removed on each reload.
    shown: RefCell<Vec<adw::ActionRow>>,
    toasts: adw::ToastOverlay,
    grant: Box<dyn Fn()>,
    dialog: adw::Dialog,
}

/// Shows the rules of `account`. `grant` runs when Gmail wants the
/// settings permission first.
pub fn present(
    core: &Rc<Core>,
    account: &Account,
    labels: Vec<Label>,
    parent: &impl IsA<gtk::Widget>,
    grant: impl Fn() + 'static,
) {
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    stack.add_named(
        &adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build(),
        Some("loading"),
    );
    let list = adw::PreferencesGroup::builder()
        .description("Gmail runs these on new mail as it arrives, even when this computer is off.")
        .build();
    let page = adw::PreferencesPage::new();
    page.add(&list);
    stack.add_named(&page, Some("list"));
    let add = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("New Rule")
        .build();
    let header = adw::HeaderBar::new();
    header.pack_start(&add);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));
    let nav = adw::NavigationView::new();
    nav.add(
        &adw::NavigationPage::builder()
            .title(format!("Rules for {}", account.email))
            .tag("rules")
            .child(&toolbar)
            .build(),
    );
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&nav));
    let dialog = adw::Dialog::builder()
        .content_width(560)
        .content_height(640)
        .child(&toasts)
        .build();
    let mut labels: Vec<Label> = labels
        .into_iter()
        .filter(|l| l.kind == LabelKind::User)
        .collect();
    labels.sort_by_key(|l| l.name.to_lowercase());
    let rules = Rc::new(Rules {
        core: Rc::clone(core),
        account: account.clone(),
        labels,
        nav,
        stack,
        list,
        shown: RefCell::new(Vec::new()),
        toasts,
        grant: Box::new(grant),
        dialog: dialog.clone(),
    });
    let weak = Rc::downgrade(&rules);
    add.connect_clicked(move |_| {
        if let Some(rules) = weak.upgrade() {
            rules.show_form();
        }
    });
    // The dialog owns the state behind its buttons until it closes.
    let keep = RefCell::new(Some(Rc::clone(&rules)));
    dialog.connect_closed(move |_| {
        keep.borrow_mut().take();
    });
    dialog.present(Some(parent));
    rules.reload();
}

impl Rules {
    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn label_name(&self, id: &str) -> Option<String> {
        self.labels
            .iter()
            .find(|l| l.id == id)
            .map(|l| l.name.replace('/', " › "))
    }

    fn reload(self: &Rc<Self>) {
        if self.core.account(self.account.id).is_none() {
            return self.problem("This account is not syncing yet.");
        }
        self.stack.set_visible_child_name("loading");
        let (this, settings, account_id) =
            (Rc::clone(self), self.core.gmail_settings(), self.account.id);
        glib::spawn_future_local(async move {
            let loaded = this
                .core
                .call(async move { settings.rules(account_id).await })
                .await;
            match loaded {
                Ok(Permitted::Done(filters)) => this.show_list(filters),
                Ok(Permitted::NeedsPermission) => this.ask_for_access(),
                Err(err) => this.problem(&err.to_string()),
            }
        });
    }

    fn show_list(self: &Rc<Self>, filters: Vec<Filter>) {
        for row in self.shown.borrow_mut().drain(..) {
            self.list.remove(&row);
        }
        if filters.is_empty() {
            let row = adw::ActionRow::builder()
                .title("No rules yet")
                .subtitle("Add one with the + button.")
                .build();
            self.list.add(&row);
            self.shown.borrow_mut().push(row);
        }
        for filter in filters {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&describe_criteria(
                    &filter.criteria,
                )))
                .subtitle(glib::markup_escape_text(&describe_action(
                    &filter.action,
                    |id| self.label_name(id),
                )))
                .build();
            let delete = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Delete Rule")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let (weak, id) = (Rc::downgrade(self), filter.id.clone());
            delete.connect_clicked(move |_| {
                if let (Some(rules), Some(id)) = (weak.upgrade(), id.clone()) {
                    rules.delete(id);
                }
            });
            row.add_suffix(&delete);
            self.list.add(&row);
            self.shown.borrow_mut().push(row);
        }
        self.stack.set_visible_child_name("list");
    }

    fn delete(self: &Rc<Self>, id: String) {
        let (this, settings, account_id) =
            (Rc::clone(self), self.core.gmail_settings(), self.account.id);
        glib::spawn_future_local(async move {
            let deleted = this
                .core
                .call(async move { settings.delete_rule(account_id, &id).await })
                .await;
            match deleted {
                Ok(Permitted::Done(())) => {
                    this.toast("Rule deleted");
                    this.reload();
                }
                Ok(Permitted::NeedsPermission) => this.ask_for_access(),
                Err(err) => this.toast(&format!("Could not delete the rule: {err}")),
            }
        });
    }

    fn problem(&self, message: &str) {
        let page = adw::StatusPage::builder()
            .icon_name("dialog-warning-symbolic")
            .title("Could Not Load the Rules")
            .description(glib::markup_escape_text(message).as_str())
            .build();
        self.replace_page("problem", &page);
    }

    fn ask_for_access(self: &Rc<Self>) {
        let page = adw::StatusPage::builder()
            .icon_name("mail-send-symbolic")
            .title("Allow Rules")
            .description(format!(
                "Penguin Mail needs permission to change Gmail settings for {}. Google asks you to confirm in your browser.",
                self.account.email
            ))
            .build();
        let button = gtk::Button::builder()
            .label("Grant Access")
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(rules) = weak.upgrade() {
                rules.dialog.close();
                (rules.grant)();
            }
        });
        page.set_child(Some(&button));
        self.replace_page("problem", &page);
    }

    fn replace_page(&self, name: &str, page: &impl IsA<gtk::Widget>) {
        if let Some(old) = self.stack.child_by_name(name) {
            self.stack.remove(&old);
        }
        self.stack.add_named(page, Some(name));
        self.stack.set_visible_child_name(name);
    }

    /// The New Rule page.
    fn show_form(self: &Rc<Self>) {
        let entry = |title: &str| adw::EntryRow::builder().title(title).build();
        let switch = |title: &str| adw::SwitchRow::builder().title(title).build();
        let (from, to, subject, has, not) = (
            entry("From"),
            entry("To"),
            entry("Subject"),
            entry("Has the Words"),
            entry("Doesn't Have"),
        );
        let attachment = switch("Has an Attachment");
        let when = adw::PreferencesGroup::builder()
            .title("When Mail Matches")
            .build();
        for row in [&from, &to, &subject, &has, &not] {
            when.add(row);
        }
        when.add(&attachment);

        let (skip, read, star, never_spam, trash) = (
            switch("Skip the Inbox"),
            switch("Mark as Read"),
            switch("Star It"),
            switch("Never Send to Spam"),
            switch("Delete It"),
        );
        let mut names = vec!["Don't Apply a Label".to_string()];
        names.extend(self.labels.iter().map(|l| l.name.replace('/', " › ")));
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let label = adw::ComboRow::builder()
            .title("Apply Label")
            .model(&gtk::StringList::new(&refs))
            .build();
        let then = adw::PreferencesGroup::builder().title("Do This").build();
        for row in [&skip, &read, &star] {
            then.add(row);
        }
        then.add(&label);
        then.add(&never_spam);
        then.add(&trash);

        let page = adw::PreferencesPage::new();
        page.add(&when);
        page.add(&then);
        let create = gtk::Button::builder()
            .label("Create")
            .css_classes(["suggested-action"])
            .build();
        let header = adw::HeaderBar::new();
        header.pack_end(&create);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&page));
        self.nav.push(
            &adw::NavigationPage::builder()
                .title("New Rule")
                .tag("new-rule")
                .child(&toolbar)
                .build(),
        );

        let weak = Rc::downgrade(self);
        create.connect_clicked(move |button| {
            let Some(rules) = weak.upgrade() else { return };
            let form = RuleForm {
                from: from.text().into(),
                to: to.text().into(),
                subject: subject.text().into(),
                has_words: has.text().into(),
                not_words: not.text().into(),
                has_attachment: attachment.is_active(),
                skip_inbox: skip.is_active(),
                mark_read: read.is_active(),
                star: star.is_active(),
                label: (label.selected() > 0)
                    .then(|| rules.labels.get(label.selected() as usize - 1))
                    .flatten()
                    .map(|l| l.id.clone()),
                never_spam: never_spam.is_active(),
                trash: trash.is_active(),
            };
            let filter = match form.filter() {
                Ok(filter) => filter,
                Err(reason) => return rules.toast(reason),
            };
            let (settings, account_id) = (rules.core.gmail_settings(), rules.account.id);
            button.set_sensitive(false);
            let button = button.clone();
            glib::spawn_future_local(async move {
                let added = rules
                    .core
                    .call(async move { settings.add_rule(account_id, filter).await })
                    .await;
                match added {
                    Ok(Permitted::Done(_)) => {
                        rules.nav.pop();
                        rules.toast("Rule added");
                        rules.reload();
                    }
                    Ok(Permitted::NeedsPermission) => {
                        button.set_sensitive(true);
                        rules.ask_for_access();
                    }
                    Err(err) => {
                        button.set_sensitive(true);
                        rules.toast(&format!("Could not add the rule: {err}"));
                    }
                }
            });
        });
    }
}
