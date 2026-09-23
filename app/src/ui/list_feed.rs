//! What the thread list loads next, and which answers still count.
//!
//! The window lists a mailbox a page at a time, re-reads the threads a
//! change event names, and waits a moment so that a burst of events costs
//! one refresh. Every one of those answers arrives after an `await`, and
//! by then the reader may have moved to another mailbox. [`ListFeed`]
//! holds the counters that sort this out: a generation that each first
//! page starts, whether more rows follow, whether a page is on its way,
//! and the threads waiting for the coalescing timer. The window feeds it
//! events, carries out what it answers, and hands every answer back
//! through it, so a stale one is dropped here and nowhere else.
//!
//! Nothing here touches GTK or a clock. The window owns the timer; the
//! feed only says when to start it and what to do when it fires.

use mailrs_domain::AccountId;
use mailrs_sync::Listing;

/// A thread a change event named.
pub type Named = (AccountId, String);

/// The generation a request went out under. An answer carrying an older
/// one belongs to a list that is no longer on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket(u64);

/// What the window does after a change event.
#[derive(Debug, PartialEq, Eq)]
pub enum Coalesce {
    /// Start the timer; call [`ListFeed::fire`] when it runs out.
    Arm,
    /// A timer is already running and will pick this change up.
    Joined,
}

/// What the timer's refresh should do.
#[derive(Debug, PartialEq, Eq)]
pub enum Refresh {
    /// List the mailbox again from the first page.
    Reload,
    /// Re-read these threads and put them back in place.
    Splice(Ticket, Vec<Named>),
}

impl Refresh {
    /// Whether the refresh covers the thread `thread_id` of `account_id`:
    /// a reload covers every thread, a splice the ones the events named.
    pub fn names(&self, account_id: AccountId, thread_id: &str) -> bool {
        match self {
            Refresh::Reload => true,
            Refresh::Splice(_, named) => named
                .iter()
                .any(|(account, id)| *account == account_id && id == thread_id),
        }
    }
}

/// What to do with the rows a splice re-read.
#[derive(Debug, PartialEq, Eq)]
pub enum Splice<T> {
    /// The list moved on while they were read; drop them.
    Stale,
    /// Put these rows in place of the named ones.
    Put(T),
    /// A remote mailbox cannot re-read threads. Drop the ones that left
    /// it and leave the rest alone.
    Prune,
    /// The mailbox could not say; list it again.
    Reload,
}

/// A first page that still belongs on screen, with the thread a reveal
/// was waiting to select once it landed.
#[derive(Debug, PartialEq, Eq)]
pub struct Landed<R> {
    pub reveal: Option<R>,
}

/// The thread list's loading state. `R` is what a reveal waits with: the
/// window keeps the thread and what to do once it is selected.
#[derive(Debug)]
pub struct ListFeed<R> {
    generation: u64,
    /// The timer is running.
    queued: bool,
    /// A queued change named no threads, so the list reloads whole.
    everything: bool,
    /// Threads the queued refresh re-reads.
    named: Vec<Named>,
    /// The mailbox holds rows past the ones loaded.
    more: bool,
    /// A page of older rows is on its way.
    loading_more: bool,
    /// The first page of the current generation is on its way.
    loading_first: bool,
    /// A thread to select once the first page lands.
    reveal: Option<R>,
}

impl<R> Default for ListFeed<R> {
    fn default() -> Self {
        ListFeed {
            generation: 0,
            queued: false,
            everything: false,
            named: Vec::new(),
            more: false,
            loading_more: false,
            // Nothing is on screen until the window lists its first
            // mailbox, so a reveal that comes in first waits for it.
            loading_first: true,
            reveal: None,
        }
    }
}

impl<R> ListFeed<R> {
    /// The store changed these threads. No threads means it cannot say
    /// which, and the list reloads whole.
    pub fn changed(&mut self, threads: Vec<Named>) -> Coalesce {
        self.everything |= threads.is_empty();
        self.named.extend(threads);
        self.arm()
    }

    /// Something changed that re-reading single threads cannot catch,
    /// such as new mail or an action on many threads.
    pub fn everything(&mut self) -> Coalesce {
        self.everything = true;
        self.arm()
    }

    fn arm(&mut self) -> Coalesce {
        match std::mem::replace(&mut self.queued, true) {
            true => Coalesce::Joined,
            false => Coalesce::Arm,
        }
    }

    /// The timer ran out. A reload wins over any threads queued with it,
    /// since it reads them too.
    pub fn fire(&mut self) -> Refresh {
        self.queued = false;
        let named = std::mem::take(&mut self.named);
        if std::mem::take(&mut self.everything) || named.is_empty() {
            Refresh::Reload
        } else {
            Refresh::Splice(Ticket(self.generation), named)
        }
    }

    /// Another mailbox is on screen. A reveal still waiting was for the
    /// one before it.
    pub fn shown(&mut self) -> Ticket {
        self.reveal = None;
        self.reload()
    }

    /// Starts listing the mailbox from its first page. Every answer to an
    /// earlier request goes stale, and no older page can follow until
    /// this one says whether there are more.
    pub fn reload(&mut self) -> Ticket {
        self.generation += 1;
        self.loading_first = true;
        self.loading_more = false;
        self.more = false;
        Ticket(self.generation)
    }

    /// The first page arrived. Gives nothing back when a later request
    /// has replaced it; otherwise the window shows it, or its error, and
    /// selects the thread a reveal was waiting on.
    pub fn first_page<E>(
        &mut self,
        ticket: Ticket,
        answer: &Result<Listing, E>,
    ) -> Option<Landed<R>> {
        if !self.current(ticket) {
            return None;
        }
        self.loading_first = false;
        if let Ok(listing) = answer {
            self.more = listing.more;
        }
        Some(Landed {
            reveal: self.reveal.take(),
        })
    }

    /// The reader scrolled near the end of the list. Answers with the
    /// ticket for the next page, or nothing when there is no next page or
    /// one is already on its way.
    pub fn scrolled_to_end(&mut self) -> Option<Ticket> {
        if !self.more || self.loading_first || self.loading_more {
            return None;
        }
        self.loading_more = true;
        Some(Ticket(self.generation))
    }

    /// A later page arrived. True when the window should append it, or
    /// report its error.
    pub fn next_page<E>(&mut self, ticket: Ticket, answer: &Result<Listing, E>) -> bool {
        if !self.current(ticket) {
            return false;
        }
        self.loading_more = false;
        if let Ok(listing) = answer {
            self.more = listing.more;
        }
        true
    }

    /// The threads a splice re-read arrived. `fresh` is nothing when the
    /// mailbox cannot re-read threads on their own, and `remote` says the
    /// mailbox lists through Gmail.
    pub fn spliced<T>(&self, ticket: Ticket, fresh: Option<T>, remote: bool) -> Splice<T> {
        match fresh {
            _ if !self.current(ticket) => Splice::Stale,
            Some(rows) => Splice::Put(rows),
            None if remote => Splice::Prune,
            None => Splice::Reload,
        }
    }

    /// Asks to select a thread once its rows are on screen. Hands it back
    /// when nothing is loading, for the window to select now; otherwise
    /// [`ListFeed::first_page`] hands it back when the page lands.
    pub fn reveal(&mut self, thread: R) -> Option<R> {
        if self.loading_first {
            self.reveal = Some(thread);
            return None;
        }
        Some(thread)
    }

    fn current(&self, ticket: Ticket) -> bool {
        ticket.0 == self.generation
    }
}

#[cfg(test)]
mod tests;
