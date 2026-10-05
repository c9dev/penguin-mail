//! The one question a move, a change or a delete of an event asks before
//! it is written: whether to go ahead, which occurrences of a repeating
//! event it covers, and whether the guests are mailed. The person
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
    /// A guest's Yes, Maybe or No. Only the organizer hears of it, so
    /// it asks which occurrences it covers and nothing about the guests.
    Answer,
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
    /// The account mails the guests whatever the person says, so the
    /// dialog says so instead of offering a choice.
    pub mailed: bool,
    /// The guests hear of it whatever the person says, because the change
    /// adds guests and their invitation is that mail.
    pub told: bool,
    /// Cancel reads "Keep Old Time": the save changed more than the
    /// time, and turning the new time down still writes the rest.
    pub keeps: bool,
    /// Whether the rest, written at the old time, reaches the guests.
    pub rest_seen: bool,
}

/// What an editor save changes, beyond the kind of action. A drag, a
/// nudge and a delete pass the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Change {
    /// The guests see the change (`draft::reaches_guests`). A move and a
    /// delete reach them whatever this says.
    pub seen: bool,
    /// The change invites someone new.
    pub adds_guests: bool,
    /// A move that also changes something besides the time.
    pub more_than_time: bool,
    /// Those other changes reach the guests on their own.
    pub rest_seen: bool,
    /// The account mails the guests of every change it writes and has no
    /// way to send nobody (`Offers::quiet_changes` is false), so no choice
    /// about the guests is offered.
    pub always_mails: bool,
}

/// What the person answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    pub scope: Option<RepeatScope>,
    pub notify: Notify,
    /// Write the other edits at the time the event had.
    pub keep_time: bool,
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
/// A delete, and an edit the guests see, ask when the event repeats or
/// has guests; any other edit only when it repeats. A change that adds
/// guests tells every guest without a choice, since the new ones need
/// their invitation.
pub fn question(action: Action, scopes: &[RepeatScope], guests: &[Guest], change: Change) -> Option<Question> {
    let seen = action != Action::Edit || change.seen;
    let guests_hear = action != Action::Answer && has_other_guests(guests) && seen;
    let ask_guests = guests_hear && !change.adds_guests && !change.always_mails;
    let mailed = guests_hear && !change.adds_guests && change.always_mails;
    let needed = match action {
        Action::Move => true,
        Action::Delete | Action::Edit | Action::Answer => !scopes.is_empty() || ask_guests || mailed,
    };
    needed.then(|| Question {
        action,
        scopes: scopes.to_vec(),
        ask_guests,
        mailed,
        told: guests_hear && change.adds_guests,
        keeps: action == Action::Move && change.more_than_time,
        rest_seen: change.rest_seen,
    })
}

/// What a change nobody was asked about sends: nothing for an edit the
/// guests do not see, an update otherwise. An account that always mails
/// cannot send nothing, so it says `Guests` and the account decides.
pub fn unasked(action: Action, change: Change) -> Answer {
    let notify = if action == Action::Edit && !change.seen && !change.always_mails { Notify::Nobody } else { Notify::Guests };
    Answer { scope: None, notify, keep_time: false }
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
        let q = question(Action::Move, &[], &[me()], Change::default()).unwrap();
        assert_eq!(ids(&q), ["cancel", "go"]);
        assert_eq!(responses(&q)[1].label, "Move");
        assert_eq!(answer(&q, "go", None, true), Some(Answer { scope: None, notify: Notify::Guests, keep_time: false }));
    }

    #[test]
    fn moving_a_meeting_asks_whether_to_send_an_update() {
        let q = question(Action::Move, &[], &[me(), ann()], Change::default()).unwrap();
        assert_eq!(ids(&q), ["cancel", "quiet", "send"]);
        assert_eq!(responses(&q)[2].label, "Send an update to the guests");
        assert_eq!(responses(&q)[1].label, "Don't send");
        assert_eq!(default_response(&q), "send");
        assert_eq!(answer(&q, "quiet", None, true).unwrap().notify, Notify::Nobody);
        assert_eq!(answer(&q, "send", None, true).unwrap().notify, Notify::Guests);
    }

    #[test]
    fn cancel_answers_nothing() {
        let q = question(Action::Move, &[], &[ann()], Change::default()).unwrap();
        assert_eq!(answer(&q, "cancel", None, true), None);
        assert_eq!(answer(&q, "close", None, true), None);
    }

    /// The repeat question and the guests question are one dialog: the
    /// occurrences become options and the buttons carry the guests.
    #[test]
    fn a_repeating_meeting_asks_both_questions_at_once() {
        let q = question(Action::Move, &EVERY, &[ann()], Change::default()).unwrap();
        assert!(scope_options(&q));
        assert_eq!(ids(&q), ["cancel", "quiet", "send"]);
        assert_eq!(
            answer(&q, "quiet", Some(RepeatScope::Following), true),
            Some(Answer { scope: Some(RepeatScope::Following), notify: Notify::Nobody, keep_time: false })
        );
    }

    #[test]
    fn a_repeating_event_without_guests_keeps_one_button_per_choice() {
        let q = question(Action::Edit, &EVERY, &[], Change::default()).unwrap();
        assert!(!scope_options(&q));
        assert_eq!(ids(&q), ["cancel", "this", "following", "all"]);
        assert_eq!(answer(&q, "all", None, true), Some(Answer { scope: Some(RepeatScope::All), notify: Notify::Guests, keep_time: false }));
    }

    #[test]
    fn answering_a_repeating_invitation_offers_this_event_or_all_events() {
        let q = question(Action::Answer, &[RepeatScope::This, RepeatScope::All], &[ann()], Change::default()).unwrap();
        assert_eq!(ids(&q), ["cancel", "this", "all"]);
        assert!(!q.ask_guests, "only the organizer hears an answer");
        assert_eq!(answer(&q, "this", None, true).and_then(|a| a.scope), Some(RepeatScope::This));
        assert_eq!(answer(&q, "all", None, true).and_then(|a| a.scope), Some(RepeatScope::All));
    }

    #[test]
    fn answering_a_one_off_invitation_asks_nothing() {
        assert_eq!(question(Action::Answer, &[], &[ann()], Change::default()), None);
    }

    #[test]
    fn an_edit_that_keeps_the_time_asks_nothing_of_a_one_off_meeting() {
        assert_eq!(question(Action::Edit, &[], &[ann()], Change::default()), None);
    }

    #[test]
    fn deleting_a_one_off_event_without_guests_asks_nothing() {
        assert_eq!(question(Action::Delete, &[], &[me()], Change::default()), None);
    }

    /// Cancelling a meeting you organize tells the guests unless you say
    /// otherwise.
    #[test]
    fn deleting_a_meeting_offers_the_cancellation_and_defaults_to_sending_it() {
        let q = question(Action::Delete, &[], &[me(), ann()], Change::default()).unwrap();
        assert_eq!(ids(&q), ["cancel", "quiet", "send"]);
        assert_eq!(responses(&q)[2].label, "Send a cancellation to the guests");
        assert_eq!(default_response(&q), "send");
        assert!(responses(&q)[1..].iter().all(|r| r.look == Look::Destructive));
    }

    #[test]
    fn a_move_that_adds_guests_tells_them_without_a_choice() {
        let q = question(Action::Move, &[], &[ann()], Change { adds_guests: true, ..Change::default() }).unwrap();
        assert!(q.told);
        assert_eq!(ids(&q), ["cancel", "go"]);
        assert_eq!(answer(&q, "go", None, true).unwrap().notify, Notify::Guests);
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

    fn seen() -> Change {
        Change { seen: true, ..Change::default() }
    }

    /// An account that mails the guests whatever the person picks, as
    /// Microsoft does, gets no choice to send nobody or to send an update.
    fn mails() -> Change {
        Change { always_mails: true, ..Change::default() }
    }

    #[test]
    fn an_account_that_always_mails_offers_no_choice_about_the_guests() {
        for action in [Action::Move, Action::Delete, Action::Edit] {
            let change = Change { seen: true, ..mails() };
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
        let change = Change { more_than_time: true, rest_seen: true, ..mails() };
        let q = question(Action::Move, &[], &[me(), ann()], change).unwrap();
        assert!(q.keeps && !send_check(&q));
        assert_eq!(answer(&q, "go", None, true).unwrap().notify, Notify::Guests);
        assert_eq!(answer(&q, "keep", None, true).unwrap().notify, Notify::Guests);
    }

    #[test]
    fn an_unasked_edit_on_such_an_account_never_promises_nobody() {
        assert_eq!(unasked(Action::Edit, Change::default()).notify, Notify::Nobody);
        assert_eq!(unasked(Action::Edit, mails()).notify, Notify::Guests);
    }

    #[test]
    fn an_account_that_always_mails_with_no_other_guests_asks_nothing_extra() {
        assert_eq!(question(Action::Delete, &[], &[me()], mails()), None);
        assert!(!question(Action::Move, &[], &[me()], mails()).unwrap().mailed);
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
    fn an_edit_the_guests_see_asks_nothing_of_an_event_without_guests() {
        assert_eq!(question(Action::Edit, &[], &[me()], seen()), None);
    }

    /// Reminders, colour and busy or free are the account's own.
    #[test]
    fn an_edit_only_the_account_sees_asks_nothing_and_mails_nobody() {
        assert_eq!(question(Action::Edit, &[], &[me(), ann()], Change::default()), None);
        assert_eq!(unasked(Action::Edit, Change::default()).notify, Notify::Nobody);
    }

    #[test]
    fn an_edit_the_guests_see_that_nobody_is_asked_about_still_tells_them() {
        assert_eq!(unasked(Action::Edit, seen()).notify, Notify::Guests);
        assert_eq!(unasked(Action::Move, Change::default()).notify, Notify::Guests);
    }

    #[test]
    fn a_repeating_meeting_edit_asks_the_occurrences_and_the_guests_at_once() {
        let q = question(Action::Edit, &EVERY, &[ann()], seen()).unwrap();
        assert!(scope_options(&q));
        assert_eq!(ids(&q), ["cancel", "quiet", "send"]);
    }

    #[test]
    fn a_meeting_that_loses_a_guest_still_asks() {
        // The caller passes the list from before the edit when it had
        // guests, so the removed guest hears of it.
        let q = question(Action::Edit, &[], &[ann()], seen()).unwrap();
        assert!(q.ask_guests);
    }

    fn move_and_more(rest_seen: bool) -> Change {
        Change { seen: true, more_than_time: true, rest_seen, ..Change::default() }
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
