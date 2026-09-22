//! Leaving a mailing list: reading a `List-Unsubscribe` header, choosing
//! how to leave, and making the one-click request. The Unsubscribe button
//! and the assistant both end here. Sending a request by mail needs the
//! app's composer, and opening a page needs a browser, so for those two
//! this hands back what the caller has left to do.

use mailrs_domain::AccountId;

use crate::{Accounts, MailActions, SyncError};

/// How to unsubscribe, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsubscribe {
    /// RFC 8058: POST `List-Unsubscribe=One-Click` to this https URL.
    OneClick(String),
    /// Send a message to this address.
    Email {
        to: String,
        subject: String,
        body: String,
    },
    /// Open the sender's unsubscribe page.
    Page(String),
}

/// The best way the header offers, if any.
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
    web.map(Unsubscribe::Page)
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
                let sync = self
                    .accounts
                    .account(account_id)
                    .ok_or(SyncError::UnknownAccount(account_id))?;
                sync.one_click_unsubscribe(&url).await?;
                Ok(Leave::Done)
            }
            Unsubscribe::Email { to, subject, body } => Ok(Leave::Send { to, subject, body }),
            Unsubscribe::Page(url) => Ok(Leave::Open(url)),
        }
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
    fn otherwise_an_email_then_the_web_page() {
        assert_eq!(
            choose(BOTH, false),
            Some(Unsubscribe::Email {
                to: "leave@news.example".into(),
                subject: "Remove me".into(),
                body: "unsubscribe".into(),
            })
        );
        assert_eq!(
            choose("<https://news.example/u/1>", false),
            Some(Unsubscribe::Page("https://news.example/u/1".into()))
        );
        assert_eq!(
            choose("<http://plain.example/u>", true),
            Some(Unsubscribe::Page("http://plain.example/u".into()))
        );
        assert_eq!(choose("", false), None);
    }
}
