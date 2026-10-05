//! The category switcher above an inbox and the Categorize Sender action.
//! `mailrs_domain::Category` holds which Gmail labels each category means.

use std::collections::HashMap;
use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::{AccountId, Category};
use mailrs_sync::{Categorized, Permitted};

use super::MainWindow;
use crate::offered::InboxBar;
use crate::permission::{Occasion, Permission};
use crate::ui::Mailbox;
use crate::ui::conversation::ConversationView;
use mailrs_domain::translate::{fill, gettext, with_reason};

mod chip;
mod strip;

use strip::CategoryStrip;

fn icon(category: Category) -> &'static str {
    match category {
        Category::All => "penguin-mail-inbox-symbolic",
        Category::Primary => "avatar-default-symbolic",
        Category::Updates => "preferences-system-notifications-symbolic",
        Category::Promotions => "penguin-mail-tag-symbolic",
        Category::Social => "system-users-symbolic",
        Category::Focused => "starred-symbolic",
        Category::Other => "mail-archive-symbolic",
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

/// The switcher above an inbox's thread list. Each chip shows its icon,
/// its name and its unread count, wrapping onto a second line when one
/// does not hold them. Past two lines, and in the phone layout, only the
/// chosen chip keeps its name. Focused and Other are worded tabs with no
/// icon.
pub(super) struct CategoryBar {
    bar: gtk::Box,
    strip: CategoryStrip,
    /// One toggle per category, grouped so one is on at a time.
    chips: Vec<gtk::ToggleButton>,
    /// The categories the bar holds, in order: Gmail's five, or Focused
    /// and Other.
    set: &'static [Category],
    counts: HashMap<Category, Badges>,
    /// Set while the window turns a chip on, so the chip does not ask the
    /// window for the category it is already showing.
    quiet: Rc<Cell<bool>>,
}

/// A chip's unread count, drawn in two places: after its name while the
/// name shows, and on the pill's corner while only the icon does.
struct Badges {
    inline: gtk::Label,
    corner: Option<gtk::Label>,
}

/// A badge, faded out until there is mail to count.
fn badge_label(place: &str, valign: gtk::Align) -> gtk::Label {
    gtk::Label::builder()
        .css_classes(["category-count", place, "no-mail"])
        .halign(gtk::Align::End)
        .valign(valign)
        .build()
}

impl CategoryBar {
    /// `chosen` is the category the window opens on, from Preferences, and
    /// `set` the categories the bar holds.
    pub(super) fn new(chosen: Category, set: &'static [Category]) -> CategoryBar {
        // Focused and Other are worded alone, as Outlook words them;
        // Gmail's categories keep an icon each.
        let worded = set.len() == Category::FOCUS.len();
        let (mut chips, mut names, mut corners, mut counts) =
            (Vec::new(), Vec::new(), Vec::new(), HashMap::new());
        for &category in set {
            let content = gtk::Box::builder()
                .css_classes(["category-chip"])
                // An icon alone is narrower than the pill's 44 px least width,
                // and sits in its middle.
                .halign(gtk::Align::Center)
                .build();
            if worded {
                content.add_css_class("category-tab");
            } else {
                let image = gtk::Image::from_icon_name(icon(category));
                image.set_pixel_size(16);
                content.append(&image);
            }
            // The name and its count open and close together, so the
            // count stays beside the name it belongs to.
            let named = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            named.append(
                &gtk::Label::builder()
                    .label(category.name())
                    .css_classes(["category-name"])
                    .build(),
            );
            let inline = badge_label(
                "inline",
                if worded { gtk::Align::Start } else { gtk::Align::Center },
            );
            named.append(&inline);
            let name = slider(&named);
            content.append(&name);
            let face = gtk::Overlay::builder().child(&content).build();
            // An icon alone carries its badge over the pill's end padding,
            // clear of the 16 px icon, so a count arriving never moves the
            // icons. A worded tab always shows its name and needs none.
            let corner = (!worded).then(|| {
                let corner = badge_label("corner", gtk::Align::Start);
                face.add_overlay(&corner);
                corner
            });
            // The child is an icon, a name that comes and goes and a
            // badge; the spoken name is the category and its count.
            let chip = gtk::ToggleButton::builder()
                .child(&face)
                .tooltip_text(category.name())
                .css_classes(["category-toggle"])
                .build();
            crate::ui::name(&chip, &category.name());
            if let Some(first) = chips.first() {
                chip.set_group(Some(first));
            }
            chips.push(chip);
            names.push(name);
            corners.push(corner.clone());
            counts.insert(category, Badges { inline, corner });
        }
        let bar = gtk::Box::builder()
            .margin_top(14)
            .margin_bottom(6)
            .margin_start(16)
            .margin_end(8)
            .visible(false)
            .build();
        let strip = CategoryStrip::new(chips.clone(), names, corners, worded);
        strip.set_hexpand(true);
        bar.append(&strip);
        let this = CategoryBar {
            bar,
            strip,
            chips,
            set,
            counts,
            quiet: Rc::new(Cell::new(false)),
        };
        this.show_names(chosen);
        this
    }

    /// Runs `chosen` with the category of a chip the person turns on.
    fn connect_chosen(&self, chosen: impl Fn(Category) + 'static) {
        let chosen = Rc::new(chosen);
        for (chip, &category) in self.chips.iter().zip(self.set) {
            let (chosen, quiet) = (Rc::clone(&chosen), Rc::clone(&self.quiet));
            chip.connect_toggled(move |chip| {
                if chip.is_active() && !quiet.get() {
                    chosen(category);
                }
            });
        }
    }

    /// Turns on `chosen`'s chip, the one that keeps its name when not
    /// every name fits.
    pub(super) fn show_names(&self, chosen: Category) {
        let Some(index) = self.set.iter().position(|&c| c == chosen) else {
            return;
        };
        self.quiet.set(true);
        self.chips[index].set_active(true);
        self.quiet.set(false);
        self.strip.choose(index);
    }

    /// Keeps the chips to the chosen name and icons while the window shows
    /// the list alone, as on a phone.
    pub(super) fn set_phone(&self, phone: bool) {
        self.strip.set_phone(phone);
    }

    pub(super) fn set_counts(&self, unread: &HashMap<Category, i64>) {
        let worded = self.set.len() == Category::FOCUS.len();
        for (chip, category) in self.chips.iter().zip(self.set) {
            let Some(badges) = self.counts.get(category) else {
                continue;
            };
            let count = unread.get(category).copied().unwrap_or(0);
            let badge = chip::badge(count);
            for label in std::iter::once(&badges.inline).chain(badges.corner.as_ref()) {
                label.set_label(&badge);
                // Nothing unread fades the badge out and leaves its place
                // empty.
                if count > 0 {
                    label.remove_css_class("no-mail");
                } else {
                    label.add_css_class("no-mail");
                }
            }
            // A Gmail chip with nothing unread ends at its name. A worded
            // tab keeps the badge's place, so it never changes width.
            badges.inline.set_visible(count > 0 || worded);
            // The name may be folded away, so the tooltip carries both it
            // and the count.
            let said = chip::spoken(*category, count);
            chip.set_tooltip_text(Some(&said));
            crate::ui::name(chip, &said);
        }
    }
}

impl MainWindow {
    /// Puts the category switcher above the thread list and adds the
    /// Categorize Sender action.
    pub(super) fn install_categories(self: &Rc<Self>) {
        // The chips sit right under the header, in the slot `ThreadList`
        // reserves before its banners, so a sign-in or Grant Access banner
        // never lands between the header and the chips.
        for bar in [&self.categories, &self.focus] {
            self.list.categories_slot.append(&bar.bar);
            let weak = Rc::downgrade(self);
            bar.connect_chosen(move |category| {
                let Some(win) = weak.upgrade() else { return };
                win.change_screen(|screen| screen.choose_category(category));
            });
            bar.set_phone(self.nav.is_collapsed());
        }
        let weak = Rc::downgrade(self);
        self.nav.connect_collapsed_notify(move |nav| {
            let Some(win) = weak.upgrade() else { return };
            for bar in [&win.categories, &win.focus] {
                bar.set_phone(nav.is_collapsed());
            }
        });
    }

    /// The switcher `mailbox` shows on screen, if any: the person has
    /// categories on, and an account the mailbox lists sorts its inbox
    /// into Gmail's categories or into Focused and Other.
    pub(super) fn inbox_bar(&self, mailbox: &Mailbox) -> Option<InboxBar> {
        let ids: Vec<AccountId> = self.accounts().iter().map(|a| a.id).collect();
        crate::offered::inbox_bar(
            self.settings_with(|s| s.inbox_categories),
            mailbox,
            &ids,
            |id| self.offers(id),
        )
    }

    /// Whether `mailbox` splits into categories or Focused and Other on
    /// screen, which is when a listing takes a slice.
    pub(super) fn shows_categories(&self, mailbox: &Mailbox) -> bool {
        self.inbox_bar(mailbox).is_some()
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
        self.conversation.set_categorize_choices(&crate::offered::categorize_choices(
            self.offers(account_id),
        ));
    }

    /// Shows Gmail's categories or Focused and Other over the list, as
    /// `offered::inbox_bar` rules, and moves the slice on screen to one the
    /// shown bar has.
    pub(super) fn follow_categories(self: &Rc<Self>) {
        let bar = self.inbox_bar(&self.shown());
        self.categories
            .bar
            .set_visible(bar == Some(InboxBar::Categories));
        self.focus.bar.set_visible(bar == Some(InboxBar::Focus));
        let Some(bar) = bar else { return };
        let current = self.screen.borrow().category();
        let default = self.settings_with(|s| s.default_category);
        let next = crate::offered::category_after(bar, current, default);
        if next != current {
            self.change_screen(|screen| screen.choose_category(next));
        }
        match bar {
            InboxBar::Categories => self.categories.show_names(next),
            InboxBar::Focus => self.focus.show_names(next),
        }
        self.refresh_counts();
    }

    /// Shows how much unread mail each category holds.
    pub(super) fn set_category_counts(&self, unread: &HashMap<Category, i64>) {
        self.categories.set_counts(unread);
        self.focus.set_counts(unread);
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
