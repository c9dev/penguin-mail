//! The calendar run, headless. Each test drives `CalendarRun` the way the
//! view does, lets a second read overtake the first where the race needs
//! it, and checks what reached the page.

use super::fake::{ACCOUNT, FakeWindow, Step, event_at, fixture_day};
use super::{Older, PageId, Unreached};
use crate::ui::calendar::block::key_of;
use crate::ui::calendar::range::{Range, ViewKind};

/// The page on screen in a fresh fake.
const ON_SCREEN: PageId = PageId(1);

#[tokio::test]
async fn a_page_draws_what_the_copy_holds_for_its_range() {
    let window = FakeWindow::new();
    window.with(|v| v.copy = vec![event_at("Standup", fixture_day(), 9)]);
    let run = window.run();
    run.fill(ON_SCREEN);
    window.settle().await;
    assert_eq!(window.view.borrow().drawn, vec![(ON_SCREEN, vec!["Standup".to_string()])]);
}

#[tokio::test]
async fn a_refill_that_lands_after_the_range_moved_is_dropped() {
    let window = FakeWindow::new();
    window.with(|v| v.copy = vec![event_at("Standup", fixture_day(), 9)]);
    let next_week = Range::around(ViewKind::Week, fixture_day()).next();
    window.during(Step::Occurrences, move |window, _| window.move_page(ON_SCREEN, next_week));
    let run = window.run();
    run.fill(ON_SCREEN);
    window.settle().await;
    assert!(window.view.borrow().drawn.is_empty());
}

#[tokio::test]
async fn an_older_read_of_a_page_gives_way_to_a_newer_one() {
    let window = FakeWindow::new();
    window.with(|v| v.copy = vec![event_at("Before", fixture_day(), 9)]);
    // The person moves the event while the first read is out; the
    // refill the move starts reads the copy after it.
    window.during(Step::Occurrences, |window, run| {
        window.with(|v| v.copy = vec![event_at("After", fixture_day(), 10)]);
        run.fill(ON_SCREEN);
    });
    let run = window.run();
    run.fill(ON_SCREEN);
    window.settle().await;
    assert_eq!(window.view.borrow().drawn, vec![(ON_SCREEN, vec!["After".to_string()])]);
}

#[tokio::test]
async fn a_fresh_grid_page_scrolls_to_its_first_event_once() {
    let window = FakeWindow::new();
    window.with(|v| v.copy = vec![event_at("Early", fixture_day(), 6)]);
    let run = window.run();
    run.fill(ON_SCREEN);
    window.settle().await;
    run.fill(ON_SCREEN);
    window.settle().await;
    assert_eq!(window.view.borrow().scrolls, vec![(ON_SCREEN, 6.0)]);
}

#[tokio::test]
async fn a_month_page_never_scrolls() {
    let window = FakeWindow::new();
    let month = Range::around(ViewKind::Month, fixture_day());
    window.move_page(ON_SCREEN, month);
    let run = window.run();
    run.fill(ON_SCREEN);
    window.settle().await;
    assert_eq!(window.count(Step::LaidOut), 0);
}

#[tokio::test]
async fn show_in_calendar_goes_to_the_day_and_opens_the_popover() {
    let window = FakeWindow::new();
    let later = fixture_day() + chrono::Days::new(14);
    let meeting = event_at("Design review", later, 15);
    window.with(|v| v.copy = vec![meeting.clone()]);
    let run = window.run();
    run.open(key_of(&meeting), meeting.start);
    window.settle().await;
    let view = window.view.borrow();
    assert_eq!(view.went_to, vec![later]);
    assert_eq!(view.popovers, vec![(ON_SCREEN, key_of(&meeting), meeting.start)]);
    assert!(run.pending().is_none());
}

/// 0ca63cbe: a reload that started while Show in Calendar's page was
/// reading took the page over, and the popover never opened.
#[tokio::test]
async fn show_in_calendar_opens_the_popover_even_when_a_refill_races_it() {
    let window = FakeWindow::new();
    let meeting = event_at("Design review", fixture_day(), 15);
    window.with(|v| v.copy = vec![meeting.clone()]);
    // The first read of the page on screen after the move meets a refill,
    // as a sync that changed the copy starts one.
    window.during(Step::Event, |window, _| {
        window.during(Step::Occurrences, |_, run| run.fill_all());
    });
    let run = window.run();
    run.open(key_of(&meeting), meeting.start);
    window.settle().await;
    assert_eq!(
        window.view.borrow().popovers,
        vec![(ON_SCREEN, key_of(&meeting), meeting.start)]
    );
}

/// The refill lands between the page drawing the event and its popover
/// opening: the popover opens on the refill's block, once.
#[tokio::test]
async fn a_refill_between_drawing_and_opening_still_opens_the_popover_once() {
    let window = FakeWindow::new();
    let meeting = event_at("Design review", fixture_day(), 15);
    window.with(|v| {
        v.copy = vec![meeting.clone()];
        v.kind = ViewKind::Month;
        for (_, range) in v.pages.iter_mut() {
            *range = Range::around(ViewKind::Month, range.first);
        }
    });
    let month = Range::around(ViewKind::Month, fixture_day());
    window.move_page(ON_SCREEN, month);
    window.during(Step::Idle, |_, run| run.fill(ON_SCREEN));
    let run = window.run();
    run.open_once_drawn(key_of(&meeting), meeting.start);
    run.fill(ON_SCREEN);
    window.settle().await;
    assert_eq!(
        window.view.borrow().popovers,
        vec![(ON_SCREEN, key_of(&meeting), meeting.start)]
    );
}

#[tokio::test]
async fn an_event_the_copy_lost_opens_nothing() {
    let window = FakeWindow::new();
    let meeting = event_at("Design review", fixture_day(), 15);
    let run = window.run();
    run.open(key_of(&meeting), meeting.start);
    window.settle().await;
    assert!(window.view.borrow().went_to.is_empty());
}

#[tokio::test]
async fn an_open_answered_after_the_calendar_left_the_screen_goes_nowhere() {
    let window = FakeWindow::new();
    let meeting = event_at("Design review", fixture_day(), 15);
    window.with(|v| v.copy = vec![meeting.clone()]);
    window.during(Step::Event, |window, _| window.with(|v| v.on_screen = false));
    let run = window.run();
    run.open(key_of(&meeting), meeting.start);
    window.settle().await;
    assert!(window.view.borrow().went_to.is_empty());
    assert!(run.pending().is_none());
}

#[tokio::test]
async fn a_move_drops_an_open_still_reading_its_event() {
    let window = FakeWindow::new();
    let meeting = event_at("Design review", fixture_day(), 15);
    window.with(|v| v.copy = vec![meeting.clone()]);
    window.during(Step::Event, |_, run| run.range_moved());
    let run = window.run();
    run.open(key_of(&meeting), meeting.start);
    window.settle().await;
    assert!(window.view.borrow().went_to.is_empty());
}

/// 066e6d01: a hidden page's first scroll waited for the grid's layout,
/// then landed on top of Show in Calendar's scroll to the event.
#[tokio::test]
async fn a_scroll_asked_while_a_page_loads_lands_once() {
    let window = FakeWindow::new();
    let meeting = event_at("Late call", fixture_day(), 20);
    window.with(|v| v.copy = vec![meeting.clone()]);
    let first_layout = window.hold(Step::LaidOut);
    let run = window.run();
    run.fill(ON_SCREEN);
    let opened = {
        let (window, run, meeting) = (window.clone(), run.clone(), meeting.clone());
        async move {
            while window.count(Step::LaidOut) == 0 {
                tokio::task::yield_now().await;
            }
            run.open_once_drawn(key_of(&meeting), meeting.start);
            run.fill(ON_SCREEN);
            while window.view.borrow().popovers.is_empty() {
                tokio::task::yield_now().await;
            }
            let _ = first_layout.send(());
        }
    };
    futures::join!(window.settle(), opened);
    let view = window.view.borrow();
    assert_eq!(view.scrolls, vec![(ON_SCREEN, 19.0)]);
    assert_eq!(view.popovers.len(), 1);
}

#[tokio::test]
async fn a_calendar_list_read_that_owes_a_refill_hands_it_to_a_newer_read() {
    let window = FakeWindow::new();
    window.during(Step::Sidebar, |_, run| run.read_sidebar(false));
    let run = window.run();
    run.read_sidebar(true);
    window.settle().await;
    let view = window.view.borrow();
    assert_eq!(view.sidebars.len(), 1, "only the newer list read draws");
    assert_eq!(view.drawn.len(), 3, "the newer read refills the three pages");
}

#[tokio::test]
async fn a_calendar_list_read_with_no_refill_owed_leaves_the_pages() {
    let window = FakeWindow::new();
    let run = window.run();
    run.read_sidebar(false);
    window.settle().await;
    assert!(window.view.borrow().drawn.is_empty());
}

#[tokio::test]
async fn only_the_newest_waiting_read_draws_the_cards() {
    let window = FakeWindow::new();
    window.during(Step::Waiting, |_, run| run.refresh_waiting());
    let run = window.run();
    run.refresh_waiting();
    window.settle().await;
    assert_eq!(window.view.borrow().waiting_drawn.len(), 1);
}

#[tokio::test]
async fn a_search_answer_for_an_old_query_is_dropped() {
    let window = FakeWindow::new();
    window.with(|v| {
        v.copy = vec![event_at("Kit check", fixture_day(), 9), event_at("Kite flying", fixture_day(), 11)];
        v.search_text = "Kit".to_string();
    });
    window.during(Step::Search, |window, run| {
        window.with(|v| v.search_text = "Kite".to_string());
        run.search("Kite");
    });
    let run = window.run();
    run.search("Kit");
    window.settle().await;
    assert_eq!(window.view.borrow().results, vec![vec!["Kite flying".to_string()]]);
}

#[tokio::test]
async fn closing_the_search_drops_the_answer_still_reading() {
    let window = FakeWindow::new();
    window.with(|v| {
        v.copy = vec![event_at("Kite flying", fixture_day(), 11)];
        v.search_text = "Kite".to_string();
    });
    window.during(Step::Search, |window, run| {
        window.with(|v| v.search_text.clear());
        run.search("");
    });
    let run = window.run();
    run.search("Kite");
    window.settle().await;
    assert!(window.view.borrow().results.is_empty());
}

#[tokio::test]
async fn the_list_draws_its_first_window_around_the_day() {
    let window = FakeWindow::new();
    window.with(|v| {
        v.showing_list = true;
        v.copy = vec![event_at("Standup", fixture_day(), 9)];
    });
    let run = window.run();
    run.fill_list();
    window.settle().await;
    let view = window.view.borrow();
    assert_eq!(view.list, ["Standup"]);
    assert_eq!(view.list_firsts, [fixture_day()]);
}

#[tokio::test]
async fn earlier_days_read_for_a_list_that_has_since_been_replaced_are_dropped() {
    let window = FakeWindow::new();
    window.with(|v| v.showing_list = true);
    let run = window.run();
    run.fill_list();
    window.settle().await;
    window.with(|v| v.copy = vec![event_at("Last month", fixture_day() - chrono::Days::new(20), 9)]);
    window.during(Step::Occurrences, |_, run| run.fill_list());
    run.load_earlier();
    window.settle().await;
    assert_eq!(window.count(Step::PrependList), 0);
}

#[tokio::test]
async fn earlier_days_go_above_what_the_list_holds() {
    let window = FakeWindow::new();
    window.with(|v| {
        v.showing_list = true;
        v.copy = vec![event_at("Last month", fixture_day() - chrono::Days::new(20), 9)];
    });
    let run = window.run();
    run.fill_list();
    window.settle().await;
    run.load_earlier();
    window.settle().await;
    let view = window.view.borrow();
    assert_eq!(view.list, ["Last month"]);
    assert_eq!(view.list_firsts.last(), Some(&(fixture_day() - chrono::Days::new(30))));
}

#[tokio::test]
async fn a_second_scroll_to_the_top_while_earlier_days_load_asks_nothing_more() {
    let window = FakeWindow::new();
    window.with(|v| v.showing_list = true);
    let run = window.run();
    run.fill_list();
    window.settle().await;
    window.during(Step::Occurrences, |_, run| run.load_earlier());
    run.load_earlier();
    window.settle().await;
    assert_eq!(window.count(Step::PrependList), 1);
}

#[tokio::test]
async fn offline_the_list_keeps_its_earlier_days_for_the_next_scroll() {
    let window = FakeWindow::new();
    window.with(|v| {
        v.showing_list = true;
        v.older_missing = true;
        v.network = false;
    });
    let run = window.run();
    run.fill_list();
    window.settle().await;
    run.load_earlier();
    window.settle().await;
    run.load_earlier();
    window.settle().await;
    assert_eq!(window.count(Step::PrependList), 0);
    assert_eq!(window.count(Step::OlderMissing), 2, "each scroll to the top asks again");
}

#[tokio::test]
async fn a_newer_ask_for_older_events_takes_the_note_over() {
    let window = FakeWindow::new();
    window.with(|v| v.older_missing = true);
    let first_fetch = window.hold(Step::ReachBack);
    let run = window.run();
    run.reach_current();
    let newer = {
        let (window, run) = (window.clone(), run.clone());
        async move {
            while window.count(Step::ReachBack) == 0 {
                tokio::task::yield_now().await;
            }
            window.with(|v| v.older_missing = false);
            run.reach_current();
            while window.view.borrow().notes.len() < 2 {
                tokio::task::yield_now().await;
            }
            let _ = first_fetch.send(());
        }
    };
    futures::join!(window.settle(), newer);
    // The first ask put up the loading line; the newer one found nothing
    // missing and took it down; the first, ending last, left it alone.
    assert_eq!(window.view.borrow().notes, vec![Some(Older::Loaded), None]);
}

#[tokio::test]
async fn older_events_fetched_for_the_range_on_screen_refill_it() {
    let window = FakeWindow::new();
    window.with(|v| v.older_missing = true);
    let run = window.run();
    run.reach_current();
    window.settle().await;
    assert_eq!(window.view.borrow().drawn.len(), 3);
}

#[tokio::test]
async fn older_events_that_cannot_load_offline_say_so() {
    let window = FakeWindow::new();
    window.with(|v| {
        v.older_missing = true;
        v.reach = Err(Unreached::Offline);
    });
    let run = window.run();
    run.reach_current();
    window.settle().await;
    let view = window.view.borrow();
    assert_eq!(view.notes, vec![Some(Older::Loaded), Some(Older::Offline)]);
    assert!(view.drawn.is_empty());
}

#[tokio::test]
async fn a_second_refresh_press_while_one_runs_starts_nothing() {
    let window = FakeWindow::new();
    let run = window.run();
    run.refresh_now();
    run.refresh_now();
    assert_eq!(window.view.borrow().refreshes, 1);
    run.refresh_done();
    run.refresh_now();
    assert_eq!(window.view.borrow().refreshes, 2);
}

#[tokio::test]
async fn a_page_the_view_took_away_draws_nothing() {
    let window = FakeWindow::new();
    window.during(Step::Occurrences, |window, run| {
        window.with(|v| v.pages.retain(|(id, _)| *id != ON_SCREEN));
        run.forget_pages([ON_SCREEN]);
    });
    let run = window.run();
    run.fill(ON_SCREEN);
    window.settle().await;
    assert!(window.view.borrow().drawn.is_empty());
    let _ = ACCOUNT;
}
