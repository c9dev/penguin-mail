//! Picture bytes turned into textures without holding up the GTK thread.
//!
//! GDK decodes PNG, JPEG and TIFF itself, in this process. It hands every
//! other format to gdk-pixbuf, which on GNOME 49 and later passes the work
//! to glycin: a loader in a bubblewrap sandbox that the app talks to over a
//! private D-Bus connection. A synchronous decode of such a picture on the
//! GTK thread waits for that process to start and answer, and nothing in
//! glycin bounds the wait. glycin 2.1, the one Ubuntu 26.04 ships, also
//! deadlocks once GIO's shared thread pool is full of callers waiting on
//! it (its 2.2 notes say so). The app decodes the three formats GDK reads
//! on the spot, and the rest on a thread of its own, for [`WAIT`] at most.

use std::time::Duration;

use gtk::{gdk, glib};

/// How long a picture may take to decode before the app gives up on it
/// and leaves its placeholder.
pub const WAIT: Duration = Duration::from_secs(10);

/// Whether GDK decodes `bytes` itself rather than through glycin: PNG,
/// JPEG and TIFF, told apart by their first bytes as GDK does.
pub fn decoded_in_process(bytes: &[u8]) -> bool {
    const SIGNATURES: [&[u8]; 4] = [b"\x89PNG\r\n\x1a\n", b"\xff\xd8", b"II*\0", b"MM\0*"];
    SIGNATURES
        .iter()
        .any(|signature| bytes.starts_with(signature))
}

/// `bytes` as a texture when GDK decodes them in this process, which takes
/// no other process and no unbounded wait. `None` for any other format,
/// and for bytes that are not a picture.
pub fn here(bytes: &glib::Bytes) -> Option<gdk::Texture> {
    decoded_in_process(bytes)
        .then(|| gdk::Texture::from_bytes(bytes).ok())
        .flatten()
}

/// `bytes` as a texture, decoded on the spot when GDK reads the format
/// itself and otherwise on a thread of its own. `None` when the bytes are
/// not a picture, or when the decode takes longer than [`WAIT`].
pub async fn decode(bytes: glib::Bytes) -> Option<gdk::Texture> {
    if decoded_in_process(&bytes) {
        return gdk::Texture::from_bytes(&bytes).ok();
    }
    off_thread(WAIT, move || gdk::Texture::from_bytes(&bytes).ok())
        .await
        .flatten()
}

/// What `work` returns, run on a new thread, or `None` once `wait` has
/// passed without an answer. A thread still stuck after that stays stuck,
/// but the caller goes on.
pub async fn off_thread<T: Send + 'static>(
    wait: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    use futures::future::{Either, select};

    let (answer, answered) = futures::channel::oneshot::channel();
    // A thread of its own rather than GIO's pool: glycin 2.1 deadlocks
    // once that pool is full of callers waiting on it.
    let spawned = std::thread::Builder::new()
        .name("decode-picture".into())
        .spawn(move || {
            let _ = answer.send(work());
        });
    if let Err(err) = spawned {
        tracing::warn!(error = %err, "could not start a thread to decode a picture");
        return None;
    }
    match select(answered, glib::timeout_future(wait)).await {
        Either::Left((Ok(value), _)) => Some(value),
        Either::Left((Err(_), _)) => None,
        Either::Right(_) => {
            tracing::warn!(?wait, "a picture took too long to decode");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    const JPEG: &[u8] = b"\xff\xd8\xff\xe0\0\x10JFIF";
    const TIFF_LITTLE: &[u8] = b"II*\0\x08\0\0\0";
    const TIFF_BIG: &[u8] = b"MM\0*\0\0\0\x08";
    const GIF: &[u8] = b"GIF89a\x01\0\x01\0";
    const WEBP: &[u8] = b"RIFF\x24\0\0\0WEBPVP8 ";

    #[test]
    fn png_jpeg_and_tiff_are_decoded_in_process() {
        for bytes in [PNG, JPEG, TIFF_LITTLE, TIFF_BIG] {
            assert!(decoded_in_process(bytes), "{bytes:?}");
        }
    }

    #[test]
    fn other_formats_go_to_the_sandboxed_loader() {
        for bytes in [
            GIF,
            WEBP,
            b"<svg".as_slice(),
            b"\x89P".as_slice(),
            b"".as_slice(),
        ] {
            assert!(!decoded_in_process(bytes), "{bytes:?}");
        }
    }

    #[test]
    fn off_thread_hands_back_what_the_work_returned() {
        let got = glib::MainContext::new().block_on(off_thread(WAIT, || 6 * 7));
        assert_eq!(got, Some(42));
    }

    #[test]
    fn off_thread_gives_up_on_work_that_never_answers() {
        let (release, stuck) = mpsc::channel::<()>();
        let started = std::time::Instant::now();
        let got = glib::MainContext::new()
            .block_on(off_thread(Duration::from_millis(50), move || {
                stuck.recv().is_ok()
            }));
        assert_eq!(got, None);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        // Lets the stuck thread finish rather than outlive the test.
        drop(release);
    }

    #[test]
    fn bytes_that_are_no_picture_decode_to_nothing() {
        let bytes = glib::Bytes::from_static(b"no picture here");
        assert!(here(&bytes).is_none());
    }
}
