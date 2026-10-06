//! A change of an event in the view, headless: the question, the write
//! through the copy, and what follows it, against the fake window.

use mailrs_domain::calendar::{Event, Notify, Occurrence};
use mailrs_sync::Permitted;
use mailrs_sync::calendar_copy::event_change::{Edit, EventChange, Undo};

use super::fake::{ACCOUNT, FakeWindow, Step, event_at, fixture_day};
use super::{Changing, Outcome};
use crate::ui::calendar::scope;

const MOVED: &str = "Moved “{title}”";
const FAILED: &str = "Could not move the event: {reason}";

/// The answer a question nobody needed to ask settles on.
fn go_ahead() -> scope::Answer {
    scope::Answer { scope: None, notify: Notify::Nobody, keep_time: false }
}

/// A move of `o` an hour later, held for Undo.
fn moving(o: &Occurrence) -> Changing {
    let edited = Event { start: o.start + 3_600_000, end: o.end + 3_600_000, ..Event::clone(&o.event) };
    Changing {
        account_id: ACCOUNT,
        change: EventChange::Edit {
            occurrence: o.clone(),
            edited,
            how: Edit { moves: true, ..Edit::default() },
        },
        kept_time: None,
        shown: Event::clone(&o.event),
        when: Some("16:00".to_string()),
        undo: Undo::Offer,
        said: MOVED.to_string(),
        failed: FAILED.to_string(),
    }
}

fn standup() -> Occurrence {
    event_at("Standup", fixture_day(), 9)
}

#[tokio::test]
async fn a_held_move_reloads_and_offers_undo() {
    let window = FakeWindow::new();
    window.with(|v| v.written = Ok(Permitted::Done(Some(7))));
    let run = window.run();
    let outcome = run.change(moving(&standup())).await;
    window.settle().await;
    assert_eq!(outcome, Outcome::Done);
    let view = window.view.borrow();
    assert_eq!(view.undos, vec![("Moved “Standup”".to_string(), 7)]);
    assert_eq!(view.sidebars.len(), 1, "the calendars are read again");
    assert_eq!(view.waiting_drawn.len(), 1, "and what waits for an answer");
    assert!(view.pushed.is_empty(), "a held change goes out once its toast closes");
}

#[tokio::test]
async fn a_cancelled_question_writes_nothing() {
    let window = FakeWindow::new();
    window.with(|v| v.chosen = Ok(None));
    let run = window.run();
    let outcome = run.change(moving(&standup())).await;
    assert_eq!(outcome, Outcome::Canceled);
    assert_eq!(window.count(Step::Write), 0);
}

#[tokio::test]
async fn a_save_with_no_undo_sends_the_change_at_once() {
    let window = FakeWindow::new();
    window.with(|v| v.written = Ok(Permitted::Done(None)));
    let run = window.run();
    let outcome = run.change(Changing { undo: Undo::Skip, ..moving(&standup()) }).await;
    assert_eq!(outcome, Outcome::Done);
    let view = window.view.borrow();
    assert_eq!(view.pushed, vec![ACCOUNT]);
    assert!(view.undos.is_empty());
}

#[tokio::test]
async fn keep_old_time_writes_the_change_without_the_move() {
    let window = FakeWindow::new();
    window.with(|v| v.chosen = Ok(Some(scope::Answer { keep_time: true, ..go_ahead() })));
    let o = standup();
    let kept = EventChange::Edit {
        occurrence: o.clone(),
        edited: Event { title: "Standup, renamed".to_string(), ..Event::clone(&o.event) },
        how: Edit::default(),
    };
    let run = window.run();
    run.change(Changing { kept_time: Some(kept), ..moving(&o) }).await;
    let written = window.view.borrow().writes.clone();
    assert_eq!(written, vec!["Standup, renamed".to_string()]);
}

#[tokio::test]
async fn a_change_the_account_lacks_the_permission_for_asks_for_it() {
    let window = FakeWindow::new();
    window.with(|v| v.written = Ok(Permitted::NeedsPermission));
    let run = window.run();
    let outcome = run.change(moving(&standup())).await;
    window.settle().await;
    assert_eq!(outcome, Outcome::NeedsPermission);
    let view = window.view.borrow();
    assert_eq!(view.permissions_asked, vec![ACCOUNT]);
    assert!(view.sidebars.is_empty(), "nothing changed to read again");
}

#[tokio::test]
async fn a_failed_write_says_why() {
    let window = FakeWindow::new();
    window.with(|v| v.written = Err("offline".to_string()));
    let run = window.run();
    let outcome = run.change(moving(&standup())).await;
    assert_eq!(outcome, Outcome::Failed);
    assert_eq!(
        window.view.borrow().toasts,
        vec!["Could not move the event: offline".to_string()]
    );
}

#[tokio::test]
async fn a_question_that_could_not_be_asked_says_why_and_writes_nothing() {
    let window = FakeWindow::new();
    window.with(|v| v.chosen = Err("no such account".to_string()));
    let run = window.run();
    let outcome = run.change(moving(&standup())).await;
    assert_eq!(outcome, Outcome::Failed);
    assert_eq!(window.count(Step::Write), 0);
    assert_eq!(window.view.borrow().toasts.len(), 1);
}
