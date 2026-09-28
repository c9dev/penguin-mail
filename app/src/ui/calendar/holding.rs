//! Changes waiting on their Undo toast. Each one is taken exactly once:
//! by Undo, by the toast closing, or by the window closing or the app
//! quitting, whichever comes first.

pub struct Holding<T> {
    next: u64,
    items: Vec<(u64, T)>,
}

impl<T> Holding<T> {
    pub fn new() -> Self {
        Holding {
            next: 0,
            items: Vec::new(),
        }
    }

    /// Holds `item` under a new id, returned so the caller can take it
    /// back later.
    pub fn hold(&mut self, item: T) -> u64 {
        self.next += 1;
        self.items.push((self.next, item));
        self.next
    }

    /// The item held under `id`, taken out. `None` once it has already
    /// been taken, such as by an earlier call with the same id.
    pub fn take(&mut self, id: u64) -> Option<T> {
        let at = self.items.iter().position(|(i, _)| *i == id)?;
        Some(self.items.remove(at).1)
    }

    /// The item held under `id`, without taking it out.
    pub fn peek(&self, id: u64) -> Option<&T> {
        self.items
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, item)| item)
    }

    /// The id of the change held most recently, if any is still held: at
    /// most one is, since holding another dismisses the toast over the
    /// last one (`offer_undo`'s own comment). Ctrl+Z reads this the same
    /// way the toast's own Undo button reads it, by the id it was handed
    /// when it was held.
    pub fn last_id(&self) -> Option<u64> {
        self.items.last().map(|(id, _)| *id)
    }

    /// Every item still held, taken out in the order it was held. For the
    /// window closing or the app quitting, when nothing is left to offer
    /// Undo over any of them.
    pub fn drain(&mut self) -> Vec<T> {
        self.items.drain(..).map(|(_, item)| item).collect()
    }
}

impl<T> Default for Holding<T> {
    fn default() -> Self {
        Holding::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_change_goes_one_way_only() {
        let mut holding = Holding::new();
        let a = holding.hold("delete lunch");
        let b = holding.hold("move review");
        assert_ne!(a, b);
        assert_eq!(holding.take(a), Some("delete lunch"));
        assert_eq!(
            holding.take(a),
            None,
            "Undo after the toast closed does nothing"
        );
    }

    #[test]
    fn peeking_leaves_the_item_held() {
        let mut holding = Holding::new();
        let a = holding.hold("delete lunch");
        assert_eq!(holding.peek(a), Some(&"delete lunch"));
        assert_eq!(
            holding.take(a),
            Some("delete lunch"),
            "peek did not take it"
        );
    }

    #[test]
    fn last_id_names_the_most_recently_held_change() {
        let mut holding: Holding<&str> = Holding::new();
        assert_eq!(holding.last_id(), None);
        let a = holding.hold("delete lunch");
        assert_eq!(holding.last_id(), Some(a));
        let b = holding.hold("move review");
        assert_eq!(holding.last_id(), Some(b));
        holding.take(b);
        assert_eq!(holding.last_id(), Some(a), "the id before it is still held");
    }

    #[test]
    fn quitting_hands_over_every_held_change() {
        let mut holding = Holding::new();
        holding.hold(1);
        let b = holding.hold(2);
        assert_eq!(holding.drain(), vec![1, 2]);
        assert_eq!(holding.take(b), None);
    }
}
