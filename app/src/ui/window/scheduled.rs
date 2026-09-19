//! The Send Later mailbox and the window's part in Undo Send.

use std::rc::Rc;

use gtk::glib;
use mailrs_domain::ThreadSummary;
use mailrs_store::scheduled::{self, Scheduled};

use super::{MainWindow, Target};
use crate::format::future_date;
use crate::ui::Mailbox;

impl MainWindow {
    /// Shows "Sending…" with an Undo button for `seconds`.
    pub fn offer_undo_send(&self, seconds: u32, on_undo: impl Fn() + 'static) {
        let toast = adw::Toast::builder()
            .title("Sending…")
            .button_label("Undo")
            .timeout(seconds)
            .priority(adw::ToastPriority::High)
            .build();
        toast.connect_button_clicked(move |_| on_undo());
        self.toasts.add_toast(toast);
    }

    pub fn toast_text(&self, text: &str) {
        self.toast(text);
    }

    /// Refreshes counts, and the list when it shows Send Later.
    pub fn scheduled_changed(self: &Rc<Self>) {
        self.refresh_counts();
        if *self.mailbox.borrow() == Mailbox::Scheduled {
            self.reload_list();
        }
    }

    /// Lists scheduled messages, soonest first.
    pub(super) fn load_scheduled(self: &Rc<Self>, generation: u64) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let loaded = this.core.read(scheduled::list).await;
            if this.list_generation.get() != generation {
                return;
            }
            match loaded {
                Ok(items) => {
                    let now = chrono::Local::now();
                    let rows: Vec<ThreadSummary> =
                        items.iter().map(|item| row(item, now)).collect();
                    let count = rows.len();
                    this.list
                        .set_rows(rows, "Nothing Scheduled", "alarm-symbolic");
                    this.follow_selection();
                    let subtitle = match count {
                        0 => String::new(),
                        1 => "1 message".into(),
                        n => format!("{n} messages"),
                    };
                    this.list.set_title("Send Later", &subtitle);
                }
                Err(err) => this.toast(&format!("Could not load scheduled mail: {err}")),
            }
        });
    }

    /// Stops scheduled sends. The drafts stay in Gmail's Drafts.
    pub(super) fn cancel_scheduled(self: &Rc<Self>, targets: Vec<Target>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let count = targets.len();
            let removed = this
                .core
                .write(move |c| {
                    for item in scheduled::list(c)? {
                        let hit = targets.iter().any(|t| {
                            t.account_id == item.account_id
                                && (t.message_id.as_deref() == Some(item.message_id.as_str())
                                    || t.thread_id == item.thread_id)
                        });
                        if hit {
                            scheduled::remove(c, item.account_id, &item.draft_id)?;
                        }
                    }
                    Ok(())
                })
                .await;
            match removed {
                Ok(()) => {
                    this.conversation.clear();
                    this.scheduled_changed();
                    this.toast(if count == 1 {
                        "Won't be sent. The message is in Drafts."
                    } else {
                        "Won't be sent. The messages are in Drafts."
                    });
                }
                Err(err) => this.toast(&format!("Could not cancel: {err}")),
            }
        });
    }
}

fn row(item: &Scheduled, now: chrono::DateTime<chrono::Local>) -> ThreadSummary {
    ThreadSummary {
        account_id: item.account_id,
        id: item.thread_id.clone(),
        message_id: Some(item.message_id.clone()),
        last_message_at: item.send_at,
        subject: item.subject.clone(),
        snippet: format!("Sends {}", future_date(item.send_at, now)),
        from: if item.recipients.is_empty() {
            "No recipients".into()
        } else {
            format!("To {}", item.recipients)
        },
        message_count: 1,
        unread: false,
        starred: false,
        has_attachments: false,
        flag_color: None,
    }
}
