//! A copy of a picture that the body being written names only by its
//! address.
//!
//! The message goes out naming that address, and the recipient's own client
//! asks for the picture. The composer fetches a copy of its own for the
//! writer to see, on a thread of its own and for [`texture::WAIT`] at most,
//! so a signature's logo draws without holding the GTK thread.
//!
//! Only the writer's own lines are asked about. A picture inside a quote
//! came in with someone else's mail, and fetching it would tell whoever
//! hosts it that this person has read that message, which is the very thing
//! the reader's picture policy exists to keep quiet.

use futures::StreamExt;
use gtk::glib;

use crate::ui::texture;

/// The schemes a picture may be asked for over the network.
const SCHEMES: [&str; 2] = ["http", "https"];

/// The largest picture the composer will fetch to draw one.
const LIMIT: usize = 8 << 20;

/// Whether `src` names a picture that may be fetched and drawn. A body
/// carries pictures by `cid:` as well, which needs no asking, and takes
/// whatever a draft or a paste held, which is not always an address.
pub fn drawable(src: &str) -> bool {
    reqwest::Url::parse(src).is_ok_and(|url| SCHEMES.contains(&url.scheme()))
}

/// A copy of the picture at `url`, or `None` when there is no copy to have:
/// the address is not one, the host refuses, the answer is not a picture or
/// is bigger than [`LIMIT`], or nothing arrives within [`texture::WAIT`].
pub async fn pull(url: &str) -> Option<glib::Bytes> {
    let url = url.to_string();
    // A thread of its own, and a runtime on it, rather than the app's: the
    // GTK thread answers nothing while it waits, and the app's runtime is
    // busy fetching mail. `off_thread` gives up on `WAIT` and leaves this
    // thread to finish on its own, which is why the client bounds the ask.
    texture::off_thread(texture::WAIT, move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?
            .block_on(fetch(&url))
    })
    .await
    .flatten()
}

/// The picture at `url`, read until [`LIMIT`] rather than to the end of
/// whatever the host decides to send.
async fn fetch(url: &str) -> Option<glib::Bytes> {
    if !drawable(url) {
        return None;
    }
    let client = reqwest::Client::builder()
        .user_agent(format!("penguin-mail/{}", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::limited(3))
        .timeout(texture::WAIT)
        .build()
        .ok()?;
    let answer = client.get(url).send().await.ok()?;
    if !answer.status().is_success() {
        return None;
    }
    if answer
        .content_length()
        .is_some_and(|size| size > LIMIT as u64)
    {
        return None;
    }
    // A host may name any type it likes, so the bytes decide: what arrives
    // has to be a picture the app can draw at all.
    let picture = answer
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|kind| kind.to_str().ok())
        .is_some_and(|kind| kind.trim_start().starts_with("image/"));
    if !picture {
        return None;
    }
    let mut body = answer.bytes_stream();
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.ok()?;
        if bytes.len() + chunk.len() > LIMIT {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    (!bytes.is_empty()).then(|| glib::Bytes::from(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_is_only_asked_for_over_http() {
        for src in [
            "https://example.com/Logo.jpg",
            "http://192.0.2.1:8080/logo.png",
        ] {
            assert!(drawable(src), "{src}");
        }
        for src in [
            "cid:logo@mailrs",
            "file:///home/me/logo.png",
            "data:image/png;base64,AAAA",
            "ftp://files.example/logo.png",
            "logo.png",
            "/logo.png",
            "",
        ] {
            assert!(!drawable(src), "{src}");
        }
    }

    #[test]
    fn a_picture_that_is_no_address_is_never_fetched() {
        for src in ["cid:logo@mailrs", "file:///home/me/logo.png", ""] {
            let context = glib::MainContext::new();
            assert!(context.block_on(fetch(src)).is_none(), "{src}");
        }
    }
}
