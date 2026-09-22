//! The window behind the assistant's tools. `crate::assistant::run` decides
//! what each tool does; this file is the adapter that gives it the window's
//! state and carries out the effects it asks for, on the GTK thread.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use mailrs_ai::ToolOutcome;
use mailrs_domain::translate::gettext;
use mailrs_domain::{Account, AccountId, Category, EpochMillis, Label, ThreadSummary};
use mailrs_sync::{MailAction, Outcome, Permitted, View};
use serde_json::Value;

use super::MainWindow;
use super::aftermath::Cause;
use crate::app::Signature;
use crate::assistant::run::{
    Answer, Background, Desk, Effects, OnScreen, OpenConversation, Permission, Tools,
};
use crate::compose::{Draft, SendWhen};
use crate::core::RunningEngine;
use crate::hide_my_email::HiddenAddress;
use crate::permission::Occasion;
use crate::settings::{Change, Settings};
use crate::unsubscribe::Unsubscribe;

impl MainWindow {
    /// Runs one tool call from the assistant.
    pub(super) async fn run_tool(self: &Rc<Self>, name: &str, input: Value) -> ToolOutcome {
        self.tools().run(name, input).await
    }

    /// The tools, with this window behind both ports.
    fn tools(self: &Rc<Self>) -> Tools<RunningEngine> {
        let ports = Rc::new(Ports(Rc::clone(self)));
        Tools::new(
            self.core.modules(),
            Rc::clone(&self.core) as Rc<dyn Background>,
            Rc::clone(&ports) as Rc<dyn Desk>,
            ports as Rc<dyn Effects>,
        )
    }
}

/// The window as the tools see it.
struct Ports(Rc<MainWindow>);

impl Desk for Ports {
    fn settings(&self) -> Settings {
        self.0.settings()
    }

    fn accounts(&self) -> Vec<Account> {
        self.0.accounts()
    }

    fn labels(&self) -> HashMap<AccountId, Vec<Label>> {
        self.0.labels()
    }

    fn view(&self) -> View {
        self.0.view()
    }

    fn on_screen(&self) -> OnScreen {
        OnScreen {
            mailbox: self.0.mailbox.borrow().title(),
            open: self.0.conversation.read(|o| OpenConversation {
                account_id: o.account_id,
                thread_id: o.thread_id.clone(),
                message_id: o.only_message.clone(),
                subject: o.subject.clone(),
            }),
            selected: self.0.list.selected_rows(),
        }
    }

    fn default_account(&self) -> Option<AccountId> {
        let app = self.0.app.upgrade()?;
        app.default_account(self.0.account_in_view())
    }

    fn downloads(&self) -> PathBuf {
        gtk::glib::user_special_dir(gtk::glib::UserDirectory::Downloads)
            .unwrap_or_else(gtk::glib::home_dir)
    }
}

/// What a tool call says when the window has already gone.
fn closing() -> String {
    gettext("The app is closing.")
}

impl Effects for Ports {
    fn confirm(&self, question: String) -> Answer<'_, bool> {
        Box::pin(async move { self.0.assistant.confirm(&question).await })
    }

    fn ask_permission(&self, account_id: AccountId, permission: Permission) {
        self.0
            .ask_permission(account_id, permission, Occasion::Needed);
    }

    fn explain_api_off(&self, service: &str, enable_url: &str) {
        self.0.explain_api_off(service, enable_url);
    }

    fn send_later(&self, draft: Draft, at: EpochMillis) -> Result<(), String> {
        let app = self.0.app.upgrade().ok_or_else(closing)?;
        let signature = match draft.draft_id {
            Some(_) => Signature::AsWritten,
            None => Signature::Add,
        };
        app.send(draft, SendWhen::At(at), signature);
        Ok(())
    }

    fn unsubscribe(
        &self,
        account_id: AccountId,
        how: Unsubscribe,
    ) -> Answer<'_, Result<(), String>> {
        Box::pin(async move { self.0.leave_list(account_id, how).await })
    }

    fn change_settings(&self, change: Change) -> Result<(), String> {
        let app = self.0.app.upgrade().ok_or_else(closing)?;
        app.change_settings(change);
        Ok(())
    }

    fn new_draft(&self, account_id: AccountId) -> Result<Draft, String> {
        let app = self.0.app.upgrade().ok_or_else(closing)?;
        Ok(app.blank_draft(account_id))
    }

    fn compose(&self, draft: Draft) -> Result<(), String> {
        let app = self.0.app.upgrade().ok_or_else(closing)?;
        app.open_composer(draft, Signature::Add);
        Ok(())
    }

    fn send(&self, draft: Draft) -> Result<(), String> {
        let app = self.0.app.upgrade().ok_or_else(closing)?;
        app.send(draft, SendWhen::Now, Signature::Add);
        Ok(())
    }

    fn show_thread(&self, summary: ThreadSummary) {
        self.0.open_thread(summary);
    }

    fn copy(&self, text: &str) {
        gtk::prelude::WidgetExt::clipboard(&self.0.window).set_text(text);
    }

    fn mail_changed(&self, action: &MailAction, outcome: &Outcome) {
        self.0.mail_changed(action, outcome);
    }

    fn relist(&self) {
        self.0.refresh_counts();
        self.0.reload_list();
    }

    fn categorize_sender(
        &self,
        account_id: AccountId,
        email: String,
        who: String,
        category: Category,
    ) {
        self.0
            .categorize_sender(account_id, email, who, None, category);
    }

    fn hide_address(
        &self,
        account_id: AccountId,
        note: String,
    ) -> Answer<'_, Result<Permitted<HiddenAddress>, String>> {
        Box::pin(async move {
            let app = self.0.app.upgrade().ok_or_else(closing)?;
            app.create_hidden_address(account_id, &note)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn set_address_active(
        &self,
        address: String,
        active: bool,
    ) -> Answer<'_, Result<Permitted<()>, String>> {
        Box::pin(async move {
            let app = self.0.app.upgrade().ok_or_else(closing)?;
            app.set_hidden_address_active(&address, active)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn reopen_unsent(&self, draft: Draft) -> Result<(), String> {
        match self.0.open_unsent(draft) {
            true => Ok(()),
            false => Err(closing()),
        }
    }

    fn queue_changed(&self) {
        self.0.scheduled_changed();
    }

    fn undone(&self, outcome: &Outcome) {
        self.0.after_mail(Cause::Undid, outcome, None);
    }

    fn image_senders_changed(&self) {
        self.0.reload_image_senders();
    }
}
