//! The Linux adapter: WebKitGTK, whose page is a GTK widget of its own.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use webkit::prelude::*;

/// How many matches WebKit may highlight and count. Nothing is capped.
const MATCH_LIMIT: u32 = u32::MAX;

/// One page, and the handle every clone of it shares.
#[derive(Clone)]
pub struct WebView {
    view: webkit::WebView,
}

/// A handle that does not keep the page alive.
pub struct WeakWebView(glib::WeakRef<webkit::WebView>);

impl WeakWebView {
    pub fn upgrade(&self) -> Option<WebView> {
        self.0.upgrade().map(|view| WebView { view })
    }
}

impl WebView {
    /// The conversation's page. Page JavaScript is off
    /// (`enable-javascript-markup` is false), so email cannot run scripts,
    /// while the app's own scripts run through [`WebView::add_script`] and
    /// [`WebView::run`]. Pictures the page asks for at an address of
    /// `scheme` go to `serve`.
    pub fn reading(scheme: &'static str, serve: impl Fn(Request) + 'static) -> WebView {
        let settings = webkit::Settings::new();
        settings.set_enable_javascript(true);
        settings.set_enable_javascript_markup(false);
        settings.set_javascript_can_open_windows_automatically(false);
        settings.set_enable_developer_extras(false);
        settings.set_enable_html5_local_storage(false);
        settings.set_enable_html5_database(false);
        settings.set_enable_page_cache(false);
        settings.set_enable_media(false);
        settings.set_enable_mediasource(false);
        settings.set_enable_encrypted_media(false);
        settings.set_enable_webaudio(false);
        settings.set_enable_webgl(false);
        settings.set_enable_webrtc(false);
        settings.set_enable_fullscreen(false);
        settings.set_enable_back_forward_navigation_gestures(false);
        settings.set_allow_file_access_from_file_urls(false);
        settings.set_enable_smooth_scrolling(true);
        settings.set_auto_load_images(true);
        let view = webkit::WebView::builder()
            .network_session(&network_session())
            .user_content_manager(&webkit::UserContentManager::new())
            .settings(&settings)
            .build();
        view.set_hexpand(true);
        let page = WebView::wrap(view);
        serve_scheme(scheme, &page.view, Rc::new(serve));
        page.view.connect_context_menu(|webview, menu, _| {
            use webkit::ContextMenuAction as Item;
            for item in menu.items() {
                if !matches!(
                    item.stock_action(),
                    Item::Copy
                        | Item::CopyLinkToClipboard
                        | Item::CopyImageToClipboard
                        | Item::SelectAll
                ) {
                    menu.remove(&item);
                }
            }
            if menu.items().is_empty() {
                return true;
            }
            // WebKit builds this menu as a popover of the web view once
            // the signal returns, and its items carry no names.
            let webview = webview.clone();
            glib::idle_add_local_once(move || crate::ui::name_menu_items_under(&webview));
            false
        });
        page
    }

    /// A page for the composer's Preview and forwarded message. It runs no
    /// script and never navigates: a click on a web link opens it in the
    /// browser, and anything else is ignored. The page itself refuses
    /// remote loads (`compose::preview_page`).
    pub fn sealed() -> WebView {
        let settings = webkit::Settings::new();
        settings.set_enable_javascript(false);
        settings.set_enable_javascript_markup(false);
        let view = webkit::WebView::builder()
            .network_session(&network_session())
            .settings(&settings)
            .build();
        let page = WebView::wrap(view);
        let weak = page.downgrade();
        page.on_navigate(move |uri| {
            if uri.starts_with("https://") || uri.starts_with("http://") {
                let window = weak
                    .upgrade()
                    .and_then(|page| page.view.root())
                    .and_downcast::<gtk::Window>();
                gtk::UriLauncher::new(uri).launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
            }
        });
        page
    }

    fn wrap(view: webkit::WebView) -> WebView {
        view.set_vexpand(true);
        keep_key_text_out_of_tab(&view);
        view.connect_realize(keep_key_text_out_of_tab);
        WebView { view }
    }

    /// The widget that takes the page's place in a window.
    pub fn widget(&self) -> gtk::Widget {
        self.view.clone().upcast()
    }

    pub fn downgrade(&self) -> WeakWebView {
        WeakWebView(self.view.downgrade())
    }

    fn content(&self) -> webkit::UserContentManager {
        self.view
            .user_content_manager()
            .expect("a WebView has a user content manager")
    }

    /// A script of the app's own that WebKit runs in the top frame once
    /// each document is parsed, so it outlives the redraws a thread goes
    /// through.
    pub fn add_script(&self, source: &str) {
        self.content().add_script(&webkit::UserScript::new(
            source,
            webkit::UserContentInjectedFrames::TopFrame,
            webkit::UserScriptInjectionTime::End,
            &[],
            &[],
        ));
    }

    /// Hears what the page posts to `window.webkit.messageHandlers[name]`,
    /// as a string.
    pub fn on_message(&self, name: &str, heard: impl Fn(&str) + 'static) {
        let content = self.content();
        content.register_script_message_handler(name, None);
        content.connect_script_message_received(Some(name), move |_, value| {
            heard(&value.to_str());
        });
    }

    /// Refuses every navigation but the page's own `about:blank` and hands
    /// the address to `asked`: a link the reader clicked, or a `mailrs:`
    /// link one of the app's scripts clicked.
    pub fn on_navigate(&self, asked: impl Fn(&str) + 'static) {
        self.view.connect_decide_policy(move |_, decision, kind| {
            use webkit::PolicyDecisionType as Kind;
            if !matches!(kind, Kind::NavigationAction | Kind::NewWindowAction) {
                return false;
            }
            let Some(navigation) = decision.downcast_ref::<webkit::NavigationPolicyDecision>()
            else {
                return false;
            };
            let uri = navigation
                .navigation_action()
                .and_then(|action| action.request())
                .and_then(|request| request.uri())
                .map(|uri| uri.to_string())
                .unwrap_or_default();
            if kind == Kind::NavigationAction && uri == "about:blank" {
                decision.use_();
                return true;
            }
            decision.ignore();
            asked(&uri);
            true
        });
    }

    /// Runs `loaded` when a document and everything it asked for is in.
    pub fn on_loaded(&self, loaded: impl Fn() + 'static) {
        self.view.connect_load_changed(move |_, event| {
            if event == webkit::LoadEvent::Finished {
                loaded();
            }
        });
    }

    /// Runs `gone` when the process that drew the page ended, and with it
    /// everything the page held.
    pub fn on_crashed(&self, gone: impl Fn() + 'static) {
        self.view.connect_web_process_terminated(move |_, _| gone());
    }

    pub fn load_html(&self, html: &str) {
        self.view.load_html(html, None);
    }

    /// Runs one of the app's scripts and forgets its answer.
    pub fn run(&self, script: &str) {
        self.view
            .evaluate_javascript(script, None, None, gio::Cancellable::NONE, |_| {});
    }

    /// Runs one of the app's scripts and hands `done` its answer as a
    /// string.
    pub fn eval(&self, script: &str, done: impl FnOnce(Result<String, String>) + 'static) {
        self.view
            .evaluate_javascript(script, None, None, gio::Cancellable::NONE, move |answer| {
                done(
                    answer
                        .map(|value| value.to_str().to_string())
                        .map_err(|err| err.to_string()),
                );
            });
    }

    /// How many of the widget's pixels one CSS pixel takes.
    pub fn zoom(&self) -> f64 {
        self.view.zoom_level()
    }

    pub fn set_zoom(&self, zoom: f64) {
        self.view.set_zoom_level(zoom);
    }

    /// The colour behind the page, seen before it draws.
    pub fn set_background(&self, color: &gdk::RGBA) {
        self.view.set_background_color(color);
    }

    /// Opens the print dialog for the page.
    pub fn print(&self, window: Option<&gtk::Window>) {
        webkit::PrintOperation::new(&self.view).run_dialog(window);
    }

    /// Stops the process that draws the page. It holds about 80 MB, and
    /// WebKit starts a new one for the next load.
    pub fn stop(&self) {
        self.view.terminate_web_process();
    }

    /// Blocks remote content with `filter`, or lets it through with none.
    pub fn set_filter(&self, filter: Option<&Filter>) {
        let content = self.content();
        content.remove_all_filters();
        if let Some(filter) = filter {
            content.add_filter(&filter.0);
        }
    }

    pub fn grab_focus(&self) -> bool {
        self.view.grab_focus()
    }

    /// Whether the keyboard is in the page.
    pub fn has_focus(&self) -> bool {
        self.view
            .state_flags()
            .contains(gtk::StateFlags::FOCUS_WITHIN)
    }

    /// Says `widget` is drawn over the page, as the event card is. GTK
    /// draws both, so there is nothing to do.
    pub fn cover(&self, _widget: &impl IsA<gtk::Widget>) {}

    pub fn finder(&self) -> Finder {
        Finder(
            self.view
                .find_controller()
                .expect("a WebView has a find controller"),
        )
    }
}

/// Whether `widget` is a page, or something inside one, so the keys
/// reach it rather than the window's shortcuts.
pub fn holds_focus(widget: &gtk::Widget) -> bool {
    widget.is::<webkit::WebView>() || widget.ancestor(webkit::WebView::static_type()).is_some()
}

/// The page's request for one address of the app's scheme. It can be
/// held and answered later.
#[derive(Clone)]
pub struct Request(webkit::URISchemeRequest);

impl Request {
    pub fn uri(&self) -> Option<String> {
        self.0.uri().map(|uri| uri.to_string())
    }

    /// Gives the page what it asked for.
    pub fn finish(&self, bytes: impl AsRef<[u8]> + Send + 'static, mime: &str) {
        let bytes = glib::Bytes::from_owned(bytes);
        let length = i64::try_from(bytes.len()).unwrap_or(-1);
        let stream = gio::MemoryInputStream::from_bytes(&bytes);
        self.0.finish(&stream, length, Some(mime));
    }

    /// Says there is nothing at that address.
    pub fn refuse(&self) {
        let mut error = glib::Error::new(gio::IOErrorEnum::NotFound, "nothing at this address");
        self.0.finish_error(&mut error);
    }
}

type Serve = Rc<dyn Fn(Request)>;

thread_local! {
    /// Who answers each page's scheme. WebKit takes one handler per scheme
    /// for the whole process, and that handler finds the page asking here.
    static SERVED: RefCell<Vec<(&'static str, glib::WeakRef<webkit::WebView>, Serve)>> =
        const { RefCell::new(Vec::new()) };
}

/// Has `serve` answer `view`'s requests for `scheme`, registering the
/// scheme with WebKit the first time: a second handler for the same scheme
/// is refused.
fn serve_scheme(scheme: &'static str, view: &webkit::WebView, serve: Serve) {
    let first = SERVED.with(|served| {
        let mut served = served.borrow_mut();
        served.retain(|(_, view, _)| view.upgrade().is_some());
        let first = !served.iter().any(|(name, _, _)| *name == scheme);
        served.push((scheme, view.downgrade(), serve));
        first
    });
    if !first {
        return;
    }
    let Some(context) = webkit::WebContext::default() else {
        return;
    };
    context.register_uri_scheme(scheme, move |request| {
        let asking = request.web_view();
        let serve = SERVED.with(|served| {
            served
                .borrow()
                .iter()
                .find(|(name, view, _)| *name == scheme && view.upgrade() == asking)
                .map(|(_, _, serve)| Rc::clone(serve))
        });
        let request = Request(request.clone());
        match serve {
            Some(serve) => serve(request),
            None => request.refuse(),
        }
    });
}

/// The compiled rules that block remote content, shared by every page.
#[derive(Clone)]
pub struct Filter(webkit::UserContentFilter);

/// Compiles `rules`, WebKit's content blocker JSON, into a filter kept in
/// `dir` under `id`.
pub async fn compile_filter(dir: &Path, id: &str, rules: &'static str) -> Result<Filter, String> {
    let store = webkit::UserContentFilterStore::new(&dir.to_string_lossy());
    store
        .save_future(id, &glib::Bytes::from_static(rules.as_bytes()))
        .await
        .map(Filter)
        .map_err(|err| err.to_string())
}

/// Finds text in one page and highlights every match. WebKit counts the
/// matches but never says which one it highlighted.
pub struct Finder(webkit::FindController);

impl Finder {
    fn options(case_sensitive: bool) -> u32 {
        let mut options = webkit::FindOptions::WRAP_AROUND;
        if !case_sensitive {
            options |= webkit::FindOptions::CASE_INSENSITIVE;
        }
        options.bits()
    }

    /// Highlights the first match of `query` and counts them all.
    pub fn search(&self, query: &str, case_sensitive: bool) {
        let options = Finder::options(case_sensitive);
        self.0.search(query, options, MATCH_LIMIT);
        self.0.count_matches(query, options, MATCH_LIMIT);
    }

    /// Counts the matches again, leaving the highlight where it is.
    pub fn count(&self, query: &str, case_sensitive: bool) {
        self.0
            .count_matches(query, Finder::options(case_sensitive), MATCH_LIMIT);
    }

    pub fn next(&self) {
        self.0.search_next();
    }

    pub fn previous(&self) {
        self.0.search_previous();
    }

    /// Takes the highlight off the page.
    pub fn finish(&self) {
        self.0.search_finish();
    }

    pub fn on_found(&self, found: impl Fn() + 'static) {
        self.0.connect_found_text(move |_, _| found());
    }

    pub fn on_counted(&self, counted: impl Fn(usize) + 'static) {
        self.0
            .connect_counted_matches(move |_, matches| counted(matches as usize));
    }

    pub fn on_failed(&self, failed: impl Fn() + 'static) {
        self.0.connect_failed_to_find_text(move |_| failed());
    }
}

/// WebKit keeps a `gtk::TextView` of its own inside the web view, at no
/// size, to turn key bindings such as Ctrl+C into editing commands. It
/// was focusable, so Tab moved into it from the page and stayed there:
/// focus sat on nothing a person could see, the page stopped getting the
/// keys, and the window took single-letter shortcuts for typing. The
/// bindings reach it without the focus, so it gives the focus up.
fn keep_key_text_out_of_tab(webview: &webkit::WebView) {
    let mut stack: Vec<gtk::Widget> = webview.first_child().into_iter().collect();
    while let Some(widget) = stack.pop() {
        if widget.is::<gtk::TextView>() {
            widget.set_focusable(false);
        }
        stack.extend(widget.next_sibling());
        stack.extend(widget.first_child());
    }
}

/// One network session for every page. Each session runs its own WebKit
/// network process, and a detached window needs no second one. Ephemeral
/// keeps cookies and caches in memory, so nothing lands on disk.
fn network_session() -> webkit::NetworkSession {
    thread_local! {
        static SESSION: webkit::NetworkSession = webkit::NetworkSession::new_ephemeral();
    }
    SESSION.with(|s| s.clone())
}
