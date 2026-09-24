//! The category switcher above an inbox and the Categorize Sender action.
//! `mailrs_domain::Category` holds which Gmail labels each category means.

use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::{AccountId, Category};
use mailrs_sync::{Categorized, Permitted};

use super::MainWindow;
use crate::permission::{Occasion, Permission};
use crate::ui::Mailbox;
use crate::ui::conversation::ConversationView;
use mailrs_domain::translate::{fill, fill_plural, gettext, with_reason};

mod strip;

use strip::CategoryStrip;

fn icon(category: Category) -> &'static str {
    match category {
        Category::All => "penguin-mail-inbox-symbolic",
        Category::Primary => "avatar-default-symbolic",
        Category::Updates => "preferences-system-notifications-symbolic",
        Category::Promotions => "penguin-mail-tag-symbolic",
        Category::Social => "system-users-symbolic",
    }
}

/// Wraps `child` in a revealer that shows or hides it at once. The name
/// used to slide open over 200 ms, and every icon drifted sideways while
/// the strip re-centred after the click, so the switch seemed to trail
/// the pointer. The strip now takes its new shape on the next frame and
/// only the name fades in; see `CategoryStrip::show_names`.
fn slider(child: &impl IsA<gtk::Widget>) -> gtk::Revealer {
    gtk::Revealer::builder()
        // A slide of no length rather than no transition: a revealer with
        // no transition keeps its child's full width while hidden, and
        // only a slide scales the width down to nothing.
        .transition_type(gtk::RevealerTransitionType::SlideLeft)
        .transition_duration(0)
        .child(child)
        .build()
}

/// The switcher above an inbox's thread list. Only the chosen category
/// shows its name, and only while it fits; the others show an icon and
/// their unread count.
pub(super) struct CategoryBar {
    bar: gtk::Box,
    group: adw::ToggleGroup,
    strip: CategoryStrip,
    counts: HashMap<Category, gtk::Label>,
}

impl CategoryBar {
    /// `chosen` is the category the window opens on, from Preferences.
    pub(super) fn new(chosen: Category) -> CategoryBar {
        let group = adw::ToggleGroup::builder()
            .homogeneous(false)
            .css_classes(["category-bar", "round"])
            .build();
        let (mut names, mut counts) = (Vec::new(), HashMap::new());
        for category in Category::ALL {
            // No spacing: the name carries its own margin, so a closed name
            // leaves no gap behind.
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            let image = gtk::Image::from_icon_name(icon(category));
            image.set_pixel_size(16);
            // The icon's box leaves room for the count in its top right corner,
            // so a count arriving or growing never moves the icons.
            image.set_size_request(24, 20);
            let count = gtk::Label::builder()
                .css_classes(["category-count", "no-mail"])
                .halign(gtk::Align::End)
                .valign(gtk::Align::Start)
                .build();
            let badge = gtk::Overlay::builder().child(&image).build();
            badge.add_overlay(&count);
            let name = slider(
                &gtk::Label::builder()
                    .label(category.name())
                    .css_classes(["category-name"])
                    .build(),
            );
            content.append(&badge);
            content.append(&name);
            group.add(
                adw::Toggle::builder()
                    .name(category.key())
                    // The child is an icon, a badge and a name that comes
                    // and goes; the label is what the toggle says out loud.
                    .label(category.name())
                    .tooltip(category.name())
                    .child(&content)
                    .build(),
            );
            names.push(name);
            counts.insert(category, count);
        }
        let bar = gtk::Box::builder()
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(8)
            .margin_end(8)
            .visible(false)
            .build();
        let strip = CategoryStrip::new(&group, names);
        strip.set_hexpand(true);
        bar.append(&strip);
        group.set_active_name(Some(chosen.key()));
        let this = CategoryBar {
            bar,
            group,
            strip,
            counts,
        };
        this.show_names(chosen);
        this
    }

    /// Opens the name of `chosen` and closes the others.
    pub(super) fn show_names(&self, chosen: Category) {
        if let Some(index) = Category::ALL.iter().position(|&c| c == chosen) {
            self.strip.choose(index);
        }
    }

    pub(super) fn set_counts(&self, unread: &HashMap<Category, i64>) {
        for (category, label) in &self.counts {
            let count = unread.get(category).copied().unwrap_or(0);
            // A long number would spill over the icon, so stop the badge at 99.
            label.set_label(&match count {
                ..=0 => String::new(),
                1..=99 => count.to_string(),
                _ => "99+".to_string(),
            });
            // Nothing unread fades the badge out and leaves its place empty.
            if count > 0 {
                label.remove_css_class("no-mail");
            } else {
                label.add_css_class("no-mail");
            }
            // The name is hidden unless the category is chosen, so the tooltip
            // carries both it and the count.
            if let Some(toggle) = self.group.toggle_by_name(category.key()) {
                let said = match count {
                    0 => category.name(),
                    count => fill_plural(
                        "{name}, {count} unread",
                        "{name}, {count} unread",
                        count.max(0) as usize,
                        &[("name", &category.name()), ("count", &count.to_string())],
                    ),
                };
                toggle.set_tooltip(&said);
                toggle.set_label(Some(&said));
            }
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
                win.change_screen(|screen| screen.choose_category(category));
            });
    }

    /// Whether `mailbox` splits into categories on screen: the person has
    /// categories on, and an account the mailbox lists sorts its inbox
    /// that way.
    pub(super) fn shows_categories(&self, mailbox: &Mailbox) -> bool {
        let ids: Vec<AccountId> = self.accounts().iter().map(|a| a.id).collect();
        crate::offered::shows_categories(
            self.settings_with(|s| s.inbox_categories),
            mailbox,
            &ids,
            |id| self.offers(id),
        )
    }

    /// Enables Block Sender and Categorize Sender only while `account_id`
    /// can do them, so the menu offers nothing the account cannot do.
    pub(super) fn follow_sender_actions(&self, account_id: AccountId) {
        for (name, enabled) in crate::offered::sender_actions(self.offers(account_id)) {
            if let Some(action) = self
                .actions
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                action.set_enabled(enabled);
            }
            if name == "categorize-sender" {
                self.conversation.offer_categorize_sender(enabled);
            }
        }
    }

    /// Shows the switcher when the list holds an inbox, and hides it elsewhere.
    pub(super) fn follow_categories(self: &Rc<Self>) {
        let shown = self.shows_categories(&self.shown());
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
        let found = view.find(|open| {
            let sender = open.other_sender()?.clone();
            let who = sender.display().to_string();
            Some((open.account_id, sender.email, who, open.thread_id.clone()))
        });
        let Some((account_id, email, who, open_thread)) = found else {
            return self.toast(&gettext("Open a message from the sender first"));
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
            let actions = this.core.actions();
            let asked = email.clone();
            let categorized = this
                .core
                .call(async move {
                    Ok::<_, std::convert::Infallible>(
                        actions
                            .categorize_sender(account_id, &asked, also.as_deref(), category)
                            .await,
                    )
                })
                .await;
            let Categorized { moved, sorted } = match categorized {
                Ok(categorized) => categorized,
                Err(err) => return this.toast(&err.to_string()),
            };
            if let Some(error) = moved.first_error() {
                this.toast(error);
            }
            // Moving mail between categories can add rows to a Gmail
            // folder, and only a fresh search shows them.
            this.core.forget_remote();
            this.reload_folder();
            let name = category.name();
            let values = [("sender", who.as_str()), ("category", name.as_str())];
            match sorted {
                Ok(Permitted::Done(())) => this.toast(&fill(
                    &gettext("Mail from {sender} now goes to {category}"),
                    &values,
                )),
                Ok(Permitted::NeedsPermission) => {
                    this.toast(&fill(
                        &gettext("Moved mail from {sender} to {category}"),
                        &values,
                    ));
                    this.ask_permission(account_id, Permission::Settings, Occasion::Needed);
                }
                Err(err) => this.toast(&with_reason(
                    &gettext(
                        "Moved mail from {sender} to {category}, but could not sort new \
                         mail: {reason}",
                    ),
                    &err,
                    &values,
                )),
            }
        });
    }
}
