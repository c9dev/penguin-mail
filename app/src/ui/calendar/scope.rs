//! The one question a move, a change or a delete of an event asks before
//! it is written: whether to go ahead, which occurrences of a repeating
//! event it covers, and whether Google mails the guests. The person
//! answers all of it in one dialog. The rules are pure functions under
//! tests; `ask` only shows them.

use adw::prelude::*;
use mailrs_domain::calendar::series::RepeatScope;
use mailrs_domain::calendar::{Event, Guest, Notify};
use mailrs_domain::translate::{fill, gettext};

/// What the person did to the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A drag, a keyboard nudge, or a new time in the editor.
    Move,
    /// Any other change in the editor.
    Edit,
    Delete,
}

/// What the dialog asks. Built by [`question`], which answers `None`
/// when nothing needs asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub action: Action,
    /// The occurrences the change may cover, for a repeating event.
    pub scopes: Vec<RepeatScope>,
    /// Offer "Send an update to the guests" and "Don't send".
    pub ask_guests: bool,
    /// The guests hear of it whatever the person says, because the change
    /// adds guests and their invitation is that mail.
    pub told: bool,
}

/// What the person answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    pub scope: Option<RepeatScope>,
    pub notify: Notify,
}

/// How a response button looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Plain,
    Suggested,
    Destructive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub id: &'static str,
    pub label: String,
    pub look: Look,
}

/// Whether anyone but the account itself is on the event's guest list.
pub fn has_other_guests(guests: &[Guest]) -> bool {
    guests.iter().any(|g| !g.me)
}

/// Whether `after` holds an address `before` does not.
pub fn adds_guests(before: &[Guest], after: &[Guest]) -> bool {
    after.iter().any(|a| !before.iter().any(|b| b.email.eq_ignore_ascii_case(&a.email)))
}

/// The question for `action` on an event with `guests`, offering
/// `scopes` (from `series::scopes`; empty for an event that does not
/// repeat). Every move asks, so a drag that slipped can be taken back.
/// A delete asks when the event repeats or has guests; an edit only when
/// it repeats. `adds_guests` says the change invites someone, whose
/// invitation must go out, so the guests are told without a choice.
pub fn question(action: Action, scopes: &[RepeatScope], guests: &[Guest], adds_guests: bool) -> Option<Question> {
    let guests_hear = has_other_guests(guests) && action != Action::Edit;
    let ask_guests = guests_hear && !adds_guests;
    let needed = match action {
        Action::Move => true,
        Action::Delete => !scopes.is_empty() || ask_guests,
        Action::Edit => !scopes.is_empty(),
    };
    needed.then(|| Question { action, scopes: scopes.to_vec(), ask_guests, told: guests_hear && adds_guests })
}

fn scope_id(scope: RepeatScope) -> &'static str {
    match scope {
        RepeatScope::This => "this",
        RepeatScope::Following => "following",
        RepeatScope::All => "all",
    }
}

fn scope_label(scope: RepeatScope) -> String {
    match scope {
        RepeatScope::This => gettext("This event only"),
        RepeatScope::Following => gettext("This and following events"),
        RepeatScope::All => gettext("All events"),
    }
}

/// The dialog's buttons, Cancel first.
pub fn responses(question: &Question) -> Vec<Response> {
    let deleting = question.action == Action::Delete;
    let doing = if deleting { Look::Destructive } else { Look::Suggested };
    let mut out = vec![Response { id: "cancel", label: gettext("Cancel"), look: Look::Plain }];
    if question.ask_guests {
        out.push(Response {
            id: "quiet",
            label: gettext("Don't send"),
            look: if deleting { Look::Destructive } else { Look::Plain },
        });
        let send = if deleting {
            gettext("Send a cancellation to the guests")
        } else {
            gettext("Send an update to the guests")
        };
        out.push(Response { id: "send", label: send, look: doing });
    } else if !question.scopes.is_empty() {
        let look = if deleting { Look::Destructive } else { Look::Plain };
        out.extend(question.scopes.iter().map(|&s| Response { id: scope_id(s), label: scope_label(s), look }));
    } else {
        let label = match question.action {
            Action::Move => gettext("Move"),
            Action::Edit => gettext("Save"),
            Action::Delete => gettext("Delete"),
        };
        out.push(Response { id: "go", label, look: doing });
    }
    out
}

/// Whether the repeat choice shows as a list of options above the
/// buttons: when the buttons already carry the guests choice.
pub fn scope_options(question: &Question) -> bool {
    question.ask_guests && !question.scopes.is_empty()
}

/// The button Enter presses: sending the update when the guests are
/// asked, since a guest who is not told turns up at the old time.
pub fn default_response(question: &Question) -> &'static str {
    if question.ask_guests {
        "send"
    } else {
        question.scopes.first().map_or("go", |&s| scope_id(s))
    }
}

/// Reads the button pressed, with the option picked in the list when
/// [`scope_options`] shows one. `None` for Cancel.
pub fn answer(question: &Question, response: &str, picked: Option<RepeatScope>) -> Option<Answer> {
    let listed = || if scope_options(question) { picked.or(question.scopes.first().copied()) } else { None };
    let notify = match response {
        "send" | "go" => Notify::Guests,
        "quiet" => Notify::Nobody,
        other => {
            let scope = question.scopes.iter().copied().find(|&s| scope_id(s) == other)?;
            return Some(Answer { scope: Some(scope), notify: Notify::Guests });
        }
    };
    Some(Answer { scope: listed(), notify })
}

/// The heading: the event's title for a one-off event, the repeat
/// question's own words for a series.
fn heading(question: &Question, title: &str) -> String {
    match (question.action, question.scopes.is_empty()) {
        (Action::Move, true) => fill(&gettext("Move “{title}”?"), &[("title", title)]),
        (Action::Delete, true) => fill(&gettext("Delete “{title}”?"), &[("title", title)]),
        (Action::Move, false) => gettext("Move a repeating event"),
        (Action::Delete, false) => gettext("Delete a repeating event"),
        (Action::Edit, _) => gettext("Change a repeating event"),
    }
}

/// Asks `question` about `event` over `parent`. `when` is the new time
/// of a move, in words. `None` for Cancel or the dialog closing another
/// way; a move then springs back and nothing is written.
pub async fn ask(parent: &impl IsA<gtk::Widget>, question: &Question, event: &Event, when: Option<&str>) -> Option<Answer> {
    let mut body: Vec<String> = when.map(str::to_string).into_iter().collect();
    if question.told {
        body.push(gettext("The new guests get their invitation, and the others get an update."));
    }
    let dialog = adw::AlertDialog::builder()
        .heading(heading(question, &event.title))
        .body(body.join("\n\n"))
        .prefer_wide_layout(false)
        .build();
    for response in responses(question) {
        dialog.add_response(response.id, &response.label);
        match response.look {
            Look::Plain => {}
            Look::Suggested => dialog.set_response_appearance(response.id, adw::ResponseAppearance::Suggested),
            Look::Destructive => dialog.set_response_appearance(response.id, adw::ResponseAppearance::Destructive),
        }
    }
    dialog.set_default_response(Some(default_response(question)));
    dialog.set_close_response("cancel");
    // A group of options for the occurrences, when the buttons already
    // carry the guests choice, so one dialog asks both.
    let options: Vec<(RepeatScope, gtk::CheckButton)> = if scope_options(question) {
        let list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let mut options = Vec::new();
        for &scope in &question.scopes {
            let option = gtk::CheckButton::with_label(&scope_label(scope));
            if let Some((_, first)) = options.first() {
                option.set_group(Some(first));
            } else {
                option.set_active(true);
            }
            list.append(&option);
            options.push((scope, option));
        }
        dialog.set_extra_child(Some(&list));
        options
    } else {
        Vec::new()
    };
    let response = dialog.choose_future(Some(parent)).await;
    let picked = options.iter().find(|(_, o)| o.is_active()).map(|(s, _)| *s);
    answer(question, response.as_str(), picked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ann() -> Guest {
        Guest { email: "ann@example.com".into(), ..Guest::default() }
    }

    fn me() -> Guest {
        Guest { email: "me@example.com".into(), me: true, organizer: true, ..Guest::default() }
    }

    fn ids(question: &Question) -> Vec<&'static str> {
        responses(question).iter().map(|r| r.id).collect()
    }

    const EVERY: [RepeatScope; 3] = [RepeatScope::This, RepeatScope::Following, RepeatScope::All];

    #[test]
    fn moving_an_event_without_guests_asks_only_to_confirm() {
        let q = question(Action::Move, &[], &[me()], false).unwrap();
        assert_eq!(ids(&q), ["cancel", "go"]);
        assert_eq!(responses(&q)[1].label, "Move");
        assert_eq!(answer(&q, "go", None), Some(Answer { scope: None, notify: Notify::Guests }));
    }

    #[test]
    fn moving_a_meeting_asks_whether_to_send_an_update() {
        let q = question(Action::Move, &[], &[me(), ann()], false).unwrap();
        assert_eq!(ids(&q), ["cancel", "quiet", "send"]);
        assert_eq!(responses(&q)[2].label, "Send an update to the guests");
        assert_eq!(responses(&q)[1].label, "Don't send");
        assert_eq!(default_response(&q), "send");
        assert_eq!(answer(&q, "quiet", None).unwrap().notify, Notify::Nobody);
        assert_eq!(answer(&q, "send", None).unwrap().notify, Notify::Guests);
    }

    #[test]
    fn cancel_answers_nothing() {
        let q = question(Action::Move, &[], &[ann()], false).unwrap();
        assert_eq!(answer(&q, "cancel", None), None);
        assert_eq!(answer(&q, "close", None), None);
    }

    /// The repeat question and the guests question are one dialog: the
    /// occurrences become options and the buttons carry the guests.
    #[test]
    fn a_repeating_meeting_asks_both_questions_at_once() {
        let q = question(Action::Move, &EVERY, &[ann()], false).unwrap();
        assert!(scope_options(&q));
        assert_eq!(ids(&q), ["cancel", "quiet", "send"]);
        assert_eq!(
            answer(&q, "quiet", Some(RepeatScope::Following)),
            Some(Answer { scope: Some(RepeatScope::Following), notify: Notify::Nobody })
        );
    }

    #[test]
    fn a_repeating_event_without_guests_keeps_one_button_per_choice() {
        let q = question(Action::Edit, &EVERY, &[], false).unwrap();
        assert!(!scope_options(&q));
        assert_eq!(ids(&q), ["cancel", "this", "following", "all"]);
        assert_eq!(answer(&q, "all", None), Some(Answer { scope: Some(RepeatScope::All), notify: Notify::Guests }));
    }

    #[test]
    fn an_edit_that_keeps_the_time_asks_nothing_of_a_one_off_meeting() {
        assert_eq!(question(Action::Edit, &[], &[ann()], false), None);
    }

    #[test]
    fn deleting_a_one_off_event_without_guests_asks_nothing() {
        assert_eq!(question(Action::Delete, &[], &[me()], false), None);
    }

    /// Cancelling a meeting you organize tells the guests unless you say
    /// otherwise.
    #[test]
    fn deleting_a_meeting_offers_the_cancellation_and_defaults_to_sending_it() {
        let q = question(Action::Delete, &[], &[me(), ann()], false).unwrap();
        assert_eq!(ids(&q), ["cancel", "quiet", "send"]);
        assert_eq!(responses(&q)[2].label, "Send a cancellation to the guests");
        assert_eq!(default_response(&q), "send");
        assert!(responses(&q)[1..].iter().all(|r| r.look == Look::Destructive));
    }

    #[test]
    fn a_move_that_adds_guests_tells_them_without_a_choice() {
        let q = question(Action::Move, &[], &[ann()], true).unwrap();
        assert!(q.told);
        assert_eq!(ids(&q), ["cancel", "go"]);
        assert_eq!(answer(&q, "go", None).unwrap().notify, Notify::Guests);
    }

    #[test]
    fn the_account_alone_on_the_list_is_no_guest() {
        assert!(!has_other_guests(&[me()]));
        assert!(has_other_guests(&[me(), ann()]));
    }

    #[test]
    fn a_guest_counts_as_added_whatever_the_case_of_the_address() {
        let bo = Guest { email: "bo@example.com".into(), ..Guest::default() };
        let loud = Guest { email: "ANN@example.com".into(), ..Guest::default() };
        assert!(!adds_guests(&[ann()], &[loud]));
        assert!(adds_guests(&[ann()], &[ann(), bo]));
    }
}
