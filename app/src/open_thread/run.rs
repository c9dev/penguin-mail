//! The life of the thread on screen, through two ports: [`Desk`] for what
//! the window has open and [`Effects`] for the store, Gmail, and the named
//! changes to the conversation view.
//!
//! A queued message has no Gmail thread behind it. Opening its row shows
//! what the outbox kept, and refreshing reads the outbox again, since the
//! message can go out or fail once more while it is on screen.
//!
//! Opening a thread shows the stored copy first, then asks Gmail for the
//! whole thread and the bodies it lacks, fetches the pictures the bodies
//! name by `cid:` once their text is on screen, marks it read when the
//! setting says so, and fetches the pictures for the attachment rows. Refreshing
//! picks up what the store changed under it. Around each of those sit the
//! cards: the invitation, the translation offer, and the engine run for a
//! signed or encrypted message. After each event the run decides which of
//! those went stale, in [`Stale::after`], so no caller has to remember.
//!
//! Every step after the first answers through [`Wanted`], keyed on the
//! target the run started on, so an answer that arrives after the reader
//! opened something else goes nowhere. Before a thread is on screen there
//! is no target to key on yet, and two quick clicks race instead: a
//! ticket from [`Desk::start_loading`] lets only the later one show.
//!
//! Nothing here touches GTK. The window is one adapter behind the ports
//! and the tests are another.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use mailrs_domain::invitation::{Invitation, Method};
use mailrs_domain::{
    AccountId, EpochMillis, FlagColor, MessageBody, MessageMeta, Target, ThreadSummary,
};
use mailrs_store::outbox::Queued;
use mailrs_sync::{Opened, Spot, outbox_id};

use super::{Loose, Parsed, loose_id};

use super::{Cleaned, InlineImage, OpenThread, ToClean, Unsent};
use crate::protection::Read;
use crate::ui::invitation::strip::Strip;
use crate::ui::invitation::{AddTo, Showing};
pub use crate::wanted::Answer;
use crate::wanted::{Screen, Wanted};

#[cfg(test)]
mod fake;
mod receipt;
#[cfg(test)]
mod tests;
mod translation;

pub use receipt::ReceiptCard;
pub use translation::Card;

/// What the store holds for a thread: its messages, oldest first, and the
/// bodies already fetched, by message id.
#[derive(Debug, Clone, Default)]
pub struct Stored {
    pub messages: Vec<MessageMeta>,
    pub bodies: HashMap<String, MessageBody>,
    /// The person left the list the newest message's sender writes from,
    /// so the thread has no Unsubscribe to offer.
    pub left: bool,
}

/// Bodies as Gmail sent them, and their HTML cleaned through
/// [`Effects::clean`].
#[derive(Debug, Clone, Default)]
pub struct Fetched {
    pub bodies: Vec<(String, Result<Arc<MessageBody>, String>)>,
    pub cleaned: HashMap<String, Cleaned>,
}

/// Inline pictures by message id, then by content id.
pub type InlinePictures = HashMap<String, HashMap<String, InlineImage>>;

/// The reader's preferences that shape the thread, read at once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preferences {
    /// Seconds to wait before marking an opened thread read, or `None`
    /// when the reader marks mail read by hand.
    pub mark_read_delay: Option<u32>,
    /// The chosen AI language. `None` when the window has closed.
    pub translation_language: Option<String>,
    /// Changes whenever the AI language changes, even if it later returns.
    pub translation_revision: Option<u64>,
    /// The languages, by code, the person reads as they are, whose
    /// messages get no card.
    pub never_translate: Vec<String>,
    /// Whether the person wants to be asked about read receipts at all.
    pub offers_receipts: bool,
}

/// What the run reads from the window. Every method answers from what the
/// window already holds, so a test fills one in without a widget. As a
/// [`Screen`] it also says whether a target is still on screen.
pub trait Desk: Screen {
    /// Hands the conversation on screen to `read`, and does nothing when
    /// none is. Everything the run reads off the thread goes through here,
    /// so the window and the tests read it the same way.
    fn open_thread(&self, read: &mut dyn FnMut(&OpenThread));
    /// Starts loading a thread and hands back its ticket. Two quick clicks
    /// can have their store reads answer out of order.
    fn start_loading(&self) -> u64;
    /// Whether `ticket` is still the latest thread asked for.
    fn still_loading(&self, ticket: u64) -> bool;
    /// The user's own addresses in the account.
    fn me(&self, account_id: AccountId) -> Vec<String>;
    /// Whether mail from all of these senders may load remote images.
    fn images_allowed(&self, senders: &[String]) -> bool;
    /// Contact photos for these senders, as `data:` URIs.
    fn photos(&self, senders: &[String]) -> crate::render::Photos;
    fn is_vip(&self, email: &str) -> bool;
    /// The invitation on the card while the card offers no Show in
    /// Calendar, which the calendar copy may since have come to hold.
    fn invitation_off_calendar(&self) -> Option<Invitation>;
    /// Where a message's words would go to be translated, in the words the
    /// card uses, or why they have nowhere to go. Not a preference: it
    /// reads the model's key.
    fn translation_destination(&self) -> Result<String, String>;
    /// The reader's preferences as they are now.
    fn preferences(&self) -> Preferences;
}

/// What the run asks of the store, Gmail and the view. A test answers with
/// what it likes and records the rest.
pub trait Effects {
    /// The thread's messages and cached bodies, from the store.
    fn stored(
        &self,
        account_id: AccountId,
        thread_id: String,
    ) -> Answer<'_, Result<Stored, String>>;
    /// Stores the whole thread, from a search that fetched it or from Gmail.
    fn ensure_thread(
        &self,
        account_id: AccountId,
        thread_id: String,
    ) -> Answer<'_, Result<(), String>>;
    /// The thread's messages as the store has them now.
    fn thread_messages(
        &self,
        account_id: AccountId,
        thread_id: String,
    ) -> Answer<'_, Result<Vec<MessageMeta>, String>>;
    /// Fetches these bodies.
    fn bodies(
        &self,
        account_id: AccountId,
        message_ids: Vec<String>,
    ) -> Answer<'_, Vec<(String, Result<MessageBody, String>)>>;
    /// Cleans HTML for the page, away from the GTK thread. The one place
    /// cleaning happens: drawing a page never cleans.
    fn clean(&self, html: ToClean) -> Answer<'_, HashMap<String, Cleaned>>;
    /// Reads a loose message's bytes once, away from the window: a file
    /// near the size ceiling takes most of a second.
    fn parse_loose(&self, raw: Arc<Vec<u8>>) -> Answer<'_, Parsed>;
    /// The pictures these bodies name by `cid:`, by message and content
    /// id. Every body asked about is in the answer, with nothing for one
    /// whose pictures would not come.
    fn inline_images(
        &self,
        account_id: AccountId,
        bodies: Vec<(String, Arc<MessageBody>)>,
    ) -> Answer<'_, InlinePictures>;
    /// Small pictures for the attachment rows of these bodies, by Gmail's
    /// attachment id, from the background share of the quota.
    fn thumbnails(
        &self,
        account_id: AccountId,
        bodies: Vec<(String, Arc<MessageBody>)>,
    ) -> Answer<'_, HashMap<String, String>>;
    /// Reads an invitation and what the user already said about it.
    fn open_invitation(
        &self,
        account_id: AccountId,
        message_id: String,
        ics: String,
    ) -> Answer<'_, Result<Option<Opened>, String>>;
    /// What else is on the user's calendar while the event runs.
    fn busy(
        &self,
        account_id: AccountId,
        invitation: Invitation,
    ) -> Answer<'_, Result<Vec<String>, String>>;
    /// How the series behind an invitation to one occurrence runs, in
    /// words, from the calendar. `None` when the calendar cannot say.
    fn series(
        &self,
        account_id: AccountId,
        invitation: Invitation,
    ) -> Answer<'_, Result<Option<String>, String>>;
    /// The calendars of the account a file's events can go on, none for
    /// an account with no calendar or one that withheld it.
    fn add_targets(&self, account_id: AccountId) -> Answer<'_, Result<Vec<AddTo>, String>>;
    /// Where the invitation's event sits in the calendar's copy on this
    /// computer, or `None` when the copy does not hold it.
    fn on_calendar(
        &self,
        account_id: AccountId,
        invitation: Invitation,
    ) -> Answer<'_, Result<Option<Spot>, String>>;
    /// The hours around the invitation's event from the calendar's copy
    /// on this computer, or `None` when the account has no synced copy.
    /// `at` is the occurrence Show in Calendar opens, when the copy holds
    /// the event; the strip then shows the same day.
    fn strip(
        &self,
        account_id: AccountId,
        invitation: Invitation,
        at: Option<EpochMillis>,
    ) -> Answer<'_, Result<Option<Strip>, String>>;
    /// The flag colour the store holds for the thread.
    fn flag_color(
        &self,
        account_id: AccountId,
        thread_id: String,
    ) -> Answer<'_, Result<Option<FlagColor>, String>>;
    /// Asks the model for the pieces in `into`.
    fn translate(
        &self,
        into: String,
        pieces: Vec<String>,
    ) -> Answer<'_, Result<Vec<Option<String>>, String>>;
    /// Waits this many seconds.
    fn sleep(&self, seconds: u32) -> Answer<'_, ()>;
    /// The queued message the outbox holds under this row id, or `None`
    /// once it has gone out or been deleted.
    fn queued(&self, id: i64) -> Answer<'_, Result<Option<Queued>, String>>;
    /// Whether the receipt `message_id` asks for was answered.
    fn receipt_answered(
        &self,
        account_id: AccountId,
        message_id: String,
    ) -> Answer<'_, Result<bool, String>>;
    /// Sends or declines the receipt and records the answer.
    fn answer_receipt(
        &self,
        account_id: AccountId,
        message_id: String,
        send: bool,
    ) -> Answer<'_, Result<(), String>>;
    /// The To and Cc addresses of the messages in the person's Sent mail
    /// whose Message-ID is `rfc822_msgid`, given without angle brackets.
    /// None when Sent holds no such message.
    fn sent_recipients(
        &self,
        account_id: AccountId,
        rfc822_msgid: String,
    ) -> Answer<'_, Result<Vec<String>, String>>;

    /// Puts a thread on screen, in place of whatever was there.
    fn show(&self, thread: OpenThread);
    /// Whether the thread's sender is a VIP, for the sender menu.
    fn sender_vip(&self, vip: bool);
    /// The messages as the store has them; gives back the ids whose
    /// bodies are still missing.
    fn messages_arrived(&self, fresh: Vec<MessageMeta>) -> Vec<String>;
    /// Replaces the messages; answers whether the ids changed.
    fn replace_messages(&self, fresh: Vec<MessageMeta>) -> bool;
    fn bodies_arrived(&self, fetched: Fetched);
    /// Cleaned copies of bodies the page said were loading.
    fn cleaned(&self, cleaned: HashMap<String, Cleaned>);
    /// The pictures the bodies name, which the page has been waiting for.
    fn images_arrived(&self, found: InlinePictures);
    fn thumbnails_arrived(&self, found: HashMap<String, String>);
    /// Draws the header buttons again, for a change the page does not show.
    fn render_buttons(&self);
    /// Empties the view, for a thread that has gone from the store.
    fn clear(&self);
    /// Puts the invitation on its card, or takes the card down.
    fn show_invitation(&self, showing: Option<Showing>);
    /// Offers Grant Access on the card when the account has a calendar
    /// and withheld the permission to read it.
    fn offer_calendar_access(&self, account_id: AccountId);
    /// Puts what else the user has on during the event on the card.
    fn clashes(&self, uid: String, busy: Vec<String>);
    /// Puts how the series runs on the card, under the time.
    fn series_known(&self, uid: String, line: String);
    /// Puts the calendars a file's events can go on into the card's
    /// picker.
    fn add_targets_known(&self, uid: String, targets: Vec<AddTo>);
    /// Puts Show in Calendar on the card, for the event at `spot`.
    fn on_calendar_known(&self, uid: String, spot: Spot);
    /// Puts the hours around the event on the card, in place of the clash
    /// line.
    fn strip_known(&self, uid: String, strip: Strip);
    /// Starts the engine run for a signed or encrypted message.
    fn start_engines(&self);
    fn translation_card(&self, card: Card);
    fn receipt_card(&self, card: ReceiptCard);
    /// Whether a receipt answers mail the person sent, and the redraw that
    /// shows it as one line when it does.
    fn report_checked(&self, message_id: String, sent: bool);
    /// One message's translation, and the redraw that shows it.
    fn translated(&self, message_id: String, translation: crate::translation::Translation);
    /// Drops cached translations into a language the reader no longer wants.
    fn keep_translations(&self, into: &str);
    /// Turns a translated message over; `false` when it has none.
    fn turn_translation(&self, message_id: &str) -> bool;
    /// What the engine said, above the message. Answers whether it opened
    /// a body.
    fn engine_answered(&self, message_id: String, read: Read) -> bool;
    fn set_flag_color(&self, color: Option<FlagColor>);
    /// What the outbox now says about the queued message on screen.
    fn unsent_changed(&self, unsent: Unsent);
    /// Marks the target read, as the reader would by hand.
    fn mark_read(&self, target: Target);
    /// Says something at the bottom of the window.
    fn toast(&self, text: String);
}

/// What happened to the thread on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The stored copy went on screen.
    Shown,
    /// Gmail's bodies arrived.
    BodiesArrived,
    /// Gmail's copy of the thread arrived, and the store already held a
    /// body for every message in it.
    NothingMissing,
    /// The engine opened an encrypted message, whose body replaced the
    /// ciphertext.
    EngineOpened,
    /// A queued message went on screen.
    QueuedShown,
    /// A loose message went on screen, cleaned, from its own bytes.
    LooseShown,
}

/// The parts of the window an event leaves stale. The page and the header
/// buttons are not here: each named change on the view redraws what it
/// touched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stale {
    /// The cleaned copies of HTML bodies, which the page waits for. First,
    /// since the translation card reads the cleaned words.
    pub cleaning: bool,
    /// The translation card, since the words arrive with the body.
    pub translation: bool,
    /// The engine run's claim, which needs a protected body to claim.
    pub protection: bool,
    /// The event card, read from the body's calendar part.
    pub invitation: bool,
    /// The pictures the bodies name by `cid:`.
    pub images: bool,
    /// The pictures on the attachment rows.
    pub thumbnails: bool,
    /// The unread mark, which the reader has now seen.
    pub unread: bool,
    /// The receipt card and the receipts drawn as a line, read from the
    /// bodies.
    pub receipt: bool,
}

impl Stale {
    pub fn after(event: Event) -> Stale {
        match event {
            // The store keeps bodies and no pictures, so a thread read
            // before asks for them as it goes on screen.
            Event::Shown => Stale {
                translation: true,
                protection: true,
                invitation: true,
                images: true,
                receipt: true,
                ..Stale::default()
            },
            Event::BodiesArrived => Stale {
                cleaning: true,
                translation: true,
                protection: true,
                invitation: true,
                images: true,
                thumbnails: true,
                unread: true,
                receipt: true,
            },
            // The stored copy brought the bodies, and what they carry was
            // brought up to date when it went on screen. The pictures and
            // the read mark wait on the whole thread.
            Event::NothingMissing => Stale {
                thumbnails: true,
                unread: true,
                ..Stale::default()
            },
            // The claim was made for the message the engine opened, and
            // its files and the pictures among them came out whole, with
            // nothing to fetch.
            Event::EngineOpened => Stale {
                cleaning: true,
                translation: true,
                invitation: true,
                receipt: true,
                ..Stale::default()
            },
            // The writer's own words need no translation, no engine and no
            // mark, and they carry no invitation. Reading for one takes
            // down the card the thread before left.
            Event::QueuedShown => Stale {
                invitation: true,
                ..Stale::default()
            },
            // The message itself is all there is: no server copy to fetch
            // pictures or thumbnails from, no mailbox to mark it read in,
            // and a receipt would mail about a message in no mailbox. The
            // cards that read the message itself still come.
            Event::LooseShown => Stale {
                translation: true,
                protection: true,
                invitation: true,
                ..Stale::default()
            },
        }
    }
}

/// The thread run, and the one way to open, complete and refresh what is
/// on screen.
pub struct ThreadRun {
    desk: Rc<dyn Desk>,
    effects: Rc<dyn Effects>,
}

type Want<'a> = Wanted<'a, dyn Effects>;

impl ThreadRun {
    pub fn new(desk: Rc<dyn Desk>, effects: Rc<dyn Effects>) -> ThreadRun {
        ThreadRun { desk, effects }
    }

    fn wanted(&self, target: Target) -> Want<'_> {
        Wanted::new(&*self.desk as &dyn Screen, &*self.effects, target)
    }

    /// The target on screen now, as a [`Wanted`].
    fn on_screen(&self) -> Option<Want<'_>> {
        Some(self.wanted(self.thread(OpenThread::target)?))
    }

    /// Reads the conversation on screen. `None` when there is none.
    fn thread<R>(&self, read: impl FnOnce(&OpenThread) -> R) -> Option<R> {
        let mut read = Some(read);
        let mut out = None;
        self.desk.open_thread(&mut |open| {
            if let Some(read) = read.take() {
                out = Some(read(open));
            }
        });
        out
    }

    /// Reads the conversation on screen for something it may not hold.
    fn find<R>(&self, read: impl FnOnce(&OpenThread) -> Option<R>) -> Option<R> {
        self.thread(read).flatten()
    }

    /// Shows the thread `summary` names: the stored copy first, then the
    /// whole thread and its bodies.
    pub async fn open(&self, summary: ThreadSummary) {
        if let Some(id) = outbox_id(&summary.id) {
            return self.open_queued(id).await;
        }
        let target = Target::from_row(&summary);
        let ticket = self.desk.start_loading();
        let stored = self
            .effects
            .stored(target.account_id, target.thread_id.clone())
            .await;
        if !self.desk.still_loading(ticket) {
            return;
        }
        let Stored {
            mut messages,
            bodies,
            left,
        } = stored.unwrap_or_else(|err| {
            tracing::info!(error = %err, "could not read the stored thread");
            Stored::default()
        });
        if let Some(id) = &target.message_id {
            messages.retain(|m| &m.id == id);
        }
        let subject = messages
            .first()
            .map_or_else(|| summary.subject.clone(), |m| m.subject.clone());
        let me = self.desk.me(target.account_id);
        let mut thread = OpenThread::new(&target, subject, messages, bodies, me);
        // The stored copy goes on screen whole rather than loading, so its
        // bodies are cleaned first.
        let html = thread.to_clean();
        if !html.is_empty() {
            let cleaned = self.effects.clean(html).await;
            if !self.desk.still_loading(ticket) {
                return;
            }
            thread.take_cleaned(cleaned);
        }
        thread.unsubscribed = left;
        let senders = thread.senders();
        let named: Vec<String> = senders.iter().filter(|s| !s.is_empty()).cloned().collect();
        thread.images_allowed = self
            .desk
            .images_allowed(std::slice::from_ref(&summary.from_email))
            || self.desk.images_allowed(&senders);
        thread.photos = self.desk.photos(&named);
        thread.flag_color = summary.flag_color;
        let vip = thread
            .other_sender()
            .is_some_and(|a| self.desk.is_vip(&a.email));
        // The one change made without a Wanted: the ticket above is what
        // says this thread is still the one asked for.
        self.effects.show(thread);
        self.effects.sender_vip(vip);
        let wanted = self.wanted(target);
        futures::join!(self.follow(&wanted, Event::Shown), self.complete(&wanted));
    }

    /// Picks up label changes and new messages in the thread on screen.
    /// Only a change in the set of messages fetches and redraws, so the
    /// reading position survives a label change.
    pub async fn refresh(&self) {
        let Some(wanted) = self.on_screen() else {
            return;
        };
        if let Some(id) = outbox_id(&wanted.target().thread_id) {
            return self.refresh_queued(&wanted, id).await;
        }
        // Nothing holds a loose message but this window.
        if loose_id(&wanted.target().thread_id).is_some() {
            return;
        }
        let target = wanted.target().clone();
        let Some(fresh) = wanted
            .ask(
                |effects| effects.thread_messages(target.account_id, target.thread_id.clone()),
                "could not read the thread again",
            )
            .await
        else {
            return;
        };
        let fresh: Vec<MessageMeta> = fresh
            .into_iter()
            .filter(|m| target.message_id.as_ref().is_none_or(|id| &m.id == id))
            .collect();
        if fresh.is_empty() {
            wanted.on_screen(|effects| effects.clear());
            return;
        }
        match wanted.on_screen(|effects| effects.replace_messages(fresh)) {
            Some(true) => self.complete(&wanted).await,
            Some(false) => {
                wanted.on_screen(|effects| effects.render_buttons());
            }
            None => {}
        }
    }

    /// Shows `loose` from its bytes: cleaned first, as the stored copy
    /// is, then the cards that read the message itself. The ticket works
    /// as it does for a thread: a later open wins over this one.
    pub async fn open_loose(&self, loose: Loose) {
        let ticket = self.desk.start_loading();
        let parsed = self.effects.parse_loose(Arc::clone(&loose.raw)).await;
        if !self.desk.still_loading(ticket) {
            return;
        }
        let me = self.desk.me(loose.account_id);
        let mut thread = OpenThread::loose(&loose, parsed, me);
        let html = thread.to_clean();
        if !html.is_empty() {
            let cleaned = self.effects.clean(html).await;
            if !self.desk.still_loading(ticket) {
                return;
            }
            thread.take_cleaned(cleaned);
        }
        let wanted = self.wanted(thread.target());
        self.effects.show(thread);
        self.follow(&wanted, Event::LooseShown).await;
    }

    /// Shows the queued message under outbox row `id`. The ticket works as
    /// it does for a thread: a later click wins over this one.
    async fn open_queued(&self, id: i64) {
        let ticket = self.desk.start_loading();
        let found = self.effects.queued(id).await;
        if !self.desk.still_loading(ticket) {
            return;
        }
        let queued = match found {
            Ok(Some(queued)) => queued,
            Ok(None) => return self.effects.clear(),
            Err(err) => {
                tracing::info!(error = %err, "could not read the queued message");
                return self.effects.clear();
            }
        };
        let unsent = Unsent::of(&queued, chrono::Local::now());
        let me = self.desk.me(queued.account_id);
        let mut thread = OpenThread::queued(&queued, unsent, me);
        let html = thread.to_clean();
        if !html.is_empty() {
            let cleaned = self.effects.clean(html).await;
            if !self.desk.still_loading(ticket) {
                return;
            }
            thread.take_cleaned(cleaned);
        }
        let wanted = self.wanted(thread.target());
        self.effects.show(thread);
        self.follow(&wanted, Event::QueuedShown).await;
    }

    /// Reads the queued message on screen again: it may have gone out,
    /// been deleted, or failed once more with a new reason and a new time
    /// for the next try.
    async fn refresh_queued(&self, wanted: &Want<'_>, id: i64) {
        let Some(found) = wanted
            .ask(
                |effects| effects.queued(id),
                "could not read the queued message again",
            )
            .await
        else {
            return;
        };
        match found {
            Some(queued) => {
                let unsent = Unsent::of(&queued, chrono::Local::now());
                wanted.on_screen(|effects| effects.unsent_changed(unsent));
            }
            None => {
                wanted.on_screen(|effects| effects.clear());
            }
        }
    }

    /// Puts what the engine said above the message in `target`. A body it
    /// opened carries its own invitation and its own language.
    pub async fn engine_answered(&self, target: Target, message_id: String, read: Read) {
        let wanted = self.wanted(target);
        if wanted.on_screen(|effects| effects.engine_answered(message_id, read)) == Some(true) {
            self.follow(&wanted, Event::EngineOpened).await;
        }
    }

    /// Looks for the card's event on the calendar again, after the
    /// calendar copy read the provider. An invitation opened before the
    /// copy's first read, in the first minute after start, found nothing
    /// there and offered Add to Calendar; this swaps in Show in Calendar
    /// once the event arrives.
    pub async fn calendar_read(&self) {
        let Some(wanted) = self.on_screen() else {
            return;
        };
        let Some(Some(invitation)) = wanted.on_screen(|_| self.desk.invitation_off_calendar())
        else {
            return;
        };
        if invitation.cancelled() {
            return;
        }
        let account_id = wanted.target().account_id;
        let uid = invitation.uid.clone();
        if let Some(Some(spot)) = wanted
            .ask(
                |effects| effects.on_calendar(account_id, invitation),
                "could not look for the event on the calendar",
            )
            .await
        {
            wanted.on_screen(|effects| effects.on_calendar_known(uid, spot));
        }
    }

    /// Reads the flag colour of the thread on screen again. The store's
    /// change events do not carry it, so an undo needs this.
    pub async fn refresh_flag_color(&self) {
        let Some(wanted) = self.on_screen() else {
            return;
        };
        let target = wanted.target().clone();
        if loose_id(&target.thread_id).is_some() {
            return;
        }
        if let Some(color) = wanted
            .ask(
                |effects| effects.flag_color(target.account_id, target.thread_id.clone()),
                "could not read the flag colour",
            )
            .await
        {
            wanted.on_screen(|effects| effects.set_flag_color(color));
        }
    }

    /// Fetches the whole thread and the bodies it lacks, then brings the
    /// rest up to date.
    async fn complete(&self, wanted: &Want<'_>) {
        let target = wanted.target().clone();
        let (account_id, thread_id) = (target.account_id, target.thread_id.clone());
        let ensured = wanted
            .wait(|effects| effects.ensure_thread(account_id, thread_id.clone()))
            .await;
        match ensured {
            None => return,
            Some(Err(err)) => tracing::info!(error = %err, "showing the stored copy of the thread"),
            Some(Ok(())) => {}
        }
        let Some(fresh) = wanted
            .wait(|effects| effects.thread_messages(account_id, thread_id))
            .await
        else {
            return;
        };
        let fresh = fresh.unwrap_or_default();
        let Some(missing) = wanted.on_screen(|effects| effects.messages_arrived(fresh)) else {
            return;
        };
        // A thread read before has every body in the store. Asking Gmail
        // for none of them and drawing the page again would cost a whole
        // reload and put the reader back at the top.
        if missing.is_empty() {
            self.follow(wanted, Event::NothingMissing).await;
            return;
        }
        let Some(bodies) = wanted
            .wait(|effects| effects.bodies(account_id, missing))
            .await
        else {
            return;
        };
        let bodies: Vec<(String, Result<Arc<MessageBody>, String>)> = bodies
            .into_iter()
            .map(|(id, body)| (id, body.map(Arc::new)))
            .collect();
        // Cleaned before they go on screen, so each article goes from
        // loading to its body in one patch.
        let html = ToClean::of(
            account_id,
            bodies
                .iter()
                .filter_map(|(id, body)| Some((id, body.as_ref().ok()?))),
        );
        let cleaned = match html.is_empty() {
            true => HashMap::new(),
            false => match wanted.wait(|effects| effects.clean(html)).await {
                Some(cleaned) => cleaned,
                None => return,
            },
        };
        let fetched = Fetched { bodies, cleaned };
        if wanted
            .on_screen(|effects| effects.bodies_arrived(fetched))
            .is_some()
        {
            self.follow(wanted, Event::BodiesArrived).await;
        }
    }

    /// Brings up to date whatever `event` left stale.
    async fn follow(&self, wanted: &Want<'_>, event: Event) {
        let stale = Stale::after(event);
        if stale.cleaning {
            self.clean(wanted).await;
        }
        if stale.translation {
            let card = self.translation_offer();
            wanted.on_screen(|effects| effects.translation_card(card));
        }
        if stale.protection {
            wanted.on_screen(|effects| effects.start_engines());
        }
        let unread = stale.unread
            && wanted.on_screen(|_| self.thread(OpenThread::unread)) == Some(Some(true));
        futures::join!(
            async {
                if stale.invitation {
                    self.invitation(wanted).await;
                }
            },
            async {
                if stale.images {
                    self.images(wanted).await;
                }
            },
            async {
                if stale.thumbnails {
                    self.thumbnails(wanted).await;
                }
            },
            async {
                if unread {
                    self.mark_read_later(wanted).await;
                }
            },
            async {
                if stale.receipt {
                    self.receipt(wanted).await;
                }
            },
        );
    }

    /// Cleans the HTML bodies the page says are loading, away from the GTK
    /// thread, and puts them on screen.
    async fn clean(&self, wanted: &Want<'_>) {
        let Some(Some(html)) = wanted.on_screen(|_| self.thread(OpenThread::to_clean)) else {
            return;
        };
        if html.is_empty() {
            return;
        }
        if let Some(cleaned) = wanted.wait(|effects| effects.clean(html)).await {
            wanted.on_screen(|effects| effects.cleaned(cleaned));
        }
    }

    /// Marks the conversation read after the delay the setting asks for,
    /// if it is still the one on screen by then.
    async fn mark_read_later(&self, wanted: &Want<'_>) {
        let Some(delay) = self.desk.preferences().mark_read_delay else {
            return;
        };
        if wanted.wait(|effects| effects.sleep(delay)).await.is_none() {
            return;
        }
        let target = wanted.target().clone();
        wanted.on_screen(|effects| effects.mark_read(target));
    }

    /// Fetches the pictures the bodies name by `cid:`. Their text is on
    /// screen already, and the page waits for these in place.
    async fn images(&self, wanted: &Want<'_>) {
        let Some(Some(bodies)) = wanted.on_screen(|_| self.thread(OpenThread::wanting_images))
        else {
            return;
        };
        if bodies.is_empty() {
            return;
        }
        let account_id = wanted.target().account_id;
        if let Some(found) = wanted
            .wait(|effects| effects.inline_images(account_id, bodies))
            .await
        {
            wanted.on_screen(|effects| effects.images_arrived(found));
        }
    }

    /// Fetches the pictures for the attachment rows. The message is
    /// already on screen, and reading it never waits on them.
    async fn thumbnails(&self, wanted: &Want<'_>) {
        let Some(Some(bodies)) = wanted.on_screen(|_| self.thread(OpenThread::wanting_thumbnails))
        else {
            return;
        };
        if bodies.is_empty() {
            return;
        }
        let account_id = wanted.target().account_id;
        let Some(found) = wanted
            .wait(|effects| effects.thumbnails(account_id, bodies))
            .await
        else {
            return;
        };
        if !found.is_empty() {
            wanted.on_screen(|effects| effects.thumbnails_arrived(found));
        }
    }

    /// The newest message on screen that carries an invitation, with the
    /// `text/calendar` part it arrived in.
    fn invitation_on_screen(&self) -> Option<(String, String)> {
        self.find(|open| {
            open.invitation()
                .map(|(meta, ics)| (meta.id.clone(), ics.to_string()))
        })
    }

    /// Reads the invitation in the thread and puts it on the card, or
    /// takes the card down when the thread carries none.
    async fn invitation(&self, wanted: &Want<'_>) {
        let Some(found) = wanted.on_screen(|_| self.invitation_on_screen()) else {
            return;
        };
        let Some((message_id, ics)) = found else {
            wanted.on_screen(|effects| effects.show_invitation(None));
            return;
        };
        let account_id = wanted.target().account_id;
        let asked = (message_id.clone(), ics.clone());
        let carrier = message_id.clone();
        // A loose message is in no mailbox and its answer could be kept
        // nowhere, so its events read as a calendar file's: something to
        // keep, with nobody to answer.
        let opened = match wanted.on_screen(|_| self.thread(OpenThread::is_loose)) {
            Some(Some(true)) => Ok(kept_events(&ics)),
            _ => match wanted
                .wait(|effects| effects.open_invitation(account_id, message_id, ics))
                .await
            {
                Some(opened) => opened,
                None => return,
            },
        };
        // New bodies may have started a second read while this one ran,
        // and whichever answers last would win. The card shows this answer
        // only while the thread still carries the invitation it read.
        if wanted.on_screen(|_| self.invitation_on_screen()) != Some(Some(asked)) {
            return;
        }
        let showing = match opened {
            Ok(opened) => opened.map(|opened| Showing {
                message_id: carrier,
                invitation: opened.invitation,
                also: opened.also,
                change: opened.change,
                answer: opened.answer,
                me: self.desk.me(account_id),
                on_calendar: None,
            }),
            Err(err) => {
                tracing::info!(error = %err, "could not read the invitation");
                None
            }
        };
        wanted.on_screen(|effects| effects.show_invitation(showing.clone()));
        if let Some(showing) = showing.as_ref().filter(|s| one_of_a_series(s)) {
            let (uid, invitation) = (showing.invitation.uid.clone(), showing.invitation.clone());
            if let Some(Some(line)) = wanted
                .ask(
                    |effects| effects.series(account_id, invitation),
                    "could not read the series",
                )
                .await
            {
                wanted.on_screen(|effects| effects.series_known(uid, line));
            }
        }
        // A file that only describes events is added to a calendar the
        // person picks, so the picker needs the account's calendars.
        if let Some(showing) = showing.as_ref().filter(|s| adds(s)) {
            let uid = showing.invitation.uid.clone();
            if let Some(targets) = wanted
                .ask(
                    |effects| effects.add_targets(account_id),
                    "could not list the calendars",
                )
                .await
                .filter(|targets| !targets.is_empty())
            {
                wanted.on_screen(|effects| effects.add_targets_known(uid, targets));
            }
        }
        // An answered invitation is still worth finding on the calendar;
        // only a cancellation has nothing there to show.
        let mut at = None;
        if let Some(showing) = showing.as_ref().filter(|s| !s.invitation.cancelled()) {
            wanted.on_screen(|effects| effects.offer_calendar_access(account_id));
            let (uid, invitation) = (showing.invitation.uid.clone(), showing.invitation.clone());
            if let Some(Some(spot)) = wanted
                .ask(
                    |effects| effects.on_calendar(account_id, invitation),
                    "could not look for the event on the calendar",
                )
                .await
            {
                at = Some(spot.start);
                wanted.on_screen(|effects| effects.on_calendar_known(uid, spot));
            }
        }
        // With the calendar's copy on this computer, the hours around the
        // event say what else is on, and the clash line is not needed. The
        // strip shows the occurrence Show in Calendar opens, which for a
        // series is not the first one the invitation names.
        let mut strip_drawn = false;
        if let Some(showing) = showing.as_ref().filter(|s| draws_a_strip(s)) {
            let (uid, invitation) = (showing.invitation.uid.clone(), showing.invitation.clone());
            if let Some(Some(strip)) = wanted
                .ask(
                    |effects| effects.strip(account_id, invitation, at),
                    "could not read the hours around the event",
                )
                .await
            {
                strip_drawn = wanted
                    .on_screen(|effects| effects.strip_known(uid, strip))
                    .is_some();
            }
        }
        let Some(showing) = showing.filter(waiting_on_an_answer) else {
            return;
        };
        if strip_drawn {
            return;
        }
        let uid = showing.invitation.uid.clone();
        if let Some(busy) = wanted
            .ask(
                |effects| effects.busy(account_id, showing.invitation),
                "could not read the calendar",
            )
            .await
        {
            wanted.on_screen(|effects| effects.clashes(uid, busy));
        }
    }
}

/// The events of a loose message's calendar part, as a file's: the first
/// to show and the rest beside it, with no change and no answer.
fn kept_events(ics: &str) -> Option<Opened> {
    let mut events = crate::ui::ics_file::events_of(ics).into_iter();
    let invitation = events.next()?;
    Some(Opened {
        invitation,
        also: events.collect(),
        change: None,
        answer: None,
    })
}

/// Whether the card holds a file that only describes events, which Add
/// to Calendar puts on a calendar of the person's choosing.
fn adds(showing: &Showing) -> bool {
    showing.invitation.card() == mailrs_domain::invitation::Card::Add
}

/// Whether the invitation asks about one occurrence of a series that is
/// still on. That card offers to answer for the occurrence or the series,
/// and carries no rule to say what the series is, so the calendar is
/// asked.
fn one_of_a_series(showing: &Showing) -> bool {
    showing.invitation.occurrence.is_some()
        && showing.invitation.method == Method::Request
        && !showing.invitation.cancelled()
}

/// Whether the card has room for the hours around the event: a request
/// that is still on, at a time of day rather than for whole days.
fn draws_a_strip(showing: &Showing) -> bool {
    showing.invitation.method == Method::Request
        && !showing.invitation.cancelled()
        && showing
            .invitation
            .when
            .as_ref()
            .is_some_and(|when| !when.all_day())
}

/// Whether the invitation still waits on the user: a request they have not
/// answered, for an event that still runs. Only then do the clashes earn
/// a place on the card.
fn waiting_on_an_answer(showing: &Showing) -> bool {
    showing.answer.is_none()
        && showing.invitation.method == Method::Request
        && !showing.invitation.cancelled()
}
