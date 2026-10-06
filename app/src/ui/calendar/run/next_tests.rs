//! The next-event card's read, headless: a card port in memory, a copy
//! that answers from a list, and something that can happen while a read
//! waits.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use mailrs_domain::EpochMillis;
use mailrs_domain::calendar::Occurrence;

use super::fake::{event_at, fixture_day};
use super::{Answer, Card, CardRead, NextCard, Work};
use crate::ui::calendar::next::NextUp;

/// What happens while a read waits for its answer.
type During = Box<dyn FnOnce(&Rc<FakeCard>, &Rc<NextCard>)>;

#[derive(Default)]
struct Shelf {
    now: EpochMillis,
    copy: Vec<Occurrence>,
    during: Vec<During>,
    spawned: Vec<Work>,
    /// What the card showed, by title, `None` for the card taken down.
    shown: Vec<Option<String>>,
}

#[derive(Default)]
struct FakeCard {
    shelf: RefCell<Shelf>,
    me: RefCell<std::rc::Weak<FakeCard>>,
    next: RefCell<std::rc::Weak<NextCard>>,
}

impl FakeCard {
    fn new(now: EpochMillis) -> (Rc<FakeCard>, Rc<NextCard>) {
        let card = Rc::new(FakeCard::default());
        card.me.replace(Rc::downgrade(&card));
        card.shelf.borrow_mut().now = now;
        let next = Rc::new(NextCard::new(Rc::clone(&card) as Rc<dyn Card>));
        card.next.replace(Rc::downgrade(&next));
        (card, next)
    }

    /// Runs what the card spawned, and whatever that spawns.
    async fn settle(&self) {
        loop {
            let spawned = std::mem::take(&mut self.shelf.borrow_mut().spawned);
            if spawned.is_empty() {
                return;
            }
            for work in spawned {
                work.await;
            }
        }
    }
}

impl Card for FakeCard {
    fn now(&self) -> EpochMillis {
        self.shelf.borrow().now
    }

    fn read(&self, from: EpochMillis, to: EpochMillis) -> Answer<'_, Result<CardRead, String>> {
        let found: Vec<Occurrence> = self
            .shelf
            .borrow()
            .copy
            .iter()
            .filter(|o| o.start < to && o.end > from)
            .cloned()
            .collect();
        let during = self.shelf.borrow_mut().during.pop();
        let (me, next) = (self.me.borrow().upgrade(), self.next.borrow().upgrade());
        Box::pin(async move {
            if let (Some(during), Some(me), Some(next)) = (during, me, next) {
                during(&me, &next);
            }
            Ok((found, HashMap::new()))
        })
    }

    fn show(&self, next: Option<(NextUp, String)>) {
        let title = next.map(|(up, _)| up.occurrence().event.title.clone());
        self.shelf.borrow_mut().shown.push(title);
    }

    fn spawn(&self, work: Work) {
        self.shelf.borrow_mut().spawned.push(work);
    }
}

/// Eight in the morning on the fixture day, local time.
fn morning() -> EpochMillis {
    event_at("clock", fixture_day(), 8).start
}

#[tokio::test]
async fn the_card_shows_the_event_starting_soonest() {
    let (card, next) = FakeCard::new(morning());
    card.shelf.borrow_mut().copy = vec![event_at("Standup", fixture_day(), 9)];
    next.refresh();
    card.settle().await;
    assert_eq!(card.shelf.borrow().shown, vec![Some("Standup".to_string())]);
}

#[tokio::test]
async fn with_nothing_ahead_the_card_comes_down() {
    let (card, next) = FakeCard::new(morning());
    next.refresh();
    card.settle().await;
    assert_eq!(card.shelf.borrow().shown, vec![None]);
}

/// 32ade70b: the minute timer and a sync each start a read, and the
/// older one, ending last, put back an event the newer one had dropped.
#[tokio::test]
async fn only_the_newest_next_event_read_writes_the_card() {
    let (card, next) = FakeCard::new(morning());
    card.shelf.borrow_mut().copy = vec![event_at("Cancelled call", fixture_day(), 9)];
    card.shelf.borrow_mut().during.push(Box::new(|card, next| {
        card.shelf.borrow_mut().copy.clear();
        next.refresh();
    }));
    next.refresh();
    card.settle().await;
    assert_eq!(card.shelf.borrow().shown, vec![None]);
}
