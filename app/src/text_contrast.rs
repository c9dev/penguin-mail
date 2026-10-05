//! WCAG contrast checks over secondary text: the stylesheet's dimmed
//! labels in the window and the dim colour of the conversation page.
//!
//! Light mode draws libadwaita's text at `rgb(0 0 6 / 80%)`, so a label at
//! opacity `x` reaches 0.8 × x of the ink; dark mode draws white. The two
//! modes need different opacities for the same strength on screen, which
//! is why most rules here carry an `.app-dark` twin.

use crate::accent_contrast::{AA, channels, contrast, custom_property, dimmed_text_contrast, rules};

/// The opacity the last rule naming `selector` in its selector list gives.
fn opacity(selector: &str) -> f64 {
    rules()
        .into_iter()
        .filter(|(head, _)| head.split(',').any(|s| s.trim() == selector))
        .filter_map(|(_, body)| {
            let at = body.find("opacity:")?;
            let rest = &body[at + "opacity:".len()..];
            rest[..rest.find(';')?].trim().parse().ok()
        })
        .last()
        .unwrap_or_else(|| panic!("style.css sets an opacity for {selector}"))
}

/// A surface colour of the window in light or dark.
fn surface(dark: bool, name: &str) -> String {
    let selector = if dark { "window.app-surfaces.app-dark" } else { "window.app-surfaces" };
    custom_property(selector, name).unwrap_or_else(|| panic!("{selector} sets {name}"))
}

/// Every accent the desktop may hand the app: libadwaita's own and the
/// Yaru shades Ubuntu's libadwaita swaps in.
const ACCENTS: &[&str] = &[
    "#3584e4", "#2190a4", "#3a944a", "#c88800", "#ed5b00", "#e62d42", "#d56199", "#9141ac",
    "#6f8396", "#b39169", "#0073e5", "#308280", "#4b8501", "#e95420", "#da3450", "#b34cb3",
    "#7764d8", "#657b69",
];

/// `share` of `colour` over `under`, as a hex colour.
fn tint(colour: &str, share: f64, under: &str) -> String {
    let (c, s) = (channels(colour), channels(under));
    let mixed: Vec<String> = (0..3)
        .map(|i| format!("{:02x}", (c[i] * share + s[i] * (1.0 - share)).round() as u8))
        .collect();
    format!("#{}", mixed.concat())
}

/// Asserts that text at `selector`'s opacity reads 4.5:1 on each of
/// `backgrounds`.
fn assert_passes(dark: bool, selector: &str, backgrounds: &[String]) {
    let opacity = opacity(selector);
    for background in backgrounds {
        let ratio = dimmed_text_contrast(dark, opacity, background);
        assert!(ratio >= AA, "{selector} at {opacity} on {background} is {ratio:.2}:1");
    }
}

#[test]
fn a_threads_preview_and_date_pass_aa_on_the_list_and_on_the_selected_card() {
    // The list sits on the window colour. A selected card tints it with
    // the accent, 16 % and 21 % under the pointer in light, 26 % and 31 %
    // in dark; the darkest accent tint is the hardest case.
    for (dark, prefix, shares) in [(false, "", [0.16, 0.21]), (true, ".app-dark ", [0.26, 0.31])] {
        let window = surface(dark, "--window-bg-color");
        let mut backgrounds = vec![window.clone()];
        for share in shares {
            backgrounds.extend(ACCENTS.iter().map(|accent| tint(accent, share, &window)));
        }
        for part in [".thread-row .snippet", ".thread-row .date"] {
            assert_passes(dark, &format!("{prefix}{part}"), &backgrounds);
        }
    }
}

#[test]
fn the_dim_labels_of_the_list_header_sidebar_and_next_event_pass_aa() {
    for (dark, prefix) in [(false, ""), (true, ".app-dark ")] {
        for (part, under) in [
            (".mailbox-row .count", "--sidebar-bg-color"),
            (".hidden-calendars .calendar-name", "--sidebar-bg-color"),
            (".list-header-title .subtitle", "--window-bg-color"),
            // The next event's card is the view colour on the sidebar.
            (".next-event .when", "--view-bg-color"),
            (".popover-when", "--view-bg-color"),
            (".quick-account", "--view-bg-color"),
            ("popover.event-popover .popover-kind", "--view-bg-color"),
        ] {
            assert_passes(dark, &format!("{prefix}{part}"), &[surface(dark, under)]);
        }
    }
}

/// `rgba(r,g,b,a)` drawn over the hex colour `under`.
fn over(rgba: &str, under: &str) -> String {
    let inner = rgba.trim_start_matches("rgba(").trim_end_matches(')');
    let parts: Vec<f64> = inner.split(',').map(|p| p.trim().parse().expect("a number")).collect();
    let back = channels(under);
    let mixed: Vec<String> = (0..3)
        .map(|i| format!("{:02x}", (parts[i] * parts[3] + back[i] * (1.0 - parts[3])).round() as u8))
        .collect();
    format!("#{}", mixed.concat())
}

#[test]
fn the_conversation_pages_dim_text_passes_aa_on_the_page_and_the_message() {
    // The thread's count, each sender's address, date and recipients and
    // the Details labels sit on the page; a quote or a signature sits on
    // the message's surface.
    for dark in [false, true] {
        let palette = crate::render::page_palette(dark);
        for under in [palette.bg, palette.surface] {
            let ratio = contrast(&over(palette.dim, under), under);
            assert!(ratio >= AA, "dim {} on {under} is {ratio:.2}:1", palette.dim);
        }
    }
}
#[test]
fn the_invitation_cards_secondary_lines_pass_aa() {
    // The card is the window colour; its day strip is the view colour.
    for (dark, prefix) in [(false, ""), (true, ".app-dark ")] {
        let card = surface(dark, "--window-bg-color");
        let strip = surface(dark, "--view-bg-color");
        for part in [".invitation-when", ".invitation-meta", "menubutton.invitation-guests > button", ".strip-heading"] {
            assert_passes(dark, &format!("{prefix}{part}"), std::slice::from_ref(&card));
        }
        assert_passes(dark, &format!("{prefix}.strip-hour"), &[strip]);
    }
}

#[test]
fn the_free_hour_verdict_passes_aa_on_the_card() {
    for (dark, selector) in [(false, ".strip-verdict.free"), (true, ".app-dark .strip-verdict.free")] {
        let colour = custom_property(selector, "color").unwrap_or_else(|| panic!("style.css colours {selector}"));
        let card = surface(dark, "--window-bg-color");
        let ratio = contrast(&colour, &card);
        assert!(ratio >= AA, "{selector} in {colour} on {card} is {ratio:.2}:1");
    }
}


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

/// An OKLab colour back in sRGB hex, clipped to the gamut as GTK does.
fn from_oklab([lightness, a, b]: [f64; 3]) -> String {
    let l = (lightness + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let m = (lightness - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let s = (lightness - 0.089_484_177_5 * a - 1.291_485_548 * b).powi(3);
    let rgb = [
        4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
        -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
        -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701 * s,
    ];
    let hex: Vec<String> = rgb.iter().map(|c| format!("{:02x}", (from_linear(*c) * 255.0).round() as u8)).collect();
    format!("#{}", hex.concat())
}

/// The number after `before` in `text`, up to `until`.
fn number_in(text: &str, before: &str, until: char) -> f64 {
    let at = text.find(before).unwrap_or_else(|| panic!("{text} holds {before}")) + before.len();
    let after = &text[at..];
    after[..after.find(until).expect("an end")].trim().parse().expect("a number")
}

/// The value of the property `name` in the rule for `selector`, matched
/// as a whole name, so `color` does not find `background-color`.
fn property(selector: &str, name: &str) -> Option<String> {
    let (_, body) = rules().into_iter().find(|(head, _)| head.trim() == selector)?;
    body.split(';').find_map(|declaration| {
        let (key, value) = declaration.split_once(':')?;
        (key.trim() == name).then(|| value.trim().to_string())
    })
}

/// The share of the accent a rule's `alpha(var(--accent-bg-color), x)`
/// background gives.
fn accent_share(selector: &str) -> f64 {
    let background = custom_property(selector, "background-color").unwrap_or_else(|| panic!("{selector} tints"));
    number_in(&background, "var(--accent-bg-color),", ')')
}

/// How a rule's `color` draws `accent` for text. Light darkens it: L, a
/// and b in OKLab scaled down together until L is under a limit, which
/// keeps the hue and stays in gamut. Dark lifts L to a floor and keeps a
/// and b; GTK clips what falls out of gamut. The rule must be written in
/// exactly one of those two shapes.
fn accent_text(selector: &str, dark: bool, accent: &str) -> String {
    let rule = property(selector, "color").unwrap_or_else(|| panic!("{selector} sets a colour"));
    let [l, a, b] = oklab(accent);
    if dark {
        let floor = number_in(&rule, "max(l,", ')');
        assert_eq!(rule, format!("oklab(from var(--accent-bg-color) max(l, {floor}) a b)"));
        from_oklab([l.max(floor), a, b])
    } else {
        let limit = number_in(&rule, "min(l,", ')');
        let scale = format!("min(1, {limit} / l)");
        assert_eq!(rule, format!("oklab(from var(--accent-bg-color) min(l, {limit}) calc(a * {scale}) calc(b * {scale}))"));
        let f = (limit / l).min(1.0);
        from_oklab([l * f, a * f, b * f])
    }
}

#[test]
fn the_selected_mailboxs_name_and_count_pass_aa_for_every_accent() {
    for (dark, selector) in [(false, ".mailboxes > row:selected"), (true, ".app-dark .mailboxes > row:selected")] {
        let sidebar = surface(dark, "--sidebar-bg-color");
        let share = accent_share(selector);
        for accent in ACCENTS {
            let drawn = accent_text(selector, dark, accent);
            let under = tint(accent, share, &sidebar);
            let ratio = contrast(&drawn, &under);
            assert!(ratio >= AA, "{selector} in {accent}: {drawn} on {under} is {ratio:.2}:1");
        }
    }
}

#[test]
fn todays_heading_in_the_agenda_passes_aa_for_every_accent() {
    for (dark, selector) in [(false, ".agenda-heading.today"), (true, ".app-dark .agenda-heading.today")] {
        let view = surface(dark, "--view-bg-color");
        for accent in ACCENTS {
            let drawn = accent_text(selector, dark, accent);
            let ratio = contrast(&drawn, &view);
            assert!(ratio >= AA, "{selector} in {accent}: {drawn} on {view} is {ratio:.2}:1");
        }
    }
}

#[test]
fn the_selected_mailboxs_count_takes_the_rows_colour() {
    let count = property(".mailboxes > row:selected .mailbox-row .count", "color");
    assert_eq!(count.as_deref(), Some("inherit"));
}
