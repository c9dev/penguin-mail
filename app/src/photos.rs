//! Contact photos, for the thread list's rows and the conversation's
//! message headers.
//!
//! The photos stay on disk. A row asks for a texture and the conversation
//! for a `data:` URI, by address; the first ask reads the file off the GTK
//! thread, and whoever listens hears which addresses arrived, so only the
//! rows and pages showing them are drawn again. What was read is kept the
//! most recently used first, up to a number of bytes, and a photo larger
//! than a face is ever drawn is shrunk first: a contact photo can be a
//! 4 MB camera picture, and the row shows it 34 pixels wide.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use base64::Engine;
use futures::future::LocalBoxFuture;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::recent::{Recent, Weighed};
use crate::ui::texture;

/// Contact photo files on disk, by lower-case address.
pub(crate) type Files = HashMap<String, PathBuf>;

/// The longest side a photo is kept at, in pixels: the 40 pixel face in a
/// message header at three times the scale, with room to spare.
const EDGE: usize = 128;

/// Bytes of row textures kept: a shrunk photo takes 64 KB at most, so
/// this is about 128 faces, more than a list shows at once.
const TEXTURE_BYTES: usize = 8 * 1024 * 1024;

/// Bytes of page photos kept.
const URI_BYTES: usize = 4 * 1024 * 1024;

/// A photo file this small goes in the page as it is, without decoding.
const SMALL_FILE: usize = 48 * 1024;

/// The most pixels a photo may say it has before it is decoded: 4096 on
/// each side, 64 MB once decoded. No address book photo comes near it.
const MOST_PIXELS: u64 = 4096 * 4096;

/// Whether a photo GDK decodes in this process says it is small enough
/// to decode. A file whose header cannot be read is refused too.
pub(crate) fn fits(bytes: &[u8]) -> bool {
    texture::declared_size(bytes)
        .is_some_and(|(width, height)| u64::from(width) * u64::from(height) <= MOST_PIXELS)
}

impl Weighed for gdk::Texture {
    fn weight(&self) -> usize {
        self.width() as usize * self.height() as usize * 4
    }
}

/// Every contact photo the app draws, by address.
pub(crate) struct ContactPhotos {
    textures: Shelf<gdk::Texture>,
    uris: Shelf<Rc<str>>,
}

impl ContactPhotos {
    pub(crate) fn new() -> ContactPhotos {
        ContactPhotos {
            textures: Shelf::new(TEXTURE_BYTES, read_textures),
            uris: Shelf::new(URI_BYTES, read_uris),
        }
    }

    /// The photo of `address` for a row, when it is in memory; otherwise
    /// it is read, and [`Self::on_texture`] says when it arrives.
    pub(crate) fn texture(&self, address: &str) -> Option<gdk::Texture> {
        self.textures.get(address)
    }

    /// The photo of `address` as a `data:` URI for the conversation page,
    /// which loads nothing from disk; when it is not in memory, it is
    /// read, and [`Self::on_uri`] says when it arrives.
    pub(crate) fn uri(&self, address: &str) -> Option<Rc<str>> {
        self.uris.get(address)
    }

    /// Calls `listener` with the addresses whose row photos arrived, for
    /// as long as it answers `true`.
    pub(crate) fn on_texture(&self, listener: impl Fn(&[String]) -> bool + 'static) {
        self.textures.on_arrival(listener);
    }

    /// Calls `listener` with the addresses whose page photos arrived, for
    /// as long as it answers `true`.
    pub(crate) fn on_uri(&self, listener: impl Fn(&[String]) -> bool + 'static) {
        self.uris.on_arrival(listener);
    }

    /// The photo file of `address`, when a contact has one on disk.
    pub(crate) fn path(&self, address: &str) -> Option<PathBuf> {
        self.textures.path(address)
    }

    /// Whether any contact has a photo. Without one, rows show no face.
    pub(crate) fn any(&self) -> bool {
        self.textures.any()
    }

    /// The photo files on disk now, after the address books load or are
    /// forgotten. Every photo read before is let go.
    pub(crate) fn set_files(&self, files: Files) {
        let files = Rc::new(files);
        self.textures.set_files(Rc::clone(&files));
        self.uris.set_files(files);
    }
}

/// Keeps the contacts whose photo file is on disk, by lower-case address.
/// Runs on a blocking thread, since it asks the disk once per contact.
pub(crate) fn on_disk(dir: PathBuf, files: Vec<(String, String)>) -> Files {
    files
        .into_iter()
        .filter_map(|(email, name)| {
            let file = dir.join(name);
            file.exists().then(|| (email.to_lowercase(), file))
        })
        .collect()
}

/// A photo file read on a worker thread: a texture when GDK decodes the
/// format itself, or the bytes for glycin's sandboxed loader.
enum Face {
    Decoded(Option<gdk::Texture>),
    Elsewhere(glib::Bytes),
}

/// Reads photo files as row textures. PNG, JPEG and TIFF decode on one
/// worker thread for the batch; any other format goes to glycin on a
/// thread of its own, as [`texture`] explains.
fn read_textures(files: Vec<(String, PathBuf)>) -> LocalBoxFuture<'static, Answers<gdk::Texture>> {
    Box::pin(async move {
        let asked: Vec<String> = files.iter().map(|(address, _)| address.clone()).collect();
        let Some(read) = texture::off_thread(texture::WAIT, move || {
            files
                .into_iter()
                .map(|(address, path)| (address, face_at(&path)))
                .collect::<Vec<_>>()
        })
        .await
        else {
            // A slow disk or a dead worker says nothing about the photos.
            return asked
                .into_iter()
                .map(|address| (address, Reply::Failed))
                .collect();
        };
        let mut answers = Vec::with_capacity(read.len());
        for (address, face) in read {
            let reply = match face {
                Face::Decoded(texture) => Reply::of(texture),
                Face::Elsewhere(bytes) => {
                    match texture::off_thread(texture::WAIT, move || {
                        gdk::Texture::from_bytes(&bytes).ok().map(|t| shrunk(&t))
                    })
                    .await
                    {
                        Some(texture) => Reply::of(texture),
                        None => Reply::Failed,
                    }
                }
            };
            answers.push((address, reply));
        }
        answers
    })
}

fn face_at(path: &Path) -> Face {
    let Ok(bytes) = std::fs::read(path) else {
        return Face::Decoded(None);
    };
    let bytes = glib::Bytes::from_owned(bytes);
    if !texture::decoded_in_process(&bytes) {
        return Face::Elsewhere(bytes);
    }
    if !fits(&bytes) {
        tracing::warn!(path = %path.display(), "a contact photo says it is too large to decode");
        return Face::Decoded(None);
    }
    Face::Decoded(gdk::Texture::from_bytes(&bytes).ok().map(|t| shrunk(&t)))
}

/// `texture`, or a copy of it no longer than [`EDGE`] on either side.
fn shrunk(texture: &gdk::Texture) -> gdk::Texture {
    shrunk_to(texture, EDGE)
}

/// `texture`, or a premultiplied copy of it no longer than `edge` on
/// either side.
pub(crate) fn shrunk_to(texture: &gdk::Texture, edge: usize) -> gdk::Texture {
    let (width, height) = (texture.width() as usize, texture.height() as usize);
    if width.max(height) <= edge {
        return texture.clone();
    }
    let format = gdk::MemoryFormat::R8g8b8a8Premultiplied;
    let mut downloader = gdk::TextureDownloader::new(texture);
    downloader.set_format(format);
    let (pixels, stride) = downloader.download_bytes();
    let (small, width, height) = shrink(&pixels, width, height, stride, edge);
    gdk::MemoryTexture::new(
        width as i32,
        height as i32,
        format,
        &glib::Bytes::from_owned(small),
        width * 4,
    )
    .upcast()
}

/// A photo file read on a blocking thread for the page: its `data:` URI,
/// or the bytes of a format only glycin decodes, still to shrink.
enum Page {
    Uri(Option<String>),
    Elsewhere(glib::Bytes),
}

/// Reads photo files as `data:` URIs. Like the textures, a photo larger
/// than a face is shrunk to a PNG whatever its format, so none grows past
/// what the shelf keeps.
fn read_uris(files: Vec<(String, PathBuf)>) -> LocalBoxFuture<'static, Answers<Rc<str>>> {
    Box::pin(async move {
        let asked: Vec<String> = files.iter().map(|(address, _)| address.clone()).collect();
        let Ok(read) = gio::spawn_blocking(move || {
            files
                .into_iter()
                .map(|(address, path)| (address, page_at(&path)))
                .collect::<Vec<_>>()
        })
        .await
        else {
            return asked
                .into_iter()
                .map(|address| (address, Reply::Failed))
                .collect();
        };
        let mut answers = Vec::with_capacity(read.len());
        for (address, page) in read {
            let reply = match page {
                Page::Uri(uri) => Reply::of(uri.map(Rc::from)),
                Page::Elsewhere(bytes) => {
                    let size = bytes.len();
                    match texture::off_thread(texture::WAIT, move || {
                        let small = gdk::Texture::from_bytes(&bytes)
                            .ok()
                            .and_then(|t| shrunk_uri(&t, size));
                        small.or_else(|| Some(uri_of("image/jpeg", &bytes)))
                    })
                    .await
                    {
                        Some(uri) => Reply::of(uri.map(Rc::from)),
                        None => Reply::Failed,
                    }
                }
            };
            answers.push((address, reply));
        }
        answers
    })
}

/// The photo at `path` for the page. A file this small, or one the shrunk
/// copy would not make smaller, goes in as it is; a large PNG or JPEG goes
/// in shrunk, as a PNG; any other format is left for glycin.
fn page_at(path: &Path) -> Page {
    let Ok(bytes) = std::fs::read(path) else {
        return Page::Uri(None);
    };
    // The page would decode it in WebKit's process, which runs out of
    // memory the same way.
    if texture::decoded_in_process(&bytes) && !fits(&bytes) {
        tracing::warn!(path = %path.display(), "a contact photo says it is too large to decode");
        return Page::Uri(None);
    }
    if bytes.len() <= SMALL_FILE {
        return Page::Uri(Some(uri_of("image/jpeg", &bytes)));
    }
    if !texture::decoded_in_process(&bytes) {
        return Page::Elsewhere(glib::Bytes::from_owned(bytes));
    }
    let small = gdk::Texture::from_bytes(&glib::Bytes::from(&bytes))
        .ok()
        .and_then(|t| shrunk_uri(&t, bytes.len()));
    Page::Uri(Some(small.unwrap_or_else(|| uri_of("image/jpeg", &bytes))))
}

/// `texture` shrunk to the edge, as a PNG `data:` URI, when that is
/// smaller than the `original` bytes it came from.
fn shrunk_uri(texture: &gdk::Texture, original: usize) -> Option<String> {
    let png = shrunk(texture).save_to_png_bytes();
    (png.len() < original).then(|| uri_of("image/png", &png))
}

fn uri_of(mime: &str, data: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
    format!("data:{mime};base64,{encoded}")
}

/// What a read found for one address.
pub(crate) enum Reply<V> {
    /// The photo.
    Photo(V),
    /// No photo: the file is gone, or no decoder takes it. Not asked again.
    Absent,
    /// The read did not finish, on a slow disk or a stuck decoder. Asked
    /// again later.
    Failed,
}

impl<V> Reply<V> {
    fn of(photo: Option<V>) -> Reply<V> {
        photo.map_or(Reply::Absent, Reply::Photo)
    }
}

/// What a background read brings back: each address asked for, with what
/// was found.
type Answers<V> = Vec<(String, Reply<V>)>;

/// How long after a failed read the first retry waits; each further
/// failure doubles it, up to [`RETRY_MOST`].
const RETRY_AFTER: Duration = Duration::from_secs(5);
const RETRY_MOST: Duration = Duration::from_secs(5 * 60);

/// Reads a batch of photo files off the GTK thread.
type Reader<V> = Rc<dyn Fn(Vec<(String, PathBuf)>) -> LocalBoxFuture<'static, Answers<V>>>;

/// Told which addresses' photos just arrived. Answers whether it still
/// wants to hear.
type Listener = Box<dyn Fn(&[String]) -> bool>;

/// One kind of photo, read on first ask.
pub(crate) struct Shelf<V> {
    state: Rc<RefCell<State<V>>>,
    read: Reader<V>,
}

struct State<V> {
    files: Rc<Files>,
    kept: Recent<String, V>,
    missing: HashSet<String>,
    /// Photos whose read failed: when to try again, and how many times
    /// it failed.
    failed: HashMap<String, (Instant, u32)>,
    retry_after: Duration,
    queued: Vec<(String, PathBuf)>,
    reading: HashSet<String>,
    generation: u64,
    listeners: Vec<Listener>,
}

impl<V: Weighed + 'static> Shelf<V> {
    /// A shelf keeping up to `limit` bytes of photos, which `read` brings
    /// in from disk a batch at a time.
    pub(crate) fn new(
        limit: usize,
        read: impl Fn(Vec<(String, PathBuf)>) -> LocalBoxFuture<'static, Answers<V>> + 'static,
    ) -> Shelf<V> {
        Shelf {
            state: Rc::new(RefCell::new(State {
                files: Rc::default(),
                kept: Recent::new(limit),
                missing: HashSet::new(),
                failed: HashMap::new(),
                retry_after: RETRY_AFTER,
                queued: Vec::new(),
                reading: HashSet::new(),
                generation: 0,
                listeners: Vec::new(),
            })),
            read: Rc::new(read),
        }
    }

    /// The photo of `address`, when it is in memory. A photo on disk and
    /// not in memory is read in the background, and the listeners hear
    /// when it arrives; asking again before then reads nothing more.
    pub(crate) fn get(&self, address: &str) -> Option<V> {
        let key = address.trim().to_lowercase();
        let mut state = self.state.borrow_mut();
        if let Some(photo) = state.kept.get(&key) {
            return Some(photo);
        }
        if state.missing.contains(&key) || state.reading.contains(&key) {
            return None;
        }
        if state
            .failed
            .get(&key)
            .is_some_and(|(retry, _)| Instant::now() < *retry)
        {
            return None;
        }
        let path = state.files.get(&key)?.clone();
        state.reading.insert(key.clone());
        state.queued.push((key, path));
        // The first miss of a turn of the main loop starts the read; the
        // misses after it in the same turn, such as the other rows a list
        // binds, ride along in the same batch.
        if state.queued.len() == 1 {
            let (state, read) = (Rc::downgrade(&self.state), Rc::clone(&self.read));
            glib::spawn_future_local(read_queued(state, read));
        }
        None
    }

    /// The photo files on disk now. Everything read from the files before
    /// is let go, and a read under way is dropped when it arrives.
    pub(crate) fn set_files(&self, files: Rc<Files>) {
        let mut state = self.state.borrow_mut();
        state.files = files;
        state.kept.clear();
        state.missing.clear();
        state.failed.clear();
        state.queued.clear();
        state.reading.clear();
        state.generation += 1;
    }

    /// Calls `listener` with the addresses whose photos arrived, after
    /// each read, for as long as it answers `true`.
    pub(crate) fn on_arrival(&self, listener: impl Fn(&[String]) -> bool + 'static) {
        self.state.borrow_mut().listeners.push(Box::new(listener));
    }

    /// Whether any contact has a photo on disk.
    pub(crate) fn any(&self) -> bool {
        !self.state.borrow().files.is_empty()
    }

    /// The photo file of `address`, when there is one.
    pub(crate) fn path(&self, address: &str) -> Option<PathBuf> {
        let key = address.trim().to_lowercase();
        self.state.borrow().files.get(&key).cloned()
    }
}

/// Four-byte `pixels`, `width` by `height` with rows `stride` bytes apart,
/// cut down so the longer side is at most `edge`, each new pixel the mean
/// of the ones it covers. Answers the pixels, packed, and their size.
fn shrink(
    pixels: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    edge: usize,
) -> (Vec<u8>, usize, usize) {
    let longest = width.max(height);
    let (to_width, to_height) = match longest > edge {
        true => (
            (width * edge / longest).max(1),
            (height * edge / longest).max(1),
        ),
        false => (width, height),
    };
    let mut out = Vec::with_capacity(to_width * to_height * 4);
    for y in 0..to_height {
        let rows =
            y * height / to_height..((y + 1) * height / to_height).max(y * height / to_height + 1);
        for x in 0..to_width {
            let columns =
                x * width / to_width..((x + 1) * width / to_width).max(x * width / to_width + 1);
            let mut sum = [0u32; 4];
            for row in rows.clone() {
                for column in columns.clone() {
                    let at = row * stride + column * 4;
                    for (total, byte) in sum.iter_mut().zip(&pixels[at..at + 4]) {
                        *total += u32::from(*byte);
                    }
                }
            }
            let count = (rows.len() * columns.len()) as u32;
            out.extend(sum.map(|total| (total / count) as u8));
        }
    }
    (out, to_width, to_height)
}

/// Reads what is queued on `state`, keeps it, and tells the listeners.
async fn read_queued<V: Weighed + 'static>(
    state: std::rc::Weak<RefCell<State<V>>>,
    read: Reader<V>,
) {
    let Some((batch, generation)) = state.upgrade().map(|state| {
        let mut state = state.borrow_mut();
        (std::mem::take(&mut state.queued), state.generation)
    }) else {
        return;
    };
    if batch.is_empty() {
        return;
    }
    let asked: Vec<String> = batch.iter().map(|(key, _)| key.clone()).collect();
    let answers = read(batch).await;
    let Some(state) = state.upgrade() else {
        return;
    };
    let listeners = {
        let mut state = state.borrow_mut();
        if state.generation != generation {
            return;
        }
        for key in &asked {
            state.reading.remove(key);
        }
        let mut answered: HashMap<String, Reply<V>> = answers.into_iter().collect();
        let mut arrived = Vec::new();
        for key in asked {
            match answered.remove(&key).unwrap_or(Reply::Failed) {
                // A photo larger than the whole bound would be read again
                // on every ask, so it counts as missing.
                Reply::Photo(photo) if state.kept.fits(&photo) => {
                    state.failed.remove(&key);
                    state.kept.put(key.clone(), photo);
                    arrived.push(key);
                }
                Reply::Photo(_) | Reply::Absent => {
                    state.failed.remove(&key);
                    state.missing.insert(key);
                }
                Reply::Failed => {
                    // Left unmarked, so the next ask reads it again, but
                    // not at once: a stuck decoder would be hammered.
                    let failures = state.failed.get(&key).map_or(0, |(_, n)| *n) + 1;
                    let wait = state
                        .retry_after
                        .saturating_mul(1 << (failures - 1).min(16))
                        .min(RETRY_MOST.max(state.retry_after));
                    state.failed.insert(key, (Instant::now() + wait, failures));
                }
            }
        }
        // A large batch can push out its own first photos; only those
        // still kept are worth drawing again.
        arrived.retain(|key| state.kept.holds(key));
        if arrived.is_empty() {
            return;
        }
        (std::mem::take(&mut state.listeners), arrived)
    };
    // The listeners ask for photos while they hear, so the state is not
    // borrowed while they run.
    let (listeners, arrived) = listeners;
    let kept: Vec<Listener> = listeners
        .into_iter()
        .filter(|listener| listener(&arrived))
        .collect();
    let mut state = state.borrow_mut();
    let added = std::mem::replace(&mut state.listeners, kept);
    state.listeners.extend(added);
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use gtk::glib;

    use super::*;

    /// A fake disk: what each file holds, and which files were read.
    #[derive(Default)]
    struct Disk {
        holds: RefCell<HashMap<PathBuf, Rc<str>>>,
        reads: RefCell<Vec<PathBuf>>,
        /// Reads fail to finish while this is set.
        stuck: Cell<bool>,
    }

    impl Disk {
        fn shelf(self: &Rc<Self>, limit: usize) -> Shelf<Rc<str>> {
            let disk = Rc::clone(self);
            Shelf::new(limit, move |files| {
                let answers = files
                    .into_iter()
                    .map(|(address, path)| {
                        disk.reads.borrow_mut().push(path.clone());
                        let held = disk.holds.borrow().get(&path).cloned();
                        match disk.stuck.get() {
                            true => (address, Reply::Failed),
                            false => (address, Reply::of(held)),
                        }
                    })
                    .collect();
                Box::pin(async move { answers })
            })
        }

        fn write(&self, path: &str, photo: &str) {
            self.holds.borrow_mut().insert(path.into(), photo.into());
        }

        fn reads(&self) -> usize {
            self.reads.borrow().len()
        }
    }

    fn files(pairs: &[(&str, &str)]) -> Rc<Files> {
        Rc::new(
            pairs
                .iter()
                .map(|(address, path)| (address.to_string(), PathBuf::from(path)))
                .collect(),
        )
    }

    /// Runs `body` on a main context of its own, then lets every read it
    /// started finish.
    fn on_main_context(body: impl FnOnce(&dyn Fn())) {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let settle = || while context.iteration(false) {};
                body(&settle);
            })
            .expect("the context is free");
    }

    /// A small file can declare a picture of billions of pixels. Its
    /// header is enough to refuse it, before anything decodes it.
    #[test]
    fn a_photo_declaring_too_many_pixels_is_not_decoded() {
        let dir = tempfile::tempdir().unwrap();
        let mut bomb = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bomb.extend_from_slice(&[0, 1, 0, 0, 0, 1, 0, 0, 8, 6, 0, 0, 0]);
        let path = dir.path().join("bomb.png");
        std::fs::write(&path, &bomb).unwrap();
        assert!(matches!(face_at(&path), Face::Decoded(None)));
        assert!(matches!(page_at(&path), Page::Uri(None)));
        assert!(!fits(&bomb));
        let mut face = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        face.extend_from_slice(&[0, 0, 1, 0, 0, 0, 1, 0]);
        assert!(fits(&face), "a 256 pixel photo is fine");
    }

    #[test]
    fn the_first_ask_reads_the_file_once_and_the_next_one_is_answered_from_memory() {
        on_main_context(|settle| {
            let disk = Rc::new(Disk::default());
            disk.write("ann.jpg", "ANN");
            let shelf = disk.shelf(1_000);
            shelf.set_files(files(&[("ann@example.com", "ann.jpg")]));
            let arrived = Rc::new(RefCell::new(Vec::new()));
            let heard = Rc::clone(&arrived);
            shelf.on_arrival(move |addresses| {
                heard.borrow_mut().extend_from_slice(addresses);
                true
            });

            assert_eq!(shelf.get(" Ann@Example.com"), None, "not read yet");
            assert_eq!(shelf.get("ann@example.com"), None, "still reading");
            settle();
            assert_eq!(*arrived.borrow(), ["ann@example.com"]);
            assert_eq!(shelf.get("ann@example.com").as_deref(), Some("ANN"));
            assert_eq!(disk.reads(), 1);
        });
    }

    #[test]
    fn an_address_with_no_photo_on_disk_reads_nothing() {
        on_main_context(|settle| {
            let disk = Rc::new(Disk::default());
            let shelf = disk.shelf(1_000);
            shelf.set_files(files(&[("ann@example.com", "ann.jpg")]));
            assert_eq!(shelf.get("bo@example.com"), None);
            settle();
            assert_eq!(disk.reads(), 0);
        });
    }

    #[test]
    fn a_photo_that_would_not_read_is_remembered_as_missing() {
        on_main_context(|settle| {
            let disk = Rc::new(Disk::default());
            let shelf = disk.shelf(1_000);
            shelf.set_files(files(&[("ann@example.com", "gone.jpg")]));
            let told = Rc::new(Cell::new(false));
            let heard = Rc::clone(&told);
            shelf.on_arrival(move |_| {
                heard.set(true);
                true
            });
            assert_eq!(shelf.get("ann@example.com"), None);
            settle();
            assert_eq!(shelf.get("ann@example.com"), None);
            settle();
            assert_eq!(disk.reads(), 1, "asked for once");
            assert!(!told.get(), "nothing arrived");
        });
    }

    #[test]
    fn a_read_that_did_not_finish_is_tried_again_not_marked_missing() {
        on_main_context(|settle| {
            let disk = Rc::new(Disk::default());
            disk.write("ann.jpg", "ANN");
            let shelf = disk.shelf(1_000);
            shelf.state.borrow_mut().retry_after = Duration::ZERO;
            shelf.set_files(files(&[("ann@example.com", "ann.jpg")]));
            disk.stuck.set(true);
            assert_eq!(shelf.get("ann@example.com"), None);
            settle();
            disk.stuck.set(false);
            assert_eq!(shelf.get("ann@example.com"), None, "read again");
            settle();
            assert_eq!(shelf.get("ann@example.com").as_deref(), Some("ANN"));
            assert_eq!(disk.reads(), 2);
        });
    }

    #[test]
    fn a_failed_read_is_not_repeated_on_every_ask() {
        on_main_context(|settle| {
            let disk = Rc::new(Disk::default());
            disk.write("ann.jpg", "ANN");
            let shelf = disk.shelf(1_000);
            shelf.set_files(files(&[("ann@example.com", "ann.jpg")]));
            disk.stuck.set(true);
            shelf.get("ann@example.com");
            settle();
            for _ in 0..5 {
                shelf.get("ann@example.com");
                settle();
            }
            assert_eq!(disk.reads(), 1, "waits before the next try");
        });
    }

    #[test]
    fn a_photo_in_a_format_only_glycin_reads_still_gets_a_small_page_uri() {
        // A 600 by 600 picture, as if a large webp had decoded to it.
        let pixels: Vec<u8> = (0..600 * 600 * 4).map(|n| (n * 7 % 251) as u8).collect();
        let texture: gdk::Texture = gdk::MemoryTexture::new(
            600,
            600,
            gdk::MemoryFormat::R8g8b8a8Premultiplied,
            &glib::Bytes::from_owned(pixels),
            600 * 4,
        )
        .upcast();
        let uri = shrunk_uri(&texture, 3 * 1024 * 1024).expect("smaller than the original");
        assert!(uri.starts_with("data:image/png;base64,"));
        assert!(uri.len() < URI_BYTES / 8, "{} bytes", uri.len());
        assert_eq!(
            shrunk_uri(&texture, 10),
            None,
            "never larger than the original"
        );
    }

    #[test]
    fn the_least_recent_photo_goes_once_the_bytes_run_out() {
        on_main_context(|settle| {
            let disk = Rc::new(Disk::default());
            disk.write("ann.jpg", "AAAAA");
            disk.write("bo.jpg", "BBBBB");
            let shelf = disk.shelf(8);
            shelf.set_files(files(&[
                ("ann@example.com", "ann.jpg"),
                ("bo@example.com", "bo.jpg"),
            ]));
            shelf.get("ann@example.com");
            settle();
            shelf.get("bo@example.com");
            settle();
            assert!(shelf.get("bo@example.com").is_some());
            assert_eq!(shelf.get("ann@example.com"), None, "let go for Bo's");
            settle();
            assert_eq!(disk.reads(), 3, "Ann's is read again");
            assert!(shelf.get("ann@example.com").is_some());
        });
    }

    #[test]
    fn a_photo_larger_than_the_bound_is_left_out_rather_than_read_forever() {
        on_main_context(|settle| {
            let disk = Rc::new(Disk::default());
            disk.write("ann.jpg", "AAAAAAAAAA");
            let shelf = disk.shelf(4);
            shelf.set_files(files(&[("ann@example.com", "ann.jpg")]));
            shelf.get("ann@example.com");
            settle();
            assert_eq!(shelf.get("ann@example.com"), None);
            settle();
            assert_eq!(disk.reads(), 1);
        });
    }

    #[test]
    fn new_files_forget_what_was_read_and_drop_a_read_under_way() {
        on_main_context(|settle| {
            let disk = Rc::new(Disk::default());
            disk.write("ann.jpg", "OLD");
            disk.write("ann2.jpg", "NEW");
            let shelf = disk.shelf(1_000);
            shelf.set_files(files(&[("ann@example.com", "ann.jpg")]));
            shelf.get("ann@example.com");
            shelf.set_files(files(&[("ann@example.com", "ann2.jpg")]));
            settle();
            assert_eq!(
                shelf.get("ann@example.com"),
                None,
                "the old read is dropped"
            );
            settle();
            assert_eq!(shelf.get("ann@example.com").as_deref(), Some("NEW"));
        });
    }

    #[test]
    fn a_large_photo_shrinks_to_the_edge_averaging_the_pixels_it_covers() {
        // Four pixels by two, with a stride padded to 20 bytes.
        let mut pixels = vec![0u8; 20 * 2];
        for (x, value) in [(0, 0), (1, 100), (2, 200), (3, 40)] {
            for y in 0..2 {
                pixels[y * 20 + x * 4..y * 20 + x * 4 + 4].fill(value);
            }
        }
        let (small, width, height) = shrink(&pixels, 4, 2, 20, 2);
        assert_eq!((width, height), (2, 1));
        assert_eq!(small, [50, 50, 50, 50, 120, 120, 120, 120]);
    }

    #[test]
    fn a_photo_within_the_edge_keeps_its_size() {
        let pixels = vec![7u8; 2 * 2 * 4];
        let (small, width, height) = shrink(&pixels, 2, 2, 8, 4);
        assert_eq!((width, height), (2, 2));
        assert_eq!(small, pixels);
    }

    #[test]
    fn a_listener_may_ask_again_while_it_is_told() {
        on_main_context(|settle| {
            let disk = Rc::new(Disk::default());
            disk.write("ann.jpg", "ANN");
            let shelf = Rc::new(disk.shelf(1_000));
            shelf.set_files(files(&[("ann@example.com", "ann.jpg")]));
            let seen = Rc::new(RefCell::new(None));
            let (asks, saw) = (Rc::downgrade(&shelf), Rc::clone(&seen));
            shelf.on_arrival(move |addresses| {
                let shelf = asks.upgrade().expect("the shelf is alive");
                *saw.borrow_mut() = shelf.get(&addresses[0]);
                false
            });
            shelf.get("ann@example.com");
            settle();
            assert_eq!(seen.borrow().as_deref(), Some("ANN"));
        });
    }
}
