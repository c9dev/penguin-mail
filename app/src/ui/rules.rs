//! The Rules dialog: an account's rules, listed in plain words, with a form
//! to add one or edit one. Gmail keeps them as filters, a ManageSieve
//! server in the app's Sieve script, and an account whose server runs none
//! on this computer.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::{Account, Filter, Label, LabelKind};
use mailrs_sync::{BackendError, Permitted, Replaced, RuleList, SyncError};

use crate::core::Core;
use crate::permission::Permission;
use crate::offered::Filing;
use crate::rules::{
    RuleForm, Unshown, describe_action, describe_criteria, offers_never_spam, place_line,
    waiting_line,
};
use crate::ui::permission;
use mailrs_domain::translate::{fill, gettext, with_reason};

/// The save to repeat once the person has agreed to replace a script.
type Retry = dyn Fn(&Rc<Rules>);

struct Rules {
    core: Rc<Core>,
    account: Account,
    labels: Vec<Label>,
    filing: Filing,
    nav: adw::NavigationView,
    stack: gtk::Stack,
    page: adw::PreferencesPage,
    list: adw::PreferencesGroup,
    /// The group of rules written elsewhere, removed on each reload.
    elsewhere: RefCell<Option<adw::PreferencesGroup>>,
    /// Rows in `list` now, removed on each reload.
    shown: RefCell<Vec<gtk::Widget>>,
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
    filing: Filing,
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
    let list = adw::PreferencesGroup::new();
    let page = adw::PreferencesPage::new();
    page.add(&list);
    stack.add_named(&page, Some("list"));
    let add = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text(gettext("New Rule"))
        .build();
    crate::ui::name(&add, &gettext("New Rule"));
    let header = adw::HeaderBar::new();
    header.pack_start(&add);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));
    let nav = adw::NavigationView::new();
    nav.add(
        &adw::NavigationPage::builder()
            .title(fill(
                &gettext("Rules for {account}"),
                &[("account", &account.email)],
            ))
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
        filing,
        nav,
        stack,
        page,
        list,
        elsewhere: RefCell::new(None),
        shown: RefCell::new(Vec::new()),
        toasts,
        grant: Box::new(grant),
        dialog: dialog.clone(),
    });
    let weak = Rc::downgrade(&rules);
    add.connect_clicked(move |_| {
        if let Some(rules) = weak.upgrade() {
            rules.show_form(None);
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
        self.toasts.add_toast(crate::ui::toast(text));
    }

    /// Toasts a failure. `said` is `gettext` of the sentence, with
    /// `{reason}` where the error goes.
    fn failed(&self, said: &str, err: &impl std::fmt::Display) {
        self.toast(&with_reason(said, err, &[]));
    }

    fn label_name(&self, id: &str) -> Option<String> {
        self.labels
            .iter()
            .find(|l| l.id == id)
            .map(|l| l.name.replace('/', " › "))
    }

    fn reload(self: &Rc<Self>) {
        if self.core.account(self.account.id).is_none() {
            return self.problem(&gettext("This account is not syncing yet."));
        }
        self.stack.set_visible_child_name("loading");
        let (this, settings, account_id) =
            (Rc::clone(self), self.core.gmail_settings(), self.account.id);
        glib::spawn_future_local(async move {
            let loaded = this
                .core
                .call(async move { settings.rule_list(account_id).await })
                .await;
            match loaded {
                Ok(Permitted::Done(list)) => this.show_list(list),
                Ok(Permitted::NeedsPermission) => this.ask_for_access(),
                Err(err) => this.problem(&err.to_string()),
            }
        });
    }

    fn show_list(self: &Rc<Self>, list: RuleList) {
        for row in self.shown.borrow_mut().drain(..) {
            self.list.remove(&row);
        }
        if let Some(old) = self.elsewhere.borrow_mut().take() {
            self.page.remove(&old);
        }
        let provider = mailrs_discover::resolved_provider_name(self.account.provider_name());
        self.list.set_description(Some(&place_line(list.place, &provider)));
        if list.waiting {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&waiting_line(&provider)))
                .css_classes(["warning"])
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("network-offline-symbolic"));
            self.list.add(&row);
            self.shown.borrow_mut().push(row.upcast());
        }
        if list.rules.is_empty() {
            // The empty list offers the first rule itself rather than
            // pointing at the + in the header.
            let row = adw::ButtonRow::builder()
                .title(gettext("Add Rule"))
                .start_icon_name("list-add-symbolic")
                .build();
            let weak = Rc::downgrade(self);
            row.connect_activated(move |_| {
                if let Some(rules) = weak.upgrade() {
                    rules.show_form(None);
                }
            });
            self.list.add(&row);
            self.shown.borrow_mut().push(row.upcast());
        }
        for filter in list.rules {
            let criteria = describe_criteria(&filter.criteria);
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&criteria))
                .subtitle(glib::markup_escape_text(&describe_action(
                    &filter.action,
                    |id| self.label_name(id),
                )))
                .activatable(true)
                .build();
            if filter.read_only {
                // The server holds this rule in a shape the form cannot say,
                // so the row neither opens nor deletes it. The line says why.
                row.set_subtitle(&glib::markup_escape_text(&format!(
                    "{}\n{}",
                    describe_action(&filter.action, |id| self.label_name(id)),
                    gettext("Made elsewhere. Change it where you made it."),
                )));
                row.set_activatable(false);
                row.add_suffix(&gtk::Image::from_icon_name("changes-prevent-symbolic"));
                self.list.add(&row);
                self.shown.borrow_mut().push(row.upcast());
                continue;
            }
            // The row points its LabelledBy relation at the title, which
            // wins over a label set on it, so drop the relation first.
            row.upcast_ref::<gtk::Widget>()
                .reset_relation(gtk::AccessibleRelation::LabelledBy);
            crate::ui::name(
                &row,
                &fill(&gettext("Edit the rule for {mail}"), &[("mail", &criteria)]),
            );
            let weak = Rc::downgrade(self);
            let editing = filter.clone();
            row.connect_activated(move |_| {
                if let Some(rules) = weak.upgrade() {
                    rules.show_form(Some(editing.clone()));
                }
            });
            let delete = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text(gettext("Delete Rule"))
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            crate::ui::name(
                &delete,
                &fill(
                    &gettext("Delete the rule for {mail}"),
                    &[("mail", &describe_criteria(&filter.criteria))],
                ),
            );
            let (weak, id) = (Rc::downgrade(self), filter.id.clone());
            delete.connect_clicked(move |_| {
                if let (Some(rules), Some(id)) = (weak.upgrade(), id.clone()) {
                    rules.delete(id);
                }
            });
            row.add_suffix(&delete);
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            self.list.add(&row);
            self.shown.borrow_mut().push(row.upcast());
        }
        if !list.elsewhere.is_empty() {
            let group = adw::PreferencesGroup::builder()
                .title(gettext("Rules Written Elsewhere"))
                .description(gettext(
                    "Penguin Mail keeps these as they are. Change them where they were written.",
                ))
                .build();
            for block in &list.elsewhere {
                let first = block
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && !l.starts_with('#'))
                    .unwrap_or(block.as_str());
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(first))
                    .build();
                row.add_suffix(&gtk::Image::from_icon_name("changes-prevent-symbolic"));
                group.add(&row);
            }
            self.page.add(&group);
            *self.elsewhere.borrow_mut() = Some(group);
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
                    this.toast(&gettext("Rule deleted"));
                    this.reload();
                }
                Ok(Permitted::NeedsPermission) => this.ask_for_access(),
                Err(err) => this.failed(&gettext("Could not delete the rule: {reason}"), &err),
            }
        });
    }

    fn problem(&self, message: &str) {
        let page = adw::StatusPage::builder()
            .icon_name("dialog-warning-symbolic")
            .title(gettext("Could Not Load the Rules"))
            .description(glib::markup_escape_text(message).as_str())
            .build();
        self.replace_page("problem", &page);
    }

    fn ask_for_access(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let page = permission::page(
            &gettext("Allow Rules"),
            Permission::Settings,
            &self.account.email,
            self.account.provider,
            move || {
                if let Some(rules) = weak.upgrade() {
                    rules.dialog.close();
                    (rules.grant)();
                }
            },
        );
        self.replace_page("problem", &page);
    }

    fn replace_page(&self, name: &str, page: &impl IsA<gtk::Widget>) {
        if let Some(old) = self.stack.child_by_name(name) {
            self.stack.remove(&old);
        }
        self.stack.add_named(page, Some(name));
        self.stack.set_visible_child_name(name);
    }

    /// The rule form: New Rule when `editing` is `None`, else Edit Rule
    /// with every field filled in from that rule.
    fn show_form(self: &Rc<Self>, editing: Option<Filter>) {
        let entry = |title: &str| adw::EntryRow::builder().title(title).build();
        let switch = |title: &str| adw::SwitchRow::builder().title(title).build();
        let (from, to, subject, has, not) = (
            entry(&gettext("From")),
            entry(&gettext("To")),
            entry(&gettext("Subject")),
            entry(&gettext("Has the Words")),
            entry(&gettext("Doesn't Have")),
        );
        let attachment = switch(&gettext("Has an Attachment"));
        let when = adw::PreferencesGroup::builder()
            .title(gettext("When Mail Matches"))
            .build();
        for row in [&from, &to, &subject, &has, &not] {
            when.add(row);
        }
        when.add(&attachment);

        let (skip, read, star, never_spam, trash) = (
            switch(&gettext("Skip the Inbox")),
            switch(&gettext("Mark as Read")),
            switch(&gettext("Star It")),
            switch(&gettext("Never Send to Spam")),
            switch(&gettext("Delete It")),
        );
        let (label_title, no_label) = match self.filing {
            Filing::Labels => (gettext("Apply Label"), gettext("Don't Apply a Label")),
            Filing::Folders => (gettext("Move to Folder"), gettext("Don't Move")),
        };
        let mut names = vec![no_label];
        names.extend(self.labels.iter().map(|l| l.name.replace('/', " › ")));
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let label = adw::ComboRow::builder()
            .title(label_title)
            .model(&gtk::StringList::new(&refs))
            .build();
        let then = adw::PreferencesGroup::builder()
            .title(gettext("Do This"))
            .build();
        for row in [&skip, &read, &star] {
            then.add(row);
        }
        then.add(&label);
        if offers_never_spam(self.filing) {
            then.add(&never_spam);
        }
        then.add(&trash);

        let page = adw::PreferencesPage::new();
        page.add(&when);
        page.add(&then);

        // An edit fills the form in and sets aside what it cannot show,
        // which Save puts back.
        let unshown = match &editing {
            Some(rule) => {
                let (form, unshown) =
                    RuleForm::read(rule, |id| self.labels.iter().any(|l| l.id == id));
                from.set_text(&form.from);
                to.set_text(&form.to);
                subject.set_text(&form.subject);
                has.set_text(&form.has_words);
                not.set_text(&form.not_words);
                attachment.set_active(form.has_attachment);
                skip.set_active(form.skip_inbox);
                read.set_active(form.mark_read);
                star.set_active(form.star);
                never_spam.set_active(form.never_spam);
                trash.set_active(form.trash);
                let index = form
                    .label
                    .and_then(|id| self.labels.iter().position(|l| l.id == id));
                label.set_selected(index.map_or(0, |i| i as u32 + 1));
                if !unshown.is_empty() {
                    page.add(
                        &adw::PreferencesGroup::builder()
                            .description(gettext(
                                "This rule also does things this form can't show. Saving keeps them.",
                            ))
                            .build(),
                    );
                }
                unshown
            }
            None => Unshown::default(),
        };

        let (title, tag, verb) = match editing {
            Some(_) => (gettext("Edit Rule"), "edit-rule", gettext("Save")),
            None => (gettext("New Rule"), "new-rule", gettext("Create")),
        };
        let save = gtk::Button::builder()
            .label(verb)
            .css_classes(["suggested-action"])
            .build();
        let cancel = gtk::Button::builder().label(gettext("Cancel")).build();
        let header = adw::HeaderBar::builder().show_back_button(false).build();
        header.pack_start(&cancel);
        header.pack_end(&save);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&page));
        self.nav.push(
            &adw::NavigationPage::builder()
                .title(title)
                .tag(tag)
                .child(&toolbar)
                .build(),
        );

        let weak = Rc::downgrade(self);
        cancel.connect_clicked(move |_| {
            if let Some(rules) = weak.upgrade() {
                rules.nav.pop();
            }
        });
        let weak = Rc::downgrade(self);
        save.connect_clicked(move |button| {
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
            let filter = match form.filter_keeping(&unshown) {
                Ok(filter) => filter,
                // The form answers in English, which the assistant hands to
                // the model; sync's Sieve code puts both lines in the
                // catalogue.
                Err(reason) => return rules.toast(&gettext(reason)),
            };
            button.set_sensitive(false);
            match editing.clone() {
                Some(old) => rules.replace(old, filter, button.clone()),
                None => rules.add(filter, button.clone()),
            }
        });
    }

    fn add(self: &Rc<Self>, filter: Filter, button: gtk::Button) {
        let (this, settings, account_id) =
            (Rc::clone(self), self.core.gmail_settings(), self.account.id);
        glib::spawn_future_local(async move {
            let again = filter.clone();
            let added = this
                .core
                .call(async move { settings.add_rule(account_id, filter).await })
                .await;
            match added {
                Ok(Permitted::Done(_)) => this.saved(&gettext("Rule added")),
                Ok(Permitted::NeedsPermission) => {
                    button.set_sensitive(true);
                    this.ask_for_access();
                }
                Err(err) => {
                    button.set_sensitive(true);
                    match would_replace(&err) {
                        Some(script) => {
                            let retry = button.clone();
                            this.ask_to_replace(script, move |rules| {
                                retry.set_sensitive(false);
                                rules.add(again.clone(), retry.clone());
                            });
                        }
                        None => this.failed(&gettext("Could not add the rule: {reason}"), &err),
                    }
                }
            }
        });
    }

    /// Saves an edit: the server makes `filter` and then drops `old`.
    fn replace(self: &Rc<Self>, old: Filter, filter: Filter, button: gtk::Button) {
        let (this, settings, account_id) =
            (Rc::clone(self), self.core.gmail_settings(), self.account.id);
        glib::spawn_future_local(async move {
            let (old_again, filter_again) = (old.clone(), filter.clone());
            let replaced = this
                .core
                .call(async move { settings.replace_rule(account_id, &old, filter).await })
                .await;
            match replaced {
                Ok(Permitted::Done(Replaced::Swapped(_))) => this.saved(&gettext("Rule saved")),
                Ok(Permitted::Done(Replaced::BothRun { error, .. })) => {
                    this.nav.pop();
                    this.failed(
                        &gettext(
                            "Saved, but Gmail kept the old rule too, so both run now: {reason}",
                        ),
                        &error,
                    );
                    this.reload();
                }
                Ok(Permitted::NeedsPermission) => {
                    button.set_sensitive(true);
                    this.ask_for_access();
                }
                Err(err) => {
                    button.set_sensitive(true);
                    match would_replace(&err) {
                        Some(script) => {
                            let retry = button.clone();
                            this.ask_to_replace(script, move |rules| {
                                retry.set_sensitive(false);
                                rules.replace(old_again.clone(), filter_again.clone(), retry.clone());
                            });
                        }
                        None => this.failed(&gettext("Could not save the rule: {reason}"), &err),
                    }
                }
            }
        });
    }

    /// Asks before Penguin Mail's rules take the place of the script the
    /// person runs on a server that runs one script and cannot include
    /// another. `retry` repeats the save that was refused, after the
    /// takeover. Cancel changes nothing on the server.
    fn ask_to_replace(self: &Rc<Self>, script: String, retry: impl Fn(&Rc<Self>) + 'static) {
        let provider = mailrs_discover::resolved_provider_name(self.account.provider_name());
        let dialog = adw::AlertDialog::builder()
            .heading(fill(&gettext("Replace “{script}”?"), &[("script", &script)]))
            .body(fill(
                &gettext("{provider} runs one set of rules at a time, and it runs “{script}” now. Penguin Mail's rules would take its place; “{script}” stays on the server, switched off."),
                &[("provider", &provider), ("script", &script)],
            ))
            .build();
        dialog.add_responses(&[("cancel", &gettext("Cancel")), ("replace", &gettext("Replace"))]);
        dialog.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let retry: Rc<Retry> = Rc::new(retry);
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(rules) = weak.upgrade() else { return };
            if response != "replace" {
                return;
            }
            let (settings, account_id) = (rules.core.gmail_settings(), rules.account.id);
            let retry = Rc::clone(&retry);
            glib::spawn_future_local(async move {
                let taken = rules
                    .core
                    .call(async move { settings.take_over_rules(account_id).await })
                    .await;
                match taken {
                    Ok(()) => retry(&rules),
                    Err(err) => rules.failed(&gettext("Could not add the rule: {reason}"), &err),
                }
            });
        });
        dialog.present(Some(&self.dialog));
    }

    /// Back to the list after a save, which it then shows again.
    fn saved(self: &Rc<Self>, said: &str) {
        self.nav.pop();
        self.toast(said);
        self.reload();
    }
}

/// The name of the script a refused write would replace.
fn would_replace(err: &anyhow::Error) -> Option<String> {
    match err.downcast_ref::<SyncError>() {
        Some(SyncError::Backend(BackendError::WouldReplace { script })) => Some(script.clone()),
        _ => None,
    }
}
