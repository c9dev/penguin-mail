//! WCAG contrast checks over the accent colours in `data/style.css`, and
//! over small text it draws at less than full strength.
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
pub(crate) const AA: f64 = 4.5;

pub(crate) fn channels(hex: &str) -> [f64; 3] {
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

pub(crate) fn contrast(a: &str, b: &str) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The value a custom property takes inside the rule for `selector`.
pub(crate) fn custom_property(selector: &str, name: &str) -> Option<String> {
    let start = STYLE.find(&format!("{selector} {{"))?;
    let block = &STYLE[start..start + STYLE[start..].find('}')?];
    let after = &block[block.find(&format!("{name}:"))? + name.len() + 1..];
    Some(after[..after.find(';')?].trim().to_string())
}

/// Every rule as (selector, body).
pub(crate) fn rules() -> Vec<(&'static str, &'static str)> {
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

/// The contrast of text drawn at `opacity` over `background`: in light
/// libadwaita's text is `rgb(0 0 6 / 80%)`, in dark it is white.
pub(crate) fn dimmed_text_contrast(dark: bool, opacity: f64, background: &str) -> f64 {
    let (text, alpha) = if dark { ([255.0; 3], opacity) } else { ([0.0, 0.0, 6.0], 0.8 * opacity) };
    let back = channels(background);
    let mixed: Vec<String> = (0..3)
        .map(|i| format!("{:02x}", (text[i] * alpha + back[i] * (1.0 - alpha)).round() as u8))
        .collect();
    contrast(&format!("#{}", mixed.concat()), background)
}

#[test]
fn a_day_headings_weekday_and_place_pass_aa_in_light_and_dark() {
    // The week view's heading row sits on the view, the month's on the
    // window, so both surfaces count.
    let surfaces = |dark: bool| {
        let selector = if dark { "window.app-surfaces.app-dark" } else { "window.app-surfaces" };
        ["--view-bg-color", "--window-bg-color"]
            .map(|name| custom_property(selector, name).expect("a surface colour"))
    };
    for (dark, selectors) in [
        (false, [".day-heading .weekday", ".day-place"]),
        (true, [".app-dark .day-heading:not(.today) .weekday", ".app-dark .day-heading:not(.today) .day-place"]),
    ] {
        for selector in selectors {
            let opacity: f64 = custom_property(selector, "opacity")
                .unwrap_or_else(|| panic!("style.css sets an opacity for {selector}"))
                .parse()
                .expect("a number");
            for surface in surfaces(dark) {
                let ratio = dimmed_text_contrast(dark, opacity, &surface);
                assert!(ratio >= AA, "{selector} at {opacity} on {surface} is {ratio:.2}:1");
            }
        }
    }
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

/// Every accent the desktop may hand the app in light: libadwaita's own
/// and the Yaru shades Ubuntu's libadwaita swaps in.
const ACCENTS: &[&str] = &[
    "#3584e4", "#2190a4", "#3a944a", "#c88800", "#ed5b00", "#e62d42", "#d56199", "#9141ac",
    "#6f8396", "#b39169", "#0073e5", "#308280", "#4b8501", "#e95420", "#da3450", "#b34cb3",
    "#7764d8", "#657b69",
];

fn to_linear(c: f64) -> f64 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn from_linear(c: f64) -> f64 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// `hex` in OKLab.
fn oklab(hex: &str) -> [f64; 3] {
    let [r, g, b] = channels(hex).map(|c| to_linear(c / 255.0));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    [
        0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s,
    ]
}

/// An OKLab colour back in sRGB hex, clipped to the gamut.
fn from_oklab([lightness, a, b]: [f64; 3]) -> String {
    let l = (lightness + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let m = (lightness - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let s = (lightness - 0.089_484_177_5 * a - 1.291_485_548 * b).powi(3);
    let rgb = [
        4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
        -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
        -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701 * s,
    ];
    let hex: Vec<String> = rgb
        .iter()
        .map(|c| format!("{:02x}", (from_linear(*c) * 255.0).round() as u8))
        .collect();
    format!("#{}", hex.concat())
}

/// `share` of `colour` over `surface`.
fn tint(colour: &str, share: f64, surface: &str) -> String {
    let (c, s) = (channels(colour), channels(surface));
    let mixed: Vec<String> = (0..3)
        .map(|i| format!("{:02x}", (c[i] * share + s[i] * (1.0 - share)).round() as u8))
        .collect();
    format!("#{}", mixed.concat())
}

/// The number after `before` in `text`, up to `until`.
fn number_in(text: &str, before: &str, until: char) -> f64 {
    let after = &text[text.find(before).unwrap_or_else(|| panic!("{text} holds {before}")) + before.len()..];
    after[..after.find(until).expect("an end")].trim().parse().expect("a number")
}

#[test]
fn todays_heading_passes_aa_on_its_tint_for_every_accent_in_light() {
    // The pill is the accent at a share over the surface, and its text
    // the accent with L, a and b scaled down together until L is under a
    // limit, which keeps the hue. The week's heading row sits on the
    // view and the month's on the window.
    let pill = custom_property(".day-heading.today", "background").expect("a tint");
    let share = number_in(&pill, "var(--accent-bg-color)", '%') / 100.0;
    let text = custom_property(".day-heading.today", "color").expect("a text colour");
    let limit = number_in(&text, "min(l,", ')');
    let scale = format!("min(1, {limit} / l)");
    assert_eq!(
        text,
        format!("oklab(from var(--accent-bg-color) min(l, {limit}) calc(a * {scale}) calc(b * {scale}))")
    );
    for surface in ["--view-bg-color", "--window-bg-color"] {
        let surface = custom_property("window.app-surfaces", surface).expect("a surface");
        for accent in ACCENTS {
            let [l, a, b] = oklab(accent);
            let f = (limit / l).min(1.0);
            let drawn = from_oklab([l * f, a * f, b * f]);
            let ratio = contrast(&drawn, &tint(accent, share, &surface));
            assert!(ratio >= AA, "today in {accent} on {surface} is {ratio:.2}:1");
        }
    }
}

#[test]
fn todays_heading_keeps_the_accent_text_in_dark() {
    // libadwaita lifts the accent text to an OKLab lightness of 0.85 in
    // dark, which reads 5.8:1 or better on the pill there.
    let text = custom_property(".app-dark .day-heading.today", "color").expect("a dark rule");
    assert_eq!(text, "var(--accent-color)");
}
