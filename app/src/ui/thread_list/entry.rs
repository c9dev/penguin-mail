//! Where the focus lands when Tab brings it into the thread list.
//!
//! The list is one Tab stop: GTK's `ListTabBehavior::Item` makes Tab and
//! Shift+Tab leave it, and the arrows, Home, End, Page Up and Page Down
//! move between rows. On the way in, GTK puts the focus back on the row it
//! last had, which a reload or a selection made elsewhere can leave away
//! from the open conversation. This works out the row Tab should land on
//! instead, without a widget.

/// The row Tab should move the focus to as it enters a list of `len`
/// rows, or `None` when the row GTK chose (`focused`) is right. A selected
/// row keeps the focus; otherwise it goes to the first selected row, or
/// to the first row when nothing is selected.
pub fn landing(
    len: u32,
    focused: Option<u32>,
    focused_selected: bool,
    first_selected: Option<u32>,
) -> Option<u32> {
    if len == 0 || focused_selected {
        return None;
    }
    let target = first_selected.unwrap_or(0).min(len - 1);
    (focused != Some(target)).then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_lands_on_the_selected_row() {
        // GTK kept the focus on row 0 through a reload; row 4 is open.
        assert_eq!(landing(10, Some(0), false, Some(4)), Some(4));
    }

    #[test]
    fn a_focused_row_inside_the_selection_keeps_the_focus() {
        assert_eq!(landing(10, Some(6), true, Some(4)), None);
    }

    #[test]
    fn with_nothing_selected_tab_lands_on_the_first_row() {
        assert_eq!(landing(10, Some(7), false, None), Some(0));
        assert_eq!(landing(10, Some(0), false, None), None);
    }

    #[test]
    fn an_empty_list_moves_nothing() {
        assert_eq!(landing(0, None, false, None), None);
    }
}
