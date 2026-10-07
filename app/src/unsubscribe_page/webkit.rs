//! The WebKitGTK engine behind the hidden page, on Linux. The run it
//! serves, and what the page may and may not do, are in
//! [`super::hidden`].

use std::rc::Rc;

use futures::future::LocalBoxFuture;
use webkit::prelude::*;

use super::hidden::{Engine, State};

pub struct WebkitEngine {
    view: webkit::WebView,
}

impl Engine for WebkitEngine {
    fn start(state: Rc<State>) -> WebkitEngine {
        // Ephemeral keeps the session's cookies and caches in memory,
        // and a session of its own keeps them away from the views that
        // show mail.
        let session = webkit::NetworkSession::new_ephemeral();
        session.connect_download_started(|_, download| download.cancel());

        let settings = webkit::Settings::new();
        // Most of these pages build their form in JavaScript, so it
        // stays on. Everything a page could use to reach past the view
        // goes off.
        settings.set_enable_javascript(true);
        settings.set_enable_javascript_markup(true);
        settings.set_javascript_can_open_windows_automatically(false);
        settings.set_enable_developer_extras(false);
        settings.set_enable_write_console_messages_to_stdout(false);
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
        settings.set_allow_universal_access_from_file_urls(false);
        // The page is read as text, so its pictures would be requests
        // with nothing to show for them.
        settings.set_auto_load_images(false);

        let view = webkit::WebView::builder()
            .network_session(&session)
            .settings(&settings)
            .build();

        let waiting = Rc::clone(&state);
        view.connect_load_changed(move |_, event| match event {
            webkit::LoadEvent::Started => {
                waiting.started();
            }
            webkit::LoadEvent::Finished => {
                waiting.finished();
            }
            _ => {}
        });
        let failing = Rc::clone(&state);
        view.connect_load_failed(move |_, _, _, error| {
            failing.arrived(Err(error.message().to_string()));
            // The page is nobody's to look at, so there is no error page
            // worth drawing.
            true
        });
        let navigating = Rc::clone(&state);
        view.connect_decide_policy(move |_, decision, kind| {
            use webkit::PolicyDecisionType as Kind;
            match kind {
                // A page that wants a second window is a page that wants
                // a pop-up.
                Kind::NewWindowAction => {
                    decision.ignore();
                    true
                }
                Kind::NavigationAction if !navigating.may_navigate() => {
                    decision.ignore();
                    true
                }
                // An answer WebKit cannot show is an answer it would put
                // on the disk instead.
                Kind::Response => {
                    let shows = decision
                        .downcast_ref::<webkit::ResponsePolicyDecision>()
                        .is_none_or(|response| response.is_mime_type_supported());
                    if !shows {
                        decision.ignore();
                    }
                    !shows
                }
                _ => false,
            }
        });
        view.connect_create(|_, _| None);
        view.connect_permission_request(|_, request| {
            request.deny();
            true
        });
        view.connect_query_permission_state(|_, query| {
            query.finish(webkit::PermissionState::Denied);
            true
        });
        view.connect_authenticate(|_, request| {
            request.cancel();
            true
        });
        view.connect_run_file_chooser(|_, request| {
            request.cancel();
            true
        });
        // Nobody is watching this view, so a dialog it raised would wait
        // for an answer that never comes. A confirm() gets what the state
        // says, an alert is kept, and a prompt gets no answer.
        let asked = Rc::clone(&state);
        view.connect_script_dialog(move |_, dialog| {
            use webkit::ScriptDialogType;
            match dialog.dialog_type() {
                ScriptDialogType::Confirm | ScriptDialogType::BeforeUnloadConfirm => {
                    dialog.confirm_set_confirmed(asked.confirms());
                }
                ScriptDialogType::Alert => {
                    if let Some(message) = dialog.message() {
                        asked.alerted(message.to_string());
                    }
                }
                _ => {}
            }
            true
        });
        view.connect_show_notification(|_, _| true);
        view.connect_print(|_, _| true);
        view.connect_context_menu(|_, _, _| true);

        WebkitEngine { view }
    }

    fn load(&self, url: &str) {
        self.view.load_uri(url);
    }

    fn at(&self) -> String {
        self.view
            .uri()
            .map(|uri| uri.to_string())
            .unwrap_or_default()
    }

    fn run(&self, script: &str) -> LocalBoxFuture<'_, Result<String, String>> {
        let script = script.to_string();
        Box::pin(async move {
            self.view
                .evaluate_javascript_future(&script, None, None)
                .await
                .map(|answer| answer.to_str().to_string())
                .map_err(|err| err.to_string())
        })
    }
}
