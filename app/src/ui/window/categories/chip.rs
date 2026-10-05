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

/// The space between two chips on a line, and between two lines.
pub const GAP: i32 = 6;

/// The most lines the chips may take with every name showing.
pub const MOST_LINES: usize = 3;

/// Which chips show their names beside their icons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Names {
    /// Every chip: "Primary 2", "Updates", "Social 1".
    Every,
    /// Only the chosen chip; the others show an icon and a corner badge,
    /// and their names live in the tooltip and the spoken name.
    Chosen,
}

/// How many chips of `widths` each line holds when the lines are `width`
/// pixels wide, with [`GAP`] between chips. A chip wider than a line takes
/// a line of its own.
pub fn lines(widths: &[i32], width: i32) -> Vec<usize> {
    let mut lines = Vec::new();
    let (mut count, mut used) = (0, 0);
    for &chip in widths {
        if count > 0 && used + GAP + chip > width {
            lines.push(count);
            (count, used) = (0, 0);
        }
        used += if count > 0 { GAP + chip } else { chip };
        count += 1;
    }
    if count > 0 {
        lines.push(count);
    }
    lines
}

/// The widths `widths` take on a line `width` pixels wide once the room
/// left after them and their gaps is shared out, the first chips taking
/// the odd pixels.
pub fn spread(widths: &[i32], width: i32) -> Vec<i32> {
    let count = widths.len() as i32;
    let used = widths.iter().sum::<i32>() + GAP * (count - 1).max(0);
    let spare = (width - used).max(0);
    widths
        .iter()
        .enumerate()
        .map(|(index, chip)| chip + spare / count + i32::from((index as i32) < spare % count))
        .collect()
}

/// Whether every chip shows its name in a row `width` pixels wide, where
/// `named` are the chips' widths with their names. Every name shows while
/// the chips fit on [`MOST_LINES`] lines, except in the phone layout,
/// which keeps the list to one short line. Focused and Other are worded
/// tabs with no icon to fall back to, so they keep their names.
pub fn names_for(width: i32, named: &[i32], phone: bool, worded: bool) -> Names {
    if worded || (!phone && lines(named, width).len() <= MOST_LINES) {
        Names::Every
    } else {
        Names::Chosen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chips with their names, as the demo measures them in English.
    const NAMED: [i32; 5] = [86, 117, 100, 140, 108];

    #[test]
    fn chips_fill_a_line_and_wrap_onto_the_next() {
        // 86 + 6 + 117 + 6 + 100 = 315 holds three; the other two wrap.
        assert_eq!(lines(&NAMED, 328), [3, 2]);
        assert_eq!(lines(&NAMED, 600), [5]);
    }

    #[test]
    fn a_wrapped_line_shares_its_spare_room_so_both_edges_line_up() {
        // 86 + 117 + 100 and two gaps leave 13 pixels of a 328 line.
        assert_eq!(spread(&NAMED[..3], 328), [91, 121, 104]);
        // 140 + 108 and one gap leave 74, 37 for each.
        assert_eq!(spread(&NAMED[3..], 328), [177, 145]);
    }

    #[test]
    fn a_chip_wider_than_its_line_keeps_its_width() {
        assert_eq!(spread(&[200], 120), [200]);
    }

    #[test]
    fn a_chip_wider_than_the_line_takes_a_line_of_its_own() {
        assert_eq!(lines(&[50, 200, 50], 120), [1, 1, 1]);
    }

    #[test]
    fn every_chip_keeps_its_name_when_two_lines_hold_them() {
        // The list at the default window width, and at its widest.
        assert_eq!(names_for(328, &NAMED, false, false), Names::Every);
        assert_eq!(names_for(396, &NAMED, false, false), Names::Every);
    }

    #[test]
    fn every_chip_keeps_its_name_on_three_lines() {
        // The narrowest list beside a conversation needs three lines, as
        // the Portuguese names do at the default width.
        assert_eq!(lines(&NAMED, 276).len(), 3);
        assert_eq!(names_for(276, &NAMED, false, false), Names::Every);
    }

    #[test]
    fn only_the_chosen_chip_keeps_its_name_past_three_lines() {
        assert_eq!(lines(&NAMED, 230).len(), 4);
        assert_eq!(names_for(230, &NAMED, false, false), Names::Chosen);
    }

    #[test]
    fn a_phone_layout_shows_the_chosen_name_alone_whatever_the_room() {
        assert_eq!(names_for(600, &NAMED, true, false), Names::Chosen);
    }

    #[test]
    fn worded_tabs_keep_their_names_since_they_have_no_icon() {
        assert_eq!(names_for(100, &[90, 80], true, true), Names::Every);
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
