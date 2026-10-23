//! The listing run with no window: a list of rows in memory behind the
//! pane, a store and servers that answer from fixtures, and a log of what
//! the run asked for, in order. Spawned work waits until a test settles
//! it, the way the main loop runs it after the event that started it.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use mailrs_domain::{AccountId, Category, Folder, Target, ThreadSummary};
use mailrs_sync::{Changed, Listing, Loaded, PAGE, Scope, Stop, View};

use super::{Answer, FollowAccount, ListingRun, Lists, Pane, Work};
use crate::ui::Mailbox;
use crate::ui::list_feed::{Named, Refresh, order};
use crate::ui::window::on_screen::{Redraw, Revealed};

/// One thing the run asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Call {
    List,
    StoredSearch,
    ServerSearch,
    Changed,
    GoneFrom,
    Follow,
    Redraw,
    Refreshing,
    SetRows,
    Append,
    Replace,
    DropRows,
    Loading,
    Select,
    Failed,
    Preview,
    PreviewArrived,
}

/// What a test reaches into a call with, while the call is out.
type During = Box<dyn FnOnce(&Rc<ListingRun>)>;

#[derive(Default)]
pub struct Shown {
    /// What the store lists for a stored mailbox, newest first.
    pub store: Vec<ThreadSummary>,
    pub answers: VecDeque<anyhow::Result<Listing>>,
    pub remote_labels: bool,
    /// The rows of the threads that left the folder on screen.
    pub gone: Vec<Target>,
    /// The rows on screen.
    pub rows: Vec<ThreadSummary>,
    /// The ids of each first page put on screen, oldest first.
    pub pages: Vec<Vec<String>>,
    /// Whether the spinner is up.
    pub spinner: bool,
    pub selected: Vec<Revealed>,
    /// The rows held each time the run asked for a later page.
    pub held: Vec<usize>,
    /// The stop each server search went out with.
    pub stops: Vec<Stop>,
    pub followed: Vec<(AccountId, String)>,
    pub follow_answers: VecDeque<anyhow::Result<()>>,
    pub accounts: HashMap<AccountId, Arc<()>>,
    pub labels: HashMap<String, Vec<ThreadSummary>>,
    pub failures: Vec<String>,
    pub previews: Vec<Target>,
    pub preview_answers: VecDeque<anyhow::Result<Option<String>>>,
    pub preview_arrivals: Vec<Target>,
    /// What the run asked for, oldest first.
    pub calls: Vec<Call>,
    during: HashMap<Call, During>,
    waiting: HashMap<Call, tokio::sync::oneshot::Receiver<()>>,
    spawned: Vec<Work>,
}

pub struct Fake {
    pub shown: RefCell<Shown>,
    run: RefCell<Weak<ListingRun>>,
}

impl Fake {
    /// A window with nothing listed yet, and its run.
    pub fn new() -> (Rc<Fake>, Rc<ListingRun>) {
        let fake = Rc::new(Fake {
            shown: RefCell::new(Shown {
                accounts: [(1, Arc::new(())), (2, Arc::new(()))].into(),
                ..Shown::default()
            }),
            run: RefCell::new(Weak::new()),
        });
        let run = Rc::new(ListingRun::new(
            Category::All,
            Rc::clone(&fake) as Rc<dyn Pane>,
            Rc::clone(&fake) as Rc<dyn Lists>,
        ));
        *fake.run.borrow_mut() = Rc::downgrade(&run);
        (fake, run)
    }

    pub fn with<T>(&self, change: impl FnOnce(&mut Shown) -> T) -> T {
        change(&mut self.shown.borrow_mut())
    }

    /// Runs `during` while the next `call` is out, before it answers.
    pub fn during(&self, call: Call, during: impl FnOnce(&Rc<ListingRun>) + 'static) {
        self.with(|s| s.during.insert(call, Box::new(during)));
    }

    /// Holds the next answer to `call` until the test releases it. The
    /// answer captures its rows before it waits, as an old store read can.
    pub fn hold(&self, call: Call) -> tokio::sync::oneshot::Sender<()> {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.with(|s| assert!(s.waiting.insert(call, receive).is_none()));
        send
    }

    /// The ids of the rows on screen.
    pub fn ids(&self) -> Vec<String> {
        self.with(|s| s.rows.iter().map(|r| r.id.clone()).collect())
    }

    pub fn calls(&self, call: Call) -> usize {
        self.with(|s| s.calls.iter().filter(|c| **c == call).count())
    }

    /// Runs everything the run spawned, and whatever that spawns, until
    /// nothing is left.
    pub async fn settle(&self) {
        let mut running: FuturesUnordered<Work> = FuturesUnordered::new();
        let all = futures::future::poll_fn(|cx| {
            loop {
                let spawned = std::mem::take(&mut self.shown.borrow_mut().spawned);
                let spawned_any = !spawned.is_empty();
                running.extend(spawned);
                match running.poll_next_unpin(cx) {
                    Poll::Ready(Some(())) => continue,
                    Poll::Ready(None) if !spawned_any && self.shown.borrow().spawned.is_empty() => {
                        return Poll::Ready(());
                    }
                    Poll::Ready(None) => continue,
                    Poll::Pending if !self.shown.borrow().spawned.is_empty() => continue,
                    Poll::Pending => return Poll::Pending,
                }
            }
        });
        tokio::time::timeout(Duration::from_secs(5), all)
            .await
            .expect("the run settled");
    }

    /// Notes `call` and runs what the test asked to happen while it is out.
    fn call(&self, call: Call) {
        let during = self.with(|s| {
            s.calls.push(call);
            s.during.remove(&call)
        });
        if let (Some(during), Some(run)) = (during, self.run.borrow().upgrade()) {
            during(&run);
        }
    }

    fn answer<T: 'static>(&self, call: Call, value: T) -> Answer<'_, T> {
        let waiting = self.with(|s| s.waiting.remove(&call));
        self.call(call);
        Box::pin(async move {
            if let Some(waiting) = waiting {
                waiting.await.expect("the test released the answer");
            }
            value
        })
    }
}

/// A row of account 1 for thread `id`, `age` minutes old.
pub fn row(id: &str, age: i64) -> ThreadSummary {
    ThreadSummary {
        account_id: 1,
        id: id.to_string(),
        last_message_at: 1_000_000_000 - age * 60_000,
        subject: id.to_string(),
        ..ThreadSummary::default()
    }
}

/// A page of the rows in `all` after `held`, as the store pages them.
fn page_after(all: &[ThreadSummary], held: &Loaded) -> Listing {
    let after: Vec<&ThreadSummary> = all
        .iter()
        .filter(|r| held.last.as_ref().is_none_or(|last| order(r) < order(last)))
        .collect();
    Listing {
        more: after.len() > PAGE,
        rows: after.into_iter().take(PAGE).cloned().collect(),
        ..Listing::default()
    }
}

/// What a search finds: one row named for the query and where it looked.
fn found(query: &str, place: &str) -> Listing {
    Listing {
        rows: vec![row(&format!("{query}-{place}"), 0)],
        ..Listing::default()
    }
}

impl Pane for Fake {
    fn scope(&self) -> Scope {
        Scope::default()
    }

    fn view(&self, _mailbox: &Mailbox, _category: Category) -> View {
        View::default()
    }

    fn follows(&self, mailbox: &Mailbox) -> Option<(AccountId, String)> {
        match mailbox {
            Mailbox::Label {
                account_id,
                label_id,
                ..
            } => Some((*account_id, label_id.clone())),
            _ => None,
        }
    }

    fn loaded(&self) -> Loaded {
        self.with(|s| Loaded::rows(&s.rows))
    }

    fn redraw(&self, _redraw: &Redraw) {
        self.call(Call::Redraw);
    }

    fn refreshing(&self, _refresh: &Refresh) {
        self.call(Call::Refreshing);
    }

    fn set_rows(&self, listing: Listing) {
        self.call(Call::SetRows);
        self.with(|s| {
            s.pages
                .push(listing.rows.iter().map(|r| r.id.clone()).collect());
            s.rows = listing.rows;
            s.spinner = false;
        });
    }

    fn append(&self, listing: Listing) {
        self.call(Call::Append);
        self.with(|s| s.rows.extend(listing.rows));
    }

    fn replace(&self, changed: &[Named], rows: Vec<ThreadSummary>, _title: &str, _subtitle: &str) {
        self.call(Call::Replace);
        self.with(|s| {
            s.rows.retain(|r| {
                !changed
                    .iter()
                    .any(|(a, id)| *a == r.account_id && *id == r.id)
            });
            for row in rows {
                let at = s.rows.partition_point(|held| order(held) > order(&row));
                s.rows.insert(at, row);
            }
        });
    }

    fn drop_rows(&self, gone: &[Target]) {
        self.call(Call::DropRows);
        self.with(|s| s.rows.retain(|r| !gone.contains(&Target::from_row(r))));
    }

    fn preview_arrived(&self, target: &Target, text: &str) {
        self.call(Call::PreviewArrived);
        self.with(|s| {
            s.preview_arrivals.push(target.clone());
            if let Some(row) = s
                .rows
                .iter_mut()
                .find(|r| Target::for_preview(r) == *target)
            {
                row.snippet = text.to_string();
            }
        });
    }

    fn loading(&self) {
        self.call(Call::Loading);
        self.with(|s| s.spinner = true);
    }

    fn select(&self, revealed: Revealed) {
        self.call(Call::Select);
        self.with(|s| s.selected.push(revealed));
    }

    fn failed(&self, err: &anyhow::Error) {
        self.call(Call::Failed);
        self.with(|s| s.failures.push(err.to_string()));
    }
}

impl Lists for Fake {
    fn is_remote(&self, mailbox: &Mailbox, _scope: &Scope) -> bool {
        mailbox.is_remote()
            || matches!(mailbox, Mailbox::Label { .. }) && self.with(|s| s.remote_labels)
    }

    fn list(
        &self,
        mailbox: Mailbox,
        _scope: Scope,
        _view: View,
        held: Loaded,
    ) -> Answer<'_, anyhow::Result<Listing>> {
        if held.count > 0 {
            self.with(|s| s.held.push(held.count));
        }
        if let Some(answer) = self.with(|s| s.answers.pop_front()) {
            return self.answer(Call::List, answer);
        }
        let listing = match &mailbox {
            Mailbox::Label { label_id, .. } => self.with(|s| {
                let rows = s
                    .labels
                    .get(label_id)
                    .cloned()
                    .unwrap_or_else(|| vec![row(&format!("{label_id}-1"), 0)]);
                page_after(&rows, &held)
            }),
            _ => self.with(|s| page_after(&s.store, &held)),
        };
        self.answer(Call::List, Ok(listing))
    }

    fn stored_search(
        &self,
        query: String,
        _only: Option<AccountId>,
        _scope: Scope,
        _view: View,
    ) -> Answer<'_, anyhow::Result<Listing>> {
        self.answer(Call::StoredSearch, Ok(found(&query, "stored")))
    }

    fn server_search(
        &self,
        mailbox: Mailbox,
        _scope: Scope,
        _view: View,
        stop: Stop,
    ) -> Answer<'_, anyhow::Result<Listing>> {
        let Mailbox::Search { query, .. } = mailbox else {
            panic!("a server search lists a search");
        };
        self.with(|s| s.stops.push(stop));
        self.answer(Call::ServerSearch, Ok(found(&query, "server")))
    }

    fn changed(
        &self,
        mailbox: Mailbox,
        named: Vec<Named>,
        _view: View,
    ) -> Answer<'_, Option<Changed>> {
        let changed = (!self.is_remote(&mailbox, &Scope::of_mail_accounts(&[]))).then(|| Changed {
            rows: self.with(|s| {
                s.store
                    .iter()
                    .filter(|r| {
                        named
                            .iter()
                            .any(|(a, id)| *a == r.account_id && *id == r.id)
                    })
                    .cloned()
                    .collect()
            }),
            ..Changed::default()
        });
        self.answer(Call::Changed, changed)
    }

    fn gone_from(&self, _folder: Folder, targets: Vec<Target>) -> Answer<'_, Vec<Target>> {
        let gone = self.with(|s| targets.into_iter().filter(|t| s.gone.contains(t)).collect());
        self.answer(Call::GoneFrom, gone)
    }

    fn account(&self, account_id: AccountId) -> Option<FollowAccount> {
        self.with(|s| s.accounts.get(&account_id).cloned().map(FollowAccount::of))
    }

    fn follow(
        &self,
        account_id: AccountId,
        server_mailbox: String,
    ) -> Answer<'_, anyhow::Result<()>> {
        let answer = self.with(|s| {
            s.followed.push((account_id, server_mailbox));
            if s.accounts.contains_key(&account_id) {
                s.follow_answers.pop_front().unwrap_or(Ok(()))
            } else {
                Err(mailrs_sync::SyncError::UnknownAccount(account_id).into())
            }
        });
        self.answer(Call::Follow, answer)
    }

    fn preview(&self, target: Target) -> Answer<'_, anyhow::Result<Option<String>>> {
        let answer = self.with(|s| {
            s.previews.push(target);
            s.preview_answers
                .pop_front()
                .unwrap_or(Ok(Some("Preview".into())))
        });
        self.answer(Call::Preview, answer)
    }

    fn sleep(&self, _time: Duration) -> Answer<'_, ()> {
        Box::pin(async {})
    }

    fn spawn(&self, work: Work) {
        self.with(|s| s.spawned.push(work));
    }
}
