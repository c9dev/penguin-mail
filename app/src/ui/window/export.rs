//! Saving mail out of the app, as the mbox file other mail programs read.
//! One conversation, the whole selection, or the one message a row stands
//! for when the list shows messages rather than conversations.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::Target;
use mailrs_sync::export;

use super::MainWindow;
use crate::ui::conversation::ConversationView;
use mailrs_domain::translate::{fill, fill_plural, gettext};

impl MainWindow {
    /// Writes the selected conversations to one mbox file, or the one
    /// `view` shows when nothing is selected. A separate window has no list
    /// of its own, so it writes its conversation.
    pub(super) fn export(self: &Rc<Self>, view: &ConversationView) {
        let rows = match view.detached() {
            true => Vec::new(),
            false => self.list.selected_rows(),
        };
        if rows.is_empty() {
            return self.export_conversation(view);
        }
        let newest = rows
            .iter()
            .map(|row| row.last_message_at)
            .max()
            .unwrap_or(0);
        let name = match rows.len() {
            1 => export::file_name(&rows[0].subject, rows[0].last_message_at, "mbox"),
            count => {
                let many = fill_plural(
                    "{count} conversation",
                    "{count} conversations",
                    count,
                    &[("count", &count.to_string())],
                );
                export::file_name(&many, newest, "mbox")
            }
        };
        let wanted = rows
            .iter()
            .map(|row| Target {
                account_id: row.account_id,
                thread_id: row.id.clone(),
                message_id: row.message_id.clone(),
            })
            .collect();
        self.save_mbox(wanted, name);
    }

    /// Writes one message of `view` as the `.eml` file other mail
    /// programs read: the bytes Gmail holds, headers and all.
    pub(super) fn export_message(self: &Rc<Self>, view: &ConversationView, message_id: &str) {
        let found = view.find(|open| {
            let meta = open.messages.iter().find(|m| m.id == message_id)?;
            let subject = match meta.subject.trim().is_empty() {
                true => open.subject.clone(),
                false => meta.subject.clone(),
            };
            Some((
                open.account_id,
                meta.id.clone(),
                export::file_name(&subject, meta.date, "eml"),
            ))
        });
        let Some((account_id, message_id, name)) = found else {
            return self.toast(&gettext("Open a conversation first"));
        };
        if self.core.account(account_id).is_none() {
            return self.toast(&gettext("That account is not connected"));
        }
        let actions = self.core.actions();
        let dialog = gtk::FileDialog::builder()
            .title(gettext("Export Mail"))
            .initial_name(&name)
            .build();
        // The dialog belongs over the window it was asked from, which for
        // a conversation in a window of its own is not the main one.
        let parent = view
            .window()
            .unwrap_or_else(|| self.window.clone().upcast());
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let Ok(file) = dialog.save_future(Some(&parent)).await else {
                return;
            };
            let Some(path) = file.path() else { return };
            this.toast(&gettext("Exporting…"));
            let written = this
                .core
                .call(async move {
                    let raw = actions.export_message(account_id, &message_id).await?;
                    tokio::task::spawn_blocking(move || std::fs::write(&path, &raw)).await??;
                    Ok::<(), anyhow::Error>(())
                })
                .await;
            match written {
                Ok(()) => this.toast(&fill(&gettext("Saved {file}"), &[("file", &name)])),
                Err(err) => this.failed(&gettext("Could not export the mail: {reason}"), &err),
            }
        });
    }

    /// Writes the conversation `view` shows.
    fn export_conversation(self: &Rc<Self>, view: &ConversationView) {
        let open = view.read(|open| {
            let date = open.messages.last().map(|m| m.date).unwrap_or_default();
            (
                Target {
                    account_id: open.account_id,
                    thread_id: open.thread_id.clone(),
                    message_id: open.only_message.clone(),
                },
                export::file_name(&open.subject, date, "mbox"),
            )
        });
        let Some((wanted, name)) = open else {
            return self.toast(&gettext("Open a conversation first"));
        };
        self.save_mbox(vec![wanted], name);
    }

    /// Asks where the file goes, then writes every conversation in
    /// `wanted` into it one after another, through
    /// `MailActions::export_mbox`. The accounts are looked up first, so a
    /// conversation from an account that is not connected stops the
    /// export before a dialog opens.
    fn save_mbox(self: &Rc<Self>, wanted: Vec<Target>, name: String) {
        if wanted
            .iter()
            .any(|target| self.core.account(target.account_id).is_none())
        {
            return self.toast(&gettext("That account is not connected"));
        }
        let actions = self.core.actions();
        let dialog = gtk::FileDialog::builder()
            .title(gettext("Export Mail"))
            .initial_name(&name)
            .build();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let Ok(file) = dialog.save_future(Some(&this.window)).await else {
                return;
            };
            let Some(path) = file.path() else { return };
            this.toast(&gettext("Exporting…"));
            let written = this
                .core
                .call(async move {
                    let mbox = actions.export_mbox(&wanted).await?;
                    tokio::task::spawn_blocking(move || std::fs::write(&path, &mbox)).await??;
                    Ok::<(), anyhow::Error>(())
                })
                .await;
            match written {
                Ok(()) => this.toast(&fill(&gettext("Saved {file}"), &[("file", &name)])),
                Err(err) => this.failed(&gettext("Could not export the mail: {reason}"), &err),
            }
        });
    }
}
