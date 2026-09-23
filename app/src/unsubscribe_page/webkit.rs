//! The hidden WebKit view, the adapter that loads a real unsubscribe
//! page.
//!
//! The view is never put in a window and never shown. It runs on a
//! network session of its own, so the cookies a newsletter's provider
//! sets never meet the ones a message's remote images set, and the
//! session is ephemeral, so nothing it collects outlives the run.
//! Downloads, pop-ups, dialogs and every permission request are refused
//! before the page can ask.
//!
//! Two scripts do all the work in the page. [`EXTRACT`] reads the page
//! into a [`PageForm`] and leaves each control marked with the id it
//! reported, and [`SUBMIT`] finds those ids again to fill, tick and
//! press. Neither hands back markup, and the only text that goes in is
//! the address the newsletter was sent to, checked here one last time
//! before a key is typed.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use futures::channel::oneshot;
use futures::future::{Either, select};
use gtk::glib;
use webkit::prelude::*;

use super::{Answer, Browser, PageError, PageForm, Plan};

/// How long a page has to load before the run gives up on it.
const LIMIT: Duration = Duration::from_secs(20);
/// How long a page that has loaded gets before a script reads it. Many
/// of these pages draw their form from JavaScript after the load ends.
const SETTLE: Duration = Duration::from_millis(500);
/// How long a press has to take the page somewhere. Past this the page
/// answered where it stands, which is as common as posting a form.
const AFTER: Duration = Duration::from_secs(5);
/// How often a page that has been pressed and gone nowhere yet is
/// looked at again.
const GLANCE: Duration = Duration::from_millis(200);

const EXTRACT: &str = include_str!("extract.js");
const SUBMIT: &str = include_str!("submit.js");
/// How much visible text the page holds. A page that answered where it
/// stands holds a different amount from the page that was pressed.
const LENGTH: &str = "String(document.body ? document.body.innerText.length : 0)";

pub struct WebkitBrowser {
    view: webkit::WebView,
    state: Rc<State>,
}

/// What the signal handlers and the run both have to know.
struct State {
    /// Set once the first page has finished loading. Until then the
    /// chain of redirects an unsubscribe link starts may go wherever it
    /// likes, including to another site.
    settled: Cell<bool>,
    /// Set while a submission is on its way. Everything the press sets
    /// off belongs to it, which covers a form posting to its provider's
    /// own domain and the redirect that often follows.
    pressing: Cell<bool>,
    /// What the page's alerts said during the press. Some pages announce
    /// the result in an alert rather than on the page itself.
    alerts: RefCell<Vec<String>>,
    /// Whoever is waiting for the page to start loading.
    starting: RefCell<Option<oneshot::Sender<()>>>,
    /// Whoever is waiting for the page to finish loading.
    waiting: RefCell<Option<oneshot::Sender<Result<(), String>>>>,
}

impl State {
    /// Whether the page may go somewhere: while the first page is still
    /// settling, and while a submission is on its way. A page that goes
    /// somewhere on its own, once it has been read and before anything
    /// was pressed, is a page taking the reader out of the run.
    fn may_navigate(&self) -> bool {
        !self.settled.get() || self.pressing.get()
    }

    /// Hands whoever is waiting the news that the page stopped loading.
    fn arrived(&self, answer: Result<(), String>) {
        if let Some(tell) = self.waiting.borrow_mut().take() {
            let _ = tell.send(answer);
        }
    }
}

impl Default for WebkitBrowser {
    fn default() -> WebkitBrowser {
        WebkitBrowser::new()
    }
}

impl WebkitBrowser {
    pub fn new() -> WebkitBrowser {
        let state = Rc::new(State {
            settled: Cell::new(false),
            pressing: Cell::new(false),
            alerts: RefCell::new(Vec::new()),
            starting: RefCell::new(None),
            waiting: RefCell::new(None),
        });

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
                if let Some(tell) = waiting.starting.borrow_mut().take() {
                    let _ = tell.send(());
                }
            }
            webkit::LoadEvent::Finished => {
                waiting.settled.set(true);
                waiting.arrived(Ok(()));
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
        // for an answer that never comes.
        // A page may ask "Are you sure?" with confirm() once its button is
        // pressed. The person already said yes in the app's own dialog, so
        // during a press the page's question gets OK; answered Cancel, as
        // WebKit does when nobody answers, the page never acts. Outside a
        // press nothing is agreed to. An alert is kept, since some pages
        // say the result there, and a prompt gets no answer.
        let asked = Rc::clone(&state);
        view.connect_script_dialog(move |_, dialog| {
            use webkit::ScriptDialogType;
            match dialog.dialog_type() {
                ScriptDialogType::Confirm | ScriptDialogType::BeforeUnloadConfirm => {
                    dialog.confirm_set_confirmed(asked.pressing.get());
                }
                ScriptDialogType::Alert => {
                    if let Some(message) = dialog.message() {
                        asked.alerts.borrow_mut().push(message.to_string());
                    }
                }
                _ => {}
            }
            true
        });
        view.connect_show_notification(|_, _| true);
        view.connect_print(|_, _| true);
        view.connect_context_menu(|_, _, _| true);

        WebkitBrowser { view, state }
    }

    /// Takes the place of whoever was waiting for a load, and hands back
    /// the way to hear about the next one.
    fn watch(&self) -> oneshot::Receiver<Result<(), String>> {
        let (tell, hear) = oneshot::channel();
        *self.state.waiting.borrow_mut() = Some(tell);
        hear
    }

    /// The same, for the moment a load starts rather than ends.
    fn watch_start(&self) -> oneshot::Receiver<()> {
        let (tell, hear) = oneshot::channel();
        *self.state.starting.borrow_mut() = Some(tell);
        hear
    }

    /// Whether the press took the page somewhere, waited out in slices
    /// rather than in one go.
    ///
    /// A page that answers where it stands never sets off at all, and
    /// waiting [`AFTER`] out for it would put five seconds between the
    /// person's yes and the answer. Such a page says it has answered by
    /// holding a different amount of text from the page that was
    /// pressed, so the run glances at the text between slices and stops
    /// as soon as it changes. A navigation still wins: it is looked for
    /// first, and a page being replaced changes its text too.
    async fn went(&self, mut hear: oneshot::Receiver<()>, before: usize) -> bool {
        let mut left = AFTER;
        loop {
            match hear.try_recv() {
                Ok(Some(())) => return true,
                // Nobody is left to say the page set off.
                Err(_) => return false,
                Ok(None) => {}
            }
            // A page mid-navigation may answer nothing at all, and that
            // is not an answer where it stands.
            if let Some(now) = self.length().await
                && now != before
            {
                return false;
            }
            let Some(rest) = left.checked_sub(GLANCE) else {
                return false;
            };
            left = rest;
            glib::timeout_future(GLANCE).await;
        }
    }

    /// How much visible text the page holds, or nothing when it would
    /// not say.
    async fn length(&self) -> Option<usize> {
        self.run(LENGTH).await.ok()?.trim().parse().ok()
    }

    /// Waits for the page to stop loading, for at most `limit`.
    async fn arrive(
        &self,
        hear: oneshot::Receiver<Result<(), String>>,
        limit: Duration,
    ) -> Result<(), PageError> {
        match select(hear, glib::timeout_future(limit)).await {
            Either::Left((Ok(Ok(())), _)) => Ok(()),
            Either::Left((Ok(Err(why)), _)) => Err(PageError::Load(why)),
            Either::Left((Err(_), _)) => Err(PageError::Load("the view went away".to_string())),
            Either::Right(_) => Err(PageError::Timeout),
        }
    }

    async fn run(&self, script: &str) -> Result<String, PageError> {
        let answer = self
            .view
            .evaluate_javascript_future(script, None, None)
            .await
            .map_err(|err| PageError::Script(err.to_string()))?;
        Ok(answer.to_str().to_string())
    }

    async fn read(&self) -> Result<PageForm, PageError> {
        let json = self.run(EXTRACT).await?;
        serde_json::from_str(&json).map_err(|err| PageError::Script(err.to_string()))
    }

    /// What the page says once the press has had its moment. A press
    /// posts the form or answers where it stands, and a page that goes
    /// nowhere is not a failure: whatever it shows afterwards is what
    /// gets read either way.
    async fn pressed(
        &self,
        answer: Result<String, PageError>,
        started: oneshot::Receiver<()>,
        landed: oneshot::Receiver<Result<(), String>>,
        before: usize,
    ) -> Result<PageForm, PageError> {
        held(&answer?)?;
        if self.went(started, before).await {
            // The press took the page somewhere, and where it lands has
            // the same twenty seconds the first page had.
            self.arrive(landed, LIMIT).await?;
        }
        glib::timeout_future(SETTLE).await;
        let mut page = self.read().await?;
        let alerts = self.state.alerts.take();
        if !alerts.is_empty() {
            page.text.push('\n');
            page.text.push_str(&alerts.join("\n"));
        }
        Ok(page)
    }
}

impl Browser for WebkitBrowser {
    fn load(&self, url: &str) -> Answer<'_, Result<PageForm, PageError>> {
        let url = url.to_string();
        Box::pin(async move {
            self.state.settled.set(false);
            self.state.pressing.set(false);
            let settled = self.watch();
            self.view.load_uri(&url);
            self.arrive(settled, LIMIT).await?;
            glib::timeout_future(SETTLE).await;
            self.read().await
        })
    }

    fn at(&self) -> String {
        self.view
            .uri()
            .map(|uri| uri.to_string())
            .unwrap_or_default()
    }

    fn submit(&self, plan: &Plan, address: &str) -> Answer<'_, Result<PageForm, PageError>> {
        let script = orders(plan, address);
        Box::pin(async move {
            let script = script?;
            // Read before the press, so that a page which answers where
            // it stands can be told from one still thinking about it.
            let before = self.length().await.unwrap_or_default();
            let started = self.watch_start();
            let landed = self.watch();
            self.state.alerts.borrow_mut().clear();
            self.state.pressing.set(true);
            let answer = self.run(&script).await;
            let page = self.pressed(answer, started, landed, before).await;
            self.state.pressing.set(false);
            page
        })
    }
}

/// The script that carries out one plan. The address is checked here
/// once more, so a plan that would type anything else never reaches the
/// page, whoever wrote it.
fn orders(plan: &Plan, address: &str) -> Result<String, PageError> {
    if plan.fill.iter().any(|(_, text)| text != address) {
        return Err(PageError::Script(
            "the plan would type something other than the address".to_string(),
        ));
    }
    let plan = serde_json::to_string(plan)
        .map_err(|err| PageError::Script(format!("the plan would not write out: {err}")))?;
    let quoted = serde_json::to_string(&plan)
        .map_err(|err| PageError::Script(format!("the plan would not write out: {err}")))?;
    Ok(format!("({SUBMIT})({quoted})"))
}

/// What the submission script says the page still holds. A page that
/// rebuilt itself between the reading and the press loses the ids, and
/// then nothing was pressed and the run says so.
fn held(answer: &str) -> Result<(), PageError> {
    #[derive(serde::Deserialize)]
    struct Done {
        missing: Vec<usize>,
    }
    let done: Done = serde_json::from_str(answer)
        .map_err(|err| PageError::Script(format!("the page answered {answer:?}: {err}")))?;
    if done.missing.is_empty() {
        return Ok(());
    }
    Err(PageError::Script(
        "the page changed while it was waiting to be asked".to_string(),
    ))
}
