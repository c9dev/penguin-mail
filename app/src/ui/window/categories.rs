//! The category switcher above an inbox and the Categorize Sender action.
//! `mailrs_domain::Category` holds which Gmail labels each category means.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::{AccountId, Category, Filter, FilterAction, FilterCriteria, system_label};
use mailrs_store::threads::{self, ThreadFilter};
use mailrs_sync::{History, MailAction, Permitted, SyncError, TriageAction};

use super::{MainWindow, Target};
use crate::ui::Mailbox;
use crate::ui::conversation::ConversationView;

fn icon(category: Category) -> &'static str {
    match category {
        Category::All => "penguin-mail-inbox-symbolic",
        Category::Primary => "avatar-default-symbolic",
        Category::Updates => "preferences-system-notifications-symbolic",
        Category::Promotions => "penguin-mail-tag-symbolic",
        Category::Social => "system-users-symbolic",
    }
}

/// The switcher above an inbox's thread list. Only the chosen category
/// shows its name; the others show an icon and their unread count.
pub(super) struct CategoryBar {
    bar: gtk::Box,
    group: adw::ToggleGroup,
    names: HashMap<Category, gtk::Label>,
    counts: HashMap<Category, gtk::Label>,
    pub(super) chosen: Cell<Category>,
}

impl CategoryBar {
    pub(super) fn new() -> CategoryBar {
        let group = adw::ToggleGroup::builder()
            .homogeneous(false)
            .halign(gtk::Align::Center)
            .hexpand(true)
            .css_classes(["category-bar"])
            .build();
        let (mut names, mut counts) = (HashMap::new(), HashMap::new());
        for category in Category::ALL {
            let content = gtk::Box::builder().spacing(6).build();
            content.append(&gtk::Image::from_icon_name(icon(category)));
            let name = gtk::Label::new(Some(category.name()));
            let count = gtk::Label::builder()
                .css_classes(["category-count"])
                .visible(false)
                .build();
            content.append(&name);
            content.append(&count);
            group.add(
                adw::Toggle::builder()
                    .name(category.key())
                    .tooltip(category.name())
                    .child(&content)
                    .build(),
            );
            names.insert(category, name);
            counts.insert(category, count);
        }
        let bar = gtk::Box::builder()
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(12)
            .margin_end(12)
            .visible(false)
            .build();
        bar.append(&group);
        let chosen = Category::Primary;
        group.set_active_name(Some(chosen.key()));
        let this = CategoryBar {
            bar,
            group,
            names,
            counts,
            chosen: Cell::new(chosen),
        };
        this.show_names();
        this
    }

    fn show_names(&self) {
        for (category, name) in &self.names {
            name.set_visible(*category == self.chosen.get());
        }
    }

    pub(super) fn set_counts(&self, unread: &HashMap<Category, i64>) {
        for (category, label) in &self.counts {
            let count = unread.get(category).copied().unwrap_or(0);
            label.set_label(&count.to_string());
            label.set_visible(count > 0);
        }
    }
}

impl MainWindow {
    /// Puts the category switcher above the thread list and adds the
    /// Categorize Sender action.
    pub(super) fn install_categories(self: &Rc<Self>) {
        if let Some(toolbar) = self.list.page.child().and_downcast::<adw::ToolbarView>() {
            toolbar.add_top_bar(&self.categories.bar);
        }
        let weak = Rc::downgrade(self);
        self.categories
            .group
            .connect_active_name_notify(move |group| {
                let Some(win) = weak.upgrade() else { return };
                let Some(category) = group.active_name().and_then(|k| Category::from_key(&k))
                else {
                    return;
                };
                if win.categories.chosen.replace(category) == category {
                    return;
                }
                win.categories.show_names();
                win.list.unselect();
                win.conversation.clear();
                win.reload_list();
            });
        let categorize =
            gtk::gio::SimpleAction::new("categorize-sender", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        categorize.connect_activate(move |_, parameter| {
            let (Some(win), Some(category)) = (
                weak.upgrade(),
                parameter
                    .and_then(|p| p.get::<String>())
                    .and_then(|k| Category::from_key(&k)),
            ) else {
                return;
            };
            win.categorize_sender_from(Rc::clone(&win.conversation), category);
        });
        self.actions.add_action(&categorize);
    }

    /// Whether `mailbox` splits into categories on screen.
    fn shows_categories(&self, mailbox: &Mailbox) -> bool {
        self.settings().inbox_categories && mailbox.takes_categories()
    }

    /// Shows the switcher when the list holds an inbox, and hides it elsewhere.
    pub(super) fn follow_categories(self: &Rc<Self>) {
        let shown = self.shows_categories(&self.mailbox.borrow());
        self.categories.bar.set_visible(shown);
        if shown {
            self.refresh_counts();
        }
    }

    /// Shows how much unread mail each category holds.
    pub(super) fn set_category_counts(&self, unread: &HashMap<Category, i64>) {
        self.categories.set_counts(unread);
    }

    /// Moves every stored conversation from the open message's sender into
    /// `category`, and adds a Gmail filter that sorts their future mail there.
    pub(super) fn categorize_sender_from(
        self: &Rc<Self>,
        view: Rc<ConversationView>,
        category: Category,
    ) {
        let found = view.with_open(|open| {
            let me = open.me.clone();
            let sender = open
                .messages
                .iter()
                .rev()
                .filter_map(|m| m.from.clone())
                .find(|a| !me.iter().any(|mine| mine.eq_ignore_ascii_case(&a.email)))?;
            let who = sender.display().to_string();
            Some((open.account_id, sender.email, who, open.thread_id.clone()))
        });
        let Some(Some((account_id, email, who, open_thread))) = found else {
            return self.toast("Open a message from the sender first");
        };
        self.categorize_sender(account_id, email, who, Some(open_thread), category);
    }

    /// Moves `email`'s stored conversations in the account into `category`,
    /// plus `also` when given, and sorts their future mail there.
    pub(super) fn categorize_sender(
        self: &Rc<Self>,
        account_id: AccountId,
        email: String,
        who: String,
        also: Option<String>,
        category: Category,
    ) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let key = email.clone();
            let mut ids: Vec<String> = this
                .core
                .read(move |c| {
                    let theirs = ThreadFilter::account(account_id, "").from_senders(vec![key]);
                    Ok(threads::list_threads(c, &theirs, 0, 10_000)?
                        .into_iter()
                        .map(|t| t.id)
                        .collect())
                })
                .await
                .unwrap_or_default();
            if let Some(thread) = also
                && !ids.contains(&thread)
            {
                ids.push(thread);
            }
            let targets: Vec<Target> = ids
                .into_iter()
                .map(|thread_id| Target {
                    account_id,
                    thread_id,
                    message_id: None,
                })
                .collect();
            let label = category.gmail_label();
            let relabel = TriageAction::Relabel {
                add: vec![label.into()],
                remove: system_label::CATEGORIES
                    .iter()
                    .filter(|l| **l != label)
                    .map(|l| l.to_string())
                    .collect(),
            };
            // No undo: putting back the old labels would need each thread's own.
            this.perform(targets, MailAction::Triage(relabel), History::Skip, None);
            let name = category.name();
            match this.sort_future_mail(account_id, &email, label).await {
                Ok(Permitted::Done(())) => {
                    this.toast(&format!("Mail from {who} now goes to {name}"))
                }
                Ok(Permitted::NeedsPermission) => {
                    this.toast(&format!("Moved mail from {who} to {name}"));
                    this.ask_for_settings_access(account_id);
                }
                Err(err) => this.toast(&format!(
                    "Moved mail from {who} to {name}, but could not sort new mail: {err}"
                )),
            }
        });
    }

    /// Replaces any category filter for `email` with one that adds `label`.
    async fn sort_future_mail(
        self: &Rc<Self>,
        account_id: AccountId,
        email: &str,
        label: &'static str,
    ) -> anyhow::Result<Permitted<()>> {
        if self.core.account(account_id).is_none() {
            anyhow::bail!("that account is not connected");
        }
        let (settings, from) = (self.core.gmail_settings(), email.to_string());
        self.core
            .call(async move {
                let Permitted::Done(rules) = settings.rules(account_id).await? else {
                    return Ok(Permitted::NeedsPermission);
                };
                for old in rules {
                    let sorts_sender = old
                        .criteria
                        .from
                        .as_deref()
                        .is_some_and(|f| f.eq_ignore_ascii_case(&from))
                        && !old.action.add_label_ids.is_empty()
                        && old
                            .action
                            .add_label_ids
                            .iter()
                            .all(|l| system_label::is_category(l));
                    if let (true, Some(id)) = (sorts_sender, old.id.as_deref())
                        && settings.delete_rule(account_id, id).await? == Permitted::NeedsPermission
                    {
                        return Ok(Permitted::NeedsPermission);
                    }
                }
                let rule = Filter {
                    id: None,
                    criteria: FilterCriteria {
                        from: Some(from),
                        ..FilterCriteria::default()
                    },
                    action: FilterAction {
                        add_label_ids: vec![label.into()],
                        ..FilterAction::default()
                    },
                };
                Ok::<_, SyncError>(match settings.add_rule(account_id, rule).await? {
                    Permitted::Done(_) => Permitted::Done(()),
                    Permitted::NeedsPermission => Permitted::NeedsPermission,
                })
            })
            .await
    }
}
