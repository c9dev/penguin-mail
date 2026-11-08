//! The WebView2 engine behind the hidden page, on Windows. The run it
//! serves, and what the page may and may not do, are in
//! [`super::hidden`].
//!
//! The page's controller hangs on a message-only window, so it renders
//! nothing and reaches no screen. Each engine gets a working folder of
//! its own under the temp dir, the way the macOS engine's non-persistent
//! store keeps the run's cookies out of everything else; the folder is
//! removed when the engine drops, best effort, since the browser may
//! still hold it while its processes wind down.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::channel::oneshot;
use futures::future::LocalBoxFuture;
use gtk::glib;
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    ExecuteScriptCompletedHandler, NavigationCompletedEventHandler, NavigationStartingEventHandler,
    NewWindowRequestedEventHandler, PermissionRequestedEventHandler,
    ScriptDialogOpeningEventHandler, WebResourceRequestedEventHandler,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
};
use windows::core::{HSTRING, PWSTR};

use super::hidden::{Engine, State};

// The hidden page must never signal an open by fetching a tracking picture.
const BLOCK_IMAGES: &str =
    r#"[{"trigger":{"url-filter":".*","resource-type":["image"]},"action":{"type":"block"}}]"#;

/// Numbers the engines' working folders apart within one process.
static FOLDER: AtomicU64 = AtomicU64::new(0);

struct Core {
    controller: ICoreWebView2Controller,
    web: ICoreWebView2,
    env: ICoreWebView2Environment,
}

type Op = Box<dyn FnOnce(&Core)>;

struct Inner {
    state: Rc<State>,
    core: RefCell<Option<Rc<Core>>>,
    /// Calls made before the page exists, in the order they were made.
    queued: RefCell<Vec<Op>>,
    /// Why the page never came up, when the runtime or the controller
    /// would not start. Checked by callers so a run hears the miss.
    failed: RefCell<Option<String>>,
    filter: RefCell<Option<crate::web::Filter>>,
    /// The message-only window the page's own windows parent to.
    window: Cell<HWND>,
    dir: PathBuf,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(core) = self.core.borrow().as_ref() {
            unsafe {
                let _ = core.controller.Close();
            }
        }
        if !self.window.get().is_invalid() {
            unsafe {
                let _ = DestroyWindow(self.window.get());
            }
        }
        // The folder is the browser's working set; it may still hold it
        // while its processes wind down, so a failed delete is left for
        // the temp cleaner.
        let _ = std::fs::remove_dir_all(&self.dir);
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

    /// Says the page will never come up, through every channel a run
    /// might be listening on.
    fn dead(&self, why: String) {
        *self.failed.borrow_mut() = Some(why.clone());
        self.state.arrived(Err(why));
    }

    /// The settings and events the hidden page gets once it exists.
    fn wire(self: &Rc<Inner>, core: &Rc<Core>) {
        let web = &core.web;
        unsafe {
            if let Ok(settings) = web.Settings() {
                // These pages build their form in JavaScript, so it stays
                // on. Everything a page could use to reach past the view
                // goes off.
                let _ = settings.SetIsScriptEnabled(true);
                let _ = settings.SetIsWebMessageEnabled(false);
                let _ = settings.SetAreHostObjectsAllowed(false);
                let _ = settings.SetAreDefaultContextMenusEnabled(false);
                // The runtime's own dialog would hang a page nobody can
                // see: off, so the answer comes through
                // ScriptDialogOpening instead.
                let _ = settings.SetAreDefaultScriptDialogsEnabled(false);
                let _ = settings.SetAreDevToolsEnabled(false);
                let _ = settings.SetIsStatusBarEnabled(false);
                let _ = settings.SetIsZoomControlEnabled(false);
            }
            let _ = web.AddWebResourceRequestedFilter(
                &HSTRING::from("*"),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
            );
            let _ = core.controller.SetIsVisible(false);

            let inner = Rc::downgrade(self);
            let handler = WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                let (Some(inner), Some(args)) = (inner.upgrade(), args) else {
                    return Ok(());
                };
                let Some(filter) = inner.filter.borrow().clone() else {
                    return Ok(());
                };
                let (Ok(request), Some(env)) = (
                    args.Request(),
                    inner.core.borrow().as_ref().map(|core| core.env.clone()),
                ) else {
                    return Ok(());
                };
                let mut raw = PWSTR::null();
                let _ = request.Uri(&mut raw);
                let uri = if raw.is_null() {
                    String::new()
                } else {
                    raw.to_string().unwrap_or_default()
                };
                let mut context = COREWEBVIEW2_WEB_RESOURCE_CONTEXT::default();
                let _ = args.ResourceContext(&mut context);
                if !filter.blocks(&uri, context) {
                    return Ok(());
                }
                let refused = crate::web::respond(&env, &[], 404, "Not Found", "");
                if let Ok(refused) = refused {
                    let _ = args.SetResponse(&refused);
                }
                Ok(())
            }));
            let mut token = 0;
            let _ = web.add_WebResourceRequested(&handler, &mut token);

            let inner = Rc::downgrade(self);
            let handler = NavigationStartingEventHandler::create(Box::new(move |_, args| {
                let (Some(inner), Some(args)) = (inner.upgrade(), args) else {
                    return Ok(());
                };
                if inner.state.may_navigate() {
                    inner.state.started();
                } else {
                    let _ = args.SetCancel(true);
                }
                Ok(())
            }));
            let mut token = 0;
            let _ = web.add_NavigationStarting(&handler, &mut token);

            let inner = Rc::downgrade(self);
            let handler = NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                let (Some(inner), Some(args)) = (inner.upgrade(), args) else {
                    return Ok(());
                };
                let mut done = windows::core::BOOL::default();
                let _ = args.IsSuccess(&mut done);
                if done.as_bool() {
                    inner.state.finished();
                } else {
                    let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
                    let _ = args.WebErrorStatus(&mut status);
                    inner
                        .state
                        .arrived(Err(format!("the page failed: {status:?}")));
                }
                Ok(())
            }));
            let mut token = 0;
            let _ = web.add_NavigationCompleted(&handler, &mut token);

            // No popups and no permissions: a hidden page wants neither.
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

            let inner = Rc::downgrade(self);
            let handler = ScriptDialogOpeningEventHandler::create(Box::new(move |_, args| {
                let (Some(inner), Some(args)) = (inner.upgrade(), args) else {
                    return Ok(());
                };
                let mut kind = COREWEBVIEW2_SCRIPT_DIALOG_KIND::default();
                let _ = args.Kind(&mut kind);
                if kind == COREWEBVIEW2_SCRIPT_DIALOG_KIND_ALERT {
                    let mut raw = PWSTR::null();
                    let _ = args.Message(&mut raw);
                    if !raw.is_null() {
                        inner.state.alerted(raw.to_string().unwrap_or_default());
                    }
                    let _ = args.Accept();
                } else if kind == COREWEBVIEW2_SCRIPT_DIALOG_KIND_CONFIRM && inner.state.confirms()
                {
                    let _ = args.Accept();
                }
                // Anything else, and a confirm outside a press, is
                // answered with cancel: the handler ending without an
                // Accept dismisses the dialog.
                Ok(())
            }));
            let mut token = 0;
            let _ = web.add_ScriptDialogOpening(&handler, &mut token);
        }
    }
}

/// The hidden page, on the WebView2 runtime shared with the rest of the
/// build's web pages only in name: the engine holds an environment of
/// its own so the run's data never meets the app's.
pub struct WvEngine {
    inner: Rc<Inner>,
}

impl Engine for WvEngine {
    fn start(state: Rc<State>) -> WvEngine {
        // A message-only parent keeps the page's own window off every
        // screen.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                windows::core::w!("STATIC"),
                windows::core::PCWSTR::null(),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
        }
        .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "penguin-mail-webview2-unsub-{}-{}",
            std::process::id(),
            FOLDER.fetch_add(1, Ordering::Relaxed),
        ));
        let inner = Rc::new(Inner {
            state,
            core: RefCell::new(None),
            queued: RefCell::new(Vec::new()),
            failed: RefCell::new(None),
            filter: RefCell::new(None),
            window: Cell::new(window),
            dir: dir.clone(),
        });
        let weak = Rc::downgrade(&inner);
        let handler =
            CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |result, env| {
                let Some(inner) = weak.upgrade() else {
                    return Ok(());
                };
                let Some(env) = result.ok().and(env) else {
                    inner.dead("the web engine would not start".to_string());
                    return Ok(());
                };
                let weak = Rc::downgrade(&inner);
                let env2 = env.clone();
                let done = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
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
                                inner.wire(&core);
                                let ops = std::mem::take(&mut *inner.queued.borrow_mut());
                                *inner.core.borrow_mut() = Some(core.clone());
                                for op in ops {
                                    op(&core);
                                }
                            }
                            None => inner.dead("the web engine made no page".to_string()),
                        }
                        Ok(())
                    },
                ));
                unsafe {
                    let _ = env.CreateCoreWebView2Controller(inner.window.get(), &done);
                }
                Ok(())
            }));
        let made = unsafe {
            CreateCoreWebView2EnvironmentWithOptions(
                windows::core::PCWSTR::null(),
                &HSTRING::from(&*dir.to_string_lossy()),
                None,
                &handler,
            )
        };
        if let Err(err) = made {
            inner.dead(format!("the web engine would not start: {err}"));
        }
        WvEngine { inner }
    }

    fn load(&self, url: &str) {
        if let Some(why) = self.inner.failed.borrow().clone() {
            self.inner.state.arrived(Err(why));
            return;
        }
        let url = url.to_string();
        let state = Rc::clone(&self.inner.state);
        let inner = Rc::clone(&self.inner);
        self.inner.with_core(move |core| {
            let web = core.web.clone();
            glib::spawn_future_local(async move {
                let filter = crate::web::compile_filter(
                    Path::new(""),
                    "unsubscribe-block-images",
                    BLOCK_IMAGES,
                )
                .await;
                match filter {
                    Ok(filter) => {
                        *inner.filter.borrow_mut() = Some(filter);
                        unsafe {
                            let _ = web.Navigate(&HSTRING::from(&url));
                        }
                    }
                    Err(why) => state.arrived(Err(why)),
                }
            });
        });
    }

    fn at(&self) -> String {
        self.inner
            .core
            .borrow()
            .as_ref()
            .and_then(|core| unsafe {
                let mut raw = PWSTR::null();
                core.web.Source(&mut raw).ok()?;
                if raw.is_null() {
                    None
                } else {
                    raw.to_string().ok()
                }
            })
            .unwrap_or_default()
    }

    fn run(&self, script: &str) -> LocalBoxFuture<'_, Result<String, String>> {
        let (tell, hear) = oneshot::channel();
        let tell = Cell::new(Some(tell));
        if let Some(why) = self.inner.failed.borrow().clone() {
            return Box::pin(async move { Err(why) });
        }
        let script = script.to_string();
        self.inner.with_core(move |core| unsafe {
            let handler = ExecuteScriptCompletedHandler::create(Box::new(move |result, json| {
                if let Some(tell) = tell.take() {
                    let answer = match result {
                        Ok(()) => Ok(crate::web::json_text(&json)),
                        Err(err) => Err(err.to_string()),
                    };
                    let _ = tell.send(answer);
                }
                Ok(())
            }));
            let _ = core.web.ExecuteScript(&HSTRING::from(&script), &handler);
        });
        Box::pin(async move {
            hear.await
                .unwrap_or_else(|_| Err("the page went away".to_string()))
        })
    }
}
