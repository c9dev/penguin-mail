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

