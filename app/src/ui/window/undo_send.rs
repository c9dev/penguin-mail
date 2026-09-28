//! Undo Send while messages wait out their delay: which sends wait, how
//! long the longest has left, which one a click calls back, and whether
//! the sidebar's pill or a toast shows it. The window keeps one
//! [`Waiting`] and draws what it answers.

use mailrs_domain::EpochMillis;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Send {
    id: u64,
    ends_at: EpochMillis,
}

/// The sends waiting out their delay, oldest first.
#[derive(Debug, Default)]
pub struct Waiting {
    next: u64,
    sends: Vec<Send>,
}

impl Waiting {
    /// Starts a send's delay of `seconds` at `now`, and names the send.
    pub fn add(&mut self, now: EpochMillis, seconds: u32) -> u64 {
        self.next += 1;
        self.sends.push(Send {
            id: self.next,
            ends_at: now + i64::from(seconds) * 1_000,
        });
        self.next
    }

    /// Forgets a send that was called back. False when it had gone.
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.sends.len();
        self.sends.retain(|s| s.id != id);
        self.sends.len() != before
    }

    /// Forgets the sends whose delay is over at `now`, and names them.
    pub fn tick(&mut self, now: EpochMillis) -> Vec<u64> {
        let (over, left): (Vec<Send>, Vec<Send>) =
            self.sends.iter().copied().partition(|s| s.ends_at <= now);
        self.sends = left;
        over.into_iter().map(|s| s.id).collect()
    }

    /// The time the longest wait has left, or `None` with nothing waiting.
    pub fn left(&self, now: EpochMillis) -> Option<EpochMillis> {
        self.sends.iter().map(|s| (s.ends_at - now).max(0)).max()
    }

    /// The send a click calls back: the one sent last, which was the
    /// toast on top when each send had its own.
    pub fn newest(&self) -> Option<u64> {
        self.sends.last().map(|s| s.id)
    }

    pub fn is_empty(&self) -> bool {
        self.sends.is_empty()
    }
}

/// "0:07": the time left, rounded up to the second, so the pill never
/// reads 0:00 while the send can still be called back.
pub fn countdown(left: EpochMillis) -> String {
    let seconds = (left.max(0) + 999) / 1_000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Where Undo Send shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Pill,
    Toast,
}

/// The pill needs the sidebar on screen beside the list. A collapsed
/// sidebar (a narrow window), shown over the list or not, and a hidden
/// one leave the toast.
pub fn surface(collapsed: bool, shown: bool) -> Surface {
    if shown && !collapsed {
        Surface::Pill
    } else {
        Surface::Toast
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_countdown_rounds_up_to_the_second() {
        assert_eq!(countdown(7_000), "0:07");
        assert_eq!(countdown(6_001), "0:07");
        assert_eq!(countdown(65_000), "1:05");
        assert_eq!(countdown(0), "0:00");
    }

    #[test]
    fn several_sends_show_the_longest_wait_and_call_back_the_newest() {
        let mut waiting = Waiting::default();
        let first = waiting.add(0, 30);
        let second = waiting.add(20_000, 5);
        assert_eq!(waiting.left(21_000), Some(9_000), "the first has 9 s left, the second 4 s");
        assert_eq!(waiting.newest(), Some(second));
        assert!(waiting.remove(second));
        assert_eq!(waiting.newest(), Some(first));
    }

    #[test]
    fn a_send_whose_delay_ended_cannot_be_called_back() {
        let mut waiting = Waiting::default();
        let id = waiting.add(0, 10);
        assert!(waiting.tick(9_999).is_empty());
        assert_eq!(waiting.tick(10_000), vec![id]);
        assert_eq!(waiting.newest(), None);
        assert!(waiting.is_empty());
        assert_eq!(waiting.left(10_000), None);
        assert!(!waiting.remove(id), "gone already");
    }

    #[test]
    fn the_pill_needs_the_sidebar_beside_the_list() {
        assert_eq!(surface(false, true), Surface::Pill);
        assert_eq!(surface(true, true), Surface::Toast, "an overlay that closes on the next click");
        assert_eq!(surface(true, false), Surface::Toast);
        assert_eq!(surface(false, false), Surface::Toast);
    }
}
