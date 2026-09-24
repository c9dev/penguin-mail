//! The mailbox on screen: which mailbox the main window lists, the one a
//! search goes back to when it closes, the inbox category, and the list
//! feed that loads its rows.
//!
//! The sidebar, the search bar, a notification, a deleted label and a
//! settings change all put another mailbox on screen, and each used to
//! pick its own handful of redraws: search forgot the selection and the
//! Outbox actions, and a mailbox that vanished was listed twice. Every
//! transition here changes the state and answers with a [`Redraw`] naming
//! what went stale, the way `Aftermath` does for a mail action, and
//! `MainWindow::redraw` carries it out. Nothing here touches GTK.

use mailrs_domain::{AccountId, Category, SmartMailbox};

use super::Reveal;
use crate::ui::{Mailbox, Standard};
use crate::ui::list_feed::{ListFeed, Ticket};

/// A thread to select once its row is on screen, and what to do with it.
pub(super) type Revealed = (AccountId, String, Reveal);

/// What the sidebar shows as chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Sidebar {
    /// This mailbox's row.
    Select(Mailbox),
    /// No row, for a search, which has none.
    Clear,
}

/// The parts of the window a transition leaves stale.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Redraw {
    /// Close the search bar: a mailbox chosen elsewhere replaces the search.
    pub close_search: bool,
    pub sidebar: Option<Sidebar>,
    /// The list's title and subtitle until the listing names its own.
    pub title: Option<(String, String)>,
    /// Drop the selection and the open conversation, and bring the list
    /// back into view on a narrow window.
    pub leave: bool,
    /// What hangs on the mailbox: the account column, the trash and junk
    /// buttons and their words, the Outbox actions, the category switcher
    /// and the Follow Up banner.
    pub follow: bool,
    /// Show which category is chosen.
    pub category: bool,
    /// List the first page under this ticket.
    pub list: Option<Ticket>,
}

/// The mailbox on screen and everything that hangs on it.
#[derive(Debug)]
pub(super) struct OnScreen {
    mailbox: Mailbox,
    before_search: Mailbox,
    category: Category,
    feed: ListFeed<Revealed>,
}

fn inbox() -> Mailbox {
    Mailbox::Unified(Standard::Inbox)
}

impl OnScreen {
    /// The inbox, split into `category` when categories are on.
    pub(super) fn new(category: Category) -> OnScreen {
        OnScreen {
            mailbox: inbox(),
            before_search: inbox(),
            category,
            feed: ListFeed::default(),
        }
    }

    pub(super) fn mailbox(&self) -> &Mailbox {
        &self.mailbox
    }

    pub(super) fn category(&self) -> Category {
        self.category
    }

    /// The feed, for the answers to what it asked for.
    pub(super) fn feed(&mut self) -> &mut ListFeed<Revealed> {
        &mut self.feed
    }

    /// Puts `mailbox` on screen. `search_open` says the search bar is
    /// up, which a mailbox other than a search closes.
    pub(super) fn show(&mut self, mailbox: Mailbox, search_open: bool) -> Redraw {
        if matches!(mailbox, Mailbox::Search { .. }) {
            let query = match &mailbox {
                Mailbox::Search { query, .. } => query.clone(),
                _ => String::new(),
            };
            return self.search(query);
        }
        self.before_search = mailbox.clone();
        self.mailbox = mailbox.clone();
        Redraw {
            close_search: search_open,
            sidebar: Some(Sidebar::Select(mailbox.clone())),
            title: Some((mailbox.title(), String::new())),
            leave: true,
            follow: true,
            category: false,
            list: Some(self.feed.shown()),
        }
    }

    /// Searches for `query` in the account on screen, or in every account
    /// when the mailbox spans them.
    pub(super) fn search(&mut self, query: String) -> Redraw {
        if !matches!(self.mailbox, Mailbox::Search { .. }) {
            self.before_search = self.mailbox.clone();
        }
        self.mailbox = Mailbox::Search {
            query: query.clone(),
            account_id: self.mailbox.account(),
        };
        Redraw {
            close_search: false,
            sidebar: Some(Sidebar::Clear),
            title: Some((mailbox_search_title(), query)),
            leave: true,
            follow: true,
            category: false,
            list: Some(self.feed.shown()),
        }
    }

    /// The search bar closed. A search on screen gives way to the
    /// mailbox it started from.
    pub(super) fn search_closed(&mut self) -> Option<Redraw> {
        if !matches!(self.mailbox, Mailbox::Search { .. }) {
            return None;
        }
        let back = self.before_search.clone();
        Some(self.show(back, false))
    }

    /// The accounts and their labels were read again. `still_there` says
    /// whether the mailbox on screen survived: a label deleted in the
    /// browser, or a signed-out account, leaves the inbox in its place.
    pub(super) fn accounts_read(&mut self, still_there: bool) -> Option<Redraw> {
        (!still_there).then(|| self.show(inbox(), false))
    }

    /// A label was deleted here. Its mailbox gives way to the inbox.
    pub(super) fn label_deleted(&mut self, account_id: AccountId, label_id: &str) -> Option<Redraw> {
        let showing = matches!(
            &self.mailbox,
            Mailbox::Label { account_id: a, label_id: l, .. } if *a == account_id && l == label_id
        );
        showing.then(|| self.show(inbox(), false))
    }

    /// Follow Up was turned off, which takes its mailbox away.
    pub(super) fn follow_ups_off(&mut self) -> Option<Redraw> {
        (self.mailbox == Mailbox::FollowUp).then(|| self.show(inbox(), false))
    }

    /// The smart mailboxes were saved. The one on screen carries its own
    /// copy of its conditions, so it takes the saved ones and lists again.
    pub(super) fn smart_saved(&mut self, saved: &[SmartMailbox]) -> Option<Redraw> {
        let Mailbox::Smart(shown) = &self.mailbox else {
            return None;
        };
        let fresh = saved.iter().find(|m| m.id == shown.id)?.clone();
        self.mailbox = Mailbox::Smart(fresh);
        Some(Redraw {
            list: Some(self.feed.reload()),
            ..Redraw::default()
        })
    }

    /// The list groups mail another way, so every row is another row.
    pub(super) fn list_shape(&mut self) -> Redraw {
        Redraw {
            leave: true,
            list: Some(self.feed.reload()),
            ..Redraw::default()
        }
    }

    /// The reader chose another inbox category.
    pub(super) fn choose_category(&mut self, category: Category) -> Option<Redraw> {
        if std::mem::replace(&mut self.category, category) == category {
            return None;
        }
        Some(Redraw {
            leave: true,
            category: true,
            list: Some(self.feed.reload()),
            ..Redraw::default()
        })
    }

    /// Categories were turned on or off, or the one to open on changed.
    pub(super) fn categories_changed(&mut self) -> Redraw {
        Redraw {
            follow: true,
            list: Some(self.feed.reload()),
            ..Redraw::default()
        }
    }

    /// A thread opened from outside the window, such as a notification.
    /// It lives in the inbox, which comes on screen first; the thread
    /// comes back at once when its row can be selected now, and otherwise
    /// once the inbox's first page lands.
    pub(super) fn reveal(&mut self, thread: Revealed) -> (Option<Redraw>, Option<Revealed>) {
        let redraw = (self.mailbox != inbox()).then(|| self.show(inbox(), false));
        let now = self.feed.reveal(thread);
        (redraw, now)
    }
}

/// The title a search gives the list.
fn mailbox_search_title() -> String {
    mailrs_domain::translate::gettext("Search")
}

/// Whether a request for the sidebar counts should start a count now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Count {
    /// Start one on the next turn of the main loop.
    Start,
    /// One is already waiting to start, or running and will run again.
    Joined,
}

/// Coalesces requests for the sidebar counts. An account change used to
/// count twice, and a burst of events once per event: two grouped
/// queries over the whole store each time. Requests made before a count
/// starts share it, and requests made while one runs share one more
/// after it, since that one may have read the store too early.
#[derive(Debug, Default)]
pub(super) struct Counts {
    waiting: bool,
    running: bool,
    again: bool,
}

impl Counts {
    /// Something changed what the counts would say.
    pub(super) fn ask(&mut self) -> Count {
        if self.waiting || self.running {
            self.again |= self.running;
            return Count::Joined;
        }
        self.waiting = true;
        Count::Start
    }

    /// The count starts.
    pub(super) fn start(&mut self) {
        self.waiting = false;
        self.running = true;
    }

    /// The count finished. `Start` means a request came in while it ran.
    pub(super) fn done(&mut self) -> Option<Count> {
        self.running = false;
        std::mem::take(&mut self.again).then(|| {
            self.waiting = true;
            Count::Start
        })
    }
}

#[cfg(test)]
mod tests {
    use mailrs_domain::Folder;

    use super::*;

    fn label(id: &str) -> Mailbox {
        Mailbox::Label {
            account_id: 1,
            label_id: id.into(),
            name: id.into(),
        }
    }

    fn trash() -> Mailbox {
        Mailbox::Folder {
            account_id: Some(1),
            folder: Folder::Trash,
        }
    }

    fn screen() -> OnScreen {
        OnScreen::new(Category::All)
    }

    #[test]
    fn a_mailbox_comes_on_screen_with_everything_that_hangs_on_it() {
        let mut screen = screen();
        let redraw = screen.show(trash(), false);
        assert_eq!(screen.mailbox(), &trash());
        assert!(redraw.leave && redraw.follow && redraw.list.is_some());
        assert_eq!(redraw.sidebar, Some(Sidebar::Select(trash())));
        assert!(!redraw.close_search);
    }

    #[test]
    fn a_search_drops_the_selection_and_follows_like_any_mailbox() {
        let mut screen = screen();
        screen.show(label("Work"), false);
        let redraw = screen.search("invoice".into());
        assert_eq!(
            screen.mailbox(),
            &Mailbox::Search {
                query: "invoice".into(),
                account_id: Some(1),
            }
        );
        assert!(redraw.leave, "the rows selected before are gone");
        assert!(redraw.follow, "the Outbox actions and folder buttons follow");
        assert_eq!(redraw.sidebar, Some(Sidebar::Clear));
        assert_eq!(redraw.title, Some(("Search".into(), "invoice".into())));
    }

    #[test]
    fn closing_a_search_goes_back_where_it_started_even_after_a_second_search() {
        let mut screen = screen();
        screen.show(trash(), false);
        screen.search("a".into());
        screen.search("b".into());
        let redraw = screen.search_closed().expect("a search was on screen");
        assert_eq!(screen.mailbox(), &trash());
        assert_eq!(redraw.sidebar, Some(Sidebar::Select(trash())));
        assert_eq!(screen.search_closed(), None, "nothing to close twice");
    }

    #[test]
    fn a_mailbox_chosen_during_a_search_closes_it_and_stays() {
        let mut screen = screen();
        screen.search("a".into());
        let redraw = screen.show(label("Work"), true);
        assert!(redraw.close_search);
        // The search bar reports closing; the mailbox chosen stays.
        assert_eq!(screen.search_closed(), None);
        assert_eq!(screen.mailbox(), &label("Work"));
    }

    #[test]
    fn a_vanished_mailbox_gives_way_to_the_inbox_listed_once() {
        let mut screen = screen();
        screen.show(label("Work"), false);
        assert_eq!(screen.accounts_read(true), None);
        let redraw = screen.accounts_read(false).expect("the inbox takes over");
        assert_eq!(screen.mailbox(), &inbox());
        assert!(redraw.list.is_some());
    }

    #[test]
    fn deleting_the_label_on_screen_goes_back_to_the_inbox() {
        let mut screen = screen();
        screen.show(label("Work"), false);
        assert_eq!(screen.label_deleted(1, "Travel"), None);
        assert_eq!(screen.label_deleted(2, "Work"), None);
        assert!(screen.label_deleted(1, "Work").is_some());
        assert_eq!(screen.mailbox(), &inbox());
    }

    #[test]
    fn follow_up_turned_off_takes_its_mailbox_away() {
        let mut screen = screen();
        assert_eq!(screen.follow_ups_off(), None);
        screen.show(Mailbox::FollowUp, false);
        assert!(screen.follow_ups_off().is_some());
        assert_eq!(screen.mailbox(), &inbox());
    }

    #[test]
    fn a_saved_smart_mailbox_on_screen_takes_its_new_conditions() {
        let mut screen = screen();
        let old = SmartMailbox {
            id: "s1".into(),
            name: "Ann".into(),
            account: None,
            match_all: true,
            conditions: Vec::new(),
        };
        screen.show(Mailbox::Smart(old.clone()), false);
        let saved = SmartMailbox {
            name: "Ann B".into(),
            ..old.clone()
        };
        let redraw = screen.smart_saved(std::slice::from_ref(&saved)).unwrap();
        assert_eq!(screen.mailbox(), &Mailbox::Smart(saved));
        assert!(redraw.list.is_some() && !redraw.leave);
        assert_eq!(screen.smart_saved(&[]), None);
    }

    #[test]
    fn choosing_the_category_on_screen_changes_nothing() {
        let mut screen = screen();
        assert_eq!(screen.choose_category(Category::All), None);
        let redraw = screen.choose_category(Category::Updates).unwrap();
        assert!(redraw.leave && redraw.category && redraw.list.is_some());
        assert_eq!(screen.category(), Category::Updates);
    }

    #[test]
    fn a_reveal_waits_for_the_inbox_it_brings_on_screen() {
        let mut screen = screen();
        screen.show(trash(), false);
        let (redraw, now) = screen.reveal((1, "t1".into(), Reveal::Reply));
        assert!(redraw.is_some());
        assert_eq!(now, None, "the inbox's rows are not there yet");
        let ticket = redraw.unwrap().list.unwrap();
        let landed = screen
            .feed()
            .first_page::<()>(ticket, &Ok(mailrs_sync::Listing::default()))
            .unwrap();
        assert_eq!(landed.reveal, Some((1, "t1".into(), Reveal::Reply)));
    }

    #[test]
    fn a_reveal_in_the_inbox_on_screen_selects_at_once() {
        let mut screen = screen();
        let ticket = screen.show(inbox(), false).list.unwrap();
        screen
            .feed()
            .first_page::<()>(ticket, &Ok(mailrs_sync::Listing::default()));
        let (redraw, now) = screen.reveal((1, "t1".into(), Reveal::Read));
        assert_eq!(redraw, None);
        assert_eq!(now, Some((1, "t1".into(), Reveal::Read)));
    }

    #[test]
    fn requests_for_the_counts_share_one_count() {
        let mut counts = Counts::default();
        assert_eq!(counts.ask(), Count::Start);
        assert_eq!(counts.ask(), Count::Joined);
        counts.start();
        assert_eq!(counts.done(), None, "nothing came in while it ran");
        assert_eq!(counts.ask(), Count::Start);
    }

    #[test]
    fn a_request_while_the_counts_run_counts_once_more_after() {
        let mut counts = Counts::default();
        counts.ask();
        counts.start();
        assert_eq!(counts.ask(), Count::Joined);
        assert_eq!(counts.ask(), Count::Joined);
        assert_eq!(counts.done(), Some(Count::Start));
        counts.start();
        assert_eq!(counts.done(), None);
    }
}
