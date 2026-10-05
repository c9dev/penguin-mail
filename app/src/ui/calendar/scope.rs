//! The dialog for the one question a move, a change or a delete of an
//! event asks before it is written: whether to go ahead, which
//! occurrences of a repeating event it covers, and whether the guests
//! are mailed. What to ask is the calendar copy's rule
//! (`CalendarCopy::ask_before`); this module words it, lays out its
//! buttons and reads the one pressed. The rules for the buttons are pure
//! functions under tests; `ask` only shows them.

use adw::prelude::*;
use mailrs_domain::calendar::series::RepeatScope;
use mailrs_domain::calendar::{Event, Notify};
use mailrs_domain::translate::{fill, gettext};
pub use mailrs_sync::calendar_copy::event_change::{Action, Question};
use mailrs_sync::calendar_copy::event_change::Choice;

/// What the person answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    pub scope: Option<RepeatScope>,
    pub notify: Notify,
    /// Write the other edits at the time the event had.
    pub keep_time: bool,
}

impl Answer {
    /// The scope and notify the calendar copy writes the change with.
    pub fn choice(self) -> Choice {
        Choice { scope: self.scope, notify: self.notify }
    }
}

impl From<Choice> for Answer {
    fn from(choice: Choice) -> Answer {
        Answer { scope: choice.scope, notify: choice.notify, keep_time: false }
    }
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

/// Whether the guests choice shows as a check button, because the
/// buttons already choose the time.
pub fn send_check(question: &Question) -> bool {
    question.keeps && question.ask_guests
}

/// The response Escape and closing the dialog give. It is Cancel even
/// when the buttons read "Keep Old Time", so Escape throws the whole
/// edit away rather than saving part of it.
pub const CLOSE_RESPONSE: &str = "cancel";

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

/// The dialog's buttons, Cancel first. A move that changed more than the
/// time has "Keep Old Time" in Cancel's place and "Move": the buttons
/// choose the time, and [`send_check`] carries the guests choice.
pub fn responses(question: &Question) -> Vec<Response> {
    if question.keeps {
        return vec![
            Response { id: "keep", label: gettext("Keep Old Time"), look: Look::Plain },
            Response { id: "go", label: gettext("Move"), look: Look::Suggested },
        ];
    }
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
            Action::Answer => gettext("Send"),
        };
        out.push(Response { id: "go", label, look: doing });
    }
    out
}

/// Whether the repeat choice shows as a list of options above the
/// buttons: when the buttons already carry the guests choice or the
/// choice of time.
pub fn scope_options(question: &Question) -> bool {
    (question.ask_guests || question.keeps) && !question.scopes.is_empty()
}

/// The button Enter presses: sending the update when the guests are
/// asked, since a guest who is not told turns up at the old time.
pub fn default_response(question: &Question) -> &'static str {
    if question.keeps {
        "go"
    } else if question.ask_guests {
        "send"
    } else {
        question.scopes.first().map_or("go", |&s| scope_id(s))
    }
}

/// Reads the button pressed, with the option picked in the list when
/// [`scope_options`] shows one and the state of the check when
/// [`send_check`] shows it. `None` for Cancel.
pub fn answer(question: &Question, response: &str, picked: Option<RepeatScope>, send: bool) -> Option<Answer> {
    let listed = || if scope_options(question) { picked.or(question.scopes.first().copied()) } else { None };
    if question.keeps {
        let guests = if send_check(question) && !send { Notify::Nobody } else { Notify::Guests };
        return match response {
            "go" => Some(Answer { scope: listed(), notify: guests, keep_time: false }),
            // The other edits go out at the old time. They reach the
            // guests only when they change something the guests see.
            "keep" => {
                let notify = if question.rest_seen { guests } else { Notify::Nobody };
                Some(Answer { scope: listed(), notify, keep_time: true })
            }
            _ => None,
        };
    }
    let notify = match response {
        "send" | "go" => Notify::Guests,
        "quiet" => Notify::Nobody,
        other => {
            let scope = question.scopes.iter().copied().find(|&s| scope_id(s) == other)?;
            return Some(Answer { scope: Some(scope), notify: Notify::Guests, keep_time: false });
        }
    };
    Some(Answer { scope: listed(), notify, keep_time: false })
}

/// The heading: the event's title for a one-off event, the repeat
/// question's own words for a series.
fn heading(question: &Question, title: &str) -> String {
    match (question.action, question.scopes.is_empty()) {
        (Action::Move, true) => fill(&gettext("Move “{title}”?"), &[("title", title)]),
        (Action::Delete, true) => fill(&gettext("Delete “{title}”?"), &[("title", title)]),
        (Action::Move, false) => gettext("Move a repeating event"),
        (Action::Delete, false) => gettext("Delete a repeating event"),
        (Action::Edit, true) => fill(&gettext("Save changes to “{title}”?"), &[("title", title)]),
        (Action::Edit, false) => gettext("Change a repeating event"),
        (Action::Answer, true) => fill(&gettext("Answer “{title}”?"), &[("title", title)]),
        (Action::Answer, false) => gettext("Answer a repeating event"),
    }
}

/// Asks `question` about `event` over `parent`. `when` is the new time
/// of a move, in words. `None` for Cancel or the dialog closing another
/// way; a move then springs back and nothing is written, also when the
/// question [`keeps`](Question::keeps).
pub async fn ask(parent: &impl IsA<gtk::Widget>, question: &Question, event: &Event, when: Option<&str>) -> Option<Answer> {
    let mut body: Vec<String> = when.map(str::to_string).into_iter().collect();
    if question.keeps {
        body.push(gettext("Keep Old Time saves your other changes at the time the event had."));
    }
    if question.mailed {
        body.push(if question.action == Action::Delete {
            gettext("The guests get a cancellation by mail.")
        } else {
            gettext("The guests get an update by mail.")
        });
    }
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
    dialog.set_close_response(CLOSE_RESPONSE);
    // The occurrences as options, and the guests choice as a check, when
    // the buttons already carry another choice, so one dialog asks all.
    let extra = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let mut options: Vec<(RepeatScope, gtk::CheckButton)> = Vec::new();
    if scope_options(question) {
        for &scope in &question.scopes {
            let option = gtk::CheckButton::with_label(&scope_label(scope));
            if let Some((_, first)) = options.first() {
                option.set_group(Some(first));
            } else {
                option.set_active(true);
            }
            extra.append(&option);
            options.push((scope, option));
        }
    }
    let send = send_check(question).then(|| {
        let send = gtk::CheckButton::with_label(&gettext("Send an update to the guests"));
        send.set_active(true);
        if !options.is_empty() {
            send.set_margin_top(6);
        }
        extra.append(&send);
        send
    });
    if extra.first_child().is_some() {
        dialog.set_extra_child(Some(&extra));
    }
    let response = dialog.choose_future(Some(parent)).await;
    let picked = options.iter().find(|(_, o)| o.is_active()).map(|(s, _)| *s);
    let send = send.as_ref().is_none_or(|s| s.is_active());
    answer(question, response.as_str(), picked, send)
}

#[cfg(test)]
mod tests {
    use mailrs_domain::calendar::Guest;
    use mailrs_sync::calendar_copy::event_change::{Facts, question};

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
        let q = question(Action::Move, &[], &[me()], Facts::default()).unwrap();
        assert_eq!(ids(&q), ["cancel", "go"]);
        assert_eq!(responses(&q)[1].label, "Move");
        assert_eq!(answer(&q, "go", None, true), Some(Answer { scope: None, notify: Notify::Guests, keep_time: false }));
    }

    #[test]
    fn moving_a_meeting_asks_whether_to_send_an_update() {
        let q = question(Action::Move, &[], &[me(), ann()], Facts::default()).unwrap();
        assert_eq!(ids(&q), ["cancel", "quiet", "send"]);
        assert_eq!(responses(&q)[2].label, "Send an update to the guests");
        assert_eq!(responses(&q)[1].label, "Don't send");
        assert_eq!(default_response(&q), "send");
        assert_eq!(answer(&q, "quiet", None, true).unwrap().notify, Notify::Nobody);
        assert_eq!(answer(&q, "send", None, true).unwrap().notify, Notify::Guests);
    }

    #[test]
    fn cancel_answers_nothing() {
        let q = question(Action::Move, &[], &[ann()], Facts::default()).unwrap();
        assert_eq!(answer(&q, "cancel", None, true), None);
        assert_eq!(answer(&q, "close", None, true), None);
    }

    /// The repeat question and the guests question are one dialog: the
    /// occurrences become options and the buttons carry the guests.
    #[test]
    fn a_repeating_meeting_asks_both_questions_at_once() {
        let q = question(Action::Move, &EVERY, &[ann()], Facts::default()).unwrap();
        assert!(scope_options(&q));
        assert_eq!(ids(&q), ["cancel", "quiet", "send"]);
        assert_eq!(
            answer(&q, "quiet", Some(RepeatScope::Following), true),
            Some(Answer { scope: Some(RepeatScope::Following), notify: Notify::Nobody, keep_time: false })
        );
    }

    #[test]
    fn a_repeating_event_without_guests_keeps_one_button_per_choice() {
        let q = question(Action::Edit, &EVERY, &[], Facts::default()).unwrap();
        assert!(!scope_options(&q));
        assert_eq!(ids(&q), ["cancel", "this", "following", "all"]);
        assert_eq!(answer(&q, "all", None, true), Some(Answer { scope: Some(RepeatScope::All), notify: Notify::Guests, keep_time: false }));
    }

    #[test]
    fn answering_a_repeating_invitation_offers_this_event_or_all_events() {
        let q = question(Action::Answer, &[RepeatScope::This, RepeatScope::All], &[ann()], Facts::default()).unwrap();
        assert_eq!(ids(&q), ["cancel", "this", "all"]);
        assert!(!q.ask_guests, "only the organizer hears an answer");
        assert_eq!(answer(&q, "this", None, true).and_then(|a| a.scope), Some(RepeatScope::This));
        assert_eq!(answer(&q, "all", None, true).and_then(|a| a.scope), Some(RepeatScope::All));
    }

    /// Cancelling a meeting you organize tells the guests unless you say
    /// otherwise.
    #[test]
    fn deleting_a_meeting_offers_the_cancellation_and_defaults_to_sending_it() {
        let q = question(Action::Delete, &[], &[me(), ann()], Facts::default()).unwrap();
        assert_eq!(ids(&q), ["cancel", "quiet", "send"]);
        assert_eq!(responses(&q)[2].label, "Send a cancellation to the guests");
        assert_eq!(default_response(&q), "send");
        assert!(responses(&q)[1..].iter().all(|r| r.look == Look::Destructive));
    }

    #[test]
    fn a_move_that_adds_guests_tells_them_without_a_choice() {
        let q = question(Action::Move, &[], &[ann()], Facts { adds_guests: true, ..Facts::default() }).unwrap();
        assert!(q.told);
        assert_eq!(ids(&q), ["cancel", "go"]);
        assert_eq!(answer(&q, "go", None, true).unwrap().notify, Notify::Guests);
    }

    fn seen() -> Facts {
        Facts { seen: true, ..Facts::default() }
    }

    /// An account that mails the guests whatever the person picks, as
    /// Microsoft does, gets no choice to send nobody or to send an update.
    fn mails() -> Facts {
        Facts { always_mails: true, ..Facts::default() }
    }

    #[test]
    fn an_account_that_always_mails_offers_no_choice_about_the_guests() {
        for action in [Action::Move, Action::Delete, Action::Edit] {
            let change = Facts { seen: true, ..mails() };
            let q = question(action, &[], &[me(), ann()], change).unwrap();
            assert!(!q.ask_guests, "{action:?}");
            assert!(q.mailed, "{action:?}");
            let shown = ids(&q);
            assert!(!shown.contains(&"quiet") && !shown.contains(&"send"), "{action:?}: {shown:?}");
            assert_eq!(answer(&q, "go", None, true).unwrap().notify, Notify::Guests);
        }
    }

    #[test]
    fn deleting_a_meeting_on_such_an_account_still_asks_to_confirm() {
        let q = question(Action::Delete, &[], &[me(), ann()], mails()).unwrap();
        assert_eq!(ids(&q), ["cancel", "go"]);
        assert_eq!(responses(&q)[1].label, "Delete");
    }

    #[test]
    fn a_move_with_other_changes_on_such_an_account_has_no_send_check() {
        let change = Facts { more_than_time: true, rest_seen: true, ..mails() };
        let q = question(Action::Move, &[], &[me(), ann()], change).unwrap();
        assert!(q.keeps && !send_check(&q));
        assert_eq!(answer(&q, "go", None, true).unwrap().notify, Notify::Guests);
        assert_eq!(answer(&q, "keep", None, true).unwrap().notify, Notify::Guests);
    }

    /// A new title on a meeting reaches the guests, so the save asks
    /// whether to tell them, sending by default.
    #[test]
    fn an_edit_the_guests_see_asks_whether_to_send_an_update() {
        let q = question(Action::Edit, &[], &[me(), ann()], seen()).unwrap();
        assert_eq!(ids(&q), ["cancel", "quiet", "send"]);
        assert_eq!(default_response(&q), "send");
        assert_eq!(answer(&q, "quiet", None, true).unwrap().notify, Notify::Nobody);
        assert_eq!(answer(&q, "cancel", None, true), None);
    }

    #[test]
    fn a_repeating_meeting_edit_asks_the_occurrences_and_the_guests_at_once() {
        let q = question(Action::Edit, &EVERY, &[ann()], seen()).unwrap();
        assert!(scope_options(&q));
        assert_eq!(ids(&q), ["cancel", "quiet", "send"]);
    }

    fn move_and_more(rest_seen: bool) -> Facts {
        Facts { seen: true, more_than_time: true, rest_seen, ..Facts::default() }
    }

    /// An editor save that moves the event and changes more: one dialog,
    /// whose Cancel turns only the new time down.
    #[test]
    fn a_move_with_other_edits_offers_to_keep_the_old_time() {
        let q = question(Action::Move, &[], &[me()], move_and_more(false)).unwrap();
        assert!(q.keeps);
        assert_eq!(ids(&q), ["keep", "go"]);
        assert_eq!(responses(&q)[0].label, "Keep Old Time");
        assert_eq!(answer(&q, CLOSE_RESPONSE, None, true), None, "Escape throws the whole edit away");
        assert_eq!(answer(&q, "keep", None, true), Some(Answer { scope: None, notify: Notify::Nobody, keep_time: true }));
        assert_eq!(answer(&q, "go", None, true), Some(Answer { scope: None, notify: Notify::Guests, keep_time: false }));
    }

    #[test]
    fn a_move_with_other_edits_on_a_meeting_asks_about_the_guests_with_a_check() {
        let q = question(Action::Move, &[], &[me(), ann()], move_and_more(true)).unwrap();
        assert!(send_check(&q));
        assert_eq!(ids(&q), ["keep", "go"], "the buttons choose the time, the check the guests");
        assert_eq!(answer(&q, "go", None, false).unwrap().notify, Notify::Nobody);
        assert_eq!(answer(&q, "keep", None, true), Some(Answer { scope: None, notify: Notify::Guests, keep_time: true }));
        assert_eq!(answer(&q, "keep", None, false).unwrap().notify, Notify::Nobody);
    }

    /// The other edits are a reminder the guests never see: keeping the
    /// old time sends them nothing, whatever the check says.
    #[test]
    fn keeping_the_old_time_with_edits_the_guests_do_not_see_mails_nobody() {
        let q = question(Action::Move, &[], &[ann()], move_and_more(false)).unwrap();
        assert_eq!(answer(&q, "keep", None, true).unwrap().notify, Notify::Nobody);
    }

    #[test]
    fn a_repeating_event_moved_with_other_edits_lists_the_occurrences_as_options() {
        let q = question(Action::Move, &EVERY, &[], move_and_more(false)).unwrap();
        assert!(scope_options(&q));
        assert_eq!(ids(&q), ["keep", "go"]);
        let kept = answer(&q, "keep", Some(RepeatScope::All), true).unwrap();
        assert_eq!((kept.scope, kept.keep_time), (Some(RepeatScope::All), true));
    }

    #[test]
    fn a_move_alone_still_cancels() {
        let q = question(Action::Move, &[], &[me()], seen()).unwrap();
        assert!(!q.keeps);
        assert_eq!(answer(&q, CLOSE_RESPONSE, None, true), None);
    }
}
