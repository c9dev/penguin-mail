//! The Windows adapter: a WebView2 page over an empty GTK anchor.
//!
//! GTK 4 cannot hold a native view in its widget tree, so the page is a
//! child HWND of the window's surface, and [`frame::follow`] moves it over
//! the anchor, a focusable `gtk::Box` that takes the page's room in the
//! layout, after GTK paints each frame. A GTK widget drawn over the page,
//! such as the event card, cannot show through a child HWND, so the page
//! hides itself for as long as one covers it.
//!
//! The engine lives on the GTK thread the same way COM objects do: the
//! environment and each controller are created through asynchronous calls
//! whose completions arrive on this thread's message pump, so calls made
//! before a page is ready queue up and run once it is.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    MoveFocusRequestedEventHandler, NavigationCompletedEventHandler,
    NavigationStartingEventHandler, NewWindowRequestedEventHandler,
    PermissionRequestedEventHandler, ProcessFailedEventHandler, WebMessageReceivedEventHandler,
    WebResourceRequestedEventHandler,
};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
use windows::core::{BOOL, HSTRING, Interface, PWSTR};

use super::frame;

/// The CSS class on every anchor, so [`holds_focus`] can tell one.
const ANCHOR_CLASS: &str = "web-view";

/// Lets the app's scripts post to `window.webkit.messageHandlers[name]`
/// the way they do on the WebKit adapters, forwarding every message on
/// `chrome.webview` tagged with the handler's name.
const HANDLERS_SCRIPT: &str = r#"window.webkit = {
  messageHandlers: new Proxy({}, {
    get: function (heard, name) {
      if (!(name in heard)) {
        heard[name] = {
          postMessage: function (message) {
            window.chrome.webview.postMessage({ __mailrs: String(name), message: message });
          }
        };
      }
      return heard[name];
    }
  })
};"#;

/// Counts the matches of a query in the page's text, skipping style and
/// script, and answers the number as a string. The same script serves
/// every adapter, since the page's shape is the app's own.
const COUNT: &str = r#"(function (query, sensitive) {
  if (!sensitive) query = query.toLowerCase();
  var found = 0;
  var walk = function (root) {
    var walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT);
    for (var node = walker.nextNode(); node; node = walker.nextNode()) {
      if (node.nodeType === 1) {
        if (node.shadowRoot) walk(node.shadowRoot);
        continue;
      }
      var parent = node.parentNode && node.parentNode.nodeName;
      if (parent === 'STYLE' || parent === 'SCRIPT' || parent === 'TITLE') continue;
      var text = sensitive ? node.data : node.data.toLowerCase();
      for (var at = text.indexOf(query); at !== -1; at = text.indexOf(query, at + query.length)) found++;
    }
  };
  walk(document.body || document.documentElement);
  return String(found);
})"#;

/// Clears the selection, so the next find starts from the top.
const UNSELECT: &str = "window.getSelection().removeAllRanges()";

/// WebView2 needs nothing before GTK starts; its environment is made when
/// the first page asks for it.
pub fn prepare() {}

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
type Serve = Rc<dyn Fn(Request)>;
type Op = Box<dyn FnOnce(&Core)>;
/// A page script's own voice, answering one named message.
type Message = Rc<dyn Fn(&str)>;
/// A caller waiting for the shared environment, `None` when it never came.
type Waiter = Rc<dyn Fn(Option<&ICoreWebView2Environment>)>;

/// The native page once it exists: the controller owns the child HWND,
/// the core owns the document, and the environment answers requests.
struct Core {
    controller: ICoreWebView2Controller,
    web: ICoreWebView2,
    env: ICoreWebView2Environment,
}

struct Inner {
    anchor: gtk::Box,
    /// Nothing until the environment, then the controller, answers.
    core: RefCell<Option<Rc<Core>>>,
    /// Calls made before the page exists, in the order they were made.
    queued: RefCell<Vec<Op>>,
    /// The app's scripts, run through `ExecuteScript` after every load
    /// finishes. `NavigateToString` reuses the document it finds, so
    /// `AddScriptToExecuteOnDocumentCreated` cannot be relied on to put
    /// them where WebKit's document-end scripts go.
    scripts: RefCell<Vec<Rc<str>>>,
    /// Whether the page's script engine runs, which a reader's scripts
    /// need; the rendered CSP keeps the mail's own markup silent anyway.
    /// A sealed page keeps the engine off entirely.
    scripting: Cell<bool>,
    /// The app's scheme this page serves, empty when it serves none.
    scheme: RefCell<String>,
    serve: RefCell<Option<Serve>>,
    filter: RefCell<Option<Filter>>,
    navigate: RefCell<Option<Navigate>>,
    loaded: RefCell<Vec<Rc<dyn Fn()>>>,
    crashed: RefCell<Vec<Rc<dyn Fn()>>>,
    heard: RefCell<HashMap<String, Message>>,
    /// GTK widgets drawn over the page, such as the event card's holder.
    covers: RefCell<Vec<glib::WeakRef<gtk::Widget>>>,
    /// The HWND the page holds the keyboard with, recorded after a
    /// `MoveFocus`, so a GTK focus-out that really is the page's own
    /// taking over is not mistaken for the page losing it.
    focused: Cell<HWND>,
    /// A handle back to this page: `after_paint` cannot be trusted to
    /// run once a covering overlay leaves, so a hidden page schedules
    /// its own rechecks until it shows again, one at a time.
    me: RefCell<Weak<Inner>>,
    retry: Cell<bool>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(core) = self.core.borrow().as_ref() {
            unsafe {
                let _ = core.controller.Close();
            }
        }
    }
}

impl Inner {
    /// Runs `op` now if the page exists, or queues it for when it does.
    fn with_core(&self, op: impl FnOnce(&Core) + 'static) {
        if let Some(core) = self.core.borrow().as_ref() {
            let core = core.clone();
            op(&core);
        } else {
            self.queued.borrow_mut().push(Box::new(op));
        }
    }

    fn navigated(&self, uri: &str) {
        let navigate = self.navigate.borrow().clone();
        if let Some(navigate) = navigate {
            navigate(uri);
        }
    }

    fn loaded(&self) {
        for loaded in self.loaded.borrow().iter() {
            loaded();
        }
    }

    fn crashed(&self) {
        for crashed in self.crashed.borrow().iter() {
            crashed();
        }
    }

    /// The anchor's rectangle in its window's client area, in the pixels
    /// the HWND speaks, or nothing while the anchor is unmapped.
    fn rect(&self) -> Option<RECT> {
        let native = self.anchor.native()?;
        let root: gtk::Widget = native.clone().upcast();
        self.client(self.anchor.compute_bounds(&root)?)
    }

    /// `bounds` in widget coordinates as a client-area `RECT`: widget
    /// coordinates start at the toplevel widget, inset by the surface's
    /// client-side shadow, and the child HWND is placed against the
    /// client area, which shares the surface's origin.
    fn client(&self, bounds: gtk::graphene::Rect) -> Option<RECT> {
        let native = self.anchor.native()?;
        let (dx, dy) = native.surface_transform();
        let scale = f64::from(native.surface()?.scale_factor());
        Some(RECT {
            left: ((f64::from(bounds.x()) + dx) * scale).round() as i32,
            top: ((f64::from(bounds.y()) + dy) * scale).round() as i32,
            right: ((f64::from(bounds.x() + bounds.width()) + dx) * scale).round() as i32,
            bottom: ((f64::from(bounds.y() + bounds.height()) + dy) * scale).round() as i32,
        })
    }

    /// The part of the anchor an in-surface overlay — a toast or an
    /// in-surface popover — is drawn over, since a child HWND cannot
    /// give way to part of itself.
    fn overlay(&self) -> Option<gtk::graphene::Rect> {
        let native = self.anchor.native()?;
        let surface = native.surface()?;
        let root: gtk::Widget = native.upcast();
        let own = self.anchor.compute_bounds(&root)?;
        fn over(
            widget: &gtk::Widget,
            root: &gtk::Widget,
            own: &gtk::graphene::Rect,
            surface: &gdk::Surface,
        ) -> Option<gtk::graphene::Rect> {
            if !widget.is_mapped() {
                return None;
            }
            // A widget drawn on another surface, such as a popover's own
            // popup window, already stacks over the child HWND, so it
            // and its children are no reason to hide the page.
            if widget.native().and_then(|native| native.surface()).as_ref() != Some(surface) {
                return None;
            }
            if (widget.is::<gtk::Popover>() || widget.css_name() == "toast")
                && let Some(rect) = widget
                    .compute_bounds(root)
                    .filter(|rect| rect.intersection(own).is_some())
            {
                return Some(rect);
            }

            let mut child = widget.first_child();
            while let Some(w) = child {
                if let Some(found) = over(&w, root, own, surface) {
                    return Some(found);
                }
                child = w.next_sibling();
            }
            None
        }
        over(&root, &root, &own, &surface)
    }

    /// Whether a dialog is presented over the anchor's window, in which
    /// case the page hides too: the dialog's dim and sheet are GTK
    /// drawing, which a child HWND always paints on top of.
    fn dialog_up(&self) -> bool {
        let Some(native) = self.anchor.native() else {
            return false;
        };
        let root: gtk::Widget = native.upcast();
        if let Some(window) = root.downcast_ref::<adw::Window>() {
            window.visible_dialog().is_some()
        } else if let Some(window) = root.downcast_ref::<adw::ApplicationWindow>() {
            window.visible_dialog().is_some()
        } else {
            false
        }
    }

    /// Whether a covering GTK widget intersects the anchor, meaning the
    /// page must hide since a child HWND cannot give way to part of itself.
    fn covered(&self) -> bool {
        let Some(native) = self.anchor.native() else {
            return false;
        };
        let root: gtk::Widget = native.clone().upcast();
        let Some(own) = self.anchor.compute_bounds(&root) else {
            return false;
        };
        self.covers.borrow().iter().any(|weak| {
            let Some(widget) = weak.upgrade() else {
                return false;
            };
            widget.is_visible()
                && widget
                    .compute_bounds(&root)
                    .is_some_and(|rect| rect.intersection(&own).is_some())
        })
    }

    /// Moves the page over its anchor, called after GTK paints each frame.
    fn placed(&self) {
        let Some(core) = self.core.borrow().as_ref().cloned() else {
            return;
        };
        let mut visible = self.anchor.is_mapped() && !self.covered() && !self.dialog_up();
        let full = self.rect();
        let mut rect = full;
        if visible
            && let (Some(hole), Some(area)) =
                (self.overlay().and_then(|over| self.client(over)), rect)
        {
            rect = minus(&area, &hole);
            // A sliver left of the page reads as broken, so a big
            // enough overlay hides the page whole instead.
            visible = rect.is_some_and(|left| {
                i64::from(left.right - left.left) * i64::from(left.bottom - left.top)
                    >= i64::from(area.right - area.left) * i64::from(area.bottom - area.top) / 4
            });
        }
        // A page that is not at its anchor's full rectangle keeps
        // rechecking itself: the paint clock may have gone quiet right
        // after an overlay left, and the cut or hidden state would
        // otherwise never undo.
        if !(visible && rect == full) && !self.retry.replace(true) {
            let me = self.me.borrow().clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
                if let Some(inner) = me.upgrade() {
                    inner.retry.set(false);
                    inner.placed();
                }
            });
        }

        unsafe {
            let _ = core.controller.SetIsVisible(visible);
            if let Some(rect) = rect {
                let _ = core.controller.SetBounds(rect);
            }
        }
    }

    /// A request the page made. Its own scheme goes to `serve`; anything
    /// else the compiled filter rules out is refused, and the rest passes.
    fn resource(&self, args: &ICoreWebView2WebResourceRequestedEventArgs) {
        let Ok(request) = (unsafe { args.Request() }) else {
            return;
        };
        let mut raw = PWSTR::null();
        unsafe {
            let _ = request.Uri(&mut raw);
        }
        let uri = text(raw);
        let scheme = self.scheme.borrow().clone();
        if !scheme.is_empty() && uri.starts_with(&format!("{scheme}:")) {
            let (Some(serve), Some(env)) = (
                self.serve.borrow().clone(),
                self.core.borrow().as_ref().map(|core| core.env.clone()),
            ) else {
                return;
            };
            let Ok(deferral) = (unsafe { args.GetDeferral() }) else {
                return;
            };
            serve(Request(Rc::new(RequestInner {
                args: RefCell::new(Some(args.clone())),
                env,
                deferral: RefCell::new(Some(deferral)),
                uri,
            })));
            return;
        }
        let mut context = COREWEBVIEW2_WEB_RESOURCE_CONTEXT::default();
        unsafe {
            let _ = args.ResourceContext(&mut context);
        }
        let filter = self.filter.borrow().clone();
        if filter.is_some_and(|filter| filter.blocks(&uri, context)) {
            let Some(env) = self.core.borrow().as_ref().map(|core| core.env.clone()) else {
                return;
            };
            if let Ok(response) = respond(&env, &[], 404, "Not Found", "") {
                unsafe {
                    let _ = args.SetResponse(&response);
                }
            }
        }
    }
}

/// Answers `raw`'s text, or nothing for a null out-param.
fn text(raw: PWSTR) -> String {
    if raw.is_null() {
        return String::new();
    }
    unsafe { raw.to_string() }.unwrap_or_default()
}

/// Where the engine keeps its working files for this run. The store is
/// per process and never asked to persist, the way the macOS adapter's
/// non-persistent data store leaves nothing behind.
/// Runs `script` on the page and waits for the engine to finish it,
/// used to land each of the app's scripts before a load is announced.
async fn run_script(core: &Rc<Core>, script: &str) {
    let (tell, hear) = futures::channel::oneshot::channel();
    let tell = Cell::new(Some(tell));
    let handler = webview2_com::ExecuteScriptCompletedHandler::create(Box::new(move |_, _| {
        if let Some(tell) = tell.take() {
            let _ = tell.send(());
        }
        Ok(())
    }));
    unsafe {
        let _ = core.web.ExecuteScript(&HSTRING::from(script), &handler);
    }
    let _ = hear.await;
}

fn user_data_dir() -> PathBuf {
    std::env::temp_dir().join(format!("penguin-mail-webview2-{}", std::process::id()))
}

/// The largest piece of `area` left once `hole` is cut from one side,
/// or nothing when `hole` covers `area`.
fn minus(area: &RECT, hole: &RECT) -> Option<RECT> {
    [
        RECT {
            left: hole.right,
            ..*area
        },
        RECT {
            right: hole.left,
            ..*area
        },
        RECT {
            top: hole.bottom,
            ..*area
        },
        RECT {
            bottom: hole.top,
            ..*area
        },
    ]
    .into_iter()
    .filter(|rect| rect.right > rect.left && rect.bottom > rect.top)
    .max_by_key(|rect| i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top))
}

/// Schemes the app answers itself: `reading` pages serve their own, and
/// `mailrs:` links are cancelled on their way to `on_navigate`. Each is
/// registered with the environment so requests and navigations on it run
/// through the same handler http addresses do, the way WebKit treats a
/// registered scheme.
const OWN_SCHEMES: &[&str] = &[
    "mailrs",
    "mailrs-cid",
    "mailrs-check",
    "mailrs-link-check",
    "mailrs-reopen-check",
];

/// The environment every page shares, made on the GTK thread the first
/// time a page asks for it and handed to each waiter once.
enum Env {
    Waiting(Vec<Waiter>),
    Ready(ICoreWebView2Environment),
    Failed,
}

thread_local! {
    static ENVIRONMENT: RefCell<Env> = RefCell::new(Env::Waiting(Vec::new()));
}

/// Runs `heard` with the shared environment once it exists, or `None` if
/// the WebView2 runtime is not on this computer.
fn with_environment(heard: impl Fn(Option<&ICoreWebView2Environment>) + 'static) {
    let heard = Rc::new(heard);
    enum Ask {
        Now(ICoreWebView2Environment),
        Later,
        Gone,
    }
    let ask = ENVIRONMENT.with(|cell| match &*cell.borrow() {
        Env::Ready(env) => Ask::Now(env.clone()),
        Env::Failed => Ask::Gone,
        Env::Waiting(_) => Ask::Later,
    });
    match ask {
        Ask::Now(env) => heard(Some(&env)),
        Ask::Gone => heard(None),
        Ask::Later => {
            enum Wait {
                // Queued; true when this is the first waiter, so the
                // environment still needs starting.
                Queued(bool),
                Ready(ICoreWebView2Environment),
                Failed,
            }
            // The environment's answer can arrive reentrantly, so nothing
            // but the queue itself changes while the cell is borrowed.
            let wait = ENVIRONMENT.with(|cell| match &mut *cell.borrow_mut() {
                Env::Waiting(waiters) => {
                    waiters.push(heard.clone());
                    Wait::Queued(waiters.len() == 1)
                }
                // Another call resolved it in between.
                Env::Ready(env) => Wait::Ready(env.clone()),
                Env::Failed => Wait::Failed,
            });
            match wait {
                Wait::Queued(true) => start_environment(),
                Wait::Queued(false) => {}
                Wait::Ready(env) => heard(Some(&env)),
                Wait::Failed => heard(None),
            }
        }
    }
}

/// The options the shared environment gets: the app's schemes, marked
/// secure and hostless so `mailrs:id` and `mailrs-cid:id` load like http.
fn environment_options() -> ICoreWebView2EnvironmentOptions {
    let options = webview2_com::CoreWebView2EnvironmentOptions::default();
    let registrations = OWN_SCHEMES
        .iter()
        .map(|scheme| {
            let registration =
                webview2_com::CoreWebView2CustomSchemeRegistration::new((*scheme).to_string());
            unsafe {
                registration.set_treat_as_secure(true);
                registration.set_allowed_origins(vec!["*".to_string()]);
                registration.set_has_authority_component(false);
            }
            Some(registration.into())
        })
        .collect();
    unsafe {
        options.set_scheme_registrations(registrations);
    }
    options.into()
}

/// The environment holds one user-data folder per process. Creation
/// answers on this thread's message pump.
fn start_environment() {
    let dir = user_data_dir();
    let options = environment_options();
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
        |result: windows::core::Result<()>, found: Option<ICoreWebView2Environment>| {
            let env = result.ok().and(found);
            let waiters = ENVIRONMENT.with(|cell| {
                let old = std::mem::replace(
                    &mut *cell.borrow_mut(),
                    match &env {
                        Some(env) => Env::Ready(env.clone()),
                        None => Env::Failed,
                    },
                );
                match old {
                    Env::Waiting(waiters) => waiters,
                    _ => Vec::new(),
                }
            });
            for waiter in waiters {
                waiter(env.as_ref());
            }
            Ok(())
        },
    ));
    let made = unsafe {
        CreateCoreWebView2EnvironmentWithOptions(
            windows::core::PCWSTR::null(),
            &HSTRING::from(&*dir.to_string_lossy()),
            &options,
            &handler,
        )
    };
    if made.is_err() {
        let waiters = ENVIRONMENT.with(|cell| {
            let old = std::mem::replace(&mut *cell.borrow_mut(), Env::Failed);
            match old {
                Env::Waiting(waiters) => waiters,
                _ => Vec::new(),
            }
        });
        for waiter in waiters {
            waiter(None);
        }
    }
}

/// The window's HWND the page becomes a child of, or nothing while the
/// anchor has no surface.
fn hwnd(anchor: &gtk::Widget) -> Option<HWND> {
    let native = anchor.native()?;
    let surface = native.surface()?;
    let surface = surface.downcast::<gdk4_win32::Win32Surface>().ok()?;
    Some(HWND(surface.handle().0))
}

/// Builds the page's controller once the anchor owns a surface and the
/// environment exists.
fn create(inner: &Weak<Inner>) {
    let Some(page) = inner.upgrade() else {
        return;
    };
    let Some(window) = hwnd(page.anchor.upcast_ref()) else {
        return;
    };
    let inner = inner.clone();
    with_environment(move |env| {
        let Some(page) = inner.upgrade() else {
            return;
        };
        let Some(env) = env.cloned() else {
            tracing::warn!("no WebView2 runtime: a page will draw nothing");
            return;
        };
        let weak = Rc::downgrade(&page);
        let env2 = env.clone();
        let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
            move |result, controller| {
                let Some(inner) = weak.upgrade() else {
                    return Ok(());
                };
                let core = result.ok().and(controller).and_then(|controller| unsafe {
                    controller.CoreWebView2().ok().map(|web| {
                        Rc::new(Core {
                            controller,
                            web,
                            env: env2.clone(),
                        })
                    })
                });
                match core {
                    Some(core) => {
                        wire(&inner, &core);
                        inner.placed();
                        let ops = std::mem::take(&mut *inner.queued.borrow_mut());
                        *inner.core.borrow_mut() = Some(core.clone());
                        for op in ops {
                            op(&core);
                        }
                    }
                    None => {
                        tracing::warn!("WebView2 made no controller: a page will draw nothing");
                    }
                }
                Ok(())
            },
        ));
        unsafe {
            let _ = env.CreateCoreWebView2Controller(window, &handler);
        }
    });
}

/// The settings and events every page gets once its controller exists.
fn wire(inner: &Rc<Inner>, core: &Rc<Core>) {
    let web = &core.web;
    unsafe {
        if let Ok(settings) = web.Settings() {
            // WebView2 has no markup-only script gate like WebKit's
            // `enable-javascript-markup`: a reader needs the engine for
            // its injected scripts and their listeners, while the CSP
            // in every rendered head keeps the mail's own scripts out.
            // A sealed page wants no engine at all.
            let _ = settings.SetIsScriptEnabled(inner.scripting.get());
            let _ = settings.SetAreDefaultContextMenusEnabled(false);
            let _ = settings.SetAreDefaultScriptDialogsEnabled(false);
            let _ = settings.SetAreDevToolsEnabled(false);
            let _ = settings.SetIsStatusBarEnabled(false);
            let _ = settings.SetIsZoomControlEnabled(false);
            let _ = settings.SetAreHostObjectsAllowed(false);
            let _ = settings.SetIsBuiltInErrorPageEnabled(false);
        }
        // Every request passes the handler: the app's scheme goes to
        // `serve`, and the compiled filter decides about the rest.
        let _ = web.AddWebResourceRequestedFilter(
            &HSTRING::from("*"),
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
        );

        let page = Rc::downgrade(inner);
        let handler = WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
            if let (Some(inner), Some(args)) = (page.upgrade(), args) {
                inner.resource(&args);
            }
            Ok(())
        }));
        let mut token = 0;
        let _ = web.add_WebResourceRequested(&handler, &mut token);

        let page = Rc::downgrade(inner);
        let handler = NavigationStartingEventHandler::create(Box::new(move |_, args| {
            let (Some(inner), Some(args)) = (page.upgrade(), args) else {
                return Ok(());
            };
            let mut raw = PWSTR::null();
            let _ = args.Uri(&mut raw);
            let uri = text(raw);
            let own = ["about:blank", "data:", "blob:"]
                .iter()
                .any(|start| uri.starts_with(start))
                || (!inner.scheme.borrow().is_empty()
                    && uri.starts_with(&format!("{}:", inner.scheme.borrow())));
            if own {
                return Ok(());
            }
            let _ = args.SetCancel(true);
            inner.navigated(&uri);
            Ok(())
        }));
        let mut token = 0;
        let _ = web.add_NavigationStarting(&handler, &mut token);

        let page = Rc::downgrade(inner);
        let core2 = Rc::clone(core);
        let handler = NavigationCompletedEventHandler::create(Box::new(move |_, args| {
            let mut done = BOOL::default();
            if let Some(args) = args {
                let _ = args.IsSuccess(&mut done);
            }
            let Some(inner) = page.upgrade().filter(|_| done.as_bool()) else {
                return Ok(());
            };
            let scripts: Vec<Rc<str>> = inner.scripts.borrow().clone();
            if scripts.is_empty() {
                inner.loaded();
                return Ok(());
            }
            // The app's scripts land before the load is announced, so a
            // caller waiting on `on_loaded` finds them already in place.
            let page = Rc::downgrade(&inner);
            let core = Rc::clone(&core2);
            glib::spawn_future_local(async move {
                for script in scripts {
                    run_script(&core, &script).await;
                }
                if let Some(inner) = page.upgrade() {
                    inner.loaded();
                }
            });
            Ok(())
        }));
        let mut token = 0;
        let _ = web.add_NavigationCompleted(&handler, &mut token);

        let page = Rc::downgrade(inner);
        let handler = ProcessFailedEventHandler::create(Box::new(move |_, _| {
            // A browser-process exit takes every page; a render exit takes
            // this one. Both read as a crash to the app.
            if let Some(inner) = page.upgrade() {
                inner.crashed();
            }
            Ok(())
        }));
        let mut token = 0;
        let _ = web.add_ProcessFailed(&handler, &mut token);

        let page = Rc::downgrade(inner);
        let handler = WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
            let (Some(inner), Some(args)) = (page.upgrade(), args) else {
                return Ok(());
            };
            let mut raw = PWSTR::null();
            let _ = args.WebMessageAsJson(&mut raw);
            let Ok(body) = serde_json::from_str::<serde_json::Value>(&text(raw)) else {
                return Ok(());
            };
            let Some(name) = body.get("__mailrs").and_then(|name| name.as_str()) else {
                return Ok(());
            };
            let heard = inner.heard.borrow().get(name).cloned();
            if let (Some(heard), Some(message)) = (heard, body.get("message")) {
                let said = match message {
                    serde_json::Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                heard(&said);
            }
            Ok(())
        }));
        let mut token = 0;
        let _ = web.add_WebMessageReceived(&handler, &mut token);

        // No popups, no new windows, no permissions: a mail page wants
        // none of them.
        let handler = NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
            if let Some(args) = args {
                let _ = args.SetHandled(true);
            }
            Ok(())
        }));
        let mut token = 0;
        let _ = web.add_NewWindowRequested(&handler, &mut token);

        let handler = PermissionRequestedEventHandler::create(Box::new(move |_, args| {
            if let Some(args) = args {
                let _ = args.SetState(COREWEBVIEW2_PERMISSION_STATE_DENY);
            }
            Ok(())
        }));
        let mut token = 0;
        let _ = web.add_PermissionRequested(&handler, &mut token);

        // Tab at the page's ends asks to leave it: GTK's own focus cycle
        // takes it from there, in the direction the key was going.
        let page = Rc::downgrade(inner);
        let handler = MoveFocusRequestedEventHandler::create(Box::new(move |_, args| {
            let mut reason = COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC;
            if let Some(args) = args {
                let _ = args.Reason(&mut reason);
            }
            let direction = match reason {
                COREWEBVIEW2_MOVE_FOCUS_REASON_NEXT => Some(gtk::DirectionType::TabForward),
                COREWEBVIEW2_MOVE_FOCUS_REASON_PREVIOUS => Some(gtk::DirectionType::TabBackward),
                _ => None,
            };
            if let (Some(inner), Some(direction)) = (page.upgrade(), direction)
                && let Some(root) = inner.anchor.root()
            {
                root.child_focus(direction);
            }
            Ok(())
        }));
        let mut token = 0;
        let _ = core.controller.add_MoveFocusRequested(&handler, &mut token);

        let _ = web.AddScriptToExecuteOnDocumentCreated(&HSTRING::from(HANDLERS_SCRIPT), None);
    }
}

impl WebView {
    /// The conversation's page. The mail's own markup scripts stay off
    /// (the rendered head carries `script-src 'none'`), while the app's
    /// own scripts run through [`WebView::add_script`] and
    /// [`WebView::run`]. Pictures the page asks for at an address of
    /// `scheme` go to `serve`.
    pub fn reading(scheme: &'static str, serve: impl Fn(Request) + 'static) -> WebView {
        let page = WebView::build();
        page.0.scripting.set(true);
        *page.0.scheme.borrow_mut() = scheme.to_string();
        *page.0.serve.borrow_mut() = Some(Rc::new(serve));
        page.0.anchor.set_hexpand(true);
        page
    }

    /// A page for the composer's Preview and forwarded message. It runs no
    /// script and never navigates: a click on a web link opens it in the
    /// browser, and anything else is ignored.
    pub fn sealed() -> WebView {
        let page = WebView::build();
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

    fn build() -> WebView {
        let anchor = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(true)
            .focusable(true)
            .css_classes([ANCHOR_CLASS])
            .build();
        let inner = Rc::new(Inner {
            anchor,
            core: RefCell::new(None),
            queued: RefCell::new(Vec::new()),
            scripts: RefCell::new(Vec::new()),
            scripting: Cell::new(false),
            scheme: RefCell::new(String::new()),
            serve: RefCell::new(None),
            filter: RefCell::new(None),
            navigate: RefCell::new(None),
            loaded: RefCell::new(Vec::new()),
            crashed: RefCell::new(Vec::new()),
            heard: RefCell::new(HashMap::new()),
            covers: RefCell::new(Vec::new()),
            focused: Cell::new(HWND::default()),
            me: RefCell::new(Weak::new()),
            retry: Cell::new(false),
        });
        *inner.me.borrow_mut() = Rc::downgrade(&inner);

        let weak = Rc::downgrade(&inner);
        inner.anchor.connect_realize(move |_| create(&weak));
        if inner.anchor.is_realized() {
            // The anchor may own a surface before the signal connects.
            create(&Rc::downgrade(&inner));
        }
        let weak = Rc::downgrade(&inner);
        frame::follow(inner.anchor.upcast_ref(), move || {
            if let Some(inner) = weak.upgrade() {
                inner.placed();
            }
        });
        // Focus entering the anchor enters the page, and focus leaving
        // the anchor for anything but the page's own child HWND takes it
        // back out. Moving it into the child makes the window lose the
        // OS focus, which GTK reads as a focus-out: taking it right back
        // would loop, so the check waits for the move to settle and then
        // asks Windows where the keyboard actually went.
        let weak = Rc::downgrade(&inner);
        inner.anchor.add_controller({
            let focus = gtk::EventControllerFocus::new();
            focus.connect_enter(move |_| {
                if let Some(inner) = weak.upgrade() {
                    let page = Rc::clone(&inner);
                    inner.with_core(move |core| unsafe {
                        if core
                            .controller
                            .MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
                            .is_ok()
                        {
                            page.focused.set(GetFocus());
                        }
                    });
                }
            });
            let weak = Rc::downgrade(&inner);
            focus.connect_leave(move |_| {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                glib::idle_add_local_once(move || {
                    let at = unsafe { GetFocus() };
                    if !inner.focused.get().is_invalid() && inner.focused.get() == at {
                        return;
                    }
                    inner.focused.set(HWND::default());
                    inner.with_core(|core| unsafe {
                        let mut parent = HWND::default();
                        if core.controller.ParentWindow(&mut parent).is_ok() {
                            let _ = SetFocus(Some(parent));
                        }
                    });
                });
            });
            focus
        });
        // A fresh window hands its keyboard to the first thing that can
        // hold it, which is the page's anchor: the page takes the focus
        // a focusable webview would get, unless something else has it.
        let weak = Rc::downgrade(&inner);
        inner.anchor.connect_map(move |anchor| {
            if anchor
                .root()
                .and_downcast::<gtk::Window>()
                .is_some_and(|window| gtk::prelude::GtkWindowExt::focus(&window).is_none())
                && let Some(inner) = weak.upgrade()
            {
                inner.anchor.grab_focus();
            }
        });
        WebView(inner)
    }

    /// The widget that takes the page's place in a window.
    pub fn widget(&self) -> gtk::Widget {
        self.0.anchor.clone().upcast()
    }

    pub fn downgrade(&self) -> WeakWebView {
        WeakWebView(Rc::downgrade(&self.0))
    }

    /// A script of the app's own that the engine runs in the top frame
    /// once each document is parsed. The scripts land after every load,
    /// which is where a document-end script would run them on WebKit;
    /// by then the DOM is built, so no further waiting is needed.
    pub fn add_script(&self, source: &str) {
        self.0.scripts.borrow_mut().push(Rc::from(source));
    }

    /// Hears what the page posts to `window.webkit.messageHandlers[name]`,
    /// as a string.
    pub fn on_message(&self, name: &str, heard: impl Fn(&str) + 'static) {
        self.0
            .heard
            .borrow_mut()
            .insert(name.to_string(), Rc::new(heard));
    }

    /// Refuses every navigation but the page's own and hands the address
    /// to `asked`.
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
        let html = html.to_string();
        self.0.with_core(move |core| unsafe {
            let _ = core.web.NavigateToString(&HSTRING::from(&html));
        });
    }

    /// Runs one of the app's scripts and forgets its answer.
    pub fn run(&self, script: &str) {
        self.eval(script, |_| {});
    }

    /// Runs one of the app's scripts and hands `done` its answer as a
    /// string.
    pub fn eval(&self, script: &str, done: impl FnOnce(Result<String, String>) + 'static) {
        let script = script.to_string();
        let done = Cell::new(Some(done));
        self.0.with_core(move |core| unsafe {
            let handler = webview2_com::ExecuteScriptCompletedHandler::create(Box::new(
                move |result, json| {
                    if let Some(done) = done.take() {
                        let answer = match result {
                            Ok(()) => Ok(json_text(&json)),
                            Err(err) => Err(err.to_string()),
                        };
                        done(answer);
                    }
                    Ok(())
                },
            ));
            let _ = core.web.ExecuteScript(&HSTRING::from(&script), &handler);
        });
    }

    /// How many of the widget's pixels one CSS pixel takes.
    pub fn zoom(&self) -> f64 {
        self.0
            .core
            .borrow()
            .as_ref()
            .and_then(|core| unsafe {
                let mut zoom = 0.0;
                core.controller.ZoomFactor(&mut zoom).ok().map(|_| zoom)
            })
            .unwrap_or(1.0)
    }

    pub fn set_zoom(&self, zoom: f64) {
        self.0.with_core(move |core| unsafe {
            let _ = core.controller.SetZoomFactor(zoom);
        });
    }

    /// The colour behind the page, seen past its ends.
    pub fn set_background(&self, color: &gdk::RGBA) {
        let to_u8 = |part: f32| (part.clamp(0.0, 1.0) * 255.0).round() as u8;
        let color = COREWEBVIEW2_COLOR {
            A: to_u8(color.alpha()),
            R: to_u8(color.red()),
            G: to_u8(color.green()),
            B: to_u8(color.blue()),
        };
        self.0.with_core(move |core| unsafe {
            if let Ok(controller) = core.controller.cast::<ICoreWebView2Controller2>() {
                let _ = controller.SetDefaultBackgroundColor(color);
            }
        });
    }

    /// Opens the print dialog for the page, when the runtime has one.
    pub fn print(&self, _window: Option<&gtk::Window>) {
        self.0.with_core(|core| unsafe {
            if let Ok(web) = core.web.cast::<ICoreWebView2_16>() {
                let _ = web.ShowPrintUI(COREWEBVIEW2_PRINT_DIALOG_KIND_BROWSER);
            }
        });
    }

    /// Lets go of what the page holds: it is emptied and the process
    /// idles until the next load.
    pub fn stop(&self) {
        self.load_html("");
    }

    /// Blocks remote content with `filter`, or lets it through with none.
    pub fn set_filter(&self, filter: Option<&Filter>) {
        *self.0.filter.borrow_mut() = filter.cloned();
    }

    pub fn grab_focus(&self) -> bool {
        self.0.anchor.grab_focus()
    }

    /// Whether the keyboard is in the page.
    pub fn has_focus(&self) -> bool {
        let inside = {
            let at = unsafe { GetFocus() };
            !at.is_invalid() && at == self.0.focused.get()
        };
        inside
            || self
                .0
                .anchor
                .state_flags()
                .contains(gtk::StateFlags::FOCUS_WITHIN)
    }

    /// Says `widget` is drawn over the page, as the event card is. A
    /// child HWND cannot give way to part of itself, so the page hides
    /// for as long as a cover intersects its anchor.
    pub fn cover(&self, widget: &impl IsA<gtk::Widget>) {
        self.0.covers.borrow_mut().push(widget.as_ref().downgrade());
        self.0.placed();
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

/// A response WebView2 hands to the page for one address.
pub(crate) fn respond(
    env: &ICoreWebView2Environment,
    bytes: &[u8],
    status: i32,
    reason: &str,
    headers: &str,
) -> windows::core::Result<ICoreWebView2WebResourceResponse> {
    let stream = unsafe { CreateStreamOnHGlobal(Default::default(), true) }?;
    if !bytes.is_empty() {
        unsafe {
            stream
                .Write(bytes.as_ptr() as _, bytes.len() as u32, None)
                .ok()?;
        }
    }
    unsafe {
        env.CreateWebResourceResponse(
            &stream,
            status,
            &HSTRING::from(reason),
            &HSTRING::from(headers),
        )
    }
}

/// The page's request for one address of the app's scheme. It can be
/// held and answered later.
#[derive(Clone)]
pub struct Request(Rc<RequestInner>);

struct RequestInner {
    args: RefCell<Option<ICoreWebView2WebResourceRequestedEventArgs>>,
    env: ICoreWebView2Environment,
    deferral: RefCell<Option<ICoreWebView2Deferral>>,
    uri: String,
}

impl Request {
    pub fn uri(&self) -> Option<String> {
        Some(self.0.uri.clone())
    }

    /// Takes the request off the engine's list, or says it is gone
    /// already.
    fn take(
        &self,
    ) -> Option<(
        ICoreWebView2WebResourceRequestedEventArgs,
        ICoreWebView2Deferral,
    )> {
        let args = self.0.args.borrow_mut().take()?;
        let deferral = self.0.deferral.borrow_mut().take()?;
        Some((args, deferral))
    }

    /// Gives the page what it asked for.
    pub fn finish(&self, bytes: impl AsRef<[u8]> + Send + 'static, mime: &str) {
        let Some((args, deferral)) = self.take() else {
            return;
        };
        let headers = format!("Content-Type: {mime}\r\n");
        if let Ok(response) = respond(&self.0.env, bytes.as_ref(), 200, "OK", &headers) {
            unsafe {
                let _ = args.SetResponse(&response);
            }
        }
        unsafe {
            let _ = deferral.Complete();
        }
    }

    /// Says there is nothing at that address.
    pub fn refuse(&self) {
        let Some((args, deferral)) = self.take() else {
            return;
        };
        if let Ok(response) = respond(&self.0.env, &[], 404, "Not Found", "") {
            unsafe {
                let _ = args.SetResponse(&response);
            }
        }
        unsafe {
            let _ = deferral.Complete();
        }
    }
}

/// One `url-filter`/`resource-type` pair from a WebKit rule list.
struct Rule {
    url: Option<regex::Regex>,
    /// A bitmask of `COREWEBVIEW2_WEB_RESOURCE_CONTEXT` values the rule
    /// names, or zero where it names none.
    contexts: u32,
}

/// The compiled rules that block remote content, shared by every page.
/// WebView2 compiles no rule list, so the rules are held parsed and the
/// request handler applies them itself.
#[derive(Clone)]
pub struct Filter(Rc<Vec<Rule>>);

impl Filter {
    /// Whether `uri` asked for with `context` breaks a rule.
    pub(crate) fn blocks(&self, uri: &str, context: COREWEBVIEW2_WEB_RESOURCE_CONTEXT) -> bool {
        let bit = 1u32 << context.0;
        self.0.iter().any(|rule| {
            rule.url.as_ref().is_none_or(|url| url.is_match(uri))
                && (rule.contexts == 0 || rule.contexts & bit != 0)
        })
    }
}

/// The `resource-type` names a rule can carry, translated to the
/// context bit WebView2 reports for the same kind of load.
fn context_bit(name: &str) -> Option<u32> {
    let context = match name {
        "document" => COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT,
        "stylesheet" | "style-sheet" => COREWEBVIEW2_WEB_RESOURCE_CONTEXT_STYLESHEET,
        "image" => COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE,
        "media" => COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA,
        "font" => COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FONT,
        "script" => COREWEBVIEW2_WEB_RESOURCE_CONTEXT_SCRIPT,
        "xmlhttprequest" => COREWEBVIEW2_WEB_RESOURCE_CONTEXT_XML_HTTP_REQUEST,
        "fetch" => COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FETCH,
        "text-track" => COREWEBVIEW2_WEB_RESOURCE_CONTEXT_TEXT_TRACK,
        "eventsource" => COREWEBVIEW2_WEB_RESOURCE_CONTEXT_EVENT_SOURCE,
        "websocket" => COREWEBVIEW2_WEB_RESOURCE_CONTEXT_WEBSOCKET,
        "manifest" => COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MANIFEST,
        "signed-exchange" => COREWEBVIEW2_WEB_RESOURCE_CONTEXT_SIGNED_EXCHANGE,
        "ping" => COREWEBVIEW2_WEB_RESOURCE_CONTEXT_PING,
        "csp-report" | "csp-violation-report" => {
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_CSP_VIOLATION_REPORT
        }
        "other" | "raw" | "beacon" | "prefetch" | "svg-document" | "popup" => {
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_OTHER
        }
        _ => return None,
    };
    Some(1u32 << context.0)
}

/// Parses `rules`, WebKit's content blocker JSON, into a [`Filter`]. The
/// `dir` and `id` a WebKit build would compile under go unused: WebView2
/// keeps no compiled store.
pub async fn compile_filter(_dir: &Path, _id: &str, rules: &'static str) -> Result<Filter, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(rules).map_err(|err| format!("the rules do not parse: {err}"))?;
    let list = parsed
        .as_array()
        .ok_or_else(|| "the rules are not a list".to_string())?;
    let mut compiled = Vec::with_capacity(list.len());
    for entry in list {
        if entry.pointer("/action/type").and_then(|a| a.as_str()) != Some("block") {
            continue;
        }
        let trigger = entry.get("trigger").cloned().unwrap_or_default();
        let url = trigger
            .get("url-filter")
            .and_then(|f| f.as_str())
            .map(|pattern| {
                regex::Regex::new(pattern)
                    .map_err(|err| format!("the url-filter {pattern:?} does not parse: {err}"))
            })
            .transpose()?;
        let mut contexts = 0;
        if let Some(kinds) = trigger.get("resource-type").and_then(|t| t.as_array()) {
            for kind in kinds.iter().filter_map(|k| k.as_str()) {
                if let Some(bit) = context_bit(kind) {
                    contexts |= bit;
                }
            }
            // A list of names the map knows none of matches nothing, the
            // way WebKit would match none either.
            if kinds
                .iter()
                .all(|k| context_bit(k.as_str().unwrap_or("")).is_none())
            {
                continue;
            }
        }
        compiled.push(Rule { url, contexts });
    }
    Ok(Filter(Rc::new(compiled)))
}

/// The string a script's answer reads as. `ExecuteScript` answers the
/// value JSON-encoded, so a string unquotes and anything else keeps its
/// JSON text.
pub(crate) fn json_text(json: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(serde_json::Value::String(text)) => text,
        _ => json.to_string(),
    }
}

type Heard = Rc<dyn Fn()>;

#[derive(Default)]
struct Listeners {
    found: Option<Heard>,
    counted: Option<Rc<dyn Fn(usize)>>,
    failed: Option<Heard>,
}

/// Finds text in one page and selects the match. WebView2 has no find of
/// its own, so the window's own `find` does it and the count script
/// counts.
pub struct Finder {
    page: WeakWebView,
    query: RefCell<(String, bool)>,
    listeners: Rc<RefCell<Listeners>>,
}

impl Finder {
    fn new(page: WeakWebView) -> Finder {
        Finder {
            page,
            query: RefCell::new((String::new(), false)),
            listeners: Rc::default(),
        }
    }

    /// Selects the first match of `query` and counts them all.
    pub fn search(&self, query: &str, case_sensitive: bool) {
        *self.query.borrow_mut() = (query.to_string(), case_sensitive);
        let Some(page) = self.page.upgrade() else {
            return;
        };
        page.run(UNSELECT);
        self.count(query, case_sensitive);
        self.find(false);
    }

    /// Counts the matches again, leaving the selection where it is.
    pub fn count(&self, query: &str, case_sensitive: bool) {
        if query.is_empty() {
            let counted = self.listeners.borrow().counted.clone();
            if let Some(counted) = counted {
                counted(0);
            }
            return;
        }
        let Some(page) = self.page.upgrade() else {
            return;
        };
        let (Ok(query), Ok(sensitive)) = (
            serde_json::to_string(query),
            serde_json::to_string(&case_sensitive),
        ) else {
            return;
        };
        let listeners = Rc::clone(&self.listeners);
        page.eval(&format!("{COUNT}({query},{sensitive})"), move |answer| {
            let counted = listeners.borrow().counted.clone();
            if let (Some(counted), Ok(Ok(matches))) =
                (counted, answer.map(|value| value.parse::<usize>()))
            {
                counted(matches);
            }
        });
    }

    pub fn next(&self) {
        self.find(false);
    }

    pub fn previous(&self) {
        self.find(true);
    }

    /// Takes the selection off the page.
    pub fn finish(&self) {
        if let Some(page) = self.page.upgrade() {
            page.run(UNSELECT);
        }
    }

    fn find(&self, backwards: bool) {
        let Some(page) = self.page.upgrade() else {
            return;
        };
        let (query, sensitive) = self.query.borrow().clone();
        if query.is_empty() {
            return;
        };
        let (Ok(query), Ok(sensitive), Ok(backwards)) = (
            serde_json::to_string(&query),
            serde_json::to_string(&sensitive),
            serde_json::to_string(&backwards),
        ) else {
            return;
        };
        let listeners = Rc::clone(&self.listeners);
        page.eval(
            &format!(
                "(function () {{ return window.find({query}, {sensitive}, {backwards}, true); }})()"
            ),
            move |answer| {
                let found = answer.is_ok_and(|value| value == "true");
                let listeners = listeners.borrow();
                let heard = match found {
                    true => listeners.found.clone(),
                    false => listeners.failed.clone(),
                };
                drop(listeners);
                if let Some(heard) = heard {
                    heard();
                }
            },
        );
    }

    pub fn on_found(&self, found: impl Fn() + 'static) {
        self.listeners.borrow_mut().found = Some(Rc::new(found));
    }

    pub fn on_counted(&self, counted: impl Fn(usize) + 'static) {
        self.listeners.borrow_mut().counted = Some(Rc::new(counted));
    }

    pub fn on_failed(&self, failed: impl Fn() + 'static) {
        self.listeners.borrow_mut().failed = Some(Rc::new(failed));
    }
}

/// A one-pixel PNG, served to a page by the check below.
#[cfg(test)]
const PIXEL: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

/// Runs on the existing GTK test thread, where the engine's pump lives.
/// Every page registers its requests through the shared environment, so a
/// new page must find its own `serve` answering them, and the answer's
/// bytes must land back in the page. Skips when no WebView2 runtime is
/// on the machine.
#[cfg(test)]
pub(crate) fn check_scheme_after_last_view_closes() {
    use std::time::Duration;

    glib::MainContext::default().block_on(async {
        let window = gtk::Window::builder()
            .default_width(200)
            .default_height(200)
            .build();
        let asked = Rc::new(Cell::new(false));
        let present = Rc::new(Cell::new(false));
        {
            let asked = Rc::clone(&asked);
            let present = Rc::clone(&present);
            with_environment(move |env| {
                asked.set(true);
                present.set(env.is_some());
            });
        }
        for _ in 0..100 {
            if asked.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(50)).await;
        }
        if !present.get() {
            window.close();
            return;
        }
        for _ in 0..3 {
            let received = Rc::new(Cell::new(false));
            let answer = Rc::clone(&received);
            let page = WebView::reading("mailrs-reopen-check", move |request| {
                answer.set(true);
                request.finish(PIXEL, "image/png");
            });
            window.set_child(Some(&page.widget()));
            window.present();
            page.load_html("<img src='mailrs-reopen-check:picture'>");
            for _ in 0..100 {
                if received.get() {
                    break;
                }
                glib::timeout_future(Duration::from_millis(50)).await;
            }
            assert!(received.get(), "the new view receives its scheme request");
            let mut decoded = false;
            for _ in 0..100 {
                let (send, hear) = futures::channel::oneshot::channel();
                page.eval(
                    "String(document.querySelector('img')?.naturalWidth ?? 0)",
                    move |answer| {
                        let _ = send.send(answer);
                    },
                );
                if matches!(hear.await, Ok(Ok(width)) if width == "1") {
                    decoded = true;
                    break;
                }
                glib::timeout_future(Duration::from_millis(50)).await;
            }
            assert!(decoded, "the served bytes reach the page's picture");
            let weak = page.downgrade();
            drop(page);
            window.set_child(None::<&gtk::Widget>);
            for _ in 0..100 {
                if weak.upgrade().is_none() {
                    break;
                }
                glib::timeout_future(Duration::from_millis(50)).await;
            }
            assert!(weak.upgrade().is_none(), "the view is gone");
        }
        window.close();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn minus_cuts_the_edge_the_hole_touches() {
        let area = rect(0, 0, 100, 100);
        // A hole on the bottom edge leaves the band above it.
        assert_eq!(
            minus(&area, &rect(10, 80, 90, 100)),
            Some(rect(0, 0, 100, 80))
        );
        // A hole on the right edge leaves the band at its left.
        assert_eq!(
            minus(&area, &rect(80, 10, 100, 90)),
            Some(rect(0, 0, 80, 100))
        );
        // A hole covering the area leaves nothing.
        assert_eq!(minus(&area, &rect(0, 0, 100, 100)), None);
    }

    #[test]
    fn minus_keeps_the_biggest_remaining_piece() {
        let area = rect(0, 0, 100, 100);
        // A hole inside leaves the widest cut, here the bottom band.
        assert_eq!(
            minus(&area, &rect(30, 30, 90, 60)),
            Some(rect(0, 60, 100, 100))
        );
        // A hole past the edge still cuts, by what is left inside.
        assert_eq!(
            minus(&area, &rect(50, -20, 90, 30)),
            Some(rect(0, 30, 100, 100))
        );
    }
}
