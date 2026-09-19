//! Inbox categories, after Apple Mail: Primary, Updates, Promotions, and
//! Social, built on the category labels Gmail puts on inbox mail.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::{AccountId, Filter, FilterAction, FilterCriteria};
use mailrs_store::threads::{self, ThreadFilter};
use mailrs_sync::TriageAction;

use super::{MainWindow, Target};
use crate::ui::Mailbox;
use crate::ui::conversation::ConversationView;
use crate::ui::vacation::missing_scope;

/// Every category label Gmail uses. Mail with none of them counts as Primary.
const GMAIL_LABELS: [&str; 5] = [
    "CATEGORY_PERSONAL",
    "CATEGORY_UPDATES",
    "CATEGORY_PROMOTIONS",
    "CATEGORY_SOCIAL",
    "CATEGORY_FORUMS",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Category {
    All,
    Primary,
    Updates,
    Promotions,
    Social,
}

impl Category {
    const ALL: [Category; 5] = [
        Category::All,
        Category::Primary,
        Category::Updates,
        Category::Promotions,
        Category::Social,
    ];

    fn key(self) -> &'static str {
        match self {
            Category::All => "all",
            Category::Primary => "primary",
            Category::Updates => "updates",
            Category::Promotions => "promotions",
            Category::Social => "social",
        }
    }

    pub(super) fn from_key(key: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.key() == key)
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Category::All => "All",
            Category::Primary => "Primary",
            Category::Updates => "Updates",
            Category::Promotions => "Promotions",
            Category::Social => "Social",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Category::All => "penguin-mail-inbox-symbolic",
            Category::Primary => "avatar-default-symbolic",
            Category::Updates => "preferences-system-notifications-symbolic",
            Category::Promotions => "penguin-mail-tag-symbolic",
            Category::Social => "system-users-symbolic",
        }
    }

    /// Labels a thread needs one of, and labels it must not have. Gmail
    /// files mailing lists under Forums; they show with Social here.
    fn labels(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Category::All => (&[], &[]),
            Category::Primary => (&[], &GMAIL_LABELS[1..]),
            Category::Updates => (&["CATEGORY_UPDATES"], &[]),
            Category::Promotions => (&["CATEGORY_PROMOTIONS"], &[]),
            Category::Social => (&["CATEGORY_SOCIAL", "CATEGORY_FORUMS"], &[]),
        }
    }

    /// The label Gmail gives mail sorted into this category.
    pub(super) fn gmail_label(self) -> &'static str {
        match self {
            Category::All | Category::Primary => "CATEGORY_PERSONAL",
            Category::Updates => "CATEGORY_UPDATES",
            Category::Promotions => "CATEGORY_PROMOTIONS",
            Category::Social => "CATEGORY_SOCIAL",
        }
    }

    fn narrow(self, filter: ThreadFilter) -> ThreadFilter {
        let (any, none) = self.labels();
        filter.with_labels(any, none)
    }
}

/// The switcher above an inbox's thread list. Only the chosen category
/// shows its name; the others show an icon and their unread count.
pub(super) struct CategoryBar {
    bar: gtk::Box,
    group: adw::ToggleGroup,
    names: HashMap<Category, gtk::Label>,
    counts: HashMap<Category, gtk::Label>,
    chosen: Cell<Category>,
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
            content.append(&gtk::Image::from_icon_name(category.icon()));
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

    fn set_counts(&self, unread: &HashMap<Category, i64>) {
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
        self.settings().inbox_categories && is_inbox(mailbox)
    }

    /// Shows the switcher when the list holds an inbox, and hides it elsewhere.
    pub(super) fn follow_categories(self: &Rc<Self>) {
        let shown = self.shows_categories(&self.mailbox.borrow());
        self.categories.bar.set_visible(shown);
        if shown {
            self.refresh_category_counts();
        }
    }

    /// `filter` narrowed to the chosen category, when `mailbox` has them.
    pub(super) fn in_category(&self, mailbox: &Mailbox, filter: ThreadFilter) -> ThreadFilter {
        if self.shows_categories(mailbox) {
            self.categories.chosen.get().narrow(filter)
        } else {
            filter
        }
    }

    /// Counts unread mail in each category of the inbox on screen.
    pub(super) fn refresh_category_counts(self: &Rc<Self>) {
        let mailbox = self.mailbox.borrow().clone();
        if !self.shows_categories(&mailbox) {
            return;
        }
        let Some(base) = mailbox.filter() else { return };
        let threaded = self.settings().threading;
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let counted = this
                .core
                .read(move |c| {
                    let mut unread = HashMap::new();
                    for category in Category::ALL {
                        let filter = category.narrow(base.clone());
                        let count = if threaded {
                            threads::unread_threads(c, &filter)?
                        } else {
                            threads::unread_messages(c, &filter)?
                        };
                        unread.insert(category, count);
                    }
                    Ok(unread)
                })
                .await;
            if let Ok(unread) = counted {
                this.categories.set_counts(&unread);
            }
        });
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
                remove: GMAIL_LABELS
                    .iter()
                    .filter(|l| **l != label)
                    .map(|l| l.to_string())
                    .collect(),
            };
            // No undo: putting back the old labels would need each thread's own.
            this.apply_with(targets, relabel, false, None);
            let name = category.name();
            match this.sort_future_mail(account_id, &email, label).await {
                Ok(()) => this.toast(&format!("Mail from {who} now goes to {name}")),
                Err(err) if missing_scope(&err) => {
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
    ) -> anyhow::Result<()> {
        let Some(sync) = self.core.account(account_id) else {
            anyhow::bail!("that account is not connected");
        };
        let (s, from) = (sync.clone(), email.to_string());
        self.core
            .call(async move {
                for old in s.filters().await? {
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
                            .all(|l| l.starts_with("CATEGORY_"));
                    if let (true, Some(id)) = (sorts_sender, old.id.as_deref()) {
                        s.delete_filter(id).await?;
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
                s.create_filter(rule).await?;
                Ok::<(), anyhow::Error>(())
            })
            .await
    }
}

fn is_inbox(mailbox: &Mailbox) -> bool {
    match mailbox {
        Mailbox::Unified(label) => *label == "INBOX",
        Mailbox::Label { label_id, .. } => label_id == "INBOX",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::Category;

    #[test]
    fn keys_round_trip() {
        for category in Category::ALL {
            assert_eq!(Category::from_key(category.key()), Some(category));
        }
        assert_eq!(Category::from_key("junk"), None);
    }

    #[test]
    fn primary_excludes_every_other_category_but_not_personal() {
        let (any, none) = Category::Primary.labels();
        assert!(any.is_empty());
        assert!(!none.contains(&"CATEGORY_PERSONAL"));
        assert_eq!(none.len(), 4);
    }
}
