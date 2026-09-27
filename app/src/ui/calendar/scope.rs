//! The one question a change to a repeating event asks: which of its
//! occurrences the change covers.

use adw::prelude::*;
use mailrs_domain::calendar::series::RepeatScope;
use mailrs_domain::translate::gettext;

/// Asks which occurrences a change to a repeating event covers, offering
/// only the choices in `offered` (Task 1's `series::scopes`). `None` for
/// Cancel or for the dialog closing another way. A delete still goes
/// through Undo, so this is a choice, not a confirmation, even while
/// `deleting` colours the responses to say so.
pub async fn ask(
    parent: &impl IsA<gtk::Widget>,
    deleting: bool,
    offered: &[RepeatScope],
) -> Option<RepeatScope> {
    let heading = if deleting {
        gettext("Delete a repeating event")
    } else {
        gettext("Change a repeating event")
    };
    let dialog = adw::AlertDialog::builder()
        .heading(&heading)
        .prefer_wide_layout(false)
        .build();
    dialog.add_response("cancel", &gettext("Cancel"));
    for scope in offered {
        let (id, label) = match scope {
            RepeatScope::This => ("this", gettext("This event only")),
            RepeatScope::Following => ("following", gettext("This and following events")),
            RepeatScope::All => ("all", gettext("All events")),
        };
        dialog.add_response(id, &label);
    }
    if deleting {
        for id in ["this", "following", "all"] {
            if dialog.has_response(id) {
                dialog.set_response_appearance(id, adw::ResponseAppearance::Destructive);
            }
        }
    }
    let first = match offered.first() {
        Some(RepeatScope::This) => "this",
        Some(RepeatScope::Following) => "following",
        _ => "all",
    };
    dialog.set_default_response(Some(first));
    dialog.set_close_response("cancel");
    match dialog.choose_future(Some(parent)).await.as_str() {
        "this" => Some(RepeatScope::This),
        "following" => Some(RepeatScope::Following),
        "all" => Some(RepeatScope::All),
        _ => None,
    }
}
