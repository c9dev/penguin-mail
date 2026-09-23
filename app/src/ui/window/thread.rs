//! The window behind the thread run. `crate::open_thread::run` decides
//! what happens to the thread on screen and when; this file is the adapter
//! that answers its reads from the window, makes its store and Gmail calls
//! through the core, and hands its changes to the conversation view.
//!
//! The same [`Ports`] stand behind the engine run, in `pgp.rs`: both runs
//! read one view and ask the same question of it.

use std::collections::HashMap;
use std::rc::{Rc, Weak};

use gtk::glib;
use mailrs_domain::invitation::Invitation;
use mailrs_domain::{AccountId, FlagColor, MessageBody, MessageMeta, Target, ThreadSummary};
use mailrs_store::outbox::Queued;
use mailrs_store::{messages, threads};
use mailrs_sync::{History, MailAction, Opened, TriageAction, now_millis};

use super::pictures::Pictures;
use super::{BODY_FETCHES, MainWindow, read_cached_body};
use crate::core::Core;
use crate::open_thread::run::{Answer, Card, Desk, Effects, Fetched, Stored, ThreadRun};
use crate::open_thread::{OpenThread, Unsent};
use crate::protection::Read;
use crate::settings::MarkRead;
use crate::translation::{self, Language, Prose, Translation};
use crate::ui::conversation::ConversationView;
use crate::ui::invitation::Showing;
use crate::wanted::Screen;

impl MainWindow {
    /// The thread run, with this window and `view` behind both ports.
    pub(super) fn thread_run(self: &Rc<Self>, view: &Rc<ConversationView>) -> ThreadRun {
        let ports = self.ports(view);
        ThreadRun::new(Rc::clone(&ports) as Rc<dyn Desk>, ports as Rc<dyn Effects>)
    }

    /// The window and one of its conversation views, as a run sees them.
    pub(super) fn ports(self: &Rc<Self>, view: &Rc<ConversationView>) -> Rc<Ports> {
        Rc::new(Ports {
            window: Rc::downgrade(self),
            core: Rc::clone(&self.core),
            view: Rc::clone(view),
            pictures: Rc::clone(&self.pictures),
        })
    }

    /// Shows the thread `summary` names in `view`, without holding up
    /// whatever the caller does next.
    pub(super) fn load_into(self: &Rc<Self>, view: Rc<ConversationView>, summary: ThreadSummary) {
        let run = self.thread_run(&view);
        glib::spawn_future_local(async move { run.open(summary).await });
    }

    /// Picks up label changes and new messages in every conversation on
    /// screen, the ones in windows of their own among them. Each keeps
    /// its own thread, so a flag or a read mark set in one shows in all.
    pub(super) fn refresh_open_thread(self: &Rc<Self>) {
        self.refresh_open_threads(|_, _| true);
    }

    /// Does the same for the conversations whose account and thread
    /// `named` accepts, and leaves the rest alone.
    pub(super) fn refresh_open_threads(self: &Rc<Self>, named: impl Fn(AccountId, &str) -> bool) {
        for view in self.views() {
            if view.read(|open| named(open.account_id, &open.thread_id)) != Some(true) {
                continue;
            }
            let run = self.thread_run(&view);
            glib::spawn_future_local(async move { run.refresh().await });
        }
    }

    /// Reads the flag colour of every conversation on screen again, the
    /// ones in windows of their own among them. The store's change events
    /// do not carry the colour, so an undo needs this.
    pub(super) fn refresh_flag_color(self: &Rc<Self>) {
        for view in self.views() {
            let run = self.thread_run(&view);
            glib::spawn_future_local(async move { run.refresh_flag_color().await });
        }
    }

    /// The translation card's button.
    pub(super) fn translate_message(self: &Rc<Self>, view: &Rc<ConversationView>) {
        let run = self.thread_run(view);
        glib::spawn_future_local(async move { run.translate().await });
    }
}

/// The window as a run sees it.
pub(super) struct Ports {
    pub(super) window: Weak<MainWindow>,
    pub(super) core: Rc<Core>,
    pub(super) view: Rc<ConversationView>,
    pub(super) pictures: Rc<Pictures>,
}

impl Ports {
    fn window(&self) -> Option<Rc<MainWindow>> {
        self.window.upgrade()
    }

    fn sync(&self, account_id: AccountId) -> Result<std::sync::Arc<crate::core::Sync>, String> {
        self.core
            .account(account_id)
            .ok_or_else(|| "the account has stopped syncing".to_string())
    }
}

impl Screen for Ports {
    fn is_showing(&self, target: &Target) -> bool {
        self.view.is_showing(target)
    }
}

impl Desk for Ports {
    fn target(&self) -> Option<Target> {
        self.view.read(OpenThread::target)
    }

    fn start_loading(&self) -> u64 {
        self.view.start_loading()
    }

    fn still_loading(&self, ticket: u64) -> bool {
        self.view.still_loading(ticket)
    }

    fn me(&self, account_id: AccountId) -> Vec<String> {
        self.window()
            .map(|window| window.addresses_for(account_id))
            .unwrap_or_default()
    }

    fn images_allowed(&self, senders: &[String]) -> bool {
        self.window()
            .is_some_and(|window| window.images_allowed_for(senders))
    }

    fn photos(&self, senders: &[String]) -> HashMap<String, String> {
        self.window()
            .and_then(|window| window.app.upgrade())
            .map(|app| app.sender_photos(senders.iter().cloned()))
            .unwrap_or_default()
    }

    fn is_vip(&self, email: &str) -> bool {
        self.window()
            .is_some_and(|window| window.settings_with(|s| s.is_vip(email)))
    }

    fn mark_read_delay(&self) -> Option<u32> {
        match self.window()?.settings_with(|s| s.mark_read) {
            MarkRead::Immediately => Some(0),
            MarkRead::AfterDelay => Some(2),
            MarkRead::Manually => None,
        }
    }

    fn unread(&self) -> bool {
        self.view.read(OpenThread::unread).unwrap_or(false)
    }

    fn invitation(&self) -> Option<(String, String)> {
        self.view.find(|open| {
            open.invitation()
                .map(|(meta, ics)| (meta.id.clone(), ics.to_string()))
        })
    }

    fn wanting_thumbnails(&self) -> Vec<(String, MessageBody)> {
        self.view
            .read(OpenThread::wanting_thumbnails)
            .unwrap_or_default()
    }

    fn prose(&self) -> Option<(String, Prose)> {
        self.view.open_prose()
    }

    fn same_writer(&self, message_id: &str) -> String {
        self.view
            .read(|open| open.same_writer(message_id))
            .unwrap_or_default()
    }

    fn translation_of(&self, message_id: &str) -> Option<(Option<Language>, bool, bool)> {
        self.view.find(|open| open.translation_of(message_id))
    }

    fn arrived(&self, message_id: &str) -> Option<(MessageBody, HashMap<String, String>)> {
        self.view.find(|open| open.arrived(message_id))
    }

    fn interface_language(&self) -> Option<Language> {
        self.window()?.interface_language()
    }

    fn translation_destination(&self) -> Result<String, String> {
        let window = self
            .window()
            .ok_or_else(|| "the window has closed".to_string())?;
        window.settings_with(|s| translation::destination(&s.ai).map(|(_, goes)| goes))
    }
}

impl Effects for Ports {
    fn stored(
        &self,
        account_id: AccountId,
        thread_id: String,
    ) -> Answer<'_, Result<Stored, String>> {
        Box::pin(async move {
            self.core
                .read(move |c| {
                    let messages = messages::thread_messages(c, account_id, &thread_id)?;
                    let mut bodies = HashMap::new();
                    for meta in &messages {
                        if let Some(body) = read_cached_body(c, account_id, &meta.id)? {
                            bodies.insert(meta.id.clone(), body);
                        }
                    }
                    Ok(Stored { messages, bodies })
                })
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn ensure_thread(
        &self,
        account_id: AccountId,
        thread_id: String,
    ) -> Answer<'_, Result<(), String>> {
        Box::pin(async move {
            let sync = self.sync(account_id)?;
            self.core
                .call(async move { sync.open_thread(&thread_id).await })
                .await
                .map(|_| ())
                .map_err(|err| err.to_string())
        })
    }

    fn thread_messages(
        &self,
        account_id: AccountId,
        thread_id: String,
    ) -> Answer<'_, Result<Vec<MessageMeta>, String>> {
        Box::pin(async move {
            self.core
                .read(move |c| messages::thread_messages(c, account_id, &thread_id))
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn bodies(&self, account_id: AccountId, message_ids: Vec<String>) -> Answer<'_, Fetched> {
        Box::pin(async move {
            let Ok(sync) = self.sync(account_id) else {
                return Fetched::default();
            };
            let fetches = message_ids.into_iter().map(|id| {
                let (core, sync) = (Rc::clone(&self.core), sync.clone());
                async move {
                    let key = id.clone();
                    let result = core.call(async move { sync.body(&key).await }).await;
                    (id, result.map_err(|e| e.to_string()))
                }
            });
            // A long thread would otherwise fire one Gmail call per message
            // at once, and 30 of them at 5 units each is most of a second's
            // budget.
            let bodies: Vec<(String, Result<MessageBody, String>)> = {
                use futures::StreamExt;
                futures::stream::iter(fetches)
                    .buffered(BODY_FETCHES)
                    .collect()
                    .await
            };
            let images = self.pictures.inline(account_id, &sync, &bodies).await;
            Fetched { bodies, images }
        })
    }

    fn thumbnails(
        &self,
        account_id: AccountId,
        bodies: Vec<(String, MessageBody)>,
    ) -> Answer<'_, HashMap<String, String>> {
        Box::pin(async move {
            let Ok(sync) = self.sync(account_id) else {
                return HashMap::new();
            };
            self.pictures.thumbnails(account_id, &sync, &bodies).await
        })
    }

    fn open_invitation(
        &self,
        account_id: AccountId,
        message_id: String,
        ics: String,
    ) -> Answer<'_, Result<Option<Opened>, String>> {
        let invitations = self.core.invitations();
        Box::pin(async move {
            self.core
                .call(async move {
                    invitations
                        .open(account_id, &message_id, &ics, now_millis())
                        .await
                })
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn busy(
        &self,
        account_id: AccountId,
        invitation: Invitation,
    ) -> Answer<'_, Result<Vec<String>, String>> {
        let invitations = self.core.invitations();
        Box::pin(async move {
            self.core
                .call(async move { invitations.busy(account_id, &invitation).await })
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn series(
        &self,
        account_id: AccountId,
        invitation: Invitation,
    ) -> Answer<'_, Result<Option<String>, String>> {
        let invitations = self.core.invitations();
        Box::pin(async move {
            self.core
                .call(async move {
                    invitations
                        .series(account_id, &invitation, now_millis())
                        .await
                })
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn flag_color(
        &self,
        account_id: AccountId,
        thread_id: String,
    ) -> Answer<'_, Result<Option<FlagColor>, String>> {
        Box::pin(async move {
            self.core
                .read(move |c| threads::get_thread(c, account_id, &thread_id))
                .await
                .map(|summary| summary.and_then(|s| s.flag_color))
                .map_err(|err| err.to_string())
        })
    }

    fn translate(
        &self,
        into: Language,
        pieces: Vec<String>,
    ) -> Answer<'_, Result<Vec<Option<String>>, String>> {
        Box::pin(async move {
            let window = self
                .window()
                .ok_or_else(|| "the window has closed".to_string())?;
            let (config, _) = window.settings_with(|s| translation::destination(&s.ai))?;
            self.core
                .call(async move {
                    let pieces: Vec<&str> = pieces.iter().map(String::as_str).collect();
                    translation::ask(config, into, &pieces)
                        .await
                        .map_err(anyhow::Error::msg)
                })
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn sleep(&self, seconds: u32) -> Answer<'_, ()> {
        Box::pin(glib::timeout_future_seconds(seconds))
    }

    fn queued(&self, id: i64) -> Answer<'_, Result<Option<Queued>, String>> {
        let outbox = self.core.outbox();
        Box::pin(async move {
            self.core
                .call(async move { outbox.find(id).await })
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn show(&self, thread: OpenThread) {
        self.view.show(thread, true);
    }

    fn sender_vip(&self, vip: bool) {
        self.view.set_sender_vip(vip);
    }

    fn messages_arrived(&self, fresh: Vec<MessageMeta>) -> Vec<String> {
        self.view.messages_arrived(&fresh)
    }

    fn replace_messages(&self, fresh: Vec<MessageMeta>) -> bool {
        self.view.replace_messages(fresh)
    }

    fn bodies_arrived(&self, fetched: Fetched) {
        self.view.bodies_arrived(fetched.bodies, fetched.images);
    }

    fn thumbnails_arrived(&self, found: HashMap<String, String>) {
        self.view.thumbnails_arrived(found);
    }

    fn render_buttons(&self) {
        self.view.render_buttons();
    }

    fn clear(&self) {
        self.view.clear();
    }

    fn show_invitation(&self, showing: Option<Showing>) {
        self.view.show_invitation(showing);
    }

    fn offer_gnome(&self, account_id: AccountId) {
        if let Some(window) = self.window() {
            window.offer_gnome(&self.view, account_id);
        }
    }

    fn clashes(&self, uid: String, busy: Vec<String>) {
        self.view.clashes(&uid, &busy);
    }

    fn series_known(&self, uid: String, line: String) {
        self.view.series_known(&uid, line);
    }

    fn start_engines(&self) {
        if let Some(window) = self.window() {
            window.start_pgp(&self.view);
        }
    }

    fn translation_card(&self, card: Card) {
        let shown = &self.view.translate;
        match card {
            Card::Hidden => shown.hide(),
            Card::Offered { from, goes } => {
                shown.offer(from, goes.as_deref().map_err(String::as_str))
            }
            Card::Working => shown.working(),
            Card::Done {
                from,
                cut,
                shown: on,
            } => shown.done(from, cut, on),
            Card::Problem(problem) => shown.problem(&problem),
        }
    }

    fn translated(&self, message_id: String, translation: Translation) {
        self.view.translated(message_id, translation);
    }

    fn turn_translation(&self, message_id: &str) -> bool {
        self.view.turn_translation(message_id)
    }

    fn engine_answered(&self, message_id: String, read: Read) -> bool {
        self.view.engine_answered(message_id, read)
    }

    fn set_flag_color(&self, color: Option<FlagColor>) {
        self.view.set_flag_color(color);
    }

    fn unsent_changed(&self, unsent: Unsent) {
        self.view.unsent_changed(unsent);
    }

    fn mark_read(&self, target: Target) {
        if let Some(window) = self.window() {
            window.perform(
                vec![target],
                MailAction::Triage(TriageAction::MarkRead),
                History::Skip,
                None,
            );
        }
    }

    fn toast(&self, text: String) {
        if let Some(window) = self.window() {
            window.toast(&text);
        }
    }
}
