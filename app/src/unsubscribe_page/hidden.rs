//! The hidden page that loads a real unsubscribe page, whatever engine
//! stands behind it: WebKitGTK on Linux ([`super::webkit`]) and WKWebView
//! on macOS ([`super::wkwebview`]).
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
use futures::future::{Either, LocalBoxFuture, select};
use gtk::glib;
use mailrs_domain::translate::gettext;

use super::{Answer, Browser, PageError, PageForm, Plan};

/// How long a page has to load before the run gives up on it.
const LIMIT: Duration = Duration::from_secs(20);
/// How long a page that has loaded gets before a script reads it. Many
/// of these pages draw their form from JavaScript after the load ends.
const SETTLE: Duration = Duration::from_millis(500);
/// How long a press has to take the page somewhere. Past this the page
/// answered where it stands, which is as common as posting a form.
const AFTER: Duration = Duration::from_secs(5);
/// How long a page that said nothing after the press gets before it is
/// read once more. A page that fetches its answer may still show
/// "Sending…" when the first read comes.
const AGAIN: Duration = Duration::from_millis(1500);
/// How often a page that has been pressed and gone nowhere yet is
/// looked at again.
const GLANCE: Duration = Duration::from_millis(200);

const EXTRACT: &str = include_str!("extract.js");
const SUBMIT: &str = include_str!("submit.js");
/// How much visible text the page holds. A page that answered where it
/// stands holds a different amount from the page that was pressed.
const LENGTH: &str = "String(document.body ? document.body.innerText.length : 0)";

/// What an engine does for the run: load an address, say where it stands,
/// and run a script for its answer as a string. It reports loads starting,
/// finishing and failing, and the page's alerts, to the [`State`] it was
/// started with.
pub trait Engine {
    fn start(state: Rc<State>) -> Self;
    fn load(&self, url: &str);
    fn at(&self) -> String;
    fn run(&self, script: &str) -> LocalBoxFuture<'_, Result<String, String>>;
}

/// What the engine's handlers and the run both have to know.
#[derive(Default)]
pub struct State {
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
    pub fn may_navigate(&self) -> bool {
        !self.settled.get() || self.pressing.get()
    }

    /// Whether a confirm() the page asks gets OK. The person already said
    /// yes in the app's own dialog, so during a press the page's question
    /// gets OK; answered Cancel, as an engine does when nobody answers,
    /// the page never acts. Outside a press nothing is agreed to.
    pub fn confirms(&self) -> bool {
        self.pressing.get()
    }

    /// Keeps what an alert said, since some pages say the result there.
    pub fn alerted(&self, message: String) {
        self.alerts.borrow_mut().push(message);
    }

    /// Hands whoever is waiting the news that a load started.
    pub fn started(&self) {
        if let Some(tell) = self.starting.borrow_mut().take() {
            let _ = tell.send(());
        }
    }

    /// The first page and its redirects are done.
    pub fn finished(&self) {
        self.settled.set(true);
        self.arrived(Ok(()));
    }

    /// Hands whoever is waiting the news that the page stopped loading.
    pub fn arrived(&self, answer: Result<(), String>) {
        if let Some(tell) = self.waiting.borrow_mut().take() {
            let _ = tell.send(answer);
        }
    }
}

/// The hidden page and the run that drives it.
pub struct Hidden<E> {
    engine: E,
    state: Rc<State>,
}

impl<E: Engine> Default for Hidden<E> {
    fn default() -> Hidden<E> {
        Hidden::new()
    }
}

impl<E: Engine> Hidden<E> {
    pub fn new() -> Hidden<E> {
        let state = Rc::new(State::default());
        Hidden {
            engine: E::start(Rc::clone(&state)),
            state,
        }
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
            Either::Left((Err(_), _)) => Err(PageError::Load(gettext("the view went away"))),
            Either::Right(_) => Err(PageError::Timeout),
        }
    }

    async fn run(&self, script: &str) -> Result<String, PageError> {
        self.engine.run(script).await.map_err(PageError::Script)
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

impl<E: Engine> Browser for Hidden<E> {
    fn load(&self, url: &str) -> Answer<'_, Result<PageForm, PageError>> {
        let url = url.to_string();
        Box::pin(async move {
            self.state.settled.set(false);
            self.state.pressing.set(false);
            let settled = self.watch();
            self.engine.load(&url);
            self.arrive(settled, LIMIT).await?;
            glib::timeout_future(SETTLE).await;
            self.read().await
        })
    }

    fn at(&self) -> String {
        self.engine.at()
    }

    fn reread(&self) -> Answer<'_, Result<PageForm, PageError>> {
        Box::pin(async move {
            glib::timeout_future(AGAIN).await;
            self.read().await
        })
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
        return Err(PageError::Script(gettext(
            "the plan would type something other than the address",
        )));
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
    Err(PageError::Changed)
}
