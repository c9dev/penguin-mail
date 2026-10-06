//! The calendar run: every read the calendar view makes from the local
//! copy, through two ports: [`Desk`] for what the view has on screen and
//! [`Effects`] for the copy and the named changes to the page.
//!
//! The view fills several parts of its page at once, each from its own
//! read: the three carousel pages, the calendar list and mini month, the
//! "Waiting for your answer" cards, the narrow list, the search results,
//! the note about older events. Any of those reads can answer after the
//! person moved on, and two reads for the same part can answer out of
//! order. One rule decides whether an answer lands, in [`CalendarRun`]'s
//! [`Screen`]: the read must be the newest one started for its part (its
//! [`Place`]), and the part must still show what the read was asked for
//! ([`Shows`]), such as the same week. Each read starts with a [`Ticket`]
//! and reaches the page only through a [`Wanted`] for it, so no flow can
//! skip the question.
//!
//! The run also owns what the reads share: the occurrence waiting to open
//! once its page has drawn, how far the narrow list reaches, whether a
//! grid page has been scrolled to its first hour, and whether a calendar
//! list read owes the pages a refill. It makes a move, a delete or an
//! editor save as one event change ([`CalendarRun::change`]), and the
//! next-event card at the foot of the mail sidebar reads under the same
//! rule ([`NextCard`]).
//!
//! Nothing here touches GTK. The view is one adapter behind the ports
//! and the tests are another.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use chrono::{DateTime, NaiveDate, Utc};
use mailrs_domain::calendar::{Event, Occurrence};
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_domain::translate::{fill, with_reason};
use mailrs_sync::calendar_copy::Listed;
use mailrs_sync::calendar_copy::event_change::{EventChange, Undo};
use mailrs_sync::{Permitted, Waiting};

use super::block::{EventKey, key_of};
use super::range::{self, Range, ViewKind};
use super::next::{self, NextUp};
use super::{layout, scope, time_grid};
pub use crate::wanted::Answer;
use crate::wanted::{Screen, Wanted};

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod next_tests;
#[cfg(test)]
mod change_tests;

/// What `occurrences` returns at most, so a read that comes back this
/// full is known to have been cut (`store::calendar`'s `MOST_EVENTS`).
pub const MOST_EVENTS: usize = 500;

/// A carousel page, by the number it was built with. A page keeps its
/// number while the carousel moves it to another range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PageId(pub u64);

/// A part of the page a read fills. Each place has one read that counts:
/// the newest one started for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Place {
    /// One carousel page's events.
    Page(PageId),
    /// The hour a grid page scrolls to, which waits for the grid's first
    /// layout.
    Scroll(PageId),
    /// The calendar list and the mini month.
    Sidebar,
    /// "Waiting for your answer".
    Waiting,
    /// The narrow list, with its earlier and later days.
    List,
    /// The search results.
    Search,
    /// The event Show in Calendar, a Waiting card or the next-event card
    /// asked to open.
    Open,
    /// The note over the view about older events.
    Older,
    /// The next-event card at the foot of the mail sidebar.
    Next,
}

/// What a place shows, which it must still show when a read's answer
/// arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shows {
    /// A page's range.
    Range(Range),
    /// The words in the search field, trimmed.
    Text(String),
    /// The calendar is on screen.
    Calendar,
    /// The page has gone, or the calendar is off screen.
    Away,
    /// A place whose newest read is the only condition.
    Anything,
}

/// One read: the place it fills, its number, and what the place showed
/// when it started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub place: Place,
    number: u64,
    shows: Shows,
}

/// The newest read started for each place.
#[derive(Debug, Default)]
struct Ledger {
    last: Cell<u64>,
    newest: RefCell<HashMap<Place, u64>>,
}

impl Ledger {
    fn mint(&self) -> u64 {
        let number = self.last.get() + 1;
        self.last.set(number);
        number
    }

    /// Starts a read for `place`, which every earlier read for it gives
    /// way to.
    fn start(&self, place: Place, shows: Shows) -> Ticket {
        let number = self.mint();
        self.newest.borrow_mut().insert(place, number);
        Ticket { place, number, shows }
    }

    /// The newest read for `place`, for a read that adds to what that
    /// one filled, such as the list's earlier days.
    fn current(&self, place: Place) -> Ticket {
        let number = self.newest.borrow().get(&place).copied().unwrap_or(0);
        Ticket { place, number, shows: Shows::Anything }
    }

    fn is_newest(&self, ticket: &Ticket) -> bool {
        self.newest.borrow().get(&ticket.place).copied().unwrap_or(0) == ticket.number
    }

    /// Forgets a page the view took away.
    fn forget(&self, page: PageId) {
        let mut newest = self.newest.borrow_mut();
        newest.remove(&Place::Page(page));
        newest.remove(&Place::Scroll(page));
    }
}

/// What an ask for older events came to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Older {
    /// The copy already reaches back far enough, or the ask failed in a
    /// way the person cannot act on; the view has what there is.
    Held,
    /// A fetch ran, so the view may lack events it has not drawn yet.
    Loaded,
    /// A fetch was due and the computer has no network.
    Offline,
}

/// Why a fetch of older events did not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unreached {
    /// The provider could not be reached.
    Offline,
    /// Anything else, in words for the log.
    Failed(String),
}

/// How far the narrow list reaches, and the reads that extend it.
#[derive(Debug)]
struct ListSpan {
    /// The earliest day the list holds.
    first: Cell<NaiveDate>,
    /// The latest day the list holds, `None` until its first read lands.
    last: Cell<Option<NaiveDate>>,
    /// The list reaches `range::earliest_agenda_day`.
    exhausted: Cell<bool>,
    /// A read of earlier days is under way.
    earlier: Cell<bool>,
    /// A read of later days is under way.
    later: Cell<bool>,
}

/// Each account's calendars, and the occurrences on the shown ones over
/// the mini month.
pub type SidebarRead = (Vec<Listed>, Vec<Occurrence>);

/// Work the view runs on its main loop.
pub type Work = Pin<Box<dyn Future<Output = ()>>>;

/// What the run reads from the view. Every method answers from what the
/// view already holds, so a test fills one in without a widget.
pub trait Desk {
    /// The accounts whose calendars the view shows.
    fn accounts(&self) -> Vec<AccountId>;
    /// The accounts whose invitations can wait for an answer.
    fn waiting_accounts(&self) -> Vec<AccountId>;
    /// What `place` shows now.
    fn shows(&self, place: Place) -> Shows;
    /// The carousel's pages and their ranges, the one on screen first.
    fn pages(&self) -> Vec<(PageId, Range)>;
    /// Whether the narrow list is the view on screen.
    fn showing_list(&self) -> bool;
    /// The month the mini month shows.
    fn mini(&self) -> Range;
    /// The first moment the view shows, which the copy has to reach.
    fn wanted_from(&self) -> EpochMillis;
    /// The narrow list's first window, around the day the view is on.
    fn list_window(&self) -> (NaiveDate, NaiveDate);
    fn today(&self) -> NaiveDate;
    fn now(&self) -> EpochMillis;
    /// Whether the computer has a network.
    fn network(&self) -> bool;
}

/// What the run asks of the copy and the view. A test answers with what
/// it likes and records the rest.
pub trait Effects {
    /// The occurrences on the shown calendars from `from` to `to`.
    fn occurrences(
        &self,
        accounts: Vec<AccountId>,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Answer<'_, Result<Vec<Occurrence>, String>>;
    /// Each account's calendars, and the occurrences on the shown ones
    /// from `from` to `to`, for the mini month's busy days.
    fn sidebar(
        &self,
        accounts: Vec<AccountId>,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Answer<'_, Result<SidebarRead, String>>;
    /// The invitations waiting for an answer.
    fn waiting(&self, accounts: Vec<AccountId>, now: EpochMillis) -> Answer<'_, Result<Vec<Waiting>, String>>;
    /// The events on the shown calendars that mention `text`.
    fn search(
        &self,
        accounts: Vec<AccountId>,
        text: String,
        now: EpochMillis,
    ) -> Answer<'_, Result<Vec<Occurrence>, String>>;
    /// The event `key` names, `None` once the copy has lost it.
    fn event(&self, key: EventKey) -> Answer<'_, Result<Option<Event>, String>>;
    /// Whether the copy lacks events back to `from`.
    fn older_missing(&self, accounts: Vec<AccountId>, from: EpochMillis) -> Answer<'_, Result<bool, String>>;
    /// Fetches the events back to `from` into the copy.
    fn reach_back(&self, accounts: Vec<AccountId>, from: EpochMillis) -> Answer<'_, Result<(), Unreached>>;
    /// Waits until the page's grid has a height to scroll in.
    fn laid_out(&self, page: PageId) -> Answer<'_, ()>;
    /// Waits until the page has been laid out after the latest change.
    fn after_layout(&self, page: PageId) -> Answer<'_, ()>;
    /// Waits for the main loop to have nothing else to do.
    fn idle(&self) -> Answer<'_, ()>;
    /// Runs `work` on the main loop, beside whatever else runs.
    fn spawn(&self, work: Work);

    /// Draws a page's events, in place of what it showed.
    fn draw_page(&self, page: PageId, found: Vec<Occurrence>);
    /// Scrolls a grid page so `hour` is at its top.
    fn scroll_to(&self, page: PageId, hour: f64);
    /// Opens the popover of `o` on its block in `page`. `false` when the
    /// page has no block for it.
    fn open_popover(&self, page: PageId, o: &Occurrence) -> bool;
    /// Redraws the calendar list and the mini month.
    fn draw_sidebar(&self, listed: Vec<Listed>, busy: Vec<Occurrence>, mini: Range);
    /// Redraws "Waiting for your answer".
    fn draw_waiting(&self, waiting: Vec<Waiting>);
    /// Fills the narrow list with `first` to `last`, replacing it.
    fn draw_list(&self, found: Vec<Occurrence>, first: NaiveDate, last: NaiveDate);
    /// Puts `first` to `last` above what the list holds. `listed_from`
    /// is where the list began, which an event running across it already
    /// shows from.
    fn prepend_list(&self, found: Vec<Occurrence>, first: NaiveDate, last: NaiveDate, listed_from: EpochMillis);
    /// Puts `first` to `last` below what the list holds.
    fn append_list(&self, found: Vec<Occurrence>, first: NaiveDate, last: NaiveDate);
    /// Ends the list with the line that says nothing older loads.
    fn no_earlier(&self);
    /// Shows the search results.
    fn draw_results(&self, found: Vec<Occurrence>);
    /// Shows, changes or hides the note about older events.
    fn older_note(&self, saying: Option<Older>);
    /// Moves the view to the range around `day`.
    fn go_to(&self, day: NaiveDate);
    /// Starts a calendar sync of every account.
    fn refresh(&self);

    /// What the person chose about `change`: the copy's question over the
    /// page about `shown`, with `when` naming a new time, or what the copy
    /// settles on when nothing needs asking. `None` for Cancel.
    fn choose(
        &self,
        account_id: AccountId,
        change: EventChange,
        shown: Event,
        when: Option<String>,
    ) -> Answer<'_, Result<Option<scope::Answer>, String>>;
    /// Writes `change` through the copy with what the person answered.
    /// `Some` names the held change an Undo toast can take back.
    fn write(
        &self,
        account_id: AccountId,
        change: EventChange,
        answer: scope::Answer,
        undo: Undo,
    ) -> Answer<'_, Result<Permitted<Option<u64>>, String>>;
    /// Puts up the Undo toast for the held change `held`.
    fn offer_undo(&self, said: String, held: u64);
    /// Explains that the change needs the calendar permission the account
    /// withheld, and offers to ask for it.
    fn needs_permission(&self, account_id: AccountId);
    /// Says something at the bottom of the window.
    fn toast(&self, text: String);
    /// Sends the account's queued calendar changes now.
    fn push(&self, account_id: AccountId);
}

/// A change of an event the person made in the view: a move, an edit or
/// a delete.
#[derive(Debug, Clone)]
pub struct Changing {
    pub account_id: AccountId,
    pub change: EventChange,
    /// The change to write instead when the person keeps the old time:
    /// the other edits, at the time the event had.
    pub kept_time: Option<EventChange>,
    /// The event the question is about.
    pub shown: Event,
    /// The new time, in words, for a change that moves the event.
    pub when: Option<String>,
    /// Whether an Undo toast holds the change.
    pub undo: Undo,
    /// What the Undo toast says, with `{title}` for the event's title.
    pub said: String,
    /// What a failure says, with `{reason}` for why.
    pub failed: String,
}

/// What a change came to, for what the view does around it: a dragged
/// block springs back unless it is `Done`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Canceled,
    NeedsPermission,
    Failed,
}

/// The calendar run, and the one way the view reads the copy.
pub struct CalendarRun {
    desk: Rc<dyn Desk>,
    effects: Rc<dyn Effects>,
    ledger: Ledger,
    /// The occurrence to open once the page that holds it has drawn: the
    /// event and the occurrence's own start, since every occurrence of an
    /// unsplit series shares one key.
    pending: RefCell<Option<(EventKey, EpochMillis)>>,
    /// The range each grid page was last scrolled for, so a refill of the
    /// same range leaves the scroll where the person put it.
    scrolled: RefCell<HashMap<PageId, Range>>,
    list: ListSpan,
    /// A calendar list read asked to refill the pages once it lands. A
    /// newer read drops the older one's answer, so the refill carries over
    /// to whichever read lands.
    fill_owed: Cell<bool>,
    /// A Refresh press has a calendar sync running.
    refreshing: Cell<bool>,
}

type Want<'a> = Wanted<'a, dyn Effects, Ticket>;

impl Screen<Ticket> for CalendarRun {
    /// The rule for every late answer: the newest read for its place, and
    /// the place still shows what it was asked for.
    fn is_showing(&self, ticket: &Ticket) -> bool {
        self.ledger.is_newest(ticket) && self.desk.shows(ticket.place) == ticket.shows
    }
}

impl CalendarRun {
    pub fn new(desk: Rc<dyn Desk>, effects: Rc<dyn Effects>) -> CalendarRun {
        let today = desk.today();
        CalendarRun {
            desk,
            effects,
            ledger: Ledger::default(),
            pending: RefCell::new(None),
            scrolled: RefCell::new(HashMap::new()),
            list: ListSpan {
                first: Cell::new(today),
                last: Cell::new(None),
                exhausted: Cell::new(false),
                earlier: Cell::new(false),
                later: Cell::new(false),
            },
            fill_owed: Cell::new(false),
            refreshing: Cell::new(false),
        }
    }

    fn wanted(&self, ticket: Ticket) -> Want<'_> {
        Wanted::new(self as &dyn Screen<Ticket>, &*self.effects, ticket)
    }

    /// Starts a read for `place`, taking what it shows now.
    fn start(&self, place: Place) -> Ticket {
        self.ledger.start(place, self.desk.shows(place))
    }

    fn spawn(self: &Rc<Self>, work: impl Future<Output = ()> + 'static) {
        self.effects.spawn(Box::pin(work));
    }

    /// A number for a page the view builds.
    pub fn new_page(&self) -> PageId {
        PageId(self.ledger.mint())
    }

    /// Forgets pages the view took away.
    pub fn forget_pages(&self, pages: impl IntoIterator<Item = PageId>) {
        let mut scrolled = self.scrolled.borrow_mut();
        for page in pages {
            self.ledger.forget(page);
            scrolled.remove(&page);
        }
    }

    /// The occurrence waiting to open, in the flat shape
    /// [`super::shown::keep`] matches against.
    pub fn pending(&self) -> Option<(AccountId, String, String, EpochMillis)> {
        self.pending
            .borrow()
            .clone()
            .map(|((account_id, calendar, id), start)| (account_id, calendar, id, start))
    }

    /// Opens `key`'s occurrence at `start` once its page draws, for a
    /// search result the view moves to.
    pub fn open_once_drawn(&self, key: EventKey, start: EpochMillis) {
        self.pending.replace(Some((key, start)));
    }

    // ---- The pages --------------------------------------------------------

    /// Reads every page again, the one on screen first, and the list when
    /// it shows.
    pub fn fill_all(self: &Rc<Self>) {
        for (page, _) in self.desk.pages() {
            self.fill(page);
        }
        if self.desk.showing_list() {
            self.fill_list();
        }
    }

    /// Reads one page's range and draws it.
    pub fn fill(self: &Rc<Self>, page: PageId) {
        let ticket = self.start(Place::Page(page));
        let Shows::Range(range) = ticket.shows else { return };
        let accounts = self.desk.accounts();
        let this = Rc::clone(self);
        self.spawn(async move { this.fill_page(page, range, accounts, ticket).await });
    }

    async fn fill_page(&self, page: PageId, range: Range, accounts: Vec<AccountId>, ticket: Ticket) {
        let wanted = self.wanted(ticket);
        let (from, to) = range.span(&chrono::Local);
        let Some(found) = wanted
            .ask(|e| e.occurrences(accounts, from, to), "could not read the calendar")
            .await
        else {
            return;
        };
        if found.len() >= MOST_EVENTS {
            tracing::info!(
                first = %range.first,
                days = range.days,
                "the calendar range holds more events than one read shows"
            );
        }
        let opens = self.opens_on(page, &found);
        let hour = self.scroll_hour(page, range, &found, opens.as_ref());
        if wanted.on_screen(|e| e.draw_page(page, found)).is_none() {
            return;
        }
        if hour.is_some() {
            self.scrolled.borrow_mut().insert(page, range);
        }
        match hour {
            Some(hour) => self.scroll(page, hour, opens).await,
            None => {
                if let Some(opens) = opens {
                    // The block has no size until the page lays it out,
                    // and a popover needs one to point at.
                    if wanted.wait(|e| e.idle()).await.is_some() {
                        self.open_popover(&wanted, page, opens);
                    }
                }
            }
        }
    }

    /// The occurrence waiting to open, when `page` is the one on screen
    /// and `found` holds it.
    fn opens_on(&self, page: PageId, found: &[Occurrence]) -> Option<Occurrence> {
        let on_screen = self.desk.pages().first().is_some_and(|(id, _)| *id == page);
        if !on_screen {
            return None;
        }
        let (key, start) = self.pending.borrow().clone()?;
        found.iter().find(|o| key_of(o) == key && o.start == start).cloned()
    }

    /// The hour a grid page scrolls to after drawing `found`: the hour of
    /// an event about to open, however far the page was scrolled before;
    /// otherwise its first event's on a range not scrolled for yet. A
    /// month has no hours.
    fn scroll_hour(&self, page: PageId, range: Range, found: &[Occurrence], opens: Option<&Occurrence>) -> Option<f64> {
        if range.kind == ViewKind::Month {
            return None;
        }
        let opening = opens
            .filter(|o| !time_grid::in_strip(o))
            .map(|o| layout::open_hour(o.start, &chrono::Local));
        match opening {
            Some(hour) => Some(hour),
            None if self.scrolled.borrow().get(&page) != Some(&range) => Some(first_hour(found, range)),
            None => None,
        }
    }

    /// Scrolls `page` to `hour` once its grid has a height, then opens
    /// `opens`. A later scroll of the same page replaces one still
    /// waiting, such as a hidden page's first scroll that Show in
    /// Calendar's overtakes.
    async fn scroll(&self, page: PageId, hour: f64, opens: Option<Occurrence>) {
        let wanted = self.wanted(self.start(Place::Scroll(page)));
        if wanted.wait(|e| e.laid_out(page)).await.is_none() {
            return;
        }
        wanted.on_screen(|e| e.scroll_to(page, hour));
        let Some(opens) = opens else { return };
        // The popover measures the block when it opens, so it waits until
        // the grid has laid the block out at its new place.
        if wanted.wait(|e| e.after_layout(page)).await.is_some() {
            self.open_popover(&wanted, page, opens);
        }
    }

    /// Opens the popover the person asked for, and lets it go once open.
    /// Until then it stays waiting, so a refill that overtook this one
    /// opens it instead.
    fn open_popover(&self, wanted: &Want<'_>, page: PageId, o: Occurrence) {
        let (key, start) = (key_of(&o), o.start);
        if wanted.on_screen(|e| e.open_popover(page, &o)) != Some(true) {
            return;
        }
        let mut pending = self.pending.borrow_mut();
        if pending.as_ref() == Some(&(key, start)) {
            *pending = None;
        }
    }

    // ---- Moving -----------------------------------------------------------

    /// Reads what a move of the view to another range shows: the pages,
    /// the list, the mini month, and the older events the range needs.
    /// An open still reading its event gives way to the move.
    pub fn range_moved(self: &Rc<Self>) {
        self.ledger.start(Place::Open, Shows::Anything);
        self.fill_all();
        self.read_sidebar(false);
        self.reach_current();
    }

    /// Goes to the day `start` falls on and opens that occurrence's
    /// popover once the range has drawn. Nothing happens when the copy no
    /// longer holds the event, or the calendar left the screen first.
    pub fn open(self: &Rc<Self>, key: EventKey, start: EpochMillis) {
        let ticket = self.ledger.start(Place::Open, Shows::Calendar);
        let this = Rc::clone(self);
        self.spawn(async move { this.open_event(key, start, ticket).await });
    }

    async fn open_event(&self, key: EventKey, start: EpochMillis, ticket: Ticket) {
        let wanted = self.wanted(ticket);
        let asked = key.clone();
        let Some(Some(event)) = wanted
            .ask(|e| e.event(asked), "could not read the event to open")
            .await
        else {
            return;
        };
        let day = date_of(start, event.all_day);
        wanted.on_screen(|e| {
            self.pending.replace(Some((key, start)));
            e.go_to(day);
        });
    }

    // ---- The sidebar ------------------------------------------------------

    /// Reads the calendar list and the mini month's busy days, and with
    /// `then_fill` the pages after them, since a calendar's colour or
    /// shown flag may have changed.
    pub fn read_sidebar(self: &Rc<Self>, then_fill: bool) {
        if then_fill {
            self.fill_owed.set(true);
        }
        let ticket = self.start(Place::Sidebar);
        let mini = self.desk.mini();
        let accounts = self.desk.accounts();
        let this = Rc::clone(self);
        self.spawn(async move { this.sidebar(mini, accounts, ticket).await });
    }

    async fn sidebar(self: Rc<Self>, mini: Range, accounts: Vec<AccountId>, ticket: Ticket) {
        let wanted = self.wanted(ticket);
        let (from, to) = mini.span(&chrono::Local);
        let Some(read) = wanted.wait(|e| e.sidebar(accounts, from, to)).await else {
            return;
        };
        match read {
            Ok((listed, busy)) => {
                wanted.on_screen(|e| e.draw_sidebar(listed, busy, mini));
            }
            Err(err) => tracing::warn!(%err, "could not read the calendars"),
        }
        if self.fill_owed.replace(false) {
            self.fill_all();
        }
    }

    /// Reads "Waiting for your answer" again, and nothing else, so it can
    /// run while the view opens an event.
    pub fn refresh_waiting(self: &Rc<Self>) {
        let ticket = self.start(Place::Waiting);
        let accounts = self.desk.waiting_accounts();
        let now = self.desk.now();
        let this = Rc::clone(self);
        self.spawn(async move {
            let wanted = this.wanted(ticket);
            if let Some(waiting) = wanted
                .ask(|e| e.waiting(accounts, now), "could not read what is waiting for an answer")
                .await
            {
                wanted.on_screen(|e| e.draw_waiting(waiting));
            }
        });
    }

    /// Reads the calendars and the ranges on screen again, as after the
    /// copy changed.
    pub fn reload(self: &Rc<Self>) {
        self.read_sidebar(true);
        self.refresh_waiting();
    }

    /// The Refresh action: starts a calendar sync of every account, unless
    /// one it started is still running.
    pub fn refresh_now(&self) {
        if self.refreshing.replace(true) {
            return;
        }
        self.effects.refresh();
    }

    /// The sync a Refresh press started has ended, whatever it found.
    pub fn refresh_done(&self) {
        self.refreshing.set(false);
    }

    // ---- Changing events ----------------------------------------------------

    /// Asks the copy's question about a change, writes it, and reads the
    /// view again. A held change gets its Undo toast; one written for good
    /// goes out at once. The question, the toast and the permission prompt
    /// belong to the window rather than to a part of the page, so this
    /// waits for no ticket: the person stays to answer the question, and a
    /// write they made counts wherever they have moved since.
    pub async fn change(self: &Rc<Self>, changing: Changing) -> Outcome {
        let Changing { account_id, change, kept_time, shown, when, undo, said, failed } = changing;
        let title = shown.title.clone();
        let asked = self.effects.choose(account_id, change.clone(), shown, when).await;
        let answer = match asked {
            Ok(Some(answer)) => answer,
            Ok(None) => return Outcome::Canceled,
            Err(err) => {
                self.effects.toast(with_reason(&failed, &err, &[]));
                return Outcome::Failed;
            }
        };
        let change = match (answer.keep_time, kept_time) {
            (true, Some(kept)) => kept,
            _ => change,
        };
        match self.effects.write(account_id, change, answer, undo).await {
            Ok(Permitted::Done(held)) => {
                self.reload();
                match held {
                    Some(held) => self.effects.offer_undo(fill(&said, &[("title", &title)]), held),
                    None if undo == Undo::Skip => self.effects.push(account_id),
                    None => {}
                }
                Outcome::Done
            }
            Ok(Permitted::NeedsPermission) => {
                self.effects.needs_permission(account_id);
                Outcome::NeedsPermission
            }
            Err(err) => {
                self.effects.toast(with_reason(&failed, &err, &[]));
                Outcome::Failed
            }
        }
    }

    // ---- The list -----------------------------------------------------------

    /// Reads the narrow list's first window, replacing whatever it held.
    pub fn fill_list(self: &Rc<Self>) {
        let ticket = self.start(Place::List);
        let (first, last) = self.desk.list_window();
        self.list.first.set(first);
        self.list.last.set(None);
        self.list.exhausted.set(false);
        self.list.earlier.set(false);
        self.list.later.set(false);
        let accounts = self.desk.accounts();
        let this = Rc::clone(self);
        self.spawn(async move { this.list_window(first, last, accounts, ticket).await });
    }

    async fn list_window(&self, first: NaiveDate, last: NaiveDate, accounts: Vec<AccountId>, ticket: Ticket) {
        let wanted = self.wanted(ticket);
        let (from, to) = day_span(first, last);
        let Some(found) = wanted
            .ask(|e| e.occurrences(accounts, from, to), "could not read the calendar")
            .await
        else {
            return;
        };
        if wanted.on_screen(|e| e.draw_list(found, first, last)).is_some() {
            self.list.last.set(Some(last));
        }
    }

    /// Loads the 30 days before what the list holds, once the reader
    /// scrolls to its top, fetching the months the copy lacks first. Stops
    /// at `range::earliest_agenda_day`.
    pub fn load_earlier(self: &Rc<Self>) {
        if self.list.earlier.get() || self.list.exhausted.get() {
            return;
        }
        let cutoff = range::earliest_agenda_day(self.desk.today());
        let held_from = self.list.first.get();
        let Some(last) = held_from.pred_opt() else { return };
        if last < cutoff {
            self.list.exhausted.set(true);
            self.effects.no_earlier();
            return;
        }
        let first = range::earlier(held_from).max(cutoff);
        self.list.earlier.set(true);
        let ticket = self.ledger.current(Place::List);
        let accounts = self.desk.accounts();
        let this = Rc::clone(self);
        self.spawn(async move { this.list_earlier(first, last, cutoff, accounts, ticket).await });
    }

    async fn list_earlier(
        &self,
        first: NaiveDate,
        last: NaiveDate,
        cutoff: NaiveDate,
        accounts: Vec<AccountId>,
        ticket: Ticket,
    ) {
        let wanted = self.wanted(ticket);
        let (from, to) = day_span(first, last);
        // The copy may not reach this far back yet. Offline, the list
        // stays where it is, so the next scroll to the top asks again.
        if self.reach_back(from).await == Older::Offline {
            self.list.earlier.set(false);
            return;
        }
        let found = wanted.wait(|e| e.occurrences(accounts, from, to)).await;
        self.list.earlier.set(false);
        let found = match found {
            Some(Ok(found)) => found,
            Some(Err(err)) => {
                tracing::warn!(%err, "could not read the calendar");
                return;
            }
            None => return,
        };
        // `to` is where the list's earlier reads began.
        wanted.on_screen(|e| {
            e.prepend_list(found, first, last, to);
            self.list.first.set(first);
            if first <= cutoff {
                self.list.exhausted.set(true);
                e.no_earlier();
            }
        });
    }

    /// Reads the 30 days after what the list holds once the reader scrolls
    /// near its end, up to `range::latest_agenda_day`.
    pub fn load_later(self: &Rc<Self>) {
        let Some(held_to) = self.list.last.get() else { return };
        if self.list.later.get() {
            return;
        }
        let Some((first, last)) = range::agenda_later(held_to, self.desk.today()) else { return };
        self.list.later.set(true);
        let ticket = self.ledger.current(Place::List);
        let accounts = self.desk.accounts();
        let this = Rc::clone(self);
        self.spawn(async move {
            let wanted = this.wanted(ticket);
            let (from, to) = day_span(first, last);
            let Some(found) = wanted.wait(|e| e.occurrences(accounts, from, to)).await else {
                return;
            };
            this.list.later.set(false);
            match found {
                Ok(found) => {
                    wanted.on_screen(|e| e.append_list(found, first, last));
                    this.list.last.set(Some(last));
                }
                Err(err) => tracing::warn!(%err, "could not read the calendar"),
            }
        });
    }

    // ---- The search -------------------------------------------------------

    /// Lists the events that mention `text`, soonest first. Empty text
    /// drops any search still reading.
    pub fn search(self: &Rc<Self>, text: &str) {
        let ticket = self.start(Place::Search);
        if text.is_empty() {
            return;
        }
        let text = text.to_string();
        let accounts = self.desk.accounts();
        let now = self.desk.now();
        let this = Rc::clone(self);
        self.spawn(async move {
            let wanted = this.wanted(ticket);
            if let Some(found) = wanted
                .ask(|e| e.search(accounts, text, now), "could not search the calendar")
                .await
            {
                wanted.on_screen(|e| e.draw_results(found));
            }
        });
    }

    // ---- Older events -----------------------------------------------------

    /// Makes sure the copy holds what the view shows now, fetching an
    /// older range when the person went back further than the copy
    /// reaches, and reads the pages again once it arrives.
    pub fn reach_current(self: &Rc<Self>) {
        let from = self.desk.wanted_from();
        let this = Rc::clone(self);
        self.spawn(async move {
            if this.reach_back(from).await == Older::Loaded && this.desk.wanted_from() == from {
                this.fill_all();
            }
        });
    }

    /// Asks the copy for the events back to `from`, with the note over
    /// the view saying so while it waits. The answer comes back whatever
    /// the screen shows; only the note gives way to a newer ask.
    async fn reach_back(&self, from: EpochMillis) -> Older {
        let wanted = self.wanted(self.start(Place::Older));
        let note = |saying| {
            wanted.on_screen(|e| e.older_note(saying));
        };
        let accounts = self.desk.accounts();
        let asked = accounts.clone();
        match wanted.anyway(|e| e.older_missing(asked, from)).await {
            Ok(true) => {}
            Ok(false) => {
                note(None);
                return Older::Held;
            }
            Err(err) => {
                tracing::warn!(%err, "could not tell whether the copy reaches back far enough");
                note(None);
                return Older::Held;
            }
        }
        if !self.desk.network() {
            note(Some(Older::Offline));
            return Older::Offline;
        }
        note(Some(Older::Loaded));
        match wanted.anyway(|e| e.reach_back(accounts, from)).await {
            Ok(()) => {
                note(None);
                Older::Loaded
            }
            Err(Unreached::Offline) => {
                note(Some(Older::Offline));
                Older::Offline
            }
            Err(Unreached::Failed(err)) => {
                tracing::warn!(%err, "could not fetch older calendar events");
                note(None);
                Older::Held
            }
        }
    }
}

/// Local midnight of `first` to local midnight after `last`, for a read
/// covering whole days.
pub fn day_span(first: NaiveDate, last: NaiveDate) -> (EpochMillis, EpochMillis) {
    let (from, _) = Range::around(ViewKind::Day, first).span(&chrono::Local);
    let (_, to) = Range::around(ViewKind::Day, last).span(&chrono::Local);
    (from, to)
}

/// The date an event starts on: its own UTC date when it lasts all day,
/// the local date otherwise.
pub fn date_of(at: EpochMillis, all_day: bool) -> NaiveDate {
    let utc = DateTime::<Utc>::from_timestamp_millis(at).unwrap_or_default();
    match all_day {
        true => utc.date_naive(),
        false => utc.with_timezone(&chrono::Local).date_naive(),
    }
}

/// The hour the grid opens at for `range`: 08:00, or earlier when a
/// timed event starts earlier on one of its days.
fn first_hour(found: &[Occurrence], range: Range) -> f64 {
    let (from, to) = range.span(&chrono::Local);
    let starts: Vec<f64> = found
        .iter()
        .filter(|o| !time_grid::in_strip(o) && o.start >= from && o.start < to)
        .filter_map(|o| {
            let local = DateTime::<Utc>::from_timestamp_millis(o.start)?.with_timezone(&chrono::Local);
            let midnight = local.date_naive().and_hms_opt(0, 0, 0)?;
            Some(layout::wall_offset(o.start, midnight, &chrono::Local))
        })
        .collect();
    layout::first_hour(&starts)
}

/// What the next-event card reads: the occurrences on the shown calendars
/// of every account, and each calendar's colour by account and id.
pub type CardRead = (Vec<Occurrence>, HashMap<(AccountId, String), String>);

/// The next-event card at the foot of the mail sidebar, as the window
/// holds it. A test answers with what it likes and records the rest.
pub trait Card {
    fn now(&self) -> EpochMillis;
    /// The occurrences from `from` to `to`, with the calendars' colours.
    fn read(&self, from: EpochMillis, to: EpochMillis) -> Answer<'_, Result<CardRead, String>>;
    /// Puts the next event on the card with its colour, or takes the card
    /// down.
    fn show(&self, next: Option<(NextUp, String)>);
    /// Runs `work` on the main loop.
    fn spawn(&self, work: Work);
}

/// The next-event card's reads, under the calendar run's rule: the minute
/// timer and a sync can each start one, and only the newest writes the
/// card.
pub struct NextCard {
    card: Rc<dyn Card>,
    ledger: Ledger,
}

impl Screen<Ticket> for NextCard {
    fn is_showing(&self, ticket: &Ticket) -> bool {
        self.ledger.is_newest(ticket)
    }
}

impl NextCard {
    pub fn new(card: Rc<dyn Card>) -> NextCard {
        NextCard { card, ledger: Ledger::default() }
    }

    /// Reads the next event and puts it on the card, or takes the card
    /// down when nothing is under way or due today within three hours.
    pub fn refresh(self: &Rc<Self>) {
        let ticket = self.ledger.start(Place::Next, Shows::Anything);
        let now = self.card.now();
        let Some(day_ends) = local_midnight_after(now) else { return };
        let this = Rc::clone(self);
        self.card.spawn(Box::pin(async move {
            let wanted = Wanted::new(&*this as &dyn Screen<Ticket>, &*this.card, ticket);
            let Some((found, colours)) = wanted
                .ask(|card| card.read(now, now + next::AHEAD), "could not read the next event")
                .await
            else {
                return;
            };
            let up = next::next_up(&found, now, day_ends).map(|up| {
                let colour = next::colour(up.occurrence(), &colours);
                (up, colour)
            });
            wanted.on_screen(|card| card.show(up));
        }));
    }
}

/// The local midnight after `now`.
fn local_midnight_after(now: EpochMillis) -> Option<EpochMillis> {
    crate::format::local(now)
        .and_then(|today| today.date_naive().succ_opt())
        .and_then(|day| day.and_hms_opt(0, 0, 0))
        .and_then(|midnight| midnight.and_local_timezone(chrono::Local).earliest())
        .map(|midnight| midnight.timestamp_millis())
}
