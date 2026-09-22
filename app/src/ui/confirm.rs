//! The two-button question: Cancel, or one verb that goes ahead. Dialogs
//! with three or more choices, or a field to fill in, build their own
//! `adw::AlertDialog`.

use adw::prelude::*;
use mailrs_domain::translate::gettext;

/// How the verb's button looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Red, for a step nothing brings back.
    Destructive,
    /// Blue, for the step the dialog recommends.
    Suggested,
}

/// A question waiting to go on screen with [`Confirm::ask`].
pub struct Confirm {
    dialog: adw::AlertDialog,
}

/// The response id of the verb's button. Cancel and closing the dialog
/// answer anything else.
const GO: &str = "go";

/// Builds the question. `body` is the line under the heading.
pub fn confirm(heading: &str, body: &str, verb: &str, tone: Tone) -> Confirm {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_responses(&[("cancel", &gettext("Cancel")), (GO, verb)]);
    dialog.set_response_appearance(
        GO,
        match tone {
            Tone::Destructive => adw::ResponseAppearance::Destructive,
            Tone::Suggested => adw::ResponseAppearance::Suggested,
        },
    );
    dialog.set_close_response("cancel");
    Confirm { dialog }
}

impl Confirm {
    /// Labels the way out "Not Now", for an offer the person can take
    /// later rather than a step they are about to cancel.
    pub fn not_now(self) -> Self {
        self.dialog
            .set_response_label("cancel", &gettext("Not Now"));
        self
    }

    /// Names the way out, for a question whose "no" still does something.
    pub fn declining(self, label: &str) -> Self {
        self.dialog.set_response_label("cancel", label);
        self
    }

    /// Makes Enter press the verb.
    pub fn by_default(self) -> Self {
        self.dialog.set_default_response(Some(GO));
        self
    }

    /// Shows the question over `parent`. True when the person chose the
    /// verb.
    pub async fn ask(self, parent: &impl IsA<gtk::Widget>) -> bool {
        self.dialog.choose_future(Some(parent)).await == GO
    }
}
