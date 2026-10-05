//! When a mail search runs while the person types, and how far it reaches.
//!
//! A pause of [`PAUSE`] after the last key runs the search; Enter runs it
//! at once. The store answers from the first letter. The servers hear a
//! search only from [`SERVER_LEAST`] letters on, or on Enter, since one or
//! two letters match nearly everything and would cost a request for each
//! pause. Nothing here touches GTK or a clock: the window owns the timer,
//! and asks [`Typing`] what to do when a key lands and when the timer ends.

use std::time::Duration;

/// How long the typing must stop before the search runs.
pub const PAUSE: Duration = Duration::from_millis(300);

/// The fewest letters a search needs before it goes to the servers.
pub const SERVER_LEAST: usize = 3;

/// How far a search reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// The mail on this computer alone.
    Store,
    /// The mail on this computer first, then the servers.
    Servers,
}

/// A search to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub query: String,
    pub reach: Reach,
}

/// What the window does after a key changed the text.
#[derive(Debug, PartialEq, Eq)]
pub enum Typed {
    /// Start a timer of [`PAUSE`] and hand this to [`Typing::paused`] when
    /// it ends.
    Wait(Pause),
    /// The field is empty: put back the mailbox the search started from.
    Cleared,
}

/// One timer the window started. Only the latest one runs a search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pause(u64);

/// The state of the search field between keys.
#[derive(Debug, Default)]
pub struct Typing {
    /// Bumped by every key, Enter and close, so an older timer finds
    /// itself stale.
    generation: u64,
    /// The text as the last key left it.
    text: String,
    /// The search on screen, so a pause that changed nothing runs nothing.
    shown: Option<Run>,
}

impl Typing {
    /// The text in the field changed to `text`.
    pub fn typed(&mut self, text: &str) -> Typed {
        self.generation += 1;
        self.text = text.trim().to_string();
        if self.text.is_empty() {
            self.shown = None;
            return Typed::Cleared;
        }
        Typed::Wait(Pause(self.generation))
    }

    /// The timer `pause` ran out. Gives the search to run, or nothing when
    /// a later key or Enter took over, or the text is what is on screen.
    pub fn paused(&mut self, pause: Pause) -> Option<Run> {
        if pause.0 != self.generation || self.text.is_empty() {
            return None;
        }
        let reach = match self.text.chars().count() >= SERVER_LEAST {
            true => Reach::Servers,
            false => Reach::Store,
        };
        let run = Run {
            query: self.text.clone(),
            reach,
        };
        if self.shown.as_ref() == Some(&run) {
            return None;
        }
        self.shown = Some(run.clone());
        Some(run)
    }

    /// Enter: runs `text` now and as far as the servers, however short.
    pub fn entered(&mut self, text: &str) -> Option<Run> {
        self.generation += 1;
        self.text = text.trim().to_string();
        if self.text.is_empty() {
            return None;
        }
        let run = Run {
            query: self.text.clone(),
            reach: Reach::Servers,
        };
        self.shown = Some(run.clone());
        Some(run)
    }

    /// A key stopped the servers' half of the search on screen, so the
    /// next pause runs it again even when the text comes back to it.
    pub fn interrupted(&mut self) {
        self.shown = None;
    }

    /// The search bar closed. A timer still running runs nothing.
    pub fn closed(&mut self) {
        self.generation += 1;
        self.text.clear();
        self.shown = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pause(typed: Typed) -> Pause {
        match typed {
            Typed::Wait(pause) => pause,
            Typed::Cleared => panic!("the field is not empty"),
        }
    }

    fn run(query: &str, reach: Reach) -> Option<Run> {
        Some(Run {
            query: query.into(),
            reach,
        })
    }

    #[test]
    fn a_pause_after_typing_runs_the_search_once() {
        let mut typing = Typing::default();
        let waited = pause(typing.typed("roadmap"));
        assert_eq!(typing.paused(waited), run("roadmap", Reach::Servers));
        assert_eq!(typing.paused(waited), None);
    }

    #[test]
    fn a_key_before_the_pause_ends_drops_the_earlier_timer() {
        let mut typing = Typing::default();
        let first = pause(typing.typed("road"));
        let second = pause(typing.typed("roadm"));
        assert_eq!(typing.paused(first), None);
        assert_eq!(typing.paused(second), run("roadm", Reach::Servers));
    }

    #[test]
    fn one_or_two_letters_search_the_store_alone() {
        let mut typing = Typing::default();
        let one = pause(typing.typed("r"));
        assert_eq!(typing.paused(one), run("r", Reach::Store));
        let two = pause(typing.typed("ro"));
        assert_eq!(typing.paused(two), run("ro", Reach::Store));
        let three = pause(typing.typed("roa"));
        assert_eq!(typing.paused(three), run("roa", Reach::Servers));
    }

    #[test]
    fn letters_count_rather_than_bytes() {
        let mut typing = Typing::default();
        let two = pause(typing.typed("çã"));
        assert_eq!(typing.paused(two), run("çã", Reach::Store));
    }

    #[test]
    fn enter_runs_at_once_and_reaches_the_servers_however_short() {
        let mut typing = Typing::default();
        let waited = pause(typing.typed("ro"));
        assert_eq!(typing.entered("ro"), run("ro", Reach::Servers));
        assert_eq!(typing.paused(waited), None, "the timer gave way to Enter");
    }

    #[test]
    fn typing_back_to_the_search_on_screen_runs_nothing() {
        let mut typing = Typing::default();
        let waited = pause(typing.typed("kites"));
        assert!(typing.paused(waited).is_some());
        let _ = typing.typed("kitesx");
        let back = pause(typing.typed("kites "));
        assert_eq!(typing.paused(back), None);
    }

    #[test]
    fn a_search_a_key_stopped_runs_again_when_the_text_comes_back() {
        // The key stopped the servers' half of "kites", so the rows on
        // screen are the store's alone and the search is not done.
        let mut typing = Typing::default();
        let waited = pause(typing.typed("kites"));
        assert!(typing.paused(waited).is_some());
        let _ = typing.typed("kitesx");
        typing.interrupted();
        let back = pause(typing.typed("kites"));
        assert_eq!(typing.paused(back), run("kites", Reach::Servers));
    }

    #[test]
    fn an_empty_field_puts_the_mailbox_back_and_runs_nothing() {
        let mut typing = Typing::default();
        let waited = pause(typing.typed("kites"));
        assert_eq!(typing.typed("  "), Typed::Cleared);
        assert_eq!(typing.paused(waited), None);
        assert_eq!(typing.entered(""), None);
    }

    #[test]
    fn closing_the_bar_drops_a_running_timer_and_forgets_the_search() {
        let mut typing = Typing::default();
        let shown = pause(typing.typed("kites"));
        assert!(typing.paused(shown).is_some());
        let waited = pause(typing.typed("kites and"));
        typing.closed();
        assert_eq!(typing.paused(waited), None);
        let again = pause(typing.typed("kites"));
        assert_eq!(typing.paused(again), run("kites", Reach::Servers));
    }
}
