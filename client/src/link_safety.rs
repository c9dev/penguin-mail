//! Local checks of link destinations. They find signs of deception, not a site's reputation.

use mailrs_mime::html::{Piece, walk};
use url::Url;

#[derive(Debug, PartialEq, Eq)]
pub enum Concern {
    DifferentSite,
    Credentials,
    HiddenCharacters,
}

pub struct Link {
    pub site: String,
    pub address: String,
    pub concern: Option<Concern>,
}

pub fn inspect(href: &str, text: &str) -> Option<Link> {
    let url = Url::parse(href).ok()?;
    let site = match url.scheme() {
        "http" | "https" => site_of(&url)?.to_string(),
        "mailto" => url.path().to_string(),
        _ => return None,
    };
    let concern = if href.chars().any(hidden)
        || text
            .chars()
            .any(|c| hidden(c) && !matches!(c, '\n' | '\r' | '\t'))
    {
        Some(Concern::HiddenCharacters)
    } else if !url.username().is_empty() || url.password().is_some() {
        Some(Concern::Credentials)
    } else {
        displayed_url(text)
            .filter(|shown| site_of(shown) != site_of(&url))
            .map(|_| Concern::DifferentSite)
    };
    Some(Link {
        site: visible(&site),
        address: visible(url.as_str()),
        concern,
    })
}

fn site_of(url: &Url) -> Option<&str> {
    let host = url.host_str()?;
    Some(psl::domain_str(host).unwrap_or(host).trim_end_matches('.'))
}

fn displayed_url(text: &str) -> Option<Url> {
    let text = text.trim();
    if text.chars().any(char::is_whitespace) {
        return None;
    }
    let url = Url::parse(text).ok().or_else(|| {
        // Only an address-shaped label claims a destination. Ordinary
        // words such as "Sign in" say nothing about its domain.
        (text.contains('.') && !text.contains('@'))
            .then(|| Url::parse(&format!("https://{text}")).ok())
            .flatten()
            .filter(|url| {
                url.host_str().is_some_and(|host| {
                    psl::suffix(host.as_bytes()).is_some_and(|suffix| suffix.is_known())
                })
            })
    })?;
    matches!(url.scheme(), "http" | "https").then_some(url)
}

fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{00ad}' | '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
}

fn visible(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if hidden(c) {
            use std::fmt::Write;
            let _ = write!(out, "\\u{{{:04x}}}", c as u32);
        } else {
            out.push(c);
        }
    }
    out
}

pub fn suspicious_html(html: &str) -> bool {
    let mut current: Option<(String, String)> = None;
    let mut found = false;
    let mut hidden_tag = None;
    walk(html, |piece| match piece {
        Piece::Tag(tag) if matches!(tag.name, "style" | "script" | "title") => {
            if tag.closing {
                hidden_tag = None;
            } else {
                hidden_tag = Some(tag.name.to_string());
            }
        }
        _ if hidden_tag.is_some() => {}
        Piece::Tag(tag) if tag.name == "a" => {
            if let Some((href, text)) = current.take() {
                found |= inspect(&href, &text).is_some_and(|link| link.concern.is_some());
            }
            if !tag.closing {
                current = tag
                    .attribute("href")
                    .map(|href| (href.to_string(), String::new()));
            }
        }
        Piece::Text(text) => {
            if let Some((_, words)) = &mut current {
                words.push_str(text);
            }
        }
        _ => {}
    });
    found
        || current.is_some_and(|(href, text)| {
            inspect(&href, &text).is_some_and(|link| link.concern.is_some())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_displayed_address_cannot_disguise_another_site() {
        assert_eq!(
            inspect("https://evil.example/login", "https://bank.example")
                .unwrap()
                .concern,
            Some(Concern::DifferentSite)
        );
    }

    #[test]
    fn related_subdomains_and_ordinary_link_labels_do_not_raise_an_alarm() {
        for (href, text) in [
            ("https://login.example.co.uk/", "https://www.example.co.uk"),
            ("https://example.com/", "Read more"),
            ("https://example.com/", "example.com"),
            ("https://example.com/", "\n  Read more\n"),
            ("https://example.com/", "v1.0.8"),
            ("https://example.com/", "statement.pdf"),
        ] {
            assert_eq!(inspect(href, text).unwrap().concern, None, "{href}");
        }
    }

    #[test]
    fn a_domain_in_the_username_does_not_hide_the_destination() {
        let link = inspect("https://bank.example@evil.example/login", "Sign in").unwrap();
        assert_eq!(link.site, "evil.example");
        assert_eq!(link.concern, Some(Concern::Credentials));
    }

    #[test]
    fn international_domains_keep_their_ascii_spelling_without_an_alarm() {
        let link = inspect("https://exämple.org/path", "Read more").unwrap();
        assert_eq!(link.site, "xn--exmple-cua.org");
        assert_eq!(link.address, "https://xn--exmple-cua.org/path");
        assert_eq!(link.concern, None);
    }

    #[test]
    fn direction_controls_are_visible_and_flagged() {
        let link = inspect("https://example.com/\u{202e}fdp.exe", "Download").unwrap();
        assert!(!link.address.contains('\u{202e}'));
        assert_eq!(link.concern, Some(Concern::HiddenCharacters));
    }

    #[test]
    fn private_and_public_suffixes_do_not_merge_unrelated_sites() {
        for (href, text) in [
            ("https://evil.co.uk", "https://bank.co.uk"),
            ("https://evil.github.io", "https://bank.github.io"),
            ("https://bank.example.evil.com", "https://bank.example"),
        ] {
            assert_eq!(
                inspect(href, text).unwrap().concern,
                Some(Concern::DifferentSite)
            );
        }
    }

    #[test]
    fn only_external_web_and_mail_links_have_previews() {
        for href in [
            "mailrs:toggle/m1",
            "mailrs-cid:1/x",
            "javascript:alert(1)",
            "data:text/plain,x",
            "#section",
            "https://",
        ] {
            assert!(inspect(href, "").is_none(), "{href}");
        }
        assert_eq!(
            inspect("mailto:ann@example.com?subject=Hello", "Ann")
                .unwrap()
                .site,
            "ann@example.com"
        );
    }

    #[test]
    fn message_checks_read_nested_link_text_and_decoded_attributes() {
        assert!(suspicious_html(
            r#"<a href="https://evil.example/?x=1&amp;y=2"><strong>https://bank.example</strong></a>"#
        ));
        assert!(!suspicious_html(
            r#"<style>a::before { content: 'https://evil.example'; }</style><a href="https://example.com">Read more</a>"#
        ));
    }
}
