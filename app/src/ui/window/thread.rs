//! The window behind the thread run. `crate::open_thread::run` decides
//! what happens to the thread on screen and when; this file is the adapter
//! that answers its reads from the window, makes its store and Gmail calls
//! through the core, and hands its changes to the conversation view.
//!
//! The same [`Ports`] stand behind the engine run, in `pgp.rs`: both runs
//! read one view and ask the same question of it.

use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gtk::glib;
use mailrs_domain::invitation::{Invitation, When};
use mailrs_domain::translate::gettext;
use mailrs_domain::{
    AccountId, EpochMillis, FlagColor, MessageBody, MessageMeta, Target, ThreadSummary,
};
use mailrs_store::calendar::CalendarScope;
use mailrs_store::outbox::Queued;
use mailrs_store::{messages, threads, unsubscribes};
use mailrs_sync::{History, MailAction, Opened, Spot, TriageAction, now_millis};

use super::pictures::Pictures;
use super::{BODY_FETCHES, MainWindow, read_cached_body};
use crate::core::Core;
use crate::open_thread::run::{
    Answer, Card, Desk, Effects, Fetched, InlinePictures, Preferences, ReceiptCard, Stored,
    ThreadRun,
};
use crate::open_thread::{Cleaned, OpenThread, ToClean, Unsent};
use crate::protection::Read;
use crate::settings::{MarkRead, ReadReceipts};
use crate::translation::{self, Translation};
use crate::ui::conversation::ConversationView;
use crate::ui::invitation::strip::{self, Strip};
use crate::ui::invitation::{AddTo, Showing};
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

    /// Looks for each open invitation's event on the calendar again,
    /// after the calendar copy stored new events, so a card that offered
    /// Add to Calendar before the copy held the event offers Show in
    /// Calendar.
    pub fn calendar_read_for_threads(self: &Rc<Self>) {
        for view in self.views() {
            let run = self.thread_run(&view);
            glib::spawn_future_local(async move { run.calendar_read().await });
        }
    }

    /// Asks the conversations on `account_id` again whether their receipt
    /// question was answered, now that the account can say.
    pub(super) fn account_ready_for_threads(self: &Rc<Self>, account_id: AccountId) {
        for view in self.views() {
            let run = self.thread_run(&view);
            glib::spawn_future_local(async move { run.account_ready(account_id).await });
        }
    }

    /// The translation card's button.
    pub(super) fn translate_message(self: &Rc<Self>, view: &Rc<ConversationView>) {
        let run = self.thread_run(view);
        glib::spawn_future_local(async move { run.translate().await });
    }

    /// The read receipt card's buttons. A card already at work has no
    /// message to answer for, so a second press sends nothing.
    pub(super) fn answer_receipt(
        self: &Rc<Self>,
        view: &Rc<ConversationView>,
        message_id: String,
        send: bool,
    ) {
        // Only the message the card asks about, and only while it asks.
        if view.find(OpenThread::receipt_asked).as_deref() != Some(message_id.as_str()) {
            return;
        }
        let run = self.thread_run(view);
        glib::spawn_future_local(async move { run.answer_receipt(message_id, send).await });
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
    fn open_thread(&self, read: &mut dyn FnMut(&OpenThread)) {
        self.view.read(read);
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

    fn photos(&self, senders: &[String]) -> crate::render::Photos {
        self.window()
            .and_then(|window| window.app.upgrade())
            .map(|app| app.sender_photos(senders.iter().cloned()))
            .unwrap_or_default()
    }

    fn is_vip(&self, email: &str) -> bool {
        self.window()
            .is_some_and(|window| window.settings_with(|s| s.is_vip(email)))
    }

    fn invitation_off_calendar(&self) -> Option<Invitation> {
        self.view
            .with_invitation(|showing| {
                showing
                    .on_calendar
                    .is_none()
                    .then(|| showing.invitation.clone())
            })
            .flatten()
    }

    fn translation_destination(&self) -> Result<String, String> {
        let window = self
            .window()
            .ok_or_else(|| "the window has closed".to_string())?;
        window.settings_with(|s| translation::destination(&s.ai).map(|(_, goes)| goes))
    }

    fn preferences(&self) -> Preferences {
        let Some(window) = self.window() else {
            return Preferences::default();
        };
        window.settings_with(|s| Preferences {
            mark_read_delay: match s.mark_read {
                MarkRead::Immediately => Some(0),
                MarkRead::AfterDelay => Some(2),
                MarkRead::Manually => None,
            },
            translation_language: Some(crate::assistant::language::selected(&s.ai.language)),
            translation_revision: Some(s.ai.language_revision),
            never_translate: s.never_translate.clone(),
            offers_receipts: s.read_receipts == ReadReceipts::Ask,
        })
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
                    // The banner offers to leave the newest message's
                    // list, so that sender is the one to look up.
                    let sender = messages.iter().rev().find_map(|m| m.from.as_ref());
                    let left = match sender {
                        Some(from) => unsubscribes::left(c, account_id, &from.email)?.is_some(),
                        None => false,
                    };
                    Ok(Stored {
                        messages,
                        bodies,
                        left,
                    })
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
                .call_for(
                    account_id,
                    async move { sync.open_thread(&thread_id).await },
                )
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

    fn bodies(
        &self,
        account_id: AccountId,
        message_ids: Vec<String>,
    ) -> Answer<'_, Vec<(String, Result<MessageBody, String>)>> {
        Box::pin(async move {
            let Ok(sync) = self.sync(account_id) else {
                return Vec::new();
            };
            let fetches = message_ids.into_iter().map(|id| {
                let (core, sync) = (Rc::clone(&self.core), sync.clone());
                async move {
                    let key = id.clone();
                    let result = core
                        .call_for(account_id, async move { sync.body(&key).await })
                        .await;
                    (id, result.map_err(|err| err.to_string()))
                }
            });
            // A long thread would otherwise fire one Gmail call per message
            // at once, and 30 of them at 5 units each is most of a second's
            // budget.
            use futures::StreamExt;
            futures::stream::iter(fetches)
                .buffered(BODY_FETCHES)
                .collect()
                .await
        })
    }

    /// Cleans on a worker thread. Forty newsletters take the GTK thread
    /// tens of milliseconds, during which the window would not draw. A
    /// body this fails to clean comes back as a failed copy, which the
    /// page draws as its text, so it does not stay loading.
    fn clean(&self, html: ToClean) -> Answer<'_, HashMap<String, Cleaned>> {
        Box::pin(async move {
            if html.is_empty() {
                return HashMap::new();
            }
            let failed = html.stand_ins();
            self.core
                .call(async move { tokio::task::spawn_blocking(move || html.clean()).await })
                .await
                .unwrap_or_else(|err| {
                    tracing::warn!(error = %err, "could not clean the bodies away from the window");
                    failed.failures()
                })
        })
    }

    fn parse_loose(&self, raw: Arc<Vec<u8>>) -> Answer<'_, crate::open_thread::Parsed> {
        Box::pin(async move {
            let shared = Arc::clone(&raw);
            self.core
                .call(async move {
                    tokio::task::spawn_blocking(move || crate::open_thread::loose::parse(&shared))
                        .await
                })
                .await
                .unwrap_or_else(|err| {
                    tracing::warn!(error = %err, "could not read the message away from the window");
                    crate::open_thread::loose::parse(&raw)
                })
        })
    }

    fn inline_images(
        &self,
        account_id: AccountId,
        bodies: Vec<(String, Arc<MessageBody>)>,
    ) -> Answer<'_, InlinePictures> {
        Box::pin(async move {
            match self.sync(account_id) {
                Ok(sync) => self.pictures.inline(account_id, &sync, &bodies).await,
                // Nothing will come, so the page may stop waiting.
                Err(_) => bodies
                    .into_iter()
                    .map(|(id, _)| (id, HashMap::new()))
                    .collect(),
            }
        })
    }

    fn thumbnails(
        &self,
        account_id: AccountId,
        bodies: Vec<(String, Arc<MessageBody>)>,
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
            let opened = self
                .core
                .call_for(account_id, async move {
                    invitations
                        .open(account_id, &message_id, &ics, now_millis())
                        .await
                })
                .await
                .map_err(|err| err.to_string());
            // Opening saved the message the invitation came in, which is
            // what gives its "Waiting for your answer" card an "Open mail"
            // door.
            if let (Ok(Some(_)), Some(win)) = (&opened, self.window()) {
                win.calendar.refresh_waiting();
            }
            opened
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
                .call_for(account_id, async move {
                    invitations.busy(account_id, &invitation).await
                })
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
                .call_for(account_id, async move {
                    invitations
                        .series(account_id, &invitation, now_millis())
                        .await
                })
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn add_targets(&self, account_id: AccountId) -> Answer<'_, Result<Vec<AddTo>, String>> {
        // An account with no calendar, or one that withheld it, leaves the
        // card handing the file to the desktop.
        let usable = self
            .window()
            .is_some_and(|w| w.offers(account_id).calendar && !w.withheld(account_id).calendar);
        let calendar = self.core.calendar();
        Box::pin(async move {
            if !usable {
                return Ok(Vec::new());
            }
            let listed = self
                .core
                .call_for(
                    account_id,
                    async move { calendar.calendars(account_id).await },
                )
                .await
                .map_err(|err| err.to_string())?;
            Ok(match listed {
                mailrs_sync::Permitted::Done(list) => {
                    crate::ui::invitation::targets_of(account_id, list, None)
                }
                mailrs_sync::Permitted::NeedsPermission => Vec::new(),
            })
        })
    }

    fn on_calendar(
        &self,
        account_id: AccountId,
        invitation: Invitation,
    ) -> Answer<'_, Result<Option<Spot>, String>> {
        let invitations = self.core.invitations();
        Box::pin(async move {
            self.core
                .call_for(account_id, async move {
                    invitations
                        .on_calendar(account_id, &invitation, now_millis())
                        .await
                })
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn strip(
        &self,
        account_id: AccountId,
        invitation: Invitation,
        at: Option<EpochMillis>,
    ) -> Answer<'_, Result<Option<Strip>, String>> {
        Box::pin(async move {
            let Some(When::At { starts_at, ends_at }) = invitation.when else {
                return Ok(None);
            };
            // An invitation with no end runs an hour, as the clash line
            // assumes. A series keeps its length on the occurrence the
            // calendar found.
            let length = ends_at.unwrap_or(starts_at + 3_600_000) - starts_at;
            let start = at.unwrap_or(starts_at);
            let end = start + length;
            let Some(span) = strip::window(start, &chrono::Local) else {
                return Ok(None);
            };
            let read = (span.0.min(start), span.1.max(end));
            let found = self
                .core
                .read(move |c| {
                    if !mailrs_store::calendar::synced(c, account_id)? {
                        return Ok(None);
                    }
                    let around = mailrs_store::calendar::occurrences(
                        c,
                        &[account_id],
                        read.0,
                        read.1,
                        CalendarScope::Owned,
                    )?;
                    let colours: HashMap<String, String> =
                        mailrs_store::calendar::calendars(c, account_id)?
                            .into_iter()
                            .map(|calendar| (calendar.id, calendar.color))
                            .collect();
                    Ok(Some((around, colours)))
                })
                .await
                .map_err(|err| err.to_string())?;
            Ok(found.map(|(around, colours)| {
                let asked = strip::Asked {
                    uid: &invitation.uid,
                    start,
                    end,
                };
                strip::build(&asked, span, &around, &colours, &chrono::Local)
            }))
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
        into: String,
        pieces: Vec<String>,
    ) -> Answer<'_, Result<Vec<Option<String>>, String>> {
        Box::pin(async move {
            let window = self
                .window()
                .ok_or_else(|| "the window has closed".to_string())?;
            let (config, reasoning) = window.settings_with(|s| {
                translation::destination(&s.ai)
                    .map(|(config, _)| (config, s.ai.translation_reasoning))
            })?;
            self.core
                .call(async move {
                    let pieces: Vec<&str> = pieces.iter().map(String::as_str).collect();
                    translation::ask(config, &into, &pieces, reasoning)
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

    fn receipt_answered(
        &self,
        account_id: AccountId,
        message_id: String,
    ) -> Answer<'_, Result<bool, String>> {
        Box::pin(async move {
            // Not connected yet: ask later rather than twice.
            let Some(sync) = self.core.account(account_id) else {
                return Ok(true);
            };
            // A receipt nobody answered still needs an address of the
            // account's that the mail came to, or the card would offer one
            // from another address; such a message counts as settled.
            self.core
                .call_for(account_id, async move {
                    sync.receipt_open(&message_id).await.map(|open| !open)
                })
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn answer_receipt(
        &self,
        account_id: AccountId,
        message_id: String,
        send: bool,
    ) -> Answer<'_, Result<(), String>> {
        Box::pin(async move {
            let sync = self
                .core
                .account(account_id)
                .ok_or_else(|| gettext("The account is not connected yet."))?;
            self.core
                .call_for(account_id, async move {
                    sync.answer_receipt(&message_id, send).await
                })
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn sent_recipients(
        &self,
        account_id: AccountId,
        rfc822_msgid: String,
    ) -> Answer<'_, Result<Vec<String>, String>> {
        Box::pin(async move {
            self.core
                .read(move |c| messages::sent_copies(c, account_id, &rfc822_msgid))
                .await
                .map(|copies| {
                    copies
                        .into_iter()
                        .flat_map(|copy| copy.to.into_iter().chain(copy.cc))
                        .map(|address| address.email)
                        .collect()
                })
                .map_err(|err| err.to_string())
        })
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
        self.view.bodies_arrived(fetched);
    }

    fn cleaned(&self, cleaned: HashMap<String, Cleaned>) {
        self.view.cleaned(cleaned);
    }

    fn thumbnails_arrived(&self, found: HashMap<String, String>) {
        self.view.thumbnails_arrived(found);
    }

    fn images_arrived(&self, found: InlinePictures) {
        self.view.images_arrived(found);
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

    fn offer_calendar_access(&self, account_id: AccountId) {
        if let Some(window) = self.window() {
            window.offer_calendar_access(&self.view, account_id);
        }
    }

    fn clashes(&self, uid: String, busy: Vec<String>) {
        self.view.clashes(&uid, &busy);
    }

    fn series_known(&self, uid: String, line: String) {
        self.view.series_known(&uid, line);
    }

    fn add_targets_known(&self, uid: String, targets: Vec<AddTo>) {
        self.view.add_targets_known(&uid, targets);
    }

    fn on_calendar_known(&self, uid: String, spot: Spot) {
        self.view.found_on_calendar(&uid, spot);
    }

    fn strip_known(&self, uid: String, strip: Strip) {
        self.view.strip_arrived(&uid, &strip);
    }

    fn start_engines(&self) {
        if let Some(window) = self.window() {
            window.start_pgp(&self.view);
        }
    }

    fn receipt_card(&self, card: ReceiptCard) {
        self.view.receipt_card(card);
    }

    fn report_checked(&self, message_id: String, sent: bool) {
        self.view.report_checked(message_id, sent);
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

    fn keep_translations(&self, into: &str) {
        self.view.keep_translations(into);
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
