//! WCAG contrast checks over the accent colours in `data/style.css`.
//!
//! The app follows the desktop accent through libadwaita, whose
//! `--accent-bg-color` is a fill: white text on it falls below AA for
//! orange (3.4:1). libadwaita already derives `--accent-color` for text
//! with a lightness limit, so only fills need a darker shade, and only
//! for orange. The stylesheet carries that shade as `--accent-fill`.

const STYLE: &str = include_str!("../data/style.css");

/// The system orange in libadwaita 1.9 (`--accent-orange`).
const DESKTOP_ORANGE: &str = "#ed5b00";
/// The mockups' orange, kept for large shapes, icons, bars and tints.
const MOCKUP_ORANGE: &str = "#e8660c";
const WHITE: &str = "#ffffff";
const AA: f64 = 4.5;

fn channels(hex: &str) -> [f64; 3] {
    let hex = hex.trim_start_matches('#');
    let byte = |at: usize| f64::from(u8::from_str_radix(&hex[at..at + 2], 16).expect("hex digits"));
    [byte(0), byte(2), byte(4)]
}

fn luminance(hex: &str) -> f64 {
    let [r, g, b] = channels(hex).map(|c| {
        let c = c / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    });
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn contrast(a: &str, b: &str) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The value a custom property takes inside the rule for `selector`.
fn custom_property(selector: &str, name: &str) -> Option<String> {
    let start = STYLE.find(&format!("{selector} {{"))?;
    let block = &STYLE[start..start + STYLE[start..].find('}')?];
    let after = &block[block.find(&format!("{name}:"))? + name.len() + 1..];
    Some(after[..after.find(';')?].trim().to_string())
}

/// Every rule as (selector, body).
fn rules() -> Vec<(&'static str, &'static str)> {
    let mut rules = Vec::new();
    let mut rest = STYLE;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else { break };
        // A comment before a rule is part of what precedes its selector.
        let head = rest[..open].rsplit("*/").next().unwrap_or("").trim();
        rules.push((head, &rest[open + 1..open + close]));
        rest = &rest[open + close + 1..];
    }
    rules
}

#[test]
fn the_raw_orange_fails_white_text_and_the_fill_passes() {
    assert!(contrast(WHITE, MOCKUP_ORANGE) < AA);
    assert!(contrast(WHITE, DESKTOP_ORANGE) < AA);
    let fill = custom_property("window.accent-orange", "--accent-fill")
        .expect("style.css sets --accent-fill for an orange accent");
    let ratio = contrast(WHITE, &fill);
    assert!(ratio >= AA, "white on {fill} is {ratio:.2}:1");
}

#[test]
fn the_fill_is_the_desktop_orange_darkened_by_the_mockup_step() {
    // #e8660c to #c1550a is a scale of 0.874 in OKLab; the same scale on
    // the desktop orange keeps its hue.
    let fill = custom_property("window.accent-orange", "--accent-fill").expect("fill");
    assert_eq!(fill, "#c64b00");
}

/// Filled controls that still draw white on the raw accent. Each entry
/// names why it is left alone.
const RAW_FILL_EXCEPTIONS: &[(&str, &str)] = &[(
    ".mini-month button.today",
    "the calendar sidebar, owned by the calendar-management work",
)];

#[test]
fn no_filled_control_carries_white_text_on_the_raw_accent() {
    let filled: Vec<&str> = rules()
        .into_iter()
        .filter(|(_, body)| {
            body.contains("background: var(--accent-bg-color)")
                && body.contains("color: var(--accent-fg-color)")
        })
        .map(|(selector, _)| selector)
        .filter(|selector| !RAW_FILL_EXCEPTIONS.iter().any(|(name, _)| selector.ends_with(name)))
        .collect();
    assert!(filled.is_empty(), "fill these with --accent-fill: {filled:?}");
}

#[test]
fn no_text_is_hard_coded_in_an_orange() {
    for (selector, body) in rules() {
        // A flag is an icon, and an icon is a shape.
        if selector.ends_with(".flag-orange") {
            continue;
        }
        for orange in [MOCKUP_ORANGE, DESKTOP_ORANGE] {
            let text = body.lines().any(|l| l.trim_start().starts_with("color:") && l.contains(orange));
            assert!(!text, "{selector} draws text in {orange}");
        }
    }
}
