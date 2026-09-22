//! The life of the thread on screen, through two ports: [`Desk`] for what
//! the window has open and [`Effects`] for the store, Gmail, and the named
//! changes to the conversation view.
//!
//! A queued message has no Gmail thread behind it. Opening its row shows
//! what the outbox kept, and refreshing reads the outbox again, since the
//! message can go out or fail once more while it is on screen.
//!
//! Opening a thread shows the stored copy first, then asks Gmail for the
//! whole thread and the bodies it lacks, marks it read when the setting
//! says so, and fetches the pictures for the attachment rows. Refreshing
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

use mailrs_domain::invitation::{Invitation, Method};
use mailrs_domain::{AccountId, FlagColor, MessageBody, MessageMeta, Target, ThreadSummary};
use mailrs_store::outbox::Queued;
use mailrs_sync::{Opened, outbox_id};

use super::{OpenThread, Unsent};
use crate::protection::Read;
use crate::translation::{Language, Prose};
use crate::ui::invitation::Showing;
pub use crate::wanted::Answer;
use crate::wanted::{Screen, Wanted};

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;
mod translation;

pub use translation::Card;

/// What the store holds for a thread: its messages, oldest first, and the
/// bodies already fetched, by message id.
#[derive(Debug, Clone, Default)]
pub struct Stored {
    pub messages: Vec<MessageMeta>,
    pub bodies: HashMap<String, MessageBody>,
}

/// Bodies as Gmail sent them, and the inline images that go in them.
#[derive(Debug, Clone, Default)]
pub struct Fetched {
    pub bodies: Vec<(String, Result<MessageBody, String>)>,
    pub images: HashMap<String, HashMap<String, String>>,
}

/// What the run reads from the window. Every method answers from what the
/// window already holds, so a test fills one in without a widget. As a
/// [`Screen`] it also says whether a target is still on screen.
pub trait Desk: Screen {
    /// The conversation on screen, if any.
    fn target(&self) -> Option<Target>;
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
    fn photos(&self, senders: &[String]) -> HashMap<String, String>;
    fn is_vip(&self, email: &str) -> bool;
    /// Seconds to wait before marking an opened thread read, or `None`
    /// when the reader marks mail read by hand.
    fn mark_read_delay(&self) -> Option<u32>;
    /// Whether the thread on screen has an unread message.
    fn unread(&self) -> bool;
    /// The newest message on screen that carries an invitation, with the
    /// `text/calendar` part it arrived in.
    fn invitation(&self) -> Option<(String, String)>;
    /// Bodies on screen with a picture attached that has no thumbnail.
    fn wanting_thumbnails(&self) -> Vec<(String, MessageBody)>;
    /// The message a translation applies to, with the prose the page
    /// draws for it.
    fn prose(&self) -> Option<(String, Prose)>;
    /// What the sender of `message_id` wrote in the thread's other
    /// messages, for a note too short to tell its language alone.
    fn same_writer(&self, message_id: &str) -> String;
    /// What the card says about a message translated here: the language
    /// it came from, whether it was cut short, and whether it is shown.
    fn translation_of(&self, message_id: &str) -> Option<(Option<Language>, bool, bool)>;
    /// A message's body as it arrived, with its inline images.
    fn arrived(&self, message_id: &str) -> Option<(MessageBody, HashMap<String, String>)>;
    /// The language the interface is in, when this app can count its
    /// words. `None` leaves every message alone.
    fn interface_language(&self) -> Option<Language>;
    /// Where a message's words would go to be translated, in the words the
    /// card uses, or why they have nowhere to go.
    fn translation_destination(&self) -> Result<String, String>;
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
    /// Fetches these bodies and the inline images they reference.
    fn bodies(&self, account_id: AccountId, message_ids: Vec<String>) -> Answer<'_, Fetched>;
    /// Small pictures for the attachment rows of these bodies, by Gmail's
    /// attachment id, from the background share of the quota.
    fn thumbnails(
        &self,
        account_id: AccountId,
        bodies: Vec<(String, MessageBody)>,
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
    /// The flag colour the store holds for the thread.
    fn flag_color(
        &self,
        account_id: AccountId,
        thread_id: String,
    ) -> Answer<'_, Result<Option<FlagColor>, String>>;
    /// Asks the model for the pieces in `into`.
    fn translate(
        &self,
        into: Language,
        pieces: Vec<String>,
    ) -> Answer<'_, Result<Vec<Option<String>>, String>>;
    /// Waits this many seconds.
    fn sleep(&self, seconds: u32) -> Answer<'_, ()>;
    /// The queued message the outbox holds under this row id, or `None`
    /// once it has gone out or been deleted.
    fn queued(&self, id: i64) -> Answer<'_, Result<Option<Queued>, String>>;

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
    fn thumbnails_arrived(&self, found: HashMap<String, String>);
    /// Draws the header buttons again, for a change the page does not show.
    fn render_buttons(&self);
    /// Empties the view, for a thread that has gone from the store.
    fn clear(&self);
    /// Puts the invitation on its card, or takes the card down.
    fn show_invitation(&self, showing: Option<Showing>);
    /// Offers the account to GNOME Online Accounts, when it is worth it.
    fn offer_gnome(&self, account_id: AccountId);
    /// Puts what else the user has on during the event on the card.
    fn clashes(&self, uid: String, busy: Vec<String>);
    /// Puts how the series runs on the card, under the time.
    fn series_known(&self, uid: String, line: String);
    /// Starts the engine run for a signed or encrypted message.
    fn start_engines(&self);
    fn translation_card(&self, card: Card);
    /// One message's translation, and the redraw that shows it.
    fn translated(&self, message_id: String, translation: crate::translation::Translation);
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
    /// The engine opened an encrypted message, whose body replaced the
    /// ciphertext.
    EngineOpened,
    /// A queued message went on screen.
    QueuedShown,
}

/// The parts of the window an event leaves stale. The page and the header
/// buttons are not here: each named change on the view redraws what it
/// touched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stale {
    /// The translation card, since the words arrive with the body.
    pub translation: bool,
    /// The engine run's claim, which needs a protected body to claim.
    pub protection: bool,
    /// The event card, read from the body's calendar part.
    pub invitation: bool,
    /// The pictures on the attachment rows.
    pub thumbnails: bool,
    /// The unread mark, which the reader has now seen.
    pub unread: bool,
}

impl Stale {
    pub fn after(event: Event) -> Stale {
        match event {
            Event::Shown => Stale {
                translation: true,
                protection: true,
                invitation: true,
                ..Stale::default()
            },
            Event::BodiesArrived => Stale {
                translation: true,
                protection: true,
                invitation: true,
                thumbnails: true,
                unread: true,
            },
            // The claim was made for the message the engine opened, and
            // its files came out whole, with nothing to fetch.
            Event::EngineOpened => Stale {
                translation: true,
                invitation: true,
                ..Stale::default()
            },
            // The writer's own words need no translation, no engine and no
            // mark, and they carry no invitation. Reading for one takes
            // down the card the thread before left.
            Event::QueuedShown => Stale {
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
        Some(self.wanted(self.desk.target()?))
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
        let thread = OpenThread::queued(&queued, unsent, me);
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

    /// Reads the flag colour of the thread on screen again. The store's
    /// change events do not carry it, so an undo needs this.
    pub async fn refresh_flag_color(&self) {
        let Some(wanted) = self.on_screen() else {
            return;
        };
        let target = wanted.target().clone();
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
        let Some(fetched) = wanted
            .wait(|effects| effects.bodies(account_id, missing))
            .await
        else {
            return;
        };
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
        if stale.translation {
            let card = self.translation_offer();
            wanted.on_screen(|effects| effects.translation_card(card));
        }
        if stale.protection {
            wanted.on_screen(|effects| effects.start_engines());
        }
        let unread = stale.unread && wanted.on_screen(|_| self.desk.unread()) == Some(true);
        futures::join!(
            async {
                if stale.invitation {
                    self.invitation(wanted).await;
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
        );
    }

    /// Marks the conversation read after the delay the setting asks for,
    /// if it is still the one on screen by then.
    async fn mark_read_later(&self, wanted: &Want<'_>) {
        let Some(delay) = self.desk.mark_read_delay() else {
            return;
        };
        if wanted.wait(|effects| effects.sleep(delay)).await.is_none() {
            return;
        }
        let target = wanted.target().clone();
        wanted.on_screen(|effects| effects.mark_read(target));
    }

    /// Fetches the pictures for the attachment rows. The message is
    /// already on screen, and reading it never waits on them.
    async fn thumbnails(&self, wanted: &Want<'_>) {
        let Some(bodies) = wanted.on_screen(|_| self.desk.wanting_thumbnails()) else {
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

    /// Reads the invitation in the thread and puts it on the card, or
    /// takes the card down when the thread carries none.
    async fn invitation(&self, wanted: &Want<'_>) {
        let Some(found) = wanted.on_screen(|_| self.desk.invitation()) else {
            return;
        };
        let Some((message_id, ics)) = found else {
            wanted.on_screen(|effects| effects.show_invitation(None));
            return;
        };
        let account_id = wanted.target().account_id;
        let asked = (message_id.clone(), ics.clone());
        let Some(opened) = wanted
            .wait(|effects| effects.open_invitation(account_id, message_id, ics))
            .await
        else {
            return;
        };
        // New bodies may have started a second read while this one ran,
        // and whichever answers last would win. The card shows this answer
        // only while the thread still carries the invitation it read.
        if wanted.on_screen(|_| self.desk.invitation()) != Some(Some(asked)) {
            return;
        }
        let showing = match opened {
            Ok(opened) => opened.map(|opened| Showing {
                invitation: opened.invitation,
                change: opened.change,
                answer: opened.answer,
                me: self.desk.me(account_id),
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
        let Some(showing) = showing.filter(waiting_on_an_answer) else {
            return;
        };
        wanted.on_screen(|effects| effects.offer_gnome(account_id));
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

/// Whether the invitation asks about one occurrence of a series that is
/// still on. That card offers to answer for the occurrence or the series,
/// and carries no rule to say what the series is, so the calendar is
/// asked.
fn one_of_a_series(showing: &Showing) -> bool {
    showing.invitation.occurrence.is_some()
        && showing.invitation.method == Method::Request
        && !showing.invitation.cancelled()
}

/// Whether the invitation still waits on the user: a request they have not
/// answered, for an event that still runs. Only then do the clashes and
/// the GNOME offer earn a place on the card.
fn waiting_on_an_answer(showing: &Showing) -> bool {
    showing.answer.is_none()
        && showing.invitation.method == Method::Request
        && !showing.invitation.cancelled()
}
