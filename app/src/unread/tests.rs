use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use mailrs_sync::fake::{FakeImap, raw_message};
use mailrs_sync::{Mailboxes, now_millis};

use super::*;
use crate::assistant::run::fake::{Connected, Harness};

/// Answers every mailbox asked with `answer`, and keeps each ask.
#[derive(Default)]
struct Fake {
    asks: RefCell<Vec<Ask>>,
    answer: i64,
}

impl Counter for Rc<Fake> {
    async fn count(&self, ask: Ask) -> Result<Counts, String> {
        let mailboxes = ask
            .mailboxes
            .iter()
            .map(|m| (m.clone(), self.answer))
            .collect();
        self.asks.borrow_mut().push(ask);
        Ok(Counts {
            mailboxes,
            categories: HashMap::new(),
            smart: HashMap::new(),
        })
    }
}

/// The real counts over a test's accounts.
impl Counter for Arc<Mailboxes<Connected>> {
    async fn count(&self, ask: Ask) -> Result<Counts, String> {
        read(self, ask).await.map_err(|e| e.to_string())
    }
}

/// A window whose sidebar holds `mailboxes`, keeping the count of each
/// one it was shown, then of account 1's inbox.
struct Window {
    mailboxes: Vec<Mailbox>,
    shown: Rc<RefCell<Vec<Vec<i64>>>>,
}

impl Shows for Window {
    fn wanted(&self) -> Option<Wanted> {
        Some(Wanted {
            mailboxes: self.mailboxes.clone(),
            shown: self.mailboxes[0].clone(),
            view: View::default(),
        })
    }

    fn show(&self, counted: &Counted) {
        let mut seen: Vec<i64> = self.mailboxes.iter().map(|m| counted.of(m)).collect();
        seen.push(counted.inbox(1));
        self.shown.borrow_mut().push(seen);
    }
}

/// Starts the counts on the test's thread.
fn started<C: Counter + 'static>(unread: &Rc<Unread<C>>) {
    tokio::task::spawn_local(unread.start(tokio::time::sleep));
}

/// Lets every task on the thread run, timers included.
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(QUIET * 2).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

fn label(account_id: AccountId) -> Mailbox {
    Mailbox::Label {
        account_id,
        label_id: "Label_kites".into(),
        name: "Kites".into(),
    }
}

#[tokio::test(start_paused = true)]
async fn a_burst_of_changes_is_one_count() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fake = Rc::new(Fake::default());
            let unread = Unread::new(Rc::clone(&fake));
            started(&unread);
            unread.set_accounts(vec![1, 2]);
            for _ in 0..50 {
                unread.changed();
            }
            unread.answered();
            settle().await;
            assert_eq!(fake.asks.borrow().len(), 1, "the burst shared one count");
            assert!(
                !fake.asks.borrow()[0].refresh,
                "no window, so no server count"
            );

            // Sync reports batch after batch. With no window open, those
            // that come while the last count is still recent share one.
            for _ in 0..40 {
                unread.changed();
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            settle().await;
            assert_eq!(
                fake.asks.borrow().len(),
                3,
                "{:?}",
                fake.asks.borrow().len()
            );
        })
        .await;
}

#[tokio::test(start_paused = true)]
async fn a_change_during_a_count_gets_one_more() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fake = Rc::new(Fake::default());
            let unread = Unread::new(Rc::clone(&fake));
            let heard = Rc::new(Cell::new(0));
            let (weak, seen) = (Rc::downgrade(&unread), Rc::clone(&heard));
            // Each listener call asks again, as a change landing while the
            // count ran would, until three counts went by.
            unread.listen(Box::new(move |_| {
                seen.set(seen.get() + 1);
                if seen.get() < 3
                    && let Some(unread) = weak.upgrade()
                {
                    unread.changed();
                    unread.changed();
                }
            }));
            started(&unread);
            unread.changed();
            settle().await;
            settle().await;
            assert_eq!(fake.asks.borrow().len(), 3);
            assert_eq!(heard.get(), 3);
        })
        .await;
}

#[tokio::test(start_paused = true)]
async fn a_server_answer_with_no_window_reads_only_what_is_kept() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fake = Rc::new(Fake::default());
            let unread = Unread::new(Rc::clone(&fake));
            started(&unread);
            unread.answered();
            settle().await;
            assert!(!fake.asks.borrow()[0].refresh, "the tray starts no polling");
        })
        .await;
}

#[tokio::test(start_paused = true)]
async fn a_mailbox_the_window_shows_gets_its_count() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fake = Rc::new(Fake {
                answer: 4,
                ..Fake::default()
            });
            let unread = Unread::new(Rc::clone(&fake));
            let shown = Rc::new(RefCell::new(Vec::new()));
            unread.show_in(Box::new(Window {
                mailboxes: vec![label(1), ALL_INBOXES],
                shown: Rc::clone(&shown),
            }));
            started(&unread);
            unread.set_accounts(vec![1]);
            settle().await;
            let asks = fake.asks.borrow();
            assert_eq!(asks.len(), 1);
            assert_eq!(asks[0].shown, label(1), "the categories follow the window");
            let asked = &asks[0].mailboxes;
            assert_eq!(
                asked.iter().filter(|m| **m == ALL_INBOXES).count(),
                1,
                "a mailbox two subscribers show is counted once: {asked:?}"
            );
            // The label, All Inboxes, and the tray's inbox of account 1.
            assert_eq!(*shown.borrow(), [vec![4, 4, 4]]);
        })
        .await;
}

/// An IMAP account whose server holds unread mail the copy here has not
/// fetched yet: the server counts it, the store does not hold it.
#[tokio::test(start_paused = true)]
async fn the_tray_says_what_the_sidebar_says_for_a_folder_account() {
    let day = 24 * 60 * 60 * 1000;
    let now = now_millis();
    let imap = Arc::new(FakeImap::new());
    imap.deliver_flagged(
        "INBOX",
        &raw_message("first", "Today", now - day, None),
        &[],
        now - day,
    );
    let h = Harness::on_imap(Arc::clone(&imap)).await;
    imap.deliver_flagged("INBOX", &raw_message("second", "Now", now, None), &[], now);
    let lists = Arc::new(h.lists());
    let account_id = h.account_id;
    let local =
        h.db.read(move |c| {
            use mailrs_store::threads::{ThreadFilter, unread_threads};
            let inbox = mailrs_domain::MailSet::Role(mailrs_domain::Role::Inbox);
            unread_threads(c, &ThreadFilter::account(account_id, inbox))
        })
        .await
        .unwrap();
    assert_eq!(local, 1, "the tray used to count only this");
    tokio::task::LocalSet::new()
        .run_until(async move {
            let unread = Unread::new(Arc::clone(&lists));
            // The app wakes the counts on each server answer the same way.
            let mut answers = lists.counts_changed();
            let weak = Rc::downgrade(&unread);
            tokio::task::spawn_local(async move {
                while answers.changed().await.is_ok() {
                    let Some(unread) = weak.upgrade() else { break };
                    unread.answered();
                }
            });
            let sidebar = Rc::new(RefCell::new(None));
            let tray = Rc::new(RefCell::new(None));
            let (into_sidebar, into_tray) = (Rc::clone(&sidebar), Rc::clone(&tray));
            unread.listen(Box::new(move |counted| {
                *into_sidebar.borrow_mut() = Some(counted.all_inboxes());
            }));
            unread.listen(Box::new(move |counted| {
                *into_tray.borrow_mut() = Some(counted.inbox(account_id));
            }));
            // The sidebar's own count is the one that asks the server.
            unread.show_in(Box::new(Window {
                mailboxes: vec![ALL_INBOXES],
                shown: Rc::default(),
            }));
            started(&unread);
            unread.set_accounts(vec![account_id]);
            for _ in 0..5 {
                settle().await;
            }
            assert_eq!(*sidebar.borrow(), Some(2), "the server's count");
            assert_eq!(*tray.borrow(), *sidebar.borrow());
        })
        .await;
}

fn threads_changed() -> mailrs_domain::ChangeEvent {
    mailrs_domain::ChangeEvent::ThreadsChanged {
        account_id: 1,
        thread_ids: vec!["t".into()],
    }
}

#[tokio::test(start_paused = true)]
async fn with_no_window_a_burst_of_events_makes_one_count_that_asks_no_server() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fake = Rc::new(Fake::default());
            let unread = Unread::new(Rc::clone(&fake));
            started(&unread);
            for _ in 0..30 {
                unread.heard(&threads_changed());
            }
            settle().await;
            let asks = fake.asks.borrow();
            assert_eq!(asks.len(), 1);
            assert!(!asks[0].refresh, "the tray starts no server count");
        })
        .await;
}

#[tokio::test(start_paused = true)]
async fn with_no_window_only_thread_and_account_events_ask_for_a_count() {
    use mailrs_domain::{AccountState, ChangeEvent};
    tokio::task::LocalSet::new()
        .run_until(async {
            let fake = Rc::new(Fake::default());
            let unread = Unread::new(Rc::clone(&fake));
            started(&unread);
            unread.heard(&ChangeEvent::NewMail {
                account_id: 1,
                message_ids: vec!["m".into()],
            });
            unread.heard(&ChangeEvent::LabelsChanged { account_id: 1 });
            settle().await;
            assert!(fake.asks.borrow().is_empty());
            unread.heard(&ChangeEvent::AccountStateChanged {
                account_id: 1,
                state: AccountState::Ok,
            });
            settle().await;
            assert_eq!(fake.asks.borrow().len(), 1);
        })
        .await;
}

#[tokio::test(start_paused = true)]
async fn with_a_window_every_event_asks_the_servers_as_before() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fake = Rc::new(Fake::default());
            let unread = Unread::new(Rc::clone(&fake));
            unread.show_in(Box::new(Window {
                mailboxes: vec![ALL_INBOXES],
                shown: Rc::default(),
            }));
            started(&unread);
            settle().await;
            tokio::time::sleep(REFRESH_GAP).await;
            unread.heard(&mailrs_domain::ChangeEvent::LabelsChanged { account_id: 1 });
            settle().await;
            let asks = fake.asks.borrow();
            assert_eq!(asks.len(), 2);
            assert!(asks[1].refresh);
        })
        .await;
}

/// A new account's first sync sends events for seconds on end. With a
/// window open each one gets its own store read at once, but the servers
/// are asked again only once the gap since the last time has passed.
#[tokio::test(start_paused = true)]
async fn with_a_window_a_long_burst_asks_the_servers_once_per_gap() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fake = Rc::new(Fake::default());
            let unread = Unread::new(Rc::clone(&fake));
            unread.show_in(Box::new(Window {
                mailboxes: vec![ALL_INBOXES],
                shown: Rc::default(),
            }));
            started(&unread);
            settle().await;
            for _ in 0..30 {
                unread.heard(&threads_changed());
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            settle().await;
            settle().await;
            let asks = fake.asks.borrow();
            assert!(asks.len() > 30, "each event read the store: {}", asks.len());
            let refreshes = asks.iter().filter(|a| a.refresh).count();
            assert_eq!(refreshes, 2, "the first count, then one after the gap");
            assert!(
                asks.last().is_some_and(|a| a.refresh),
                "the owed refresh still came"
            );
            // A count that waits on the servers still reads every mailbox
            // the window shows, not only the inboxes.
            assert!(asks.iter().all(|a| a.full), "{asks:?}");
        })
        .await;
}

/// With no window open, the tray asks each account's server once, when the
/// account has started, so it counts unread mail older than what this
/// computer keeps. After that it reads only what is kept.
#[tokio::test(start_paused = true)]
async fn with_no_window_each_account_asks_its_server_once_when_it_starts() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fake = Rc::new(Fake::default());
            let unread = Unread::new(Rc::clone(&fake));
            started(&unread);
            unread.set_accounts(vec![1, 2]);
            settle().await;
            assert!(
                fake.asks.borrow().iter().all(|a| !a.refresh),
                "nothing started yet"
            );
            unread.started(1);
            settle().await;
            // The second account starts soon after the first.
            unread.started(2);
            for _ in 0..10 {
                settle().await;
            }
            let refreshed = fake.asks.borrow().iter().filter(|a| a.refresh).count();
            assert_eq!(refreshed, 2, "one server count for each account");

            // Mail moves, servers answer, and a sync settings change starts
            // the accounts again: none of it asks a server.
            unread.heard(&threads_changed());
            unread.answered();
            unread.started(1);
            unread.started(2);
            for _ in 0..10 {
                settle().await;
            }
            let refreshed = fake.asks.borrow().iter().filter(|a| a.refresh).count();
            assert_eq!(refreshed, 2, "no more server counts without a window");
        })
        .await;
}

/// The only unread thread of `h`'s account.
async fn the_unread_thread(h: &Harness) -> String {
    let account_id = h.account_id;
    h.db.read(move |c| {
        Ok(c.query_row(
            "SELECT id FROM threads WHERE account_id = ?1 AND unread = 1",
            [account_id],
            |row| row.get::<_, String>(0),
        )?)
    })
    .await
    .expect("one unread thread")
}

/// A window that shows All Inboxes until the test closes it.
struct Closable(Rc<std::cell::Cell<bool>>);

impl Shows for Closable {
    fn wanted(&self) -> Option<Wanted> {
        self.0.get().then(|| Wanted {
            mailboxes: vec![ALL_INBOXES],
            shown: ALL_INBOXES,
            view: View::default(),
        })
    }

    fn show(&self, _: &Counted) {}
}

/// The tray shows one unread, the person reads it here, with the window
/// open when `window` says so and closed after, and the tray shows none,
/// a minute on as well.
async fn the_tray_goes_to_none_once_read(h: Harness, window: bool) {
    let lists = Arc::new(h.lists());
    let account_id = h.account_id;
    let sync = h.sync();
    let thread = the_unread_thread(&h).await;
    tokio::task::LocalSet::new()
        .run_until(async move {
            let unread = Unread::new(Arc::clone(&lists));
            let mut answers = lists.counts_changed();
            let weak = Rc::downgrade(&unread);
            tokio::task::spawn_local(async move {
                while answers.changed().await.is_ok() {
                    let Some(unread) = weak.upgrade() else { break };
                    unread.answered();
                }
            });
            let tray = Rc::new(RefCell::new(Vec::new()));
            let into_tray = Rc::clone(&tray);
            unread.listen(Box::new(move |counted| {
                into_tray
                    .borrow_mut()
                    .push((counted.all_inboxes(), counted.inbox(account_id)));
            }));
            started(&unread);
            unread.set_accounts(vec![account_id]);
            unread.started(account_id);
            for _ in 0..5 {
                settle().await;
            }
            assert_eq!(tray.borrow().last(), Some(&(1, 1)), "{:?}", tray.borrow());

            let open = Rc::new(std::cell::Cell::new(window));
            if window {
                unread.show_in(Box::new(Closable(Rc::clone(&open))));
                settle().await;
            }
            sync.triage_thread(&thread, &mailrs_sync::TriageAction::MarkRead)
                .await
                .unwrap();
            unread.heard(&mailrs_domain::ChangeEvent::ThreadsChanged {
                account_id,
                thread_ids: vec![thread.clone()],
            });
            for _ in 0..5 {
                settle().await;
            }
            assert_eq!(tray.borrow().last(), Some(&(0, 0)), "{:?}", tray.borrow());
            // The window closes once the counts have asked the servers again.
            tokio::time::sleep(REFRESH_GAP).await;
            for _ in 0..5 {
                settle().await;
            }
            open.set(false);
            unread.changed();
            for _ in 0..5 {
                settle().await;
            }
            assert_eq!(tray.borrow().last(), Some(&(0, 0)), "{:?}", tray.borrow());
            tokio::time::sleep(Duration::from_secs(61)).await;
            for _ in 0..5 {
                settle().await;
            }
            assert_eq!(tray.borrow().last(), Some(&(0, 0)), "{:?}", tray.borrow());
        })
        .await;
}

async fn gmail_with_one_unread() -> Harness {
    let now = now_millis();
    Harness::with(vec![mailrs_sync::fake::meta(
        "m1",
        "t1",
        now,
        &["INBOX", "UNREAD"],
    )])
    .await
}

async fn imap_with_one_unread() -> Harness {
    let imap = Arc::new(FakeImap::new());
    let now = now_millis();
    imap.deliver_flagged("INBOX", &raw_message("one", "One", now, None), &[], now);
    Harness::on_imap(imap).await
}

async fn jmap_with_one_unread() -> Harness {
    let jmap = Arc::new(mailrs_sync::fake::FakeJmap::new());
    let now = now_millis();
    jmap.deliver("inbox", &raw_message("one", "One", now, None), now);
    Harness::with_services(move |_, services| {
        *services = mailrs_sync::AccountServices::fake_jmap(jmap);
    })
    .await
}

#[tokio::test(start_paused = true)]
async fn the_tray_goes_to_none_once_read_on_gmail() {
    the_tray_goes_to_none_once_read(gmail_with_one_unread().await, false).await;
    the_tray_goes_to_none_once_read(gmail_with_one_unread().await, true).await;
}

#[tokio::test(start_paused = true)]
async fn the_tray_goes_to_none_once_read_on_imap() {
    the_tray_goes_to_none_once_read(imap_with_one_unread().await, false).await;
    the_tray_goes_to_none_once_read(imap_with_one_unread().await, true).await;
}

#[tokio::test(start_paused = true)]
async fn the_tray_goes_to_none_once_read_on_jmap() {
    the_tray_goes_to_none_once_read(jmap_with_one_unread().await, false).await;
    the_tray_goes_to_none_once_read(jmap_with_one_unread().await, true).await;
}
