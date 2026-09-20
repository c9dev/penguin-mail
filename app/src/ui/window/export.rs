//! Saving mail out of the app, as the mbox file other mail programs read.
//! One conversation, the whole selection, or the one message a row stands
//! for when the list shows messages rather than conversations.

use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::AccountId;
use mailrs_sync::export;

use super::MainWindow;
use crate::core::Sync;
use crate::ui::conversation::ConversationView;

/// What one entry of the file is built from: the account the mail lives
/// in, its thread, and the single message to take when a row names one.
type Wanted = (AccountId, String, Option<String>);

impl MainWindow {
    /// Writes the selected conversations to one mbox file, or the open
    /// conversation when nothing is selected.
    pub(super) fn export(self: &Rc<Self>) {
        let rows = self.list.selected_rows();
        if rows.is_empty() {
            let view = Rc::clone(&self.conversation);
            return self.export_conversation(&view);
        }
        let newest = rows
            .iter()
            .map(|row| row.last_message_at)
            .max()
            .unwrap_or(0);
        let name = match rows.len() {
            1 => export::file_name(&rows[0].subject, rows[0].last_message_at, "mbox"),
            count => export::file_name(&format!("{count} conversations"), newest, "mbox"),
        };
        let wanted = rows
            .iter()
            .map(|row| (row.account_id, row.id.clone(), row.message_id.clone()))
            .collect();
        self.save_mbox(wanted, name);
    }

    /// Writes the conversation `view` shows. A separate window has no list
    /// of its own, so its menu comes here.
    pub(super) fn export_conversation(self: &Rc<Self>, view: &ConversationView) {
        let open = view.with_open(|open| {
            let date = open.messages.last().map(|m| m.date).unwrap_or_default();
            (
                (
                    open.account_id,
                    open.thread_id.clone(),
                    open.only_message.clone(),
                ),
                export::file_name(&open.subject, date, "mbox"),
            )
        });
        let Some((wanted, name)) = open else {
            return self.toast("Open a conversation first");
        };
        self.save_mbox(vec![wanted], name);
    }

    /// Asks where the file goes, then fetches every conversation in
    /// `wanted` and writes them into it one after another. The accounts
    /// are looked up first, so a conversation from an account that is not
    /// connected stops the export before a dialog opens.
    fn save_mbox(self: &Rc<Self>, wanted: Vec<Wanted>, name: String) {
        let mut jobs: Vec<(Arc<Sync>, String, Option<String>)> = Vec::new();
        for (account_id, thread_id, message_id) in wanted {
            let Some(sync) = self.core.account(account_id) else {
                return self.toast("That account is not connected");
            };
            jobs.push((sync, thread_id, message_id));
        }
        let dialog = gtk::FileDialog::builder()
            .title("Export Mail")
            .initial_name(&name)
            .build();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let Ok(file) = dialog.save_future(Some(&this.window)).await else {
                return;
            };
            let Some(path) = file.path() else { return };
            this.toast("Exporting…");
            let written = this
                .core
                .call(async move {
                    let mut mbox = Vec::new();
                    for (sync, thread_id, message_id) in jobs {
                        mbox.extend(sync.export_mbox(&thread_id, message_id.as_deref()).await?);
                    }
                    tokio::task::spawn_blocking(move || std::fs::write(&path, &mbox)).await??;
                    Ok::<(), anyhow::Error>(())
                })
                .await;
            match written {
                Ok(()) => this.toast(&format!("Saved {name}")),
                Err(err) => this.toast(&format!("Could not export the mail: {err}")),
            }
        });
    }
}
