//! What a category chip says: its unread badge, the name the tooltip and
//! a screen reader give it, and which chips show their names in the room
//! the row has. A narrow row folds the names away, so every chip's spoken
//! name carries it, with the whole unread count even when the badge stops
//! at 99.

use mailrs_domain::Category;
use mailrs_domain::translate::fill_plural;

/// The badge on a chip's corner: nothing with nothing unread, the count
/// up to 99, and "99+" past that, since a longer number would spill off
/// the chip.
pub fn badge(unread: i64) -> String {
    match unread {
        ..=0 => String::new(),
        1..=99 => unread.to_string(),
        _ => "99+".to_string(),
    }
}

/// "Primary, 6 unread", or the name alone with nothing unread.
pub fn spoken(category: Category, unread: i64) -> String {
    if unread <= 0 {
        return category.name();
    }
    fill_plural(
        "{name}, {count} unread",
        "{name}, {count} unread",
        unread as usize,
        &[("name", &category.name()), ("count", &unread.to_string())],
    )
}

/// Which chips show their names beside their icons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Names {
    /// Every chip: "Primary 2", "Updates", "Social 1".
    Every,
    /// Only the chosen chip; the others show an icon and a corner badge.
    Chosen,
    /// No chip; each shows an icon, and its name lives in the tooltip and
    /// the spoken name.
    Icons,
}

/// The width the row of chips needs in each of the three ways to draw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Needs {
    pub icons: i32,
    pub chosen: i32,
    pub every: i32,
}

/// The most names a row `width` pixels wide holds. Focused and Other are
/// worded tabs with no icon to fall back to, so they keep their names and
/// leave the row to clip them.
pub fn names_for(width: i32, needs: Needs, worded: bool) -> Names {
    if worded || width >= needs.every {
        Names::Every
    } else if width >= needs.chosen {
        Names::Chosen
    } else {
        Names::Icons
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEEDS: Needs = Needs {
        icons: 240,
        chosen: 290,
        every: 520,
    };

    #[test]
    fn every_chip_shows_its_name_when_the_row_holds_them_all() {
        assert_eq!(names_for(520, NEEDS, false), Names::Every);
        assert_eq!(names_for(900, NEEDS, false), Names::Every);
    }

    #[test]
    fn only_the_chosen_chip_keeps_its_name_when_the_rest_do_not_fit() {
        assert_eq!(names_for(519, NEEDS, false), Names::Chosen);
        assert_eq!(names_for(290, NEEDS, false), Names::Chosen);
    }

    #[test]
    fn the_chips_fall_back_to_icons_when_no_name_fits() {
        assert_eq!(names_for(289, NEEDS, false), Names::Icons);
        assert_eq!(names_for(100, NEEDS, false), Names::Icons);
    }

    #[test]
    fn worded_tabs_keep_their_names_since_they_have_no_icon() {
        assert_eq!(names_for(100, NEEDS, true), Names::Every);
    }

    #[test]
    fn a_focused_inbox_slice_reads_with_its_unread_count() {
        assert_eq!(spoken(Category::Focused, 6), "Focused, 6 unread");
        assert_eq!(spoken(Category::Other, 0), "Other");
    }

    #[test]
    fn the_badge_stops_at_ninety_nine() {
        assert_eq!(badge(0), "");
        assert_eq!(badge(-3), "");
        assert_eq!(badge(6), "6");
        assert_eq!(badge(99), "99");
        assert_eq!(badge(150), "99+");
    }

    #[test]
    fn a_chip_says_its_name_and_its_unread_mail() {
        assert_eq!(spoken(Category::Primary, 6), "Primary, 6 unread");
        assert_eq!(spoken(Category::Social, 0), "Social");
    }

    #[test]
    fn a_chip_past_the_badge_still_says_the_whole_count() {
        assert_eq!(spoken(Category::Promotions, 150), "Promotions, 150 unread");
    }
}
