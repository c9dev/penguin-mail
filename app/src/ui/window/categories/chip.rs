//! What a category chip says: the badge on its corner, and the name the
//! tooltip and a screen reader give it. Only the chosen chip shows its
//! name on screen, so every chip's spoken name carries it, with the
//! whole unread count even when the badge stops at 99.

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

#[cfg(test)]
mod tests {
    use super::*;

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
