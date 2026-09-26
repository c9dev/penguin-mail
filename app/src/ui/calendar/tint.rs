//! An event's colour: parsed from Google's `#rrggbb`, or the accent when
//! it sends something else, and the CSS that tints an event block's
//! background with it.

use std::collections::HashSet;
use std::fmt::Write as _;

/// `colour` as red, green and blue, with or without a leading `#`.
/// `None` for anything else, such as an empty string or a named colour
/// like `tomato`, which Google's calendars never send but a hand-edited
/// one could.
pub fn parse_hex(colour: &str) -> Option<(u8, u8, u8)> {
    let hex = colour.strip_prefix('#').unwrap_or(colour);
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
}

/// The CSS class an event of this colour carries: `cal-` and its six hex
/// digits, lower case; `cal-accent`, styled from the accent colour in
/// `app/data/style.css`, for anything [`parse_hex`] cannot read.
pub fn css_class(colour: &str) -> String {
    match parse_hex(colour) {
        Some((r, g, b)) => format!("cal-{r:02x}{g:02x}{b:02x}"),
        None => "cal-accent".into(),
    }
}

/// One rule set per distinct colour in `colours`, for a `gtk::CssProvider`
/// the calendar view owns: a custom property carrying the colour whole,
/// for the bar and the unanswered border, and the event block's tinted
/// background at 14 % in light mode and 24 % in dark, through CSS
/// `color-mix` so libadwaita's own background variable does the mixing.
/// Dark is the `calendar-dark` class the view puts on its page while
/// libadwaita is dark: `prefers-color-scheme` never matched in an app
/// stylesheet on the GTK this was checked on (4.22).
/// A colour [`css_class`] cannot read writes no rule; the fixed
/// `.cal-accent` rule in `app/data/style.css` already covers it.
///
/// GTK looks this provider up before the app's stylesheet, at the same
/// priority, and the first provider that sets a property wins whatever
/// its selector's weight. So an invitation not answered yet is left out
/// here, and keeps the view colour the stylesheet gives it, and the
/// hover tints live here too.
pub fn stylesheet(colours: &[String]) -> String {
    let mut css = String::new();
    let mut written = HashSet::new();
    for colour in colours {
        let class = css_class(colour);
        if class == "cal-accent" || !written.insert(class.clone()) {
            continue;
        }
        let hex = &class[4..];
        let _ = write!(
            css,
            ".{class} {{ --cal-colour: #{hex}; }}\n\
             .event-block.{class}:not(.unanswered) {{ background-color: color-mix(in srgb, #{hex} 14%, var(--view-bg-color)); }}\n\
             .event-block.{class}:not(.unanswered):hover {{ background-color: color-mix(in srgb, #{hex} 22%, var(--view-bg-color)); }}\n\
             .calendar-dark .event-block.{class}:not(.unanswered) {{ background-color: color-mix(in srgb, #{hex} 24%, var(--view-bg-color)); }}\n\
             .calendar-dark .event-block.{class}:not(.unanswered):hover {{ background-color: color-mix(in srgb, #{hex} 32%, var(--view-bg-color)); }}\n"
        );
    }
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hex_colour_parses_with_or_without_the_hash() {
        assert_eq!(parse_hex("#3584e4"), Some((0x35, 0x84, 0xe4)));
        assert_eq!(parse_hex("3584E4"), Some((0x35, 0x84, 0xe4)));
    }

    #[test]
    fn a_colour_that_is_not_hex_takes_the_accent() {
        assert_eq!(css_class(""), "cal-accent");
        assert_eq!(css_class("tomato"), "cal-accent");
        assert!(!stylesheet(&["tomato".into()]).contains("tomato"));
    }

    #[test]
    fn each_colour_gets_a_light_and_a_dark_rule() {
        let css = stylesheet(&["#3584e4".into()]);
        assert!(css.contains(".cal-3584e4"));
        // The page carries `calendar-dark` while libadwaita is dark; GTK's
        // own dark media query does not reach an app stylesheet here.
        assert!(css.contains(".calendar-dark .event-block.cal-3584e4"));
        assert!(css.contains("24%"));
    }

    #[test]
    fn an_unanswered_block_keeps_the_view_colour() {
        // This provider is looked up before app/data/style.css, so its
        // fill would win over the unanswered rule there whatever the
        // selectors' weight.
        let css = stylesheet(&["#9141ac".into()]);
        for rule in css.lines().filter(|line| line.contains("background-color")) {
            assert!(rule.contains(":not(.unanswered)"), "{rule}");
        }
    }

    #[test]
    fn the_same_colour_twice_writes_one_rule_set() {
        let css = stylesheet(&["#3584e4".into(), "#3584E4".into()]);
        assert_eq!(
            css.matches("--cal-colour").count(),
            1,
            "one colour is one rule set, however many events share it"
        );
    }
}
