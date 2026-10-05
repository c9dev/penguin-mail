//! What the mail list's and the calendar's headers keep on a phone.
//!
//! At 360 px the title shares the bar with Search, New and the window's
//! three buttons, and "All Inboxes" and "October" ellipsized after five
//! letters. On a narrow window both headers drop the line under the title,
//! set the title a step smaller, and move New to a bar at the foot of the
//! page, where the calendar already keeps Today and the view switch.

/// The header's parts for one width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderLayout {
    /// Whether the line under the title shows: the unread count under
    /// "All Inboxes", or the year and week beside the month.
    pub subtitle: bool,
    /// Whether the title takes its smaller size.
    pub small_title: bool,
    /// Whether New sits in the bottom bar rather than the header.
    pub new_below: bool,
}

/// The header for a window that is `narrow` (the window's phone
/// breakpoint) or not.
pub fn header_layout(narrow: bool) -> HeaderLayout {
    HeaderLayout {
        subtitle: !narrow,
        small_title: narrow,
        new_below: narrow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_header_keeps_its_subtitle_and_new() {
        assert_eq!(
            header_layout(false),
            HeaderLayout { subtitle: true, small_title: false, new_below: false }
        );
    }

    #[test]
    fn a_narrow_header_drops_the_subtitle_shrinks_the_title_and_moves_new_down() {
        assert_eq!(
            header_layout(true),
            HeaderLayout { subtitle: false, small_title: true, new_below: true }
        );
    }
}
