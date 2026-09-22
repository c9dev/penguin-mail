//! Follow Up: sent mail nobody has answered, in a mailbox of its own and
//! in a banner above All Inboxes.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use mailrs_domain::system_label;
use mailrs_sync::{History, MailAction};

use super::{MainWindow, Target};
use crate::ui::Mailbox;
use crate::ui::conversation::ConversationView;
use mailrs_domain::translate::{fill_plural, gettext};

/// "2 sent messages have had no reply" with Review and close buttons.
/// Closing it hides it until the app quits.
pub(super) struct FollowUpBanner {
    revealer: gtk::Revealer,
    title: gtk::Label,
    review: gtk::Button,
    close: gtk::Button,
    waiting: Cell<usize>,
    closed: Cell<bool>,
}

impl FollowUpBanner {
    pub(super) fn new() -> FollowUpBanner {
        let title = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .build();
        let review = gtk::Button::builder()
            .label(gettext("Review"))
            .valign(gtk::Align::Center)
            .build();
        let close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(gettext("Hide Until Next Launch"))
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        crate::ui::name(&close, &gettext("Hide Until Next Launch"));
        let content = gtk::Box::builder()
            .spacing(10)
            .css_classes(["follow-up-banner"])
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        content.append(&gtk::Image::from_icon_name("mail-reply-sender-symbolic"));
        content.append(&title);
        content.append(&review);
        content.append(&close);
        let revealer = gtk::Revealer::builder()
            .child(&content)
            .reveal_child(false)
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .build();
        FollowUpBanner {
            revealer,
            title,
            review,
            close,
            waiting: Cell::new(0),
            closed: Cell::new(false),
        }
    }
}

impl MainWindow {
    /// Adds the banner above the thread list and the Dismiss Follow-Up action.
    pub(super) fn install_follow_ups(self: &Rc<Self>) {
        if let Some(toolbar) = self.list.page.child().and_downcast::<adw::ToolbarView>() {
            toolbar.add_top_bar(&self.follow_up.revealer);
        }
        let weak = Rc::downgrade(self);
        self.follow_up.review.connect_clicked(move |_| {
            if let Some(win) = weak.upgrade() {
                win.sidebar.select(&Mailbox::FollowUp);
                win.show_mailbox(Mailbox::FollowUp);
            }
        });
        let weak = Rc::downgrade(self);
        self.follow_up.close.connect_clicked(move |_| {
            if let Some(win) = weak.upgrade() {
                win.follow_up.closed.set(true);
                win.follow_follow_ups();
            }
        });
        let dismiss = gio::SimpleAction::new("dismiss-follow-up", None);
        dismiss.set_enabled(false);
        let weak = Rc::downgrade(self);
        dismiss.connect_activate(move |_, _| {
            if let Some(win) = weak.upgrade() {
                let view = Rc::clone(&win.conversation);
                win.dismiss_follow_ups(&view, win.reach(&view).targets);
            }
        });
        self.actions.add_action(&dismiss);
    }

    /// Shows the banner over All Inboxes while replies are overdue, and
    /// offers Dismiss Follow-Up only inside the Follow Up mailbox.
    pub(super) fn follow_follow_ups(&self) {
        let mailbox = self.mailbox.borrow().clone();
        let banner = &self.follow_up;
        let count = banner.waiting.get();
        banner.title.set_label(&fill_plural(
            "{count} sent message has had no reply",
            "{count} sent messages have had no reply",
            count,
            &[("count", &count.to_string())],
        ));
        banner.revealer.set_reveal_child(
            mailbox == Mailbox::Unified(system_label::INBOX)
                && count > 0
                && !banner.closed.get()
                && self.settings_with(|s| s.suggest_follow_ups),
        );
        if let Some(action) = self
            .actions
            .lookup_action("dismiss-follow-up")
            .and_downcast::<gio::SimpleAction>()
        {
            action.set_enabled(mailbox == Mailbox::FollowUp);
        }
        if mailbox == Mailbox::FollowUp {
            self.conversation.set_trash_words(
                &gettext("Dismiss Follow-Up"),
                &gettext("Dismiss Follow-Up (Delete)"),
            );
        }
    }

    /// Records how many conversations wait on a reply, from the sidebar counts.
    pub(super) fn set_follow_up_count(&self, count: usize) {
        self.follow_up.waiting.set(count);
        self.follow_follow_ups();
    }

    /// Stops suggesting the targets, which `view` reached. The mail itself
    /// stays where it is.
    pub(super) fn dismiss_follow_ups(
        self: &Rc<Self>,
        view: &ConversationView,
        targets: Vec<Target>,
    ) {
        if targets.is_empty() {
            return;
        }
        let action = MailAction::DismissFollowUp;
        self.follow_out(view, &action);
        self.perform(targets, action, History::Record, None);
    }

    /// Refreshes counts, and the list when it shows Follow Up.
    pub(super) fn follow_ups_changed(self: &Rc<Self>) {
        self.refresh_counts();
        if *self.mailbox.borrow() == Mailbox::FollowUp {
            self.reload_list();
        }
    }
}
