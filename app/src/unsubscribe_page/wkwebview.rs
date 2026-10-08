//! The WKWebView engine behind the hidden page, on macOS. The run it
//! serves, and what the page may and may not do, are in
//! [`super::hidden`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use block2::{DynBlock, RcBlock};
use futures::future::LocalBoxFuture;
use gtk::glib;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{
    NSError, NSNumber, NSObjectProtocol, NSRect, NSString, NSURL, NSURLRequest,
};
use objc2_web_kit::{
    WKFrameInfo, WKMediaCaptureType, WKNavigation, WKNavigationAction,
    WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationResponse,
    WKNavigationResponsePolicy, WKPermissionDecision, WKSecurityOrigin, WKUIDelegate, WKWebView,
    WKWebViewConfiguration, WKWebsiteDataStore,
};

use super::hidden::{Engine, State};

// The hidden page must never signal an open by fetching a tracking picture.
const BLOCK_IMAGES: &str = r#"[{"trigger":{"url-filter":".*","resource-type":["image"]},"action":{"type":"block"}}]"#;

pub struct WkEngine {
    view: Retained<WKWebView>,
    /// WebKit holds its delegates weakly.
    _keeper: Retained<Keeper>,
    loading: RefCell<Option<glib::JoinHandle<()>>>,
}

impl Drop for WkEngine {
    fn drop(&mut self) {
        if let Some(load) = self.loading.get_mut().take() {
            load.abort();
        }
    }
}

pub struct KeeperIvars {
    state: Rc<State>,
}

define_class!(
    /// Answers for the hidden page: where it may go, what it may show, and
    /// the dialogs and permission requests nobody is there to answer.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PenguinMailUnsubscribeKeeper"]
    #[ivars = KeeperIvars]
    pub struct Keeper;

    unsafe impl NSObjectProtocol for Keeper {}

    unsafe impl WKNavigationDelegate for Keeper {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide(
            &self,
            _view: &WKWebView,
            action: &WKNavigationAction,
            handler: &DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            // A page that wants a second window is a page that wants a
            // pop-up.
            let popup = unsafe { action.targetFrame() }.is_none();
            let policy = match popup || !self.ivars().state.may_navigate() {
                true => WKNavigationActionPolicy::Cancel,
                false => WKNavigationActionPolicy::Allow,
            };
            handler.call((policy,));
        }

        /// An answer WebKit cannot show is an answer it would put on the
        /// disk instead.
        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn decide_response(
            &self,
            _view: &WKWebView,
            response: &WKNavigationResponse,
            handler: &DynBlock<dyn Fn(WKNavigationResponsePolicy)>,
        ) {
            let policy = match unsafe { response.canShowMIMEType() } {
                true => WKNavigationResponsePolicy::Allow,
                false => WKNavigationResponsePolicy::Cancel,
            };
            handler.call((policy,));
        }

        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        fn started(&self, _view: &WKWebView, _navigation: *mut WKNavigation) {
            self.ivars().state.started();
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn finished(&self, _view: &WKWebView, _navigation: *mut WKNavigation) {
            self.ivars().state.finished();
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn failed_early(&self, _view: &WKWebView, _navigation: *mut WKNavigation, error: &NSError) {
            self.ivars()
                .state
                .arrived(Err(error.localizedDescription().to_string()));
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn failed(&self, _view: &WKWebView, _navigation: *mut WKNavigation, error: &NSError) {
            self.ivars()
                .state
                .arrived(Err(error.localizedDescription().to_string()));
        }
    }

    unsafe impl WKUIDelegate for Keeper {
        #[unsafe(method(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn alert(
            &self,
            _view: &WKWebView,
            message: &NSString,
            _frame: &WKFrameInfo,
            handler: &DynBlock<dyn Fn()>,
        ) {
            self.ivars().state.alerted(message.to_string());
            handler.call(());
        }

        #[unsafe(method(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn confirm(
            &self,
            _view: &WKWebView,
            _message: &NSString,
            _frame: &WKFrameInfo,
            handler: &DynBlock<dyn Fn(Bool)>,
        ) {
            handler.call((Bool::new(self.ivars().state.confirms()),));
        }

        #[unsafe(method(webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:completionHandler:))]
        fn prompt(
            &self,
            _view: &WKWebView,
            _prompt: &NSString,
            _default: *mut NSString,
            _frame: &WKFrameInfo,
            handler: &DynBlock<dyn Fn(*mut NSString)>,
        ) {
            handler.call((std::ptr::null_mut(),));
        }

        #[unsafe(method(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:))]
        fn capture(
            &self,
            _view: &WKWebView,
            _origin: &WKSecurityOrigin,
            _frame: &WKFrameInfo,
            _kind: WKMediaCaptureType,
            handler: &DynBlock<dyn Fn(WKPermissionDecision)>,
        ) {
            handler.call((WKPermissionDecision::Deny,));
        }
    }
);

impl Engine for WkEngine {
    fn start(state: Rc<State>) -> WkEngine {
        let mtm = MainThreadMarker::new().expect("the hidden page lives on the GTK thread");
        let config = unsafe { WKWebViewConfiguration::new(mtm) };
        unsafe {
            // A data store of its own, kept in memory, so the cookies a
            // newsletter's provider sets never meet the ones a message's
            // remote images set and nothing outlives the run.
            config.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(mtm));
            // Most of these pages build their form in JavaScript, so it
            // stays on. Everything a page could use to reach past the
            // view goes off.
            config
                .preferences()
                .setJavaScriptCanOpenWindowsAutomatically(false);
            config.preferences().setElementFullscreenEnabled(false);
            config.setMediaTypesRequiringUserActionForPlayback(
                objc2_web_kit::WKAudiovisualMediaTypes::All,
            );
        }
        let view = unsafe {
            WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), NSRect::default(), &config)
        };
        let keeper = Keeper::alloc(mtm).set_ivars(KeeperIvars { state });
        let keeper: Retained<Keeper> = unsafe { msg_send![super(keeper), init] };
        unsafe {
            view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*keeper)));
            view.setUIDelegate(Some(ProtocolObject::from_ref(&*keeper)));
        }
        WkEngine {
            view,
            _keeper: keeper,
            loading: RefCell::new(None),
        }
    }

    fn load(&self, url: &str) {
        if let Some(load) = self.loading.borrow_mut().take() {
            load.abort();
        }
        let address = NSString::from_str(url);
        let Some(target) = NSURL::URLWithString(&address) else {
            self._keeper.ivars().state.arrived(Err(format!("not an address: {url}")));
            return;
        };
        let view = self.view.clone();
        let state = Rc::clone(&self._keeper.ivars().state);
        let load = glib::spawn_future_local(async move {
            let dir = glib::user_cache_dir()
                .join(mailrs_sync::config::DIR_NAME)
                .join("content-filters");
            let filter = async {
                std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
                crate::web::compile_filter(&dir, "unsubscribe-block-images", BLOCK_IMAGES).await
            }.await;
            let filter = match filter {
                Ok(filter) => filter,
                Err(err) => {
                    state.arrived(Err(err));
                    return;
                }
            };
            unsafe {
                let content = view.configuration().userContentController();
                content.removeAllContentRuleLists();
                content.addContentRuleList(&filter.0);
                if target.isFileURL() {
                    // A file may read its own folder and nothing beside it.
                    let folder = target.URLByDeletingLastPathComponent().unwrap_or(target.clone());
                    view.loadFileURL_allowingReadAccessToURL(&target, &folder);
                } else {
                    view.loadRequest(&NSURLRequest::requestWithURL(&target));
                }
            }
        });
        *self.loading.borrow_mut() = Some(load);
    }

    fn at(&self) -> String {
        unsafe { self.view.URL() }
            .and_then(|url| url.absoluteString())
            .map(|uri| uri.to_string())
            .unwrap_or_default()
    }

    fn run(&self, script: &str) -> LocalBoxFuture<'_, Result<String, String>> {
        let (tell, hear) = futures::channel::oneshot::channel();
        let tell = Cell::new(Some(tell));
        let done = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
            let answer = match unsafe { error.as_ref() } {
                Some(error) => Err(error.localizedDescription().to_string()),
                None => Ok(unsafe { value.as_ref() }.map(as_text).unwrap_or_default()),
            };
            if let Some(tell) = tell.take() {
                let _ = tell.send(answer);
            }
        });
        unsafe {
            self.view
                .evaluateJavaScript_completionHandler(&NSString::from_str(script), Some(&done));
        }
        Box::pin(async move {
            hear.await
                .unwrap_or_else(|_| Err("the page went away".to_string()))
        })
    }
}

/// What a script answered, as text.
fn as_text(value: &AnyObject) -> String {
    if let Some(text) = value.downcast_ref::<NSString>() {
        return text.to_string();
    }
    if let Some(number) = value.downcast_ref::<NSNumber>() {
        return number.stringValue().to_string();
    }
    String::new()
}
