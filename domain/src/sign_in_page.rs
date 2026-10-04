//! The page the browser shows when a sign-in hands back to the app. Google
//! and Microsoft end on the same one, so the two loopbacks share it.

use crate::translate::gettext;

/// The app's own icon, drawn at the top of the page the browser shows
/// when sign-in hands back. It is inline, so the page loads nothing.
const APP_ICON: &str =
    include_str!("../../app/data/icons/scalable/apps/io.github.c9dev.PenguinMail.svg");

/// "Penguin Mail" in Manrope Bold, as outlines, so the page shows the
/// brand's lettering without loading or bundling the font. It takes its
/// colour from the page.
const WORDMARK: &str = include_str!("wordmark.svg");

/// How a sign-in ended, as far as the page the browser shows is
/// concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finished {
    Signed,
    /// The person said no, or an administrator did.
    Denied,
    Failed,
    /// A request that is not the redirect, such as a favicon fetch.
    Nothing,
}

/// The status line and page the browser shows when a sign-in hands back
/// to the app. The words are fixed ones: nothing from the redirect's
/// address reaches the page, so a crafted address cannot write into it.
pub fn page(outcome: Finished) -> (&'static str, String) {
    let (status, mark, heading, detail) = match outcome {
        Finished::Signed => (
            "200 OK",
            "ok",
            gettext("Signed in to Penguin Mail"),
            gettext("You can close this tab and go back to Penguin Mail."),
        ),
        Finished::Denied => (
            "400 Bad Request",
            "no",
            gettext("You didn't allow access"),
            gettext(
                "Penguin Mail has not been signed in. To try again, go back to Penguin Mail and press Grant Access.",
            ),
        ),
        Finished::Failed => (
            "400 Bad Request",
            "no",
            gettext("Sign-in didn't finish"),
            gettext("Go back to Penguin Mail to see why and try again. You can close this tab."),
        ),
        Finished::Nothing => (
            "404 Not Found",
            "none",
            gettext("Nothing here"),
            String::new(),
        ),
    };
    let glyph = match mark {
        "ok" => "<path d=\"M7.5 12.4l3 3L16.5 9\"/>",
        "no" => "<path d=\"M8.5 8.5l7 7M15.5 8.5l-7 7\"/>",
        _ => "",
    };
    let badge = if glyph.is_empty() {
        String::new()
    } else {
        format!(
            "<svg class=\"badge {mark}\" viewBox=\"0 0 24 24\" aria-hidden=\"true\">\
             <circle cx=\"12\" cy=\"12\" r=\"11\"/>{glyph}</svg>"
        )
    };
    let page = format!(
        "<!doctype html><html><meta charset=utf-8>\
<meta name=viewport content=\"width=device-width,initial-scale=1\">\
<title>{title}</title><style>{css}</style>\
<main><div class=\"icon\" aria-hidden=\"true\">{icon}{badge}</div>{wordmark}\
<h1>{heading}</h1><p>{detail}</p></main></html>",
        title = html_escape(&heading),
        css = FINISHED_CSS,
        // The page sizes the icon in CSS. The file's own 128 px size would
        // take precedence in some browsers and crop it, so only its viewBox
        // stays.
        icon = APP_ICON.replacen(" width=\"128\" height=\"128\"", "", 1),
        wordmark = WORDMARK.trim_end(),
        heading = html_escape(&heading),
        detail = html_escape(&detail),
    );
    (status, page)
}

/// The page's look, in the brand's colours: ink on paper, or paper on ink
/// when the system is dark, with orange for the rule under the wordmark.
/// The badges' greens and reds keep 4.5:1 against their glyph and 3:1
/// against the card.
const FINISHED_CSS: &str = ":root{color-scheme:light dark;--bg:#fbf7f0;--card:#fffdf9;--fg:#2a2623;\
--dim:#6b635b;--line:rgba(42,38,35,.10);--accent:#e8660c;--ok:#2f7a4a;--no:#b42a22;--glyph:#fbf7f0;\
--shadow:rgba(42,38,35,.08)}\
@media (prefers-color-scheme: dark){:root{--bg:#2a2623;--card:#34302c;--fg:#fbf7f0;\
--dim:#b8aea3;--line:rgba(251,247,240,.08);--ok:#86c99a;--no:#f0a092;--glyph:#2a2623;\
--shadow:rgba(0,0,0,.25)}}\
*{box-sizing:border-box}html,body{height:100%}\
body{margin:0;display:grid;place-items:center;background:var(--bg);color:var(--fg);\
font:15px/1.5 \"Adwaita Sans\",Cantarell,system-ui,-apple-system,\"Segoe UI\",Roboto,sans-serif}\
main{width:min(420px,calc(100% - 32px));padding:40px 36px 36px;text-align:center;background:var(--card);\
border:1px solid var(--line);border-radius:24px;box-shadow:0 12px 40px var(--shadow)}\
.icon{position:relative;width:96px;height:96px;margin:0 auto 14px}.icon>svg:first-child{width:96px;height:96px}\
.badge{position:absolute;right:-8px;bottom:0;width:34px;height:34px}\
.badge circle{fill:var(--ok);stroke:var(--card);stroke-width:2}.badge.no circle{fill:var(--no)}\
.badge path{fill:none;stroke:var(--glyph);stroke-width:2.6;stroke-linecap:round;stroke-linejoin:round}\
.wordmark{display:block;height:22px;margin:0 auto;fill:var(--fg)}\
h1::before{content:\"\";display:block;width:28px;height:3px;\
margin:18px auto 20px;border-radius:2px;background:var(--accent)}\
h1{margin:0 0 8px;font-size:22px;font-weight:800;letter-spacing:-.01em}\
p{margin:0;color:var(--dim)}\
@media (prefers-reduced-motion: no-preference){main{animation:rise .35s ease-out both}\
@keyframes rise{from{opacity:0;transform:translateY(8px)}to{opacity:1;transform:none}}}";

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_granted_sign_in_says_it_worked_and_what_to_do_next() {
        let (status, html) = page(Finished::Signed);
        assert_eq!(status, "200 OK");
        assert!(html.contains("Signed in to Penguin Mail"), "{html}");
        assert!(html.contains("close this tab"), "{html}");
        assert!(
            html.contains("<svg"),
            "the page carries the app icon: {html}"
        );
        assert!(html.contains("prefers-color-scheme: dark"), "{html}");
    }

    #[test]
    fn the_icon_scales_rather_than_crops() {
        let (_, html) = page(Finished::Signed);
        let icon = &html[html.find("<div class=\"icon\"").expect("an icon box")..];
        let tag = &icon[icon.find("<svg").expect("the icon")..];
        let tag = &tag[..tag.find('>').expect("a whole tag")];
        assert!(tag.contains("viewBox="), "{tag}");
        assert!(
            !tag.contains("width="),
            "a fixed size would crop it at 96 px: {tag}"
        );
    }

    #[test]
    fn the_page_carries_the_outlined_wordmark() {
        let (_, html) = page(Finished::Signed);
        let mark = &html[html
            .find("<svg class=\"wordmark\"")
            .expect("a wordmark: {html}")..];
        let mark = &mark[..mark.find("</svg>").expect("a whole svg")];
        assert!(
            mark.contains("<path d=\"M"),
            "the letters are outlines: {mark}"
        );
        assert!(!mark.contains("<text"), "no font needed: {mark}");
    }

    #[test]
    fn the_page_wears_the_brand_paper_and_ink() {
        let (_, html) = page(Finished::Signed);
        for colour in ["#fbf7f0", "#2a2623", "#e8660c"] {
            assert!(html.contains(colour), "{colour} in {html}");
        }
    }

    #[test]
    fn declining_access_says_so_and_how_to_try_again() {
        let (status, html) = page(Finished::Denied);
        assert_eq!(status, "400 Bad Request");
        assert!(html.contains("didn't allow access"), "{html}");
        assert!(!html.contains("terminal"), "{html}");
    }

    #[test]
    fn a_failed_sign_in_carries_only_fixed_words() {
        let (_, html) = page(Finished::Failed);
        assert!(html.contains("Sign-in didn't finish"), "{html}");
        assert!(!html.contains("<script>"), "{html}");
    }

    #[test]
    fn the_page_loads_nothing_from_the_network() {
        let (_, html) = page(Finished::Signed);
        // The icon's own gradients are `url(#id)`, inside the page.
        for load in ["<link", "<script", "<img", "url(http", "url(//", "src="] {
            assert!(!html.contains(load), "{load} in {html}");
        }
    }
}
