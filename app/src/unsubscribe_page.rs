//! The unsubscribe page a newsletter sends people to, read as data and
//! decided on before anything is pressed.
//!
//! One injected script turns the page into a [`PageForm`]: its address,
//! its title, its visible text, and the fields and buttons of its forms.
//! The script hands back no markup, so neither the rules here nor a model
//! ever see what the sender wrote. [`pick`] reads that data and answers
//! either a [`Plan`], which names by id the fields to fill, the boxes to
//! tick and the button to press, or that it is [`Unsure`]. A model may
//! answer a plan of its own when the rules cannot, and [`valid`] holds it
//! to the same rules, so the only text that reaches a page is the address
//! the newsletter was sent to.
//!
//! Nothing here starts a widget or a network request. [`Browser`] is the
//! one way out to WebKit, with the hidden view behind it and a table in
//! memory behind the fake, and [`prepare`] and [`finish`] are the run the
//! window and the assistant share: prepare reads the page and decides,
//! the person confirms, finish submits and reads the page back.
//!
//! The words a person reads are the window's, not this module's. An
//! [`Outcome`] says what happened in plain English for the log and for
//! the assistant's answer, and whoever shows it writes it out through
//! `translate`.

// The window, the assistant and the WebKit adapter reach this module in
// the steps that follow; until they do, the compiler sees its types and
// its re-exports as unused.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

mod rules;
mod run;
pub mod words;

#[cfg(test)]
pub mod fake;
#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use rules::{pick, says_done, valid};
#[allow(unused_imports)]
pub use run::{Adviser, Answer, Browser, Prepared, Step, finish, prepare};

/// A page as the extraction script reports it. Every id in it is the
/// script's own, unique across the page, and a [`Plan`] names nothing
/// else.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PageForm {
    /// Where the page ended up, after every redirect.
    pub url: String,
    /// The page's title, cut to 120 characters.
    pub title: String,
    /// The visible text, cut to 2,000 characters.
    pub text: String,
    /// The forms on the page, at most five. A link styled as a button
    /// whose text reads as unsubscribe counts as a form with no fields.
    pub forms: Vec<Form>,
    /// A known captcha frame or widget is on the page.
    pub captcha: bool,
    /// A password field is on the page, so it wants an account.
    pub password: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Form {
    pub id: usize,
    pub fields: Vec<Field>,
    pub buttons: Vec<Button>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Field {
    pub id: usize,
    pub kind: FieldKind,
    /// The associated `<label>`, `aria-label`, placeholder or nearby
    /// text, cut to 80 characters. Empty when the page labels nothing.
    pub label: String,
    /// What the field holds already. A field the page filled in is left
    /// as it is.
    pub value: String,
    pub checked: bool,
    pub required: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Email,
    #[default]
    Text,
    Checkbox,
    Radio {
        group: String,
    },
    Select {
        options: Vec<String>,
    },
    Hidden,
    Password,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Button {
    pub id: usize,
    /// The button's text or `aria-label`, cut to 80 characters.
    pub label: String,
}

/// What to do to one form of a page, by id alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Plan {
    /// The form to act on, by [`Form::id`].
    pub form: usize,
    /// The fields to type into, by [`Field::id`]. The text is always the
    /// address the newsletter was sent to, and [`valid`] refuses
    /// anything else.
    pub fill: Vec<(usize, String)>,
    /// The checkboxes and radios that must end up ticked, by
    /// [`Field::id`]. One the page ticked already stays ticked, so
    /// submitting a plan twice does the same thing once.
    pub tick: Vec<usize>,
    /// The button to press, by [`Button::id`].
    pub press: usize,
}

/// What the rules, or a model, make of a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    Submit(Plan),
    /// The page says the address is off the list already, which some
    /// links do on load.
    AlreadyOff,
    Unsure(Unsure),
}

/// Why nothing can be pressed without the person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsure {
    Captcha,
    Login,
    NoForm,
    Ambiguous,
}

/// Why a page could not be read or submitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageError {
    Timeout,
    Load(String),
    Script(String),
}

impl std::fmt::Display for PageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PageError::Timeout => write!(f, "the page took longer than 20 seconds"),
            PageError::Load(why) => write!(f, "the page did not load: {why}"),
            PageError::Script(why) => write!(f, "the page could not be read: {why}"),
        }
    }
}

/// How leaving one list ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// The form went in and the page said nothing either way.
    Unclear,
    Failed(String),
    /// Nobody but the person can finish this one. The address is the
    /// page to open for them.
    OpenInBrowser(String),
}
