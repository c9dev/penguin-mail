//! Follow Up: sent mail nobody has answered, in a mailbox of its own and
//! in a banner above All Inboxes.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::ThreadSummary;
use mailrs_store::{follow_ups, threads};

use super::{MainWindow, Target};
use crate::ui::Mailbox;

const DAY: i64 = 24 * 60 * 60 * 1000;

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
            .label("Review")
            .valign(gtk::Align::Center)
            .build();
        let close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Hide Until Next Launch")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        let content = gtk::Box::builder()
            .spacing(10)
            .css_classes(["follow-up-banner"])
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
                win.dismiss_follow_ups(win.targets());
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
        banner.title.set_label(&if count == 1 {
            "1 sent message has had no reply".to_string()
        } else {
            format!("{count} sent messages have had no reply")
        });
        banner.revealer.set_reveal_child(
            mailbox == Mailbox::Unified("INBOX")
                && count > 0
                && !banner.closed.get()
                && self.settings().suggest_follow_ups,
        );
        if let Some(action) = self
            .actions
            .lookup_action("dismiss-follow-up")
            .and_downcast::<gio::SimpleAction>()
        {
            action.set_enabled(mailbox == Mailbox::FollowUp);
        }
        if mailbox == Mailbox::FollowUp {
            self.conversation
                .set_trash_tooltip("Dismiss Follow-Up (Delete)");
        }
    }

    /// Records how many conversations wait on a reply, from the sidebar counts.
    pub(super) fn set_follow_up_count(&self, count: usize) {
        self.follow_up.waiting.set(count);
        self.follow_follow_ups();
    }

    /// Lists conversations waiting on a reply, newest first.
    pub(super) fn load_follow_ups(self: &Rc<Self>, generation: u64) {
        let now = chrono::Utc::now().timestamp_millis();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let loaded = this
                .core
                .read(move |c| {
                    let mut rows = Vec::new();
                    for item in follow_ups::waiting(c, now)? {
                        let stored = threads::get_thread(c, item.account_id, &item.thread_id)?;
                        rows.push((item, stored));
                    }
                    Ok(rows)
                })
                .await;
            if this.list_generation.get() != generation {
                return;
            }
            let Ok(loaded) = loaded else {
                return this.toast("Could not load follow-ups");
            };
            let rows: Vec<ThreadSummary> = loaded
                .into_iter()
                .map(|(item, stored)| {
                    let mut row = stored.unwrap_or_else(|| ThreadSummary {
                        account_id: item.account_id,
                        id: item.thread_id.clone(),
                        subject: item.subject.clone(),
                        message_count: 1,
                        ..ThreadSummary::default()
                    });
                    let names: Vec<&str> = item.to.iter().map(|a| a.display()).collect();
                    row.from = if names.is_empty() {
                        "No recipients".into()
                    } else {
                        format!("To {}", names.join(", "))
                    };
                    row.snippet = waited(now - item.sent_at);
                    row.last_message_at = item.sent_at;
                    row
                })
                .collect();
            let subtitle = match rows.len() {
                0 => String::new(),
                1 => "1 conversation".into(),
                n => format!("{n} conversations"),
            };
            this.list
                .set_rows(rows, "No Follow-Ups", "mail-reply-sender-symbolic");
            this.follow_selection();
            this.list.set_title("Follow Up", &subtitle);
        });
    }

    /// Stops suggesting the targets. The mail itself stays where it is.
    pub(super) fn dismiss_follow_ups(self: &Rc<Self>, targets: Vec<Target>) {
        if targets.is_empty() {
            return;
        }
        let next = self.list.neighbour_of_selected();
        self.conversation.clear();
        self.list.unselect();
        self.list.retain(|row| {
            !targets
                .iter()
                .any(|t| t.account_id == row.account_id && t.thread_id == row.id)
        });
        match next {
            Some(next) => self
                .list
                .select(next.account_id, &next.id, next.message_id.as_deref()),
            None => self.nav.set_show_content(false),
        }
        let now = chrono::Utc::now().timestamp_millis();
        let (gone, count) = (targets.clone(), targets.len());
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let saved = this
                .core
                .write(move |c| {
                    for target in &gone {
                        follow_ups::dismiss(c, target.account_id, &target.thread_id, now)?;
                    }
                    Ok(())
                })
                .await;
            if let Err(err) = saved {
                return this.toast(&format!("Could not dismiss: {err}"));
            }
            this.follow_ups_changed();
            let toast = adw::Toast::builder()
                .title(if count == 1 {
                    "Dismissed follow-up".to_string()
                } else {
                    format!("Dismissed {count} follow-ups")
                })
                .button_label("Undo")
                .timeout(5)
                .build();
            let weak = Rc::downgrade(&this);
            toast.connect_button_clicked(move |_| {
                if let Some(win) = weak.upgrade() {
                    win.restore_follow_ups(targets.clone());
                }
            });
            this.toasts.add_toast(toast);
        });
    }

    fn restore_follow_ups(self: &Rc<Self>, targets: Vec<Target>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let restored = this
                .core
                .write(move |c| {
                    for target in &targets {
                        follow_ups::restore(c, target.account_id, &target.thread_id)?;
                    }
                    Ok(())
                })
                .await;
            match restored {
                Ok(()) => this.follow_ups_changed(),
                Err(err) => this.toast(&format!("Could not undo: {err}")),
            }
        });
    }

    /// Refreshes counts, and the list when it shows Follow Up.
    fn follow_ups_changed(self: &Rc<Self>) {
        self.refresh_counts();
        if *self.mailbox.borrow() == Mailbox::FollowUp {
            self.reload_list();
        }
    }
}

/// "Sent 5 days ago, no reply yet".
fn waited(elapsed: i64) -> String {
    match elapsed / DAY {
        1 => "Sent yesterday, no reply yet".into(),
        days => format!("Sent {days} days ago, no reply yet"),
    }
}

#[cfg(test)]
mod tests {
    use super::{DAY, waited};

    #[test]
    fn the_wait_reads_in_whole_days() {
        assert_eq!(waited(5 * DAY + 3), "Sent 5 days ago, no reply yet");
        assert_eq!(waited(DAY), "Sent yesterday, no reply yet");
    }
}
