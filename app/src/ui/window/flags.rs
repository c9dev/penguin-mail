//! Flags: Gmail's star, with Apple Mail's colours kept on this computer.

use std::rc::Rc;

use mailrs_domain::FlagColor;

use super::MainWindow;
use super::press::Press;

impl MainWindow {
    /// Flags what the conversation reaches with `color`, or takes the flag
    /// off with `None`. A colour picked here becomes the one the flag
    /// button uses next.
    pub(super) fn flag(self: &Rc<Self>, color: Option<FlagColor>) {
        let view = Rc::clone(&self.conversation);
        self.press(&view, Press::Flag(color));
    }
}
