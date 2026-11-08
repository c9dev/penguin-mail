//! Safe popup helpers.
//!
//! GTK's win32 backend can report a display with zero monitors — a
//! disconnected remote session is one way — and `gdk_popup_present`
//! dereferences the missing primary monitor while placing the popup.
//! That path serves every popup, including the internal popover of a
//! `MenuButton` or `SplitButton`, and it is unfixed upstream as of GTK
//! 4.24.1. Guard programmatic popups with `may_present`, or use
//! `safe_popup`/`safe_popup_at` for a `gtk::Popover`.

use gtk::{gdk, prelude::*};

/// Result of attempting to show a popover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopoverResult {
    /// The popover was shown.
    Shown,
    /// The display reported no monitors, so nothing was shown.
    NoMonitors,
}

/// Whether `widget`'s display can present a popup.
pub fn may_present(widget: &impl IsA<gtk::Widget>) -> bool {
    widget.display().monitors().n_items() > 0
}

/// Pops up `popover` when its display can present it.
pub fn safe_popup<P>(popover: &P) -> PopoverResult
where
    P: IsA<gtk::Popover>,
{
    let popover: &gtk::Popover = popover.upcast_ref();
    if !may_present(popover) {
        return PopoverResult::NoMonitors;
    }
    popover.popup();
    PopoverResult::Shown
}

/// Sets the pointing rectangle and pops up `popover` safely.
pub fn safe_popup_at<P>(popover: &P, rect: &gdk::Rectangle) -> PopoverResult
where
    P: IsA<gtk::Popover>,
{
    popover.set_pointing_to(Some(rect));
    safe_popup(popover)
}
