//! Leaving one list through its page: read it, decide, wait for the
//! person, submit, read it again.
//!
//! The run is cut in two at the one place a person has to say yes.
//! [`prepare`] does everything that costs time and nothing that a sender
//! can see as an act: it loads the page out of sight and works out what
//! would be pressed, so the confirmation can name the button and the
//! address. [`finish`] runs after the yes and submits. A page the rules
//! and the model cannot read ends as [`Step::Browser`], which is the
//! page opening in the person's own browser, as it did before any of
//! this existed.
//!
//! [`Browser`] is the only way out of this module. The hidden WebKit
//! view is one adapter behind it and the fake is the other, so every
//! rule above tests without a widget.

use super::{Outcome, PageError, PageForm, Pick, Plan, Unsure, rules};
pub use crate::wanted::Answer;

/// The hidden view, as the run needs it.
pub trait Browser {
    /// Loads `url` out of sight and reads the page it settles on.
    fn load(&self, url: &str) -> Answer<'_, Result<PageForm, PageError>>;
    /// Fills the fields, ticks the boxes, presses the button, and reads
    /// back whatever the page says afterwards.
    fn submit(&self, plan: &Plan, address: &str) -> Answer<'_, Result<PageForm, PageError>>;
}

/// The model that answers when the rules cannot. It sees the page as
/// data and never as markup.
pub trait Adviser {
    fn advise(&self, form: &PageForm, address: &str) -> Answer<'_, Option<Plan>>;
}

/// One list's page, read and decided on, waiting for the person.
pub struct Prepared {
    /// The page as it ended up, after every redirect.
    pub url: String,
    /// The address the newsletter was sent to, and the only text that
    /// will be typed into the page.
    pub address: String,
    /// What the button about to be pressed says, so the confirmation can
    /// name it. Empty when nothing will be pressed.
    pub button: String,
    pub step: Step,
}

/// What [`finish`] will do once the person says yes.
pub enum Step {
    Submit(Plan),
    /// The page said the address is off the list on its own.
    AlreadyOff,
    /// Nobody but the person can finish this one. The address is the
    /// page to open for them.
    Browser(String),
}

/// Reads the page at `url` and decides what would be pressed. Nothing is
/// submitted here, and `adviser` is asked only where the rules give up
/// on a page that has no captcha and no login.
pub async fn prepare(
    browser: &dyn Browser,
    adviser: Option<&dyn Adviser>,
    url: &str,
    address: &str,
) -> Prepared {
    let page = match browser.load(url).await {
        Ok(page) => page,
        Err(err) => {
            tracing::info!(error = %err, url, "the unsubscribe page could not be read");
            return Prepared {
                url: url.to_string(),
                address: address.to_string(),
                button: String::new(),
                step: Step::Browser(url.to_string()),
            };
        }
    };
    let step = match rules::pick(&page, address) {
        Pick::Submit(plan) => Step::Submit(plan),
        Pick::AlreadyOff => Step::AlreadyOff,
        Pick::Unsure(Unsure::Captcha | Unsure::Login) => Step::Browser(page.url.clone()),
        Pick::Unsure(_) => match advised(adviser, &page, address).await {
            Some(plan) => Step::Submit(plan),
            None => Step::Browser(page.url.clone()),
        },
    };
    Prepared {
        address: address.to_string(),
        button: pressed(&page, &step),
        url: page.url,
        step,
    }
}

/// What the button the plan presses says. A page whose ids the plan does
/// not hold cannot happen, since every plan has been through
/// [`rules::valid`] or was written from this page, but an empty label
/// leaves the confirmation naming the site rather than guessing.
fn pressed(page: &PageForm, step: &Step) -> String {
    let Step::Submit(plan) = step else {
        return String::new();
    };
    page.forms
        .iter()
        .find(|form| form.id == plan.form)
        .and_then(|form| form.buttons.iter().find(|button| button.id == plan.press))
        .map(|button| button.label.clone())
        .unwrap_or_default()
}

/// Submits what [`prepare`] worked out and says how it ended. The page
/// after the submission has to say the person is off the list; a page
/// that says nothing leaves the outcome [`Outcome::Unclear`], because
/// the form went in and only the sender knows what it did.
pub async fn finish(browser: &dyn Browser, prepared: &Prepared) -> Outcome {
    match &prepared.step {
        Step::AlreadyOff => Outcome::Done,
        Step::Browser(url) => Outcome::OpenInBrowser(url.clone()),
        Step::Submit(plan) => match browser.submit(plan, &prepared.address).await {
            Ok(page) if rules::says_done(&page) => Outcome::Done,
            Ok(_) => Outcome::Unclear,
            Err(err) => Outcome::Failed(err.to_string()),
        },
    }
}

/// What the model makes of a page the rules could not read, held to the
/// same rules as a plan the rules wrote themselves.
async fn advised(adviser: Option<&dyn Adviser>, page: &PageForm, address: &str) -> Option<Plan> {
    let plan = adviser?.advise(page, address).await?;
    rules::valid(page, &plan, address).then_some(plan)
}
