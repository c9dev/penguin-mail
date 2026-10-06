//! The calendar run with no window: the view's state in memory behind
//! both ports, a copy that answers from a list of events, and a log of
//! what the run asked for, in order.
//!
//! A call that takes time can be held until the test lets it go
//! ([`FakeWindow::hold`]), and something can happen while it waits
//! ([`FakeWindow::during`]), which is how a test makes a second read
//! overtake the first. Work the run spawns waits in a queue that
//! [`FakeWindow::settle`] runs, the way the main loop would.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use chrono::NaiveDate;
use futures::StreamExt;
use futures::channel::oneshot;
use futures::stream::FuturesUnordered;
use mailrs_domain::calendar::{Calendar, Event, Occurrence};
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_sync::calendar_copy::event_change::{EventChange, Undo};
use mailrs_sync::{Permitted, Waiting};
use mailrs_sync::calendar_copy::Listed;

use super::{Answer, CalendarRun, Desk, Effects, Older, PageId, Place, Shows, SidebarRead, Unreached, Work};
use crate::ui::calendar::block::{EventKey, key_of};
use crate::ui::calendar::range::{Range, ViewKind};
use crate::ui::calendar::scope;

/// The one account every fixture belongs to.
pub const ACCOUNT: AccountId = 1;

/// One thing the run asked the window for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    Occurrences,
    Sidebar,
    Waiting,
    Search,
    Event,
    OlderMissing,
    ReachBack,
    LaidOut,
    AfterLayout,
    Idle,
    DrawPage,
    ScrollTo,
    OpenPopover,
    DrawSidebar,
    DrawWaiting,
    DrawList,
    PrependList,
    AppendList,
    NoEarlier,
    DrawResults,
    OlderNote,
    GoTo,
    Refresh,
    Choose,
    Write,
    OfferUndo,
    NeedsPermission,
    Toast,
    Push,
}

/// Something that happens while a call waits for its answer.
type During = Box<dyn FnOnce(&Rc<FakeWindow>, &Rc<CalendarRun>)>;

/// The view the run reads and writes.
pub struct View {
    pub accounts: Vec<AccountId>,
    pub kind: ViewKind,
    pub day: NaiveDate,
    pub today: NaiveDate,
    /// The carousel's pages, the one on screen first.
    pub pages: Vec<(PageId, Range)>,
    pub showing_list: bool,
    pub search_text: String,
    /// Whether the calendar is on screen.
    pub on_screen: bool,
    pub network: bool,
    /// What the copy holds.
    pub copy: Vec<Occurrence>,
    pub waiting: Vec<Waiting>,
    /// Whether the copy lacks older events, and what fetching them says.
    pub older_missing: bool,
    pub reach: Result<(), Unreached>,
    /// The pages that have a block for each event, which the popover
    /// points at. Every drawn event has one unless a test says otherwise.
    pub no_blocks: bool,
    pub steps: Vec<Step>,
    /// The events each page drew, by title, oldest first.
    pub drawn: Vec<(PageId, Vec<String>)>,
    pub scrolls: Vec<(PageId, f64)>,
    pub popovers: Vec<(PageId, EventKey, EpochMillis)>,
    /// The calendar lists drawn, by the number of accounts in each.
    pub sidebars: Vec<usize>,
    pub waiting_drawn: Vec<usize>,
    /// The list's contents, by title, as each change left it.
    pub list: Vec<String>,
    pub list_firsts: Vec<NaiveDate>,
    pub results: Vec<Vec<String>>,
    pub notes: Vec<Option<Older>>,
    pub went_to: Vec<NaiveDate>,
    pub refreshes: usize,
    /// What the question over a change answers.
    pub chosen: Result<Option<scope::Answer>, String>,
    /// What writing a change answers: the Undo it is held for, if any.
    pub written: Result<Permitted<Option<u64>>, String>,
    /// The title each written change left its event with.
    pub writes: Vec<String>,
    /// The Undo toasts offered, with the held change each is for.
    pub undos: Vec<(String, u64)>,
    pub pushed: Vec<AccountId>,
    pub permissions_asked: Vec<AccountId>,
    pub toasts: Vec<String>,
    holds: HashMap<Step, VecDeque<oneshot::Receiver<()>>>,
    during: HashMap<Step, VecDeque<During>>,
    spawned: Vec<Work>,
}

pub struct FakeWindow {
    pub view: RefCell<View>,
    run: RefCell<Weak<CalendarRun>>,
    me: RefCell<Weak<FakeWindow>>,
}

pub fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("a real date")
}

/// The day the fixture's view starts on: a Wednesday.
pub fn fixture_day() -> NaiveDate {
    day(2026, 10, 7)
}

/// A one-hour event on the primary calendar at `hour` on `on`, local
/// time.
pub fn event_at(id: &str, on: NaiveDate, hour: u32) -> Occurrence {
    let start = on
        .and_hms_opt(hour, 0, 0)
        .and_then(|t| t.and_local_timezone(chrono::Local).earliest())
        .map(|t| t.timestamp_millis())
        .expect("a local time");
    Occurrence {
        account_id: ACCOUNT,
        event: Arc::new(Event {
            calendar: "primary".to_string(),
            id: id.to_string(),
            title: id.to_string(),
            start,
            end: start + 3_600_000,
            ..Event::default()
        }),
        start,
        end: start + 3_600_000,
    }
}

/// The three ranges around `day`: on screen, before, after.
fn ranges(kind: ViewKind, day: NaiveDate) -> [Range; 3] {
    let current = Range::around(kind, day);
    [current, current.previous(), current.next()]
}

impl FakeWindow {
    /// A week view on [`fixture_day`] with one account, an empty copy,
    /// and three pages numbered 1 to 3, the one on screen first.
    pub fn new() -> Rc<FakeWindow> {
        let kind = ViewKind::Week;
        let day = fixture_day();
        let pages = ranges(kind, day)
            .into_iter()
            .enumerate()
            .map(|(at, range)| (PageId(at as u64 + 1), range))
            .collect();
        let window = Rc::new(FakeWindow {
            view: RefCell::new(View {
                accounts: vec![ACCOUNT],
                kind,
                day,
                today: day,
                pages,
                showing_list: false,
                search_text: String::new(),
                on_screen: true,
                network: true,
                copy: Vec::new(),
                waiting: Vec::new(),
                older_missing: false,
                reach: Ok(()),
                no_blocks: false,
                steps: Vec::new(),
                drawn: Vec::new(),
                scrolls: Vec::new(),
                popovers: Vec::new(),
                sidebars: Vec::new(),
                waiting_drawn: Vec::new(),
                list: Vec::new(),
                list_firsts: Vec::new(),
                results: Vec::new(),
                notes: Vec::new(),
                went_to: Vec::new(),
                refreshes: 0,
                chosen: Ok(Some(scope::Answer {
                    scope: None,
                    notify: mailrs_domain::calendar::Notify::Nobody,
                    keep_time: false,
                })),
                written: Ok(Permitted::Done(None)),
                writes: Vec::new(),
                undos: Vec::new(),
                pushed: Vec::new(),
                permissions_asked: Vec::new(),
                toasts: Vec::new(),
                holds: HashMap::new(),
                during: HashMap::new(),
                spawned: Vec::new(),
            }),
            run: RefCell::new(Weak::new()),
            me: RefCell::new(Weak::new()),
        });
        window.me.replace(Rc::downgrade(&window));
        window
    }

    /// The run, with this window behind both ports. Keep it: the window
    /// holds it weakly.
    pub fn run(self: &Rc<Self>) -> Rc<CalendarRun> {
        let run = Rc::new(CalendarRun::new(
            Rc::clone(self) as Rc<dyn Desk>,
            Rc::clone(self) as Rc<dyn Effects>,
        ));
        self.run.replace(Rc::downgrade(&run));
        run
    }

    pub fn with<R>(&self, change: impl FnOnce(&mut View) -> R) -> R {
        change(&mut self.view.borrow_mut())
    }

    pub fn count(&self, step: Step) -> usize {
        self.view.borrow().steps.iter().filter(|s| **s == step).count()
    }

    /// Holds the next call of `step` until the sender fires or drops.
    pub fn hold(&self, step: Step) -> oneshot::Sender<()> {
        let (send, receive) = oneshot::channel();
        self.with(|v| v.holds.entry(step).or_default().push_back(receive));
        send
    }

    /// Runs `what` while the next call of `step` waits for its answer,
    /// after the call took what it answers with.
    pub fn during(&self, step: Step, what: impl FnOnce(&Rc<FakeWindow>, &Rc<CalendarRun>) + 'static) {
        self.with(|v| v.during.entry(step).or_default().push_back(Box::new(what)));
    }

    /// Moves a page to another range without reading it, as the carousel
    /// does before the read it then starts.
    pub fn move_page(&self, page: PageId, range: Range) {
        self.with(|v| {
            if let Some(entry) = v.pages.iter_mut().find(|(id, _)| *id == page) {
                entry.1 = range;
            }
        });
    }

    /// Runs everything the run spawned, and whatever that spawns, until
    /// nothing is left that can move. Fails a test that waits on a held
    /// call nobody lets go of.
    pub async fn settle(&self) {
        let mut running: FuturesUnordered<Work> = FuturesUnordered::new();
        let all = futures::future::poll_fn(|cx| loop {
            let spawned = std::mem::take(&mut self.view.borrow_mut().spawned);
            let spawned_any = !spawned.is_empty();
            running.extend(spawned);
            match running.poll_next_unpin(cx) {
                Poll::Ready(Some(())) => continue,
                Poll::Ready(None) if !spawned_any && self.view.borrow().spawned.is_empty() => {
                    return Poll::Ready(());
                }
                Poll::Ready(None) => continue,
                Poll::Pending if !self.view.borrow().spawned.is_empty() => continue,
                Poll::Pending => return Poll::Pending,
            }
        });
        tokio::time::timeout(Duration::from_secs(5), all)
            .await
            .expect("the run settled");
    }

    /// Records `step`, and gives back the hold and the happening waiting
    /// for it, which the answer goes through before it arrives.
    fn take(&self, step: Step) -> (Option<oneshot::Receiver<()>>, Option<During>) {
        let mut v = self.view.borrow_mut();
        v.steps.push(step);
        let hold = v.holds.get_mut(&step).and_then(VecDeque::pop_front);
        let during = v.during.get_mut(&step).and_then(VecDeque::pop_front);
        (hold, during)
    }

    /// An answer that arrives once `step`'s hold lets go, after whatever
    /// happens while it waits.
    fn answer<T: 'static>(&self, step: Step, value: T) -> Answer<'_, T> {
        let (hold, during) = self.take(step);
        let (window, run) = (self.me.borrow().upgrade(), self.run.borrow().upgrade());
        Box::pin(async move {
            if let (Some(during), Some(window), Some(run)) = (during, window, run) {
                during(&window, &run);
            }
            if let Some(hold) = hold {
                let _ = hold.await;
            }
            value
        })
    }

    fn record(&self, step: Step) {
        self.view.borrow_mut().steps.push(step);
    }

    fn found(&self, from: EpochMillis, to: EpochMillis) -> Vec<Occurrence> {
        self.view
            .borrow()
            .copy
            .iter()
            .filter(|o| o.start < to && o.end > from)
            .cloned()
            .collect()
    }
}

fn titles(found: &[Occurrence]) -> Vec<String> {
    found.iter().map(|o| o.event.title.clone()).collect()
}

impl Desk for FakeWindow {
    fn accounts(&self) -> Vec<AccountId> {
        self.view.borrow().accounts.clone()
    }

    fn waiting_accounts(&self) -> Vec<AccountId> {
        self.view.borrow().accounts.clone()
    }

    fn shows(&self, place: Place) -> Shows {
        let v = self.view.borrow();
        match place {
            Place::Page(page) => v
                .pages
                .iter()
                .find(|(id, _)| *id == page)
                .map_or(Shows::Away, |(_, range)| Shows::Range(*range)),
            Place::Search => Shows::Text(v.search_text.clone()),
            Place::Open => match v.on_screen {
                true => Shows::Calendar,
                false => Shows::Away,
            },
            _ => Shows::Anything,
        }
    }

    fn pages(&self) -> Vec<(PageId, Range)> {
        self.view.borrow().pages.clone()
    }

    fn showing_list(&self) -> bool {
        self.view.borrow().showing_list
    }

    fn mini(&self) -> Range {
        Range::around(ViewKind::Month, self.view.borrow().day)
    }

    fn wanted_from(&self) -> EpochMillis {
        self.view.borrow().pages[0].1.span(&chrono::Local).0
    }

    fn list_window(&self) -> (NaiveDate, NaiveDate) {
        crate::ui::calendar::range::agenda_window(self.view.borrow().day)
    }

    fn today(&self) -> NaiveDate {
        self.view.borrow().today
    }

    fn now(&self) -> EpochMillis {
        0
    }

    fn network(&self) -> bool {
        self.view.borrow().network
    }
}

impl Effects for FakeWindow {
    fn occurrences(
        &self,
        _accounts: Vec<AccountId>,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Answer<'_, Result<Vec<Occurrence>, String>> {
        let found = self.found(from, to);
        self.answer(Step::Occurrences, Ok(found))
    }

    fn sidebar(
        &self,
        accounts: Vec<AccountId>,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Answer<'_, Result<SidebarRead, String>> {
        let listed = accounts
            .into_iter()
            .map(|account_id| Listed {
                account_id,
                calendars: vec![Calendar { id: "primary".to_string(), shown: true, ..Calendar::default() }],
                unlisted: Vec::new(),
            })
            .collect();
        let busy = self.found(from, to);
        self.answer(Step::Sidebar, Ok((listed, busy)))
    }

    fn waiting(&self, _accounts: Vec<AccountId>, _now: EpochMillis) -> Answer<'_, Result<Vec<Waiting>, String>> {
        let waiting = self.view.borrow().waiting.clone();
        self.answer(Step::Waiting, Ok(waiting))
    }

    fn search(
        &self,
        _accounts: Vec<AccountId>,
        text: String,
        _now: EpochMillis,
    ) -> Answer<'_, Result<Vec<Occurrence>, String>> {
        let found: Vec<Occurrence> = self
            .view
            .borrow()
            .copy
            .iter()
            .filter(|o| o.event.title.contains(&text))
            .cloned()
            .collect();
        self.answer(Step::Search, Ok(found))
    }

    fn event(&self, key: EventKey) -> Answer<'_, Result<Option<Event>, String>> {
        let found = self
            .view
            .borrow()
            .copy
            .iter()
            .find(|o| key_of(o) == key)
            .map(|o| Event::clone(&o.event));
        self.answer(Step::Event, Ok(found))
    }

    fn older_missing(&self, _accounts: Vec<AccountId>, _from: EpochMillis) -> Answer<'_, Result<bool, String>> {
        let missing = self.view.borrow().older_missing;
        self.answer(Step::OlderMissing, Ok(missing))
    }

    fn reach_back(&self, _accounts: Vec<AccountId>, _from: EpochMillis) -> Answer<'_, Result<(), Unreached>> {
        let reach = self.view.borrow().reach.clone();
        self.answer(Step::ReachBack, reach)
    }

    fn laid_out(&self, _page: PageId) -> Answer<'_, ()> {
        self.answer(Step::LaidOut, ())
    }

    fn after_layout(&self, _page: PageId) -> Answer<'_, ()> {
        self.answer(Step::AfterLayout, ())
    }

    fn idle(&self) -> Answer<'_, ()> {
        self.answer(Step::Idle, ())
    }

    fn spawn(&self, work: Work) {
        self.view.borrow_mut().spawned.push(work);
    }

    fn draw_page(&self, page: PageId, found: Vec<Occurrence>) {
        self.record(Step::DrawPage);
        self.with(|v| v.drawn.push((page, titles(&found))));
    }

    fn scroll_to(&self, page: PageId, hour: f64) {
        self.record(Step::ScrollTo);
        self.with(|v| v.scrolls.push((page, hour)));
    }

    fn open_popover(&self, page: PageId, o: &Occurrence) -> bool {
        self.record(Step::OpenPopover);
        self.with(|v| {
            if v.no_blocks {
                return false;
            }
            v.popovers.push((page, key_of(o), o.start));
            true
        })
    }

    fn draw_sidebar(&self, listed: Vec<Listed>, _busy: Vec<Occurrence>, _mini: Range) {
        self.record(Step::DrawSidebar);
        self.with(|v| v.sidebars.push(listed.len()));
    }

    fn draw_waiting(&self, waiting: Vec<Waiting>) {
        self.record(Step::DrawWaiting);
        self.with(|v| v.waiting_drawn.push(waiting.len()));
    }

    fn draw_list(&self, found: Vec<Occurrence>, first: NaiveDate, _last: NaiveDate) {
        self.record(Step::DrawList);
        self.with(|v| {
            v.list = titles(&found);
            v.list_firsts.push(first);
        });
    }

    fn prepend_list(&self, found: Vec<Occurrence>, first: NaiveDate, _last: NaiveDate, _listed_from: EpochMillis) {
        self.record(Step::PrependList);
        self.with(|v| {
            let mut list = titles(&found);
            list.append(&mut v.list);
            v.list = list;
            v.list_firsts.push(first);
        });
    }

    fn append_list(&self, found: Vec<Occurrence>, _first: NaiveDate, _last: NaiveDate) {
        self.record(Step::AppendList);
        self.with(|v| v.list.extend(titles(&found)));
    }

    fn no_earlier(&self) {
        self.record(Step::NoEarlier);
    }

    fn draw_results(&self, found: Vec<Occurrence>) {
        self.record(Step::DrawResults);
        self.with(|v| v.results.push(titles(&found)));
    }

    fn older_note(&self, saying: Option<Older>) {
        self.record(Step::OlderNote);
        self.with(|v| v.notes.push(saying));
    }

    /// Moves the view as the window does: the day, the pages' ranges, and
    /// the reads the move starts.
    fn go_to(&self, to: NaiveDate) {
        self.record(Step::GoTo);
        self.with(|v| {
            v.went_to.push(to);
            v.day = to;
            let ranges = ranges(v.kind, to);
            for ((_, range), new) in v.pages.iter_mut().zip(ranges) {
                *range = new;
            }
        });
        if let Some(run) = self.run.borrow().upgrade() {
            run.range_moved();
        }
    }

    fn refresh(&self) {
        self.record(Step::Refresh);
        self.with(|v| v.refreshes += 1);
    }

    fn choose(
        &self,
        _account_id: AccountId,
        _change: EventChange,
        _shown: Event,
        _when: Option<String>,
    ) -> Answer<'_, Result<Option<scope::Answer>, String>> {
        let chosen = self.view.borrow().chosen.clone();
        self.answer(Step::Choose, chosen)
    }

    fn write(
        &self,
        _account_id: AccountId,
        change: EventChange,
        _answer: scope::Answer,
        _undo: Undo,
    ) -> Answer<'_, Result<Permitted<Option<u64>>, String>> {
        let title = match &change {
            EventChange::New(event) | EventChange::Edit { edited: event, .. } => event.title.clone(),
            EventChange::Remove(o) => o.event.title.clone(),
        };
        let written = self.with(|v| {
            v.writes.push(title);
            v.written.clone()
        });
        self.answer(Step::Write, written)
    }

    fn offer_undo(&self, said: String, held: u64) {
        self.record(Step::OfferUndo);
        self.with(|v| v.undos.push((said, held)));
    }

    fn needs_permission(&self, account_id: AccountId) {
        self.record(Step::NeedsPermission);
        self.with(|v| v.permissions_asked.push(account_id));
    }

    fn toast(&self, text: String) {
        self.record(Step::Toast);
        self.with(|v| v.toasts.push(text));
    }

    fn push(&self, account_id: AccountId) {
        self.record(Step::Push);
        self.with(|v| v.pushed.push(account_id));
    }
}
