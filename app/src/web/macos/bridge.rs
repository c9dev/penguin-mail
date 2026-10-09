//! The Objective-C objects WebKit calls back into: one bridge per page for
//! its navigation, script messages and scheme, and the web view itself,
//! made a class of the app's own to trim its context menu.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use block2::DynBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{NSEvent, NSMenu, NSUserInterfaceItemIdentification, NSView};
use objc2_foundation::{NSNumber, NSObjectProtocol, NSRect, NSString};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate,
    WKScriptMessage, WKScriptMessageHandler, WKURLSchemeHandler, WKURLSchemeTask,
    WKUserContentController, WKWebView, WKWebViewConfiguration,
};

use super::{Inner, Request};

/// The items of WebKit's context menu a page keeps, as on Linux: copying
/// what is selected, a link or a picture. Everything else either leaves
/// the app (Look Up, Search, Share, Translate) or navigates the page.
const KEPT: [&str; 3] = [
    "WKMenuItemIdentifierCopy",
    "WKMenuItemIdentifierCopyLink",
    "WKMenuItemIdentifierCopyImage",
];

type Serve = Rc<dyn Fn(Request)>;
type Heard = Rc<dyn Fn(&str)>;

pub(super) struct BridgeIvars {
    pub owner: RefCell<Weak<Inner>>,
    pub serve: RefCell<Option<Serve>>,
    pub heard: RefCell<HashMap<String, Heard>>,
    /// The scheme tasks WebKit has not stopped yet, each by its address,
    /// with the flag its `Request` reads before it answers. WebKit throws
    /// an exception at an answer to a task it stopped.
    pub tasks: RefCell<Vec<(usize, Rc<Cell<bool>>)>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PenguinMailWebBridge"]
    #[ivars = BridgeIvars]
    pub(super) struct Bridge;

    unsafe impl NSObjectProtocol for Bridge {}

    unsafe impl WKNavigationDelegate for Bridge {
        /// Lets the page's own `about:blank` load and refuses everything
        /// else, handing the address to whoever asked to hear it, as the
        /// Linux page does.
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide(
            &self,
            _view: &WKWebView,
            action: &WKNavigationAction,
            handler: &DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            let uri = unsafe { action.request() }
                .URL()
                .and_then(|url| url.absoluteString())
                .map(|uri| uri.to_string())
                .unwrap_or_default();
            // A link that asks for a new window has no frame to load in.
            let in_frame = unsafe { action.targetFrame() }.is_some();
            if in_frame && uri == "about:blank" {
                handler.call((WKNavigationActionPolicy::Allow,));
                return;
            }
            handler.call((WKNavigationActionPolicy::Cancel,));
            if let Some(owner) = self.ivars().owner.borrow().upgrade() {
                owner.navigated(&uri);
            }
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn finished(&self, _view: &WKWebView, _navigation: *mut WKNavigation) {
            if let Some(owner) = self.ivars().owner.borrow().upgrade() {
                owner.loaded();
            }
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn terminated(&self, _view: &WKWebView) {
            if let Some(owner) = self.ivars().owner.borrow().upgrade() {
                owner.crashed();
            }
        }
    }

    unsafe impl WKScriptMessageHandler for Bridge {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn received(&self, _content: &WKUserContentController, message: &WKScriptMessage) {
            let name = unsafe { message.name() }.to_string();
            let heard = self.ivars().heard.borrow().get(&name).cloned();
            if let Some(heard) = heard {
                let body = unsafe { message.body() };
                heard(&as_text(&body));
            }
        }
    }

    unsafe impl WKURLSchemeHandler for Bridge {
        #[unsafe(method(webView:startURLSchemeTask:))]
        fn start(&self, _view: &WKWebView, task: &ProtocolObject<dyn WKURLSchemeTask>) {
            let stopped = Rc::new(Cell::new(false));
            let address = task as *const _ as *const () as usize;
            self.ivars()
                .tasks
                .borrow_mut()
                .push((address, Rc::clone(&stopped)));
            let request = Request::new(task.retain(), stopped, self.retain());
            let serve = self.ivars().serve.borrow().clone();
            match serve {
                Some(serve) => serve(request),
                None => request.refuse(),
            }
        }

        #[unsafe(method(webView:stopURLSchemeTask:))]
        fn stop(&self, _view: &WKWebView, task: &ProtocolObject<dyn WKURLSchemeTask>) {
            let address = task as *const _ as *const () as usize;
            self.ivars().tasks.borrow_mut().retain(|(at, stopped)| {
                if *at == address {
                    stopped.set(true);
                }
                *at != address
            });
        }
    }
);

impl Bridge {
    pub(super) fn new(mtm: MainThreadMarker) -> Retained<Bridge> {
        let this = Bridge::alloc(mtm).set_ivars(BridgeIvars {
            owner: RefCell::new(Weak::new()),
            serve: RefCell::new(None),
            heard: RefCell::new(HashMap::new()),
            tasks: RefCell::new(Vec::new()),
        });
        unsafe { msg_send![super(this), init] }
    }

    /// Forgets a task once its answer is in.
    pub(super) fn done(&self, stopped: &Rc<Cell<bool>>) {
        self.ivars()
            .tasks
            .borrow_mut()
            .retain(|(_, flag)| !Rc::ptr_eq(flag, stopped));
    }
}

/// What a script handed the app, as text: a string as it is, a number in
/// figures, and anything else as Foundation describes it.
pub(super) fn as_text(value: &AnyObject) -> String {
    if let Some(text) = value.downcast_ref::<NSString>() {
        return text.to_string();
    }
    if let Some(number) = value.downcast_ref::<NSNumber>() {
        return number.stringValue().to_string();
    }
    let described: Retained<NSString> = unsafe { msg_send![value, description] };
    described.to_string()
}

define_class!(
    #[unsafe(super(WKWebView, NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "PenguinMailWKWebView"]
    pub(super) struct PageView;

    impl PageView {
        #[unsafe(method(willOpenMenu:withEvent:))]
        fn will_open_menu(&self, menu: &NSMenu, event: &NSEvent) {
            let items: Vec<_> = menu.itemArray().iter().collect();
            for item in items.into_iter().rev() {
                let kept = item
                    .identifier()
                    .is_some_and(|id| KEPT.contains(&id.to_string().as_str()));
                if !kept {
                    menu.removeItem(&item);
                }
            }
            unsafe { msg_send![super(self), willOpenMenu: menu, withEvent: event] }
        }
    }
);

impl PageView {
    pub(super) fn new(
        mtm: MainThreadMarker,
        config: &WKWebViewConfiguration,
    ) -> Retained<PageView> {
        let frame = NSRect::default();
        unsafe { msg_send![PageView::alloc(mtm), initWithFrame: frame, configuration: config] }
    }
}
