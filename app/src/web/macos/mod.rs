//! The macOS adapter: a WKWebView over an empty GTK widget.
//!
//! GTK 4 cannot hold a native view in its widget tree, so the page is a
//! subview of the window's content view, and [`place`] moves it over the
//! anchor, a focusable `gtk::Box` that takes the page's room in the layout,
//! on every frame. What GTK draws inside the window over the page, a
//! toast or the event card, shows through a hole cut in the page's layer;
//! while a dialog is up, the page gives way to a picture of itself so the
//! dialog and its backdrop draw on top. [`events`] passes the page the
//! keys and scrolls GTK would otherwise claim on their way to it.

mod bridge;
#[cfg(debug_assertions)]
mod checks;
#[cfg(debug_assertions)]
pub use checks::check_macos;
mod events;
mod find;
mod place;

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use block2::RcBlock;
use gtk::{gdk, gio};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{NSColor, NSPrintInfo, NSView};
use objc2_foundation::{
    NSData, NSError, NSNumber, NSObjectNSKeyValueCoding, NSString, NSURL, NSURLErrorDomain,
    NSURLResponse, ns_string,
};
use objc2_web_kit::{
    WKContentRuleList, WKContentRuleListStore, WKURLSchemeTask, WKUserScript,
    WKUserScriptInjectionTime, WKWebViewConfiguration, WKWebsiteDataStore,
};

use bridge::{Bridge, PageView};
pub use events::prepare;
pub use find::Finder;
use place::Float;

/// The CSS class on every anchor, so [`holds_focus`] can tell one.
const ANCHOR_CLASS: &str = "web-view";

/// One page, and the handle every clone of it shares.
#[derive(Clone)]
pub struct WebView(Rc<Inner>);

/// A handle that does not keep the page alive.
pub struct WeakWebView(Weak<Inner>);

impl WeakWebView {
    pub fn upgrade(&self) -> Option<WebView> {
        self.0.upgrade().map(WebView)
    }
}

type Navigate = Rc<dyn Fn(&str)>;

pub(super) struct Inner {
    view: Retained<PageView>,
    /// Holds the page and clips it to what of the anchor is on screen,
    /// such as the part of a forwarded message inside the composer's
    /// scrolled window.
    clip: Retained<NSView>,
    bridge: Retained<Bridge>,
    anchor: gtk::Box,
    /// Stands in for the page while a dialog is up.
    picture: gtk::Picture,
    float: Float,
    navigate: RefCell<Option<Navigate>>,
    loaded: RefCell<Vec<Rc<dyn Fn()>>>,
    crashed: RefCell<Vec<Rc<dyn Fn()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.clip.removeFromSuperview();
    }
}

impl Inner {
    fn navigated(&self, uri: &str) {
        let navigate = self.navigate.borrow().clone();
        if let Some(navigate) = navigate {
            navigate(uri);
        }
    }

    fn loaded(&self) {
        let loaded = self.loaded.borrow().clone();
        for loaded in loaded {
            loaded();
        }
    }

    fn crashed(&self) {
        let crashed = self.crashed.borrow().clone();
        for crashed in crashed {
            crashed();
        }
    }
}

fn main_thread() -> MainThreadMarker {
    MainThreadMarker::new().expect("web views live on the GTK thread")
}

impl WebView {
    /// The conversation's page. Page JavaScript is off, so email cannot
    /// run scripts, while the app's own scripts run through
    /// [`WebView::add_script`] and [`WebView::run`]. Pictures the page asks
    /// for at an address of `scheme` go to `serve`.
    pub fn reading(scheme: &'static str, serve: impl Fn(Request) + 'static) -> WebView {
        let bridge = Bridge::new(main_thread());
        *bridge.ivars().serve.borrow_mut() = Some(Rc::new(serve));
        let config = configuration();
        unsafe {
            config.setURLSchemeHandler_forURLScheme(
                Some(ProtocolObject::from_ref(&*bridge)),
                &NSString::from_str(scheme),
            );
        }
        let page = WebView::build(&config, bridge);
        page.0.anchor.set_hexpand(true);
        page
    }

    /// A page for the composer's Preview and forwarded message. It runs no
    /// script and never navigates: a click on a web link opens it in the
    /// browser, and anything else is ignored. The page itself refuses
    /// remote loads (`compose::preview_page`).
    pub fn sealed() -> WebView {
        let page = WebView::build(&configuration(), Bridge::new(main_thread()));
        let weak = page.downgrade();
        page.on_navigate(move |uri| {
            if uri.starts_with("https://") || uri.starts_with("http://") {
                let window = weak
                    .upgrade()
                    .and_then(|page| page.0.anchor.root())
                    .and_downcast::<gtk::Window>();
                gtk::UriLauncher::new(uri).launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
            }
        });
        page
    }

    fn build(config: &WKWebViewConfiguration, bridge: Retained<Bridge>) -> WebView {
        let mtm = main_thread();
        let view = PageView::new(mtm, config);
        unsafe {
            view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*bridge)));
            // A force click would fetch a link's page to preview it, and
            // mail never reaches the network on its own.
            view.setAllowsLinkPreview(false);
            // Until the page draws, the colour behind it is the window's,
            // not white.
            view.setValue_forKey(Some(&NSNumber::new_bool(false)), ns_string!("drawsBackground"));
        }
        let clip = NSView::new(mtm);
        clip.setWantsLayer(true);
        if let Some(layer) = clip.layer() {
            layer.setMasksToBounds(true);
        }
        clip.addSubview(&view);
        clip.setHidden(true);

        let anchor = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(true)
            .focusable(true)
            .css_classes([ANCHOR_CLASS])
            .build();
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Fill)
            .hexpand(true)
            .vexpand(true)
            .visible(false)
            .build();
        anchor.append(&picture);

        let inner = Rc::new(Inner {
            view,
            clip,
            bridge,
            anchor,
            picture,
            float: Float::default(),
            navigate: RefCell::new(None),
            loaded: RefCell::new(Vec::new()),
            crashed: RefCell::new(Vec::new()),
        });
        *inner.bridge.ivars().owner.borrow_mut() = Rc::downgrade(&inner);
        place::follow(&inner);
        events::register(&inner);
        WebView(inner)
    }

    /// The widget that takes the page's place in a window.
    pub fn widget(&self) -> gtk::Widget {
        self.0.anchor.clone().upcast()
    }

    pub fn downgrade(&self) -> WeakWebView {
        WeakWebView(Rc::downgrade(&self.0))
    }

    fn content(&self) -> Retained<objc2_web_kit::WKUserContentController> {
        unsafe { self.0.view.configuration().userContentController() }
    }

    /// A script of the app's own that WebKit runs in the top frame once
    /// each document is parsed.
    pub fn add_script(&self, source: &str) {
        let script = unsafe {
            WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
                WKUserScript::alloc(main_thread()),
                &NSString::from_str(source),
                WKUserScriptInjectionTime::AtDocumentEnd,
                true,
            )
        };
        unsafe { self.content().addUserScript(&script) };
    }

    /// Hears what the page posts to `window.webkit.messageHandlers[name]`,
    /// as a string.
    pub fn on_message(&self, name: &str, heard: impl Fn(&str) + 'static) {
        self.0
            .bridge
            .ivars()
            .heard
            .borrow_mut()
            .insert(name.to_string(), Rc::new(heard));
        unsafe {
            self.content().addScriptMessageHandler_name(
                ProtocolObject::from_ref(&*self.0.bridge),
                &NSString::from_str(name),
            );
        }
    }

    /// Refuses every navigation but the page's own `about:blank` and hands
    /// the address to `asked`.
    pub fn on_navigate(&self, asked: impl Fn(&str) + 'static) {
        *self.0.navigate.borrow_mut() = Some(Rc::new(asked));
    }

    /// Runs `loaded` when a document and everything it asked for is in.
    pub fn on_loaded(&self, loaded: impl Fn() + 'static) {
        self.0.loaded.borrow_mut().push(Rc::new(loaded));
    }

    /// Runs `gone` when the process that drew the page ended.
    pub fn on_crashed(&self, gone: impl Fn() + 'static) {
        self.0.crashed.borrow_mut().push(Rc::new(gone));
    }

    pub fn load_html(&self, html: &str) {
        unsafe {
            self.0
                .view
                .loadHTMLString_baseURL(&NSString::from_str(html), None)
        };
    }

    /// Runs one of the app's scripts and forgets its answer.
    pub fn run(&self, script: &str) {
        self.eval(script, |_| {});
    }

    /// Runs one of the app's scripts and hands `done` its answer as a
    /// string.
    pub fn eval(&self, script: &str, done: impl FnOnce(Result<String, String>) + 'static) {
        let done = Cell::new(Some(done));
        let block = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
            let Some(done) = done.take() else { return };
            let answer = match unsafe { error.as_ref() } {
                Some(error) => Err(error.localizedDescription().to_string()),
                None => Ok(unsafe { value.as_ref() }
                    .map(bridge::as_text)
                    .unwrap_or_default()),
            };
            done(answer);
        });
        unsafe {
            self.0
                .view
                .evaluateJavaScript_completionHandler(&NSString::from_str(script), Some(&block));
        }
    }

    /// How many of the widget's pixels one CSS pixel takes.
    pub fn zoom(&self) -> f64 {
        unsafe { self.0.view.pageZoom() }
    }

    pub fn set_zoom(&self, zoom: f64) {
        unsafe { self.0.view.setPageZoom(zoom) };
    }

    /// The colour behind the page, seen past its ends.
    pub fn set_background(&self, color: &gdk::RGBA) {
        let color = NSColor::colorWithSRGBRed_green_blue_alpha(
            f64::from(color.red()),
            f64::from(color.green()),
            f64::from(color.blue()),
            f64::from(color.alpha()),
        );
        unsafe { self.0.view.setUnderPageBackgroundColor(Some(&color)) };
    }

    /// Opens the print panel for the page, as a sheet on its window.
    pub fn print(&self, _window: Option<&gtk::Window>) {
        let info = NSPrintInfo::sharedPrintInfo();
        let operation = unsafe { self.0.view.printOperationWithPrintInfo(&info) };
        // WebKit lays the page out to the print view's frame, which starts
        // empty and prints blank pages.
        if let Some(view) = operation.view() {
            let paper = info.paperSize();
            view.setFrame(objc2_foundation::NSRect::new(
                objc2_foundation::NSPoint::new(0.0, 0.0),
                paper,
            ));
        }
        match self.0.view.window() {
            Some(window) => unsafe {
                operation.runOperationModalForWindow_delegate_didRunSelector_contextInfo(
                    &window,
                    None,
                    None,
                    std::ptr::null_mut(),
                )
            },
            None => {
                operation.runOperation();
            }
        }
    }

    /// Lets go of what the page holds. WebKit offers no way to end the
    /// process behind one page, so the page is emptied instead and the
    /// process idles until the next load.
    pub fn stop(&self) {
        self.load_html("");
    }

    /// Blocks remote content with `filter`, or lets it through with none.
    pub fn set_filter(&self, filter: Option<&Filter>) {
        let content = self.content();
        unsafe {
            content.removeAllContentRuleLists();
            if let Some(filter) = filter {
                content.addContentRuleList(&filter.0);
            }
        }
    }

    pub fn grab_focus(&self) -> bool {
        self.0.anchor.grab_focus()
    }

    /// Whether the keyboard is in the page.
    pub fn has_focus(&self) -> bool {
        self.0
            .anchor
            .state_flags()
            .contains(gtk::StateFlags::FOCUS_WITHIN)
    }

    /// Says `widget` is drawn over the page, as the event card is. The
    /// page cuts a hole for it wherever it sits.
    pub fn cover(&self, widget: &impl IsA<gtk::Widget>) {
        self.0
            .float
            .covers
            .borrow_mut()
            .push(widget.as_ref().downgrade());
    }

    pub fn finder(&self) -> Finder {
        Finder::new(self.downgrade())
    }
}

/// Whether `widget` is a page, or something inside one, so the keys
/// reach it rather than the window's shortcuts.
pub fn holds_focus(widget: &gtk::Widget) -> bool {
    let mut at = Some(widget.clone());
    while let Some(widget) = at {
        if widget.has_css_class(ANCHOR_CLASS) {
            return true;
        }
        at = widget.parent();
    }
    false
}

/// What every page starts from: one ephemeral data store shared by all,
/// so nothing a page touches lands on disk and a detached window needs no
/// second network process, and page scripts off.
fn configuration() -> Retained<WKWebViewConfiguration> {
    thread_local! {
        static STORE: Retained<WKWebsiteDataStore> =
            unsafe { WKWebsiteDataStore::nonPersistentDataStore(main_thread()) };
    }
    let mtm = main_thread();
    let config = unsafe { WKWebViewConfiguration::new(mtm) };
    STORE.with(|store| unsafe { config.setWebsiteDataStore(store) });
    unsafe {
        // The app's own scripts still run: user scripts and
        // evaluateJavaScript are not the page's.
        config
            .defaultWebpagePreferences()
            .setAllowsContentJavaScript(false);
        let preferences = config.preferences();
        preferences.setJavaScriptCanOpenWindowsAutomatically(false);
        preferences.setElementFullscreenEnabled(false);
        config.setMediaTypesRequiringUserActionForPlayback(
            objc2_web_kit::WKAudiovisualMediaTypes::All,
        );
    }
    config
}

/// The page's request for one address of the app's scheme. It can be
/// held and answered later.
#[derive(Clone)]
pub struct Request(Rc<RequestInner>);

struct RequestInner {
    task: Retained<ProtocolObject<dyn WKURLSchemeTask>>,
    /// Set when WebKit gave up on the task, or once it is answered.
    stopped: Rc<Cell<bool>>,
    bridge: Retained<Bridge>,
}

impl Drop for RequestInner {
    fn drop(&mut self) {
        self.bridge.done(&self.stopped);
    }
}

impl Request {
    fn new(
        task: Retained<ProtocolObject<dyn WKURLSchemeTask>>,
        stopped: Rc<Cell<bool>>,
        bridge: Retained<Bridge>,
    ) -> Request {
        Request(Rc::new(RequestInner {
            task,
            stopped,
            bridge,
        }))
    }

    pub fn uri(&self) -> Option<String> {
        unsafe { self.0.task.request() }
            .URL()
            .and_then(|url| url.absoluteString())
            .map(|uri| uri.to_string())
    }

    /// Takes the request off WebKit's list, or says it is gone already.
    fn take(&self) -> bool {
        if self.0.stopped.replace(true) {
            return false;
        }
        self.0.bridge.done(&self.0.stopped);
        true
    }

    /// Gives the page what it asked for.
    pub fn finish(&self, bytes: impl AsRef<[u8]> + Send + 'static, mime: &str) {
        let Some(url) = unsafe { self.0.task.request() }.URL() else {
            return self.refuse();
        };
        if !self.take() {
            return;
        }
        let bytes = bytes.as_ref();
        unsafe {
            let response =
                NSURLResponse::initWithURL_MIMEType_expectedContentLength_textEncodingName(
                    NSURLResponse::alloc(),
                    &url,
                    Some(&NSString::from_str(mime)),
                    bytes.len() as isize,
                    None,
                );
            self.0.task.didReceiveResponse(&response);
            self.0.task.didReceiveData(&NSData::with_bytes(bytes));
            self.0.task.didFinish();
        }
    }

    /// Says there is nothing at that address.
    pub fn refuse(&self) {
        if !self.take() {
            return;
        }
        // NSURLErrorFileDoesNotExist.
        let error = unsafe { NSError::new(-1100, NSURLErrorDomain) };
        unsafe { self.0.task.didFailWithError(&error) };
    }
}

/// The compiled rules that block remote content, shared by every page.
#[derive(Clone)]
pub struct Filter(pub(crate) Retained<WKContentRuleList>);

/// Compiles `rules`, WebKit's content blocker JSON, into a filter kept in
/// `dir` under `id`.
pub async fn compile_filter(dir: &Path, id: &str, rules: &'static str) -> Result<Filter, String> {
    let (tell, hear) = futures::channel::oneshot::channel();
    let tell = Cell::new(Some(tell));
    let done = RcBlock::new(move |list: *mut WKContentRuleList, error: *mut NSError| {
        let answer = match (unsafe { list.as_ref() }, unsafe { error.as_ref() }) {
            (Some(list), _) => Ok(Filter(list.retain())),
            (None, Some(error)) => Err(error.localizedDescription().to_string()),
            (None, None) => Err("WebKit compiled no rules".to_string()),
        };
        if let Some(tell) = tell.take() {
            let _ = tell.send(answer);
        }
    });
    let url = NSURL::fileURLWithPath(&NSString::from_str(&dir.to_string_lossy()));
    let Some(store) = (unsafe { WKContentRuleListStore::storeWithURL(Some(&url), main_thread()) })
    else {
        return Err("WebKit keeps no rule lists there".to_string());
    };
    unsafe {
        store.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
            Some(&NSString::from_str(id)),
            Some(&NSString::from_str(rules)),
            Some(&done),
        );
    }
    hear.await
        .unwrap_or_else(|_| Err("WebKit never answered".to_string()))
}
