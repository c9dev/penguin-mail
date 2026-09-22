//! The thread run with no window: the thread on screen in memory behind
//! both ports, an answer waiting for each call that takes time, and a log
//! of what the run asked for, in order.
//!
//! The named changes go through the same [`OpenThread`] methods the
//! conversation view uses, so the thread under test changes the way the
//! one on screen does. Nothing here starts a widget or talks to Gmail.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use futures::channel::oneshot;
use mailrs_domain::invitation::Invitation;
use mailrs_domain::{
    AccountId, Address, FlagColor, MessageBody, MessageMeta, Target, ThreadSummary, system_label,
};
use mailrs_store::outbox::Queued;
use mailrs_sync::Opened;

use super::{Answer, Card, Desk, Effects, Fetched, Stored, ThreadRun};
use crate::open_thread::{OpenThread, Unsent};
use crate::protection::Read;
use crate::translation::{self, Body, Language, Prose, Translation};
use crate::ui::invitation::Showing;
use crate::wanted::Screen as OnScreen;

/// The account and thread every fixture belongs to.
pub const ACCOUNT: AccountId = 1;
pub const THREAD: &str = "t1";
/// The thread the reader opens instead, mid-run.
pub const ELSEWHERE: &str = "t2";

/// One thing the run asked the window for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    Stored,
    Show,
    Ensure,
    Messages,
    MessagesArrived,
    Bodies,
    BodiesArrived,
    Replace,
    Buttons,
    Clear,
    Thumbnails,
    ThumbnailsArrived,
    OpenInvitation,
    ShowInvitation,
    OfferGnome,
    Busy,
    Clashes,
    Series,
    SeriesKnown,
    Engines,
    Card,
    Sleep,
    MarkRead,
    Translate,
    Translated,
    Turn,
    EngineAnswered,
    FlagColor,
    SetFlag,
    Queued,
    Unsent,
}

/// The window the run reads and writes.
pub struct Screen {
    /// The thread on screen, or `None` with nothing open.
    pub open: Option<OpenThread>,
    /// The latest thread asked for.
    pub ticket: u64,
    /// What the store holds, by thread id.
    pub stored: HashMap<String, Stored>,
    /// Holds the store's answer for a thread until the test lets go.
    pub holds: HashMap<String, oneshot::Receiver<()>>,
    /// What the store lists for the thread after Gmail answered.
    pub messages: Vec<MessageMeta>,
    /// What Gmail hands back, by message id.
    pub gmail: HashMap<String, MessageBody>,
    pub thumbnails: HashMap<String, String>,
    pub invitation: Result<Option<Opened>, String>,
    pub busy: Result<Vec<String>, String>,
    /// What the calendar says about the series, in words.
    pub series: Result<Option<String>, String>,
    /// The series lines put on the card.
    pub series_lines: Vec<String>,
    pub flag_color: Option<FlagColor>,
    /// What the outbox holds, by row id.
    pub queued: HashMap<i64, Queued>,
    pub translation: Result<Vec<Option<String>>, String>,
    /// The Mark as Read setting.
    pub delay: Option<u32>,
    pub interface: Option<Language>,
    pub destination: Result<String, String>,
    /// The step the reader opens something else during.
    pub moves_on: Option<Step>,
    /// What opening something else does to the thread on screen. Another
    /// thread, unless a test says otherwise.
    pub moving: fn(&mut OpenThread),
    /// What the run asked for, oldest first.
    pub steps: Vec<Step>,
    /// The threads that went on screen, in order.
    pub shown: Vec<String>,
    pub cards: Vec<Card>,
    /// The invitations put on the card, by UID, and `None` for the card
    /// taken down.
    pub invitations: Vec<Option<String>>,
    pub marked: Vec<Target>,
    pub toasts: Vec<String>,
}

pub struct FakeWindow(pub RefCell<Screen>);

/// A message of the fixture thread from Ann.
pub fn meta(id: &str, unread: bool) -> MessageMeta {
    MessageMeta {
        account_id: ACCOUNT,
        id: id.to_string(),
        thread_id: THREAD.to_string(),
        rfc822_msgid: None,
        from: Some(Address {
            name: Some("Ann".to_string()),
            email: "ann@example.com".to_string(),
        }),
        to: Vec::new(),
        cc: Vec::new(),
        subject: "Kite plans".to_string(),
        date: 0,
        snippet: String::new(),
        size: 0,
        has_attachments: false,
        label_ids: match unread {
            true => vec![system_label::UNREAD.to_string()],
            false => Vec::new(),
        },
    }
}

/// A body in English, which the interface is in too.
pub fn body(text: &str) -> MessageBody {
    MessageBody {
        text: Some(text.to_string()),
        ..MessageBody::default()
    }
}

/// A body in European Portuguese, which the card offers to translate.
pub fn portuguese() -> MessageBody {
    body(
        "Olá Ana, a reunião de amanhã fica para as dez horas. Não te esqueças de \
         trazer os documentos que eu te pedi, para podermos ver tudo com calma \
         antes de falar com o banco. Um abraço e até amanhã.",
    )
}

/// A body carrying an invitation to a meeting nobody has answered yet.
pub fn invited() -> MessageBody {
    MessageBody {
        calendar: Some(ics()),
        ..body("You are invited.")
    }
}

pub fn ics() -> String {
    [
        "BEGIN:VCALENDAR",
        "METHOD:REQUEST",
        "BEGIN:VEVENT",
        "UID:kites@example.com",
        "SEQUENCE:0",
        "SUMMARY:Kite flying",
        "DTSTART:20300310T090000Z",
        "DTEND:20300310T100000Z",
        "ORGANIZER;CN=Ann:mailto:ann@example.com",
        "END:VEVENT",
        "END:VCALENDAR",
        "",
    ]
    .join("\r\n")
}

/// What reading that invitation gives back.
pub fn opened_invitation() -> Opened {
    Opened {
        invitation: mailrs_domain::invitation::read(&ics()).expect("the fixture reads"),
        change: None,
        answer: None,
    }
}

/// What reading an invitation to one Tuesday of a weekly event gives
/// back. It carries no rule of its own.
pub fn opened_occurrence() -> Opened {
    let ics = ics().replace("SEQUENCE:0", "SEQUENCE:0\r\nRECURRENCE-ID:20300310T090000Z");
    Opened {
        invitation: mailrs_domain::invitation::read(&ics).expect("the fixture reads"),
        change: None,
        answer: None,
    }
}

/// A body with a picture attached, which wants a thumbnail.
pub fn with_picture() -> MessageBody {
    MessageBody {
        attachments: vec![mailrs_domain::Attachment {
            part_id: "2".to_string(),
            filename: "kite.png".to_string(),
            mime_type: "image/png".to_string(),
            size: 10,
            attachment_id: Some("a1".to_string()),
            content_id: None,
        }],
        ..body("A picture of the kite.")
    }
}

/// The row a reader clicks to open the fixture thread.
pub fn row(thread_id: &str) -> ThreadSummary {
    ThreadSummary {
        account_id: ACCOUNT,
        id: thread_id.to_string(),
        subject: "Kite plans".to_string(),
        from_email: "ann@example.com".to_string(),
        ..ThreadSummary::default()
    }
}

/// A message the outbox holds under row 7: to Ann, and stuck when
/// `problem` says why.
pub fn queued(problem: Option<&str>) -> Queued {
    let draft = crate::open_thread::queued::draft_to(ACCOUNT, "ann@example.com", "See you.");
    Queued {
        id: 7,
        account_id: ACCOUNT,
        subject: draft.subject.clone(),
        recipients: "ann@example.com".to_string(),
        composer: serde_json::to_string(&draft).expect("a draft writes"),
        problem: problem.map(str::to_string),
        attempts: 1,
        ..Queued::default()
    }
}

/// The English interface.
pub fn english() -> Language {
    translation::interface_language("", "en", &[]).expect("English is known")
}

impl FakeWindow {
    /// A window with nothing open, where the store holds the fixture
    /// thread with one unread message and no body, and Gmail has that
    /// body.
    pub fn new() -> Rc<FakeWindow> {
        let stored = Stored {
            messages: vec![meta("m1", true)],
            bodies: HashMap::new(),
        };
        Rc::new(FakeWindow(RefCell::new(Screen {
            open: None,
            ticket: 0,
            stored: HashMap::from([(THREAD.to_string(), stored)]),
            holds: HashMap::new(),
            messages: vec![meta("m1", true)],
            gmail: HashMap::from([("m1".to_string(), body("Hello"))]),
            thumbnails: HashMap::from([("a1".to_string(), "data:image/png;base64,".to_string())]),
            invitation: Ok(Some(opened_invitation())),
            busy: Ok(vec!["Design crit".to_string()]),
            series: Ok(Some("Every Tuesday, 6 left".to_string())),
            series_lines: Vec::new(),
            flag_color: Some(FlagColor::Orange),
            queued: HashMap::new(),
            translation: Ok(vec![Some("Hello Ana".to_string())]),
            delay: Some(2),
            interface: Some(english()),
            destination: Ok("The message goes to a model on this computer.".to_string()),
            moves_on: None,
            moving: |open| open.thread_id = ELSEWHERE.to_string(),
            steps: Vec::new(),
            shown: Vec::new(),
            cards: Vec::new(),
            invitations: Vec::new(),
            marked: Vec::new(),
            toasts: Vec::new(),
        })))
    }

    /// A window where Gmail and the store both hold `body` for m1.
    pub fn with_body(body: MessageBody) -> Rc<FakeWindow> {
        let window = FakeWindow::new();
        window.with(|screen| {
            screen.gmail.insert("m1".to_string(), body);
        });
        window
    }

    pub fn with<R>(&self, change: impl FnOnce(&mut Screen) -> R) -> R {
        change(&mut self.0.borrow_mut())
    }

    /// The run, with this window behind both ports.
    pub fn run(self: &Rc<Self>) -> ThreadRun {
        ThreadRun::new(
            Rc::clone(self) as Rc<dyn Desk>,
            Rc::clone(self) as Rc<dyn Effects>,
        )
    }

    pub fn steps(&self) -> Vec<Step> {
        self.0.borrow().steps.clone()
    }

    pub fn took(&self, step: Step) -> bool {
        self.0.borrow().steps.contains(&step)
    }

    /// The thread on screen, read.
    pub fn open<R>(&self, read: impl FnOnce(&OpenThread) -> R) -> Option<R> {
        self.0.borrow().open.as_ref().map(read)
    }

    /// Notes a step, and moves the reader on when the test asked for that
    /// to happen during this one.
    fn reached(&self, step: Step) {
        self.with(|screen| {
            screen.steps.push(step);
            if screen.moves_on == Some(step)
                && let Some(open) = screen.open.as_mut()
            {
                (screen.moving)(open);
            }
        });
    }

    /// Changes the thread on screen, as a named change on the view does.
    fn change<R: Default>(&self, step: Step, change: impl FnOnce(&mut OpenThread) -> R) -> R {
        self.reached(step);
        self.with(|screen| screen.open.as_mut().map(change).unwrap_or_default())
    }

    fn read<R: Default>(&self, read: impl FnOnce(&OpenThread) -> R) -> R {
        self.open(read).unwrap_or_default()
    }
}

impl OnScreen for FakeWindow {
    fn is_showing(&self, target: &Target) -> bool {
        self.read(|open| open.target() == *target)
    }
}

impl Desk for FakeWindow {
    fn target(&self) -> Option<Target> {
        self.open(OpenThread::target)
    }

    fn start_loading(&self) -> u64 {
        self.with(|screen| {
            screen.ticket += 1;
            screen.ticket
        })
    }

    fn still_loading(&self, ticket: u64) -> bool {
        self.with(|screen| screen.ticket == ticket)
    }

    fn me(&self, _account_id: AccountId) -> Vec<String> {
        vec!["me@example.com".to_string()]
    }

    fn images_allowed(&self, _senders: &[String]) -> bool {
        false
    }

    fn photos(&self, _senders: &[String]) -> HashMap<String, String> {
        HashMap::new()
    }

    fn is_vip(&self, email: &str) -> bool {
        email == "ann@example.com"
    }

    fn mark_read_delay(&self) -> Option<u32> {
        self.with(|screen| screen.delay)
    }

    fn unread(&self) -> bool {
        self.read(OpenThread::unread)
    }

    fn invitation(&self) -> Option<(String, String)> {
        self.open(|open| {
            open.invitation()
                .map(|(meta, ics)| (meta.id.clone(), ics.to_string()))
        })
        .flatten()
    }

    fn wanting_thumbnails(&self) -> Vec<(String, MessageBody)> {
        self.read(OpenThread::wanting_thumbnails)
    }

    /// The newest open message's text. The window reads the cleaned HTML;
    /// the fixtures have none.
    fn prose(&self) -> Option<(String, Prose)> {
        self.open(|open| {
            let meta = open.messages.iter().rev().find(|meta| {
                open.expanded.contains(&meta.id)
                    && open.bodies.get(&meta.id).is_some_and(Result::is_ok)
            })?;
            let body = open.bodies.get(&meta.id)?.as_ref().ok()?;
            let text = body.text.as_deref().unwrap_or("");
            Some((meta.id.clone(), Prose::read(Body::Text(text))))
        })
        .flatten()
    }

    fn same_writer(&self, message_id: &str) -> String {
        self.read(|open| open.same_writer(message_id))
    }

    fn translation_of(&self, message_id: &str) -> Option<(Option<Language>, bool, bool)> {
        self.open(|open| open.translation_of(message_id)).flatten()
    }

    fn arrived(&self, message_id: &str) -> Option<(MessageBody, HashMap<String, String>)> {
        self.open(|open| open.arrived(message_id)).flatten()
    }

    fn interface_language(&self) -> Option<Language> {
        self.with(|screen| screen.interface)
    }

    fn translation_destination(&self) -> Result<String, String> {
        self.with(|screen| screen.destination.clone())
    }
}

impl Effects for FakeWindow {
    fn stored(
        &self,
        _account_id: AccountId,
        thread_id: String,
    ) -> Answer<'_, Result<Stored, String>> {
        self.reached(Step::Stored);
        let (held, stored) = self.with(|screen| {
            (
                screen.holds.remove(&thread_id),
                screen.stored.get(&thread_id).cloned().unwrap_or_default(),
            )
        });
        Box::pin(async move {
            if let Some(held) = held {
                let _ = held.await;
            }
            Ok(stored)
        })
    }

    fn ensure_thread(
        &self,
        _account_id: AccountId,
        _thread_id: String,
    ) -> Answer<'_, Result<(), String>> {
        self.reached(Step::Ensure);
        Box::pin(async { Ok(()) })
    }

    fn thread_messages(
        &self,
        _account_id: AccountId,
        _thread_id: String,
    ) -> Answer<'_, Result<Vec<MessageMeta>, String>> {
        self.reached(Step::Messages);
        let messages = self.with(|screen| screen.messages.clone());
        Box::pin(async move { Ok(messages) })
    }

    fn bodies(&self, _account_id: AccountId, message_ids: Vec<String>) -> Answer<'_, Fetched> {
        self.reached(Step::Bodies);
        let bodies = self.with(|screen| {
            message_ids
                .into_iter()
                .map(|id| {
                    let body = screen.gmail.get(&id).cloned().ok_or("gone".to_string());
                    (id, body)
                })
                .collect()
        });
        Box::pin(async move {
            Fetched {
                bodies,
                images: HashMap::new(),
            }
        })
    }

    fn thumbnails(
        &self,
        _account_id: AccountId,
        _bodies: Vec<(String, MessageBody)>,
    ) -> Answer<'_, HashMap<String, String>> {
        self.reached(Step::Thumbnails);
        let found = self.with(|screen| screen.thumbnails.clone());
        Box::pin(async move { found })
    }

    fn open_invitation(
        &self,
        _account_id: AccountId,
        _message_id: String,
        _ics: String,
    ) -> Answer<'_, Result<Option<Opened>, String>> {
        self.reached(Step::OpenInvitation);
        let opened = self.with(|screen| screen.invitation.clone());
        Box::pin(async move { opened })
    }

    fn busy(
        &self,
        _account_id: AccountId,
        _invitation: Invitation,
    ) -> Answer<'_, Result<Vec<String>, String>> {
        self.reached(Step::Busy);
        let busy = self.with(|screen| screen.busy.clone());
        Box::pin(async move { busy })
    }

    fn series(
        &self,
        _account_id: AccountId,
        _invitation: Invitation,
    ) -> Answer<'_, Result<Option<String>, String>> {
        self.reached(Step::Series);
        let series = self.with(|screen| screen.series.clone());
        Box::pin(async move { series })
    }

    fn flag_color(
        &self,
        _account_id: AccountId,
        _thread_id: String,
    ) -> Answer<'_, Result<Option<FlagColor>, String>> {
        self.reached(Step::FlagColor);
        let color = self.with(|screen| screen.flag_color);
        Box::pin(async move { Ok(color) })
    }

    fn translate(
        &self,
        _into: Language,
        _pieces: Vec<String>,
    ) -> Answer<'_, Result<Vec<Option<String>>, String>> {
        self.reached(Step::Translate);
        let said = self.with(|screen| screen.translation.clone());
        Box::pin(async move { said })
    }

    fn sleep(&self, _seconds: u32) -> Answer<'_, ()> {
        self.reached(Step::Sleep);
        Box::pin(async {})
    }

    fn show(&self, thread: OpenThread) {
        self.reached(Step::Show);
        self.with(|screen| {
            screen.shown.push(thread.thread_id.clone());
            screen.open = Some(thread);
        });
    }

    fn queued(&self, id: i64) -> Answer<'_, Result<Option<Queued>, String>> {
        self.reached(Step::Queued);
        let found = self.with(|screen| screen.queued.get(&id).cloned());
        Box::pin(async move { Ok(found) })
    }

    fn sender_vip(&self, _vip: bool) {}

    fn messages_arrived(&self, fresh: Vec<MessageMeta>) -> Vec<String> {
        self.change(Step::MessagesArrived, |open| open.take_messages(&fresh))
    }

    fn replace_messages(&self, fresh: Vec<MessageMeta>) -> bool {
        self.change(Step::Replace, |open| open.replace_messages(fresh))
    }

    fn bodies_arrived(&self, fetched: Fetched) {
        self.change(Step::BodiesArrived, |open| {
            open.take_bodies(fetched.bodies, fetched.images)
        });
    }

    fn thumbnails_arrived(&self, found: HashMap<String, String>) {
        self.change(Step::ThumbnailsArrived, |open| {
            open.thumbnails.extend(found)
        });
    }

    fn render_buttons(&self) {
        self.reached(Step::Buttons);
    }

    fn clear(&self) {
        self.reached(Step::Clear);
        self.with(|screen| screen.open = None);
    }

    fn show_invitation(&self, showing: Option<Showing>) {
        self.reached(Step::ShowInvitation);
        self.with(|screen| {
            screen
                .invitations
                .push(showing.map(|showing| showing.invitation.uid))
        });
    }

    fn offer_gnome(&self, _account_id: AccountId) {
        self.reached(Step::OfferGnome);
    }

    fn clashes(&self, _uid: String, _busy: Vec<String>) {
        self.reached(Step::Clashes);
    }

    fn series_known(&self, _uid: String, line: String) {
        self.reached(Step::SeriesKnown);
        self.with(|screen| screen.series_lines.push(line));
    }

    fn start_engines(&self) {
        self.reached(Step::Engines);
    }

    fn translation_card(&self, card: Card) {
        self.reached(Step::Card);
        self.with(|screen| screen.cards.push(card));
    }

    fn translated(&self, message_id: String, translation: Translation) {
        let card = Card::Done {
            from: translation.from,
            cut: translation.cut,
            shown: true,
        };
        self.change(Step::Translated, |open| {
            open.translations.insert(message_id, translation);
        });
        self.with(|screen| screen.cards.push(card));
    }

    fn turn_translation(&self, message_id: &str) -> bool {
        let turned = self.change(Step::Turn, |open| open.turn_translation(message_id));
        let Some((from, cut, shown)) = turned else {
            return false;
        };
        self.with(|screen| screen.cards.push(Card::Done { from, cut, shown }));
        true
    }

    fn engine_answered(&self, message_id: String, read: Read) -> bool {
        self.change(Step::EngineAnswered, |open| {
            open.take_engine_answer(message_id, read)
        })
    }

    fn set_flag_color(&self, color: Option<FlagColor>) {
        self.change(Step::SetFlag, |open| open.flag_color = color);
    }

    fn unsent_changed(&self, unsent: Unsent) {
        self.change(Step::Unsent, |open| open.take_unsent(unsent));
    }

    fn mark_read(&self, target: Target) {
        self.reached(Step::MarkRead);
        self.with(|screen| screen.marked.push(target));
    }

    fn toast(&self, text: String) {
        self.with(|screen| screen.toasts.push(text));
    }
}
