//! Leaving a mailing list: reading a `List-Unsubscribe` header, choosing
//! how to leave, and making the one-click request. The Unsubscribe button
//! and the assistant both end here. Sending a request by mail needs the
//! app's composer, and opening a page needs a browser, so for those two
//! this hands back what the caller has left to do.

use mailrs_domain::AccountId;
use mailrs_gmail::html_to_text;
use mailrs_store::unsubscribes::{self, How};

use crate::{Accounts, MailActions, SyncError};

/// How to unsubscribe, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsubscribe {
    /// RFC 8058: POST `List-Unsubscribe=One-Click` to this https URL.
    OneClick(String),
    /// Open the sender's unsubscribe page.
    Page(String),
    /// Send a message to this address.
    Email {
        to: String,
        subject: String,
        body: String,
    },
    /// Open the unsubscribe page a link in the message body points at.
    BodyLink(String),
}

/// Words a link uses when it takes the reader off a list, in the two
/// languages the owner reads. They match what a sender wrote, not what
/// Penguin Mail says, so they stay here rather than in the po files.
const LEAVING: [&str; 8] = [
    "unsubscribe",
    "opt out",
    "opt-out",
    "manage preferences",
    "email preferences",
    "cancelar subscrição",
    "anular subscrição",
    "deixar de receber",
];

/// The best way the header offers, if any. A page comes before a mail
/// request: senders answer a page and often ignore the mail.
pub fn choose(header: &str, one_click: bool) -> Option<Unsubscribe> {
    let links: Vec<&str> = header
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            part.strip_prefix('<')
                .and_then(|p| p.strip_suffix('>'))
                .map(str::trim)
        })
        .collect();
    let web = links
        .iter()
        .find(|l| {
            let lower = l.to_ascii_lowercase();
            lower.starts_with("https://") || lower.starts_with("http://")
        })
        .map(|l| l.to_string());
    // The automatic POST goes only over https.
    if one_click
        && let Some(url) = web
            .as_ref()
            .filter(|u| u.to_ascii_lowercase().starts_with("https://"))
    {
        return Some(Unsubscribe::OneClick(url.clone()));
    }
    if let Some(url) = web {
        return Some(Unsubscribe::Page(url));
    }
    if let Some(mailto) = links.iter().find_map(|l| strip_prefix_ci(l, "mailto:")) {
        let (address, query) = mailto.split_once('?').unwrap_or((mailto, ""));
        let param = |name: &str| {
            query.split('&').find_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                key.eq_ignore_ascii_case(name)
                    .then(|| percent_decode(value))
            })
        };
        let to = percent_decode(address);
        if to.contains('@') {
            return Some(Unsubscribe::Email {
                to,
                subject: param("subject").unwrap_or_else(|| "unsubscribe".into()),
                body: param("body").unwrap_or_else(|| "unsubscribe".into()),
            });
        }
    }
    None
}

/// The best way out of a list: what the header offers, else a link in
/// `html`, the newest message's body. The body comes last because a link
/// there can be anything the sender made it, while the header is a
/// promise.
pub fn choose_with_body(
    header: Option<&str>,
    one_click: bool,
    html: Option<&str>,
) -> Option<Unsubscribe> {
    header
        .and_then(|header| choose(header, one_click))
        .or_else(|| html.and_then(body_link).map(Unsubscribe::BodyLink))
}

/// The first web link in `html` whose text or description reads as
/// leaving a list.
///
/// The reader walks `<a>` tags rather than parsing the document, which is
/// what mail HTML allows: it is written by every marketing tool there is,
/// and it does not have to be well formed to show a link.
fn body_link(html: &str) -> Option<String> {
    // Tag and attribute names are ASCII, so lower-casing that way keeps
    // every position the same as in the original.
    let lower = html.to_ascii_lowercase();
    let mut at = 0;
    while let Some(found) = lower[at..].find("<a") {
        let open = at + found;
        let mut after = lower[open + 2..].chars();
        at = open + 2;
        if !matches!(after.next(), Some(c) if c.is_ascii_whitespace() || c == '>') {
            continue;
        }
        let Some(shut) = lower[open..].find('>') else {
            break;
        };
        let tag = &html[open + 2..open + shut];
        at = open + shut + 1;
        let end = lower[at..].find("</a").map_or(html.len(), |i| at + i);
        let Some(href) = attribute(tag, "href") else {
            continue;
        };
        let scheme = href.to_ascii_lowercase();
        if !scheme.starts_with("http://") && !scheme.starts_with("https://") {
            continue;
        }
        let text = html_to_text(&html[at..end]);
        let described = attribute(tag, "aria-label").unwrap_or_default();
        if reads_as_leaving(&text) || reads_as_leaving(&described) {
            // An address in a document carries entities, `&amp;` above
            // all. There is no markup in it left to strip.
            return Some(html_to_text(&href));
        }
    }
    None
}

/// Whether `text` says, in either language, that the link leaves a list.
fn reads_as_leaving(text: &str) -> bool {
    let words = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let words = words.to_lowercase();
    LEAVING.iter().any(|word| words.contains(word))
}

/// The value of the `name` attribute in a tag's text, quoted or bare.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let lower = tag.to_ascii_lowercase();
    let mut at = 0;
    while let Some(found) = lower[at..].find(name) {
        let start = at + found;
        at = start + name.len();
        if start > 0 && !bytes[start - 1].is_ascii_whitespace() {
            continue;
        }
        let mut i = at;
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if bytes.get(i) != Some(&b'=') {
            continue;
        }
        i += 1;
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        let quote = bytes.get(i).copied().filter(|b| *b == b'"' || *b == b'\'');
        let from = if quote.is_some() { i + 1 } else { i };
        let mut to = from;
        while let Some(byte) = bytes.get(to) {
            let ends = match quote {
                Some(q) => *byte == q,
                None => byte.is_ascii_whitespace(),
            };
            if ends {
                break;
            }
            to += 1;
        }
        return Some(tag[from..to].to_string());
    }
    None
}

/// What leaving a list leaves for the caller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leave {
    /// The list took the request, and nothing is left to do.
    Done,
    /// Send this request from the account.
    Send {
        to: String,
        subject: String,
        body: String,
    },
    /// Open the sender's unsubscribe page in the browser.
    Open(String),
}

impl<A: Accounts> MailActions<A> {
    /// Leaves a mailing list the way `how` says, from the account. A
    /// one-click list hears from this at once; the other two ways come
    /// back as what the caller still has to do.
    pub async fn unsubscribe(
        &self,
        account_id: AccountId,
        how: Unsubscribe,
    ) -> Result<Leave, SyncError> {
        match how {
            Unsubscribe::OneClick(url) => {
                // The list's server takes the request, but only on behalf
                // of an account that is connected.
                self.accounts
                    .account(account_id)
                    .ok_or(SyncError::UnknownAccount(account_id))?;
                self.one_click.post(&url).await?;
                Ok(Leave::Done)
            }
            Unsubscribe::Email { to, subject, body } => Ok(Leave::Send { to, subject, body }),
            Unsubscribe::Page(url) | Unsubscribe::BodyLink(url) => Ok(Leave::Open(url)),
        }
    }

    /// Notes that the person left the list `sender` writes from, once the
    /// list has let go. The window and the assistant both call this, so
    /// a conversation from that sender stops offering Unsubscribe and the
    /// assistant can say when the list was left.
    pub async fn left(
        &self,
        account_id: AccountId,
        sender: &str,
        how: How,
    ) -> Result<(), SyncError> {
        let sender = sender.to_string();
        let at = crate::now_millis();
        self.db
            .write(move |c| unsubscribes::record(c, account_id, &sender, how, at))
            .await?;
        Ok(())
    }
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &text[prefix.len()..])
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = text.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: &str =
        "<mailto:leave@news.example?subject=Remove%20me>, <https://news.example/u/1>";

    #[test]
    fn one_click_wins_when_the_sender_supports_it() {
        assert_eq!(
            choose(BOTH, true),
            Some(Unsubscribe::OneClick("https://news.example/u/1".into()))
        );
    }

    #[test]
    fn a_page_comes_before_a_mail_request() {
        let header = "<mailto:leave@list.example?subject=bye>, <https://list.example/u/1>";
        assert_eq!(
            choose(header, false),
            Some(Unsubscribe::Page("https://list.example/u/1".into()))
        );
    }

    #[test]
    fn one_click_still_comes_first() {
        let header = "<mailto:leave@list.example>, <https://list.example/u/1>";
        assert_eq!(
            choose(header, true),
            Some(Unsubscribe::OneClick("https://list.example/u/1".into()))
        );
    }

    #[test]
    fn a_mail_request_is_all_a_header_without_a_page_offers() {
        assert_eq!(
            choose("<mailto:leave@news.example?subject=Remove%20me>", false),
            Some(Unsubscribe::Email {
                to: "leave@news.example".into(),
                subject: "Remove me".into(),
                body: "unsubscribe".into(),
            })
        );
        // The automatic POST goes only over https, so a plain link is a page.
        assert_eq!(
            choose("<http://plain.example/u>", true),
            Some(Unsubscribe::Page("http://plain.example/u".into()))
        );
        assert_eq!(choose("", false), None);
    }

    #[test]
    fn a_body_link_is_the_last_resort() {
        let html = r#"<p>Hi</p><a href="https://shop.example/out?u=9">Cancelar subscrição</a>"#;
        assert_eq!(
            choose_with_body(None, false, Some(html)),
            Some(Unsubscribe::BodyLink("https://shop.example/out?u=9".into()))
        );
        let header = "<mailto:leave@list.example>";
        assert!(matches!(
            choose_with_body(Some(header), false, Some(html)),
            Some(Unsubscribe::Email { .. })
        ));
    }

    #[test]
    fn a_body_link_needs_words_that_read_as_leaving() {
        let html = r#"<a href="https://shop.example/sale">See the sale</a>"#;
        assert_eq!(choose_with_body(None, false, Some(html)), None);
        let aria = r#"<a aria-label="Unsubscribe" href="https://x.example/o">✕</a>"#;
        assert!(matches!(
            choose_with_body(None, false, Some(aria)),
            Some(Unsubscribe::BodyLink(_))
        ));
    }

    #[test]
    fn a_body_link_has_to_be_a_web_link() {
        let mailto = r#"<a href="mailto:leave@shop.example">Unsubscribe</a>"#;
        assert_eq!(choose_with_body(None, false, Some(mailto)), None);
        let last = r##"<a href="#top">Opt out</a>
            <a href="HTTPS://shop.example/o?a=1&amp;b=2"><img alt=""> Manage preferences </a>"##;
        assert_eq!(
            choose_with_body(None, false, Some(last)),
            Some(Unsubscribe::BodyLink(
                "HTTPS://shop.example/o?a=1&b=2".into()
            ))
        );
    }
}
