//! The calendar view behind the calendar run's ports: [`Desk`] answers
//! from the view's fields, and [`Effects`] reads the local copy through
//! `mailrs_sync` and draws on the view's widgets.
//!
//! The adapter holds the view weakly, since the view owns the run. Once
//! the view has gone, a read answers with an error and a change does
//! nothing.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use chrono::NaiveDate;
use futures::channel::oneshot;
use gtk::glib;
use mailrs_domain::calendar::{Event, Occurrence};
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_store::calendar::CalendarScope;
use mailrs_sync::calendar_copy::event_change::{Changed, EventChange, Undo};
use mailrs_sync::{Permitted, Waiting};
use mailrs_sync::calendar_copy::Listed;
use mailrs_domain::translate::gettext;

use super::block::EventKey;
use super::range::{self, Range, ViewKind};
use super::run::{Answer, Desk, SidebarRead, Effects, Older, PageId, Place, Shows, Unreached, Work};
use super::shown::{self, Showing};
use super::{CalendarView, PageView, SEARCH_LIMIT, scope, sidebar};

/// The view, as the run sees it.
pub(super) struct Ports(pub(super) Weak<CalendarView>);

/// The answer a read gives once the view has gone.
fn closed<T>() -> Result<T, String> {
    Err("the calendar closed".to_string())
}

/// Whether a scrolled window's content has a height to scroll in.
fn has_height(adjustment: &gtk::Adjustment) -> bool {
    adjustment.page_size() > 0.0 && adjustment.upper() > adjustment.page_size()
}

impl Ports {
    fn view(&self) -> Option<Rc<CalendarView>> {
        self.0.upgrade()
    }

    /// The scrolled hours of a grid page.
    fn scroller(&self, page: PageId) -> Option<gtk::ScrolledWindow> {
        let view = self.view()?;
        let page = view.page_by_id(page)?;
        let shown = page.view.borrow();
        match &*shown {
            PageView::Grid(grid) => Some(grid.scroller.clone()),
            PageView::Month(_) => None,
        }
    }
}

impl Desk for Ports {
    fn accounts(&self) -> Vec<AccountId> {
        self.view().map(|view| view.account_ids()).unwrap_or_default()
    }

    fn waiting_accounts(&self) -> Vec<AccountId> {
        self.view()
            .map(|view| sidebar::waiting_accounts(&view.accounts.borrow()))
            .unwrap_or_default()
    }

    fn shows(&self, place: Place) -> Shows {
        let Some(view) = self.view() else { return Shows::Away };
        match place {
            Place::Page(id) => view
                .page_by_id(id)
                .map_or(Shows::Away, |page| Shows::Range(page.range.get())),
            Place::Search => Shows::Text(view.search_entry.text().trim().to_string()),
            Place::Open => match view.page.is_mapped() {
                true => Shows::Calendar,
                false => Shows::Away,
            },
            Place::Scroll(_) | Place::Sidebar | Place::Waiting | Place::List | Place::Older | Place::Next => {
                Shows::Anything
            }
        }
    }

    fn pages(&self) -> Vec<(PageId, Range)> {
        let Some(view) = self.view() else { return Vec::new() };
        let pages = view.pages.borrow();
        // The page on screen first, so its read is not queued behind its
        // neighbours'.
        [1, 0, 2]
            .into_iter()
            .filter_map(|index| pages.get(index))
            .map(|page| (page.id, page.range.get()))
            .collect()
    }

    fn showing_list(&self) -> bool {
        self.view().is_some_and(|view| view.showing() == Showing::Agenda)
    }

    fn mini(&self) -> Range {
        let day = self.view().map_or_else(|| self.today(), |view| view.day.get());
        Range::around(ViewKind::Month, day)
    }

    fn wanted_from(&self) -> EpochMillis {
        self.view().map_or(0, |view| view.wanted_from())
    }

    fn list_window(&self) -> (NaiveDate, NaiveDate) {
        let day = self.view().map_or_else(|| self.today(), |view| view.day.get());
        range::agenda_window(day)
    }

    fn today(&self) -> NaiveDate {
        chrono::Local::now().date_naive()
    }

    fn now(&self) -> EpochMillis {
        mailrs_sync::now_millis()
    }

    fn network(&self) -> bool {
        self.view().is_some_and(|view| view.core.network())
    }
}

impl Effects for Ports {
    fn occurrences(
        &self,
        accounts: Vec<AccountId>,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Answer<'_, Result<Vec<Occurrence>, String>> {
        let Some(view) = self.view() else { return Box::pin(async { closed() }) };
        let (core, copy) = (Rc::clone(&view.core), view.core.calendar_copy());
        Box::pin(async move {
            core.call(async move { copy.occurrences(&accounts, from, to, CalendarScope::Shown).await })
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn sidebar(
        &self,
        accounts: Vec<AccountId>,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Answer<'_, Result<SidebarRead, String>> {
        let Some(view) = self.view() else { return Box::pin(async { closed() }) };
        let (core, copy) = (Rc::clone(&view.core), view.core.calendar_copy());
        Box::pin(async move {
            core.call(async move {
                let listed = copy.listed(&accounts).await?;
                let busy = copy.occurrences(&accounts, from, to, CalendarScope::Shown).await?;
                Ok::<_, mailrs_sync::SyncError>((listed, busy))
            })
            .await
            .map_err(|err| err.to_string())
        })
    }

    fn waiting(&self, accounts: Vec<AccountId>, now: EpochMillis) -> Answer<'_, Result<Vec<Waiting>, String>> {
        let Some(view) = self.view() else { return Box::pin(async { closed() }) };
        let (core, invitations) = (Rc::clone(&view.core), view.core.invitations());
        Box::pin(async move {
            // `Db::read`'s `spawn_blocking` needs the tokio runtime, which
            // `call` gives it and the GTK loop does not.
            core.call(async move { invitations.waiting_for_answer(&accounts, now).await })
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn search(
        &self,
        accounts: Vec<AccountId>,
        text: String,
        now: EpochMillis,
    ) -> Answer<'_, Result<Vec<Occurrence>, String>> {
        let Some(view) = self.view() else { return Box::pin(async { closed() }) };
        let (core, copy) = (Rc::clone(&view.core), view.core.calendar_copy());
        Box::pin(async move {
            core.call(async move { copy.search(&accounts, &text, now, CalendarScope::Shown, SEARCH_LIMIT).await })
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn event(&self, key: EventKey) -> Answer<'_, Result<Option<Event>, String>> {
        let Some(view) = self.view() else { return Box::pin(async { closed() }) };
        let (core, copy) = (Rc::clone(&view.core), view.core.calendar_copy());
        let (account_id, calendar, id) = key;
        Box::pin(async move {
            core.call(async move { copy.event(account_id, &calendar, &id).await })
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn older_missing(&self, accounts: Vec<AccountId>, from: EpochMillis) -> Answer<'_, Result<bool, String>> {
        let Some(view) = self.view() else { return Box::pin(async { closed() }) };
        let (core, copy) = (Rc::clone(&view.core), view.core.calendar_copy());
        Box::pin(async move {
            core.call(async move { copy.older_missing(&accounts, from).await })
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn reach_back(&self, accounts: Vec<AccountId>, from: EpochMillis) -> Answer<'_, Result<(), Unreached>> {
        let Some(view) = self.view() else {
            return Box::pin(async { Err(Unreached::Failed("the calendar closed".to_string())) });
        };
        let (core, copy) = (Rc::clone(&view.core), view.core.calendar_copy());
        Box::pin(async move {
            match core.call(async move { copy.reach_back(&accounts, from).await }).await {
                Ok(_) => Ok(()),
                Err(err) => {
                    let offline = err
                        .downcast_ref::<mailrs_sync::SyncError>()
                        .is_some_and(|e| matches!(e, mailrs_sync::SyncError::Backend(b) if b.is_transient()));
                    Err(match offline {
                        true => Unreached::Offline,
                        false => Unreached::Failed(err.to_string()),
                    })
                }
            }
        })
    }

    fn laid_out(&self, page: PageId) -> Answer<'_, ()> {
        let Some(scroller) = self.scroller(page) else { return Box::pin(async {}) };
        let adjustment = scroller.vadjustment();
        if has_height(&adjustment) {
            return Box::pin(async {});
        }
        let (send, receive) = oneshot::channel::<()>();
        let send = Cell::new(Some(send));
        let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::new(RefCell::new(None));
        let slot = Rc::clone(&handler);
        let id = adjustment.connect_changed(move |adjustment| {
            if !has_height(adjustment) {
                return;
            }
            if let Some(id) = slot.borrow_mut().take() {
                adjustment.disconnect(id);
            }
            // The scrolled window sets its own value while it lays out
            // the first time, after this signal, so the scroll waits for
            // that.
            if let Some(send) = send.take() {
                glib::idle_add_local_once(move || {
                    let _ = send.send(());
                });
            }
        });
        handler.replace(Some(id));
        Box::pin(async move {
            let _ = receive.await;
        })
    }

    fn after_layout(&self, page: PageId) -> Answer<'_, ()> {
        let widget: Option<gtk::Widget> = self.scroller(page).map(|s| s.upcast()).or_else(|| {
            let view = self.view()?;
            let page = view.page_by_id(page)?;
            Some(page.holder.clone().upcast())
        });
        let Some(widget) = widget else { return Box::pin(async {}) };
        let (send, receive) = oneshot::channel::<()>();
        super::after_layout(&widget, move || {
            let _ = send.send(());
        });
        Box::pin(async move {
            let _ = receive.await;
        })
    }

    fn idle(&self) -> Answer<'_, ()> {
        let (send, receive) = oneshot::channel::<()>();
        glib::idle_add_local_once(move || {
            let _ = send.send(());
        });
        Box::pin(async move {
            let _ = receive.await;
        })
    }

    fn spawn(&self, work: Work) {
        glib::spawn_future_local(work);
    }

    fn draw_page(&self, page: PageId, found: Vec<Occurrence>) {
        let Some(view) = self.view() else { return };
        if let Some(page) = view.page_by_id(page) {
            view.draw_page(&page, found);
        }
    }

    fn scroll_to(&self, page: PageId, hour: f64) {
        let Some(view) = self.view() else { return };
        let Some(page) = view.page_by_id(page) else { return };
        let shown = page.view.borrow();
        if let PageView::Grid(grid) = &*shown {
            let y = grid.grid.scroll_to_hour(hour);
            let adjustment = grid.scroller.vadjustment();
            adjustment.set_value(y.min(adjustment.upper() - adjustment.page_size()));
        }
    }

    fn open_popover(&self, page: PageId, o: &Occurrence) -> bool {
        let Some(view) = self.view() else { return false };
        let Some(page) = view.page_by_id(page) else { return false };
        let key = super::block::key_of(o);
        let block = match &*page.view.borrow() {
            PageView::Grid(grid) => grid
                .grid
                .block_at(&key, o.start)
                .or_else(|| grid.strip.block_at(&key, o.start)),
            PageView::Month(month) => month.block_at(&key, o.start),
        };
        let Some(block) = block else { return false };
        view.show_event(&block, o);
        true
    }

    fn draw_sidebar(&self, listed: Vec<Listed>, busy: Vec<Occurrence>, mini: Range) {
        if let Some(view) = self.view() {
            view.show_sidebar(listed, &busy, mini);
        }
    }

    fn draw_waiting(&self, waiting: Vec<Waiting>) {
        if let Some(view) = self.view() {
            view.calendar_sidebar.show_waiting(&waiting);
            (view.hooks.waiting)(waiting.len());
        }
    }

    fn draw_list(&self, found: Vec<Occurrence>, first: NaiveDate, last: NaiveDate) {
        let Some(view) = self.view() else { return };
        let found = view.agenda_events(found, first, last);
        view.list.show(&found, &view.calendars.borrow(), &chrono::Local);
    }

    fn prepend_list(&self, found: Vec<Occurrence>, first: NaiveDate, last: NaiveDate, listed_from: EpochMillis) {
        let Some(view) = self.view() else { return };
        let found = view.agenda_events(shown::not_yet_listed(found, listed_from), first, last);
        view.list.prepend(&found, &view.calendars.borrow(), &chrono::Local);
    }

    fn append_list(&self, found: Vec<Occurrence>, first: NaiveDate, last: NaiveDate) {
        let Some(view) = self.view() else { return };
        let found = view.agenda_events(found, first, last);
        view.list.append(&found, first, &view.calendars.borrow(), &chrono::Local);
    }

    fn no_earlier(&self) {
        if let Some(view) = self.view() {
            view.list.show_no_earlier();
        }
    }

    fn draw_results(&self, found: Vec<Occurrence>) {
        if let Some(view) = self.view() {
            view.results.show(&found, &view.calendars.borrow(), &chrono::Local);
            view.views.set_visible_child_name("search");
        }
    }

    fn older_note(&self, saying: Option<Older>) {
        let Some(view) = self.view() else { return };
        let offline = saying == Some(Older::Offline);
        view.older_spinner.set_visible(!offline);
        view.older_spinner.set_spinning(saying == Some(Older::Loaded));
        view.older_label.set_label(&match offline {
            true => gettext("Older events can't load while offline"),
            false => gettext("Loading older events"),
        });
        view.older_note.set_visible(saying.is_some());
    }

    fn go_to(&self, day: NaiveDate) {
        if let Some(view) = self.view() {
            view.go_to(day);
        }
    }

    fn refresh(&self) {
        if let Some(view) = self.view() {
            (view.hooks.refresh)();
        }
    }

    fn choose(
        &self,
        account_id: AccountId,
        change: EventChange,
        shown: Event,
        when: Option<String>,
    ) -> Answer<'_, Result<Option<scope::Answer>, String>> {
        let Some(view) = self.view() else { return Box::pin(async { closed() }) };
        Box::pin(async move {
            view.choose(account_id, &change, &shown, when.as_deref())
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn write(
        &self,
        account_id: AccountId,
        change: EventChange,
        answer: scope::Answer,
        undo: Undo,
    ) -> Answer<'_, Result<Permitted<Option<u64>>, String>> {
        let Some(view) = self.view() else { return Box::pin(async { closed() }) };
        let (core, copy) = (Rc::clone(&view.core), view.core.calendar_copy());
        let weak = Rc::downgrade(&view);
        Box::pin(async move {
            let written = core
                .call(async move { copy.change(account_id, change, answer.choice(), undo).await })
                .await
                .map_err(|err| err.to_string())?;
            Ok(match written {
                // The view keeps the held change until its toast closes.
                Permitted::Done(Changed::Held(held)) => match weak.upgrade() {
                    Some(view) => Permitted::Done(Some(view.holding.borrow_mut().hold(held))),
                    None => Permitted::Done(None),
                },
                Permitted::Done(Changed::Queued(_)) => Permitted::Done(None),
                Permitted::NeedsPermission => Permitted::NeedsPermission,
            })
        })
    }

    fn offer_undo(&self, said: String, held: u64) {
        if let Some(view) = self.view() {
            view.offer_undo(said, held);
        }
    }

    fn needs_permission(&self, account_id: AccountId) {
        if let Some(view) = self.view() {
            (view.hooks.needs_permission)(account_id);
        }
    }

    fn toast(&self, text: String) {
        if let Some(view) = self.view() {
            (view.hooks.toast)(&text);
        }
    }

    fn push(&self, account_id: AccountId) {
        if let Some(view) = self.view() {
            (view.hooks.push)(account_id);
        }
    }
}
