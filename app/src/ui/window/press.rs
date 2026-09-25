//! What one press of a mail button, key, menu item or drop comes to.
//!
//! [`plan`] takes the press, the [`Reach`] it was made on and how it was
//! made, and answers a [`Plan`]: the targets, the action and whether Undo
//! keeps it, whether the window steps past the mail first, the question to
//! ask before erasing, and the toast's words. It joins what used to be
//! joined by hand in nine callers: [`decide`] for the mail buttons, the
//! drop rule in `crate::ui::moving`, and [`aftermath::leaves`] for moving
//! on. [`carry_out`] then makes the plan happen through [`PressEffects`],
//! which the main window stands behind and the tests fake. [`trash_words`]
//! reads the same table to word the Delete button in each mailbox.

use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_domain::{Account, AccountId, FlagColor, Target};
use mailrs_sync::{History, MailAction, Offers, TriageAction};

use super::aftermath;
use super::reach::Reach;
use super::triage::{Cancel, Decision, decide};
use crate::ui::Mailbox;
use crate::ui::conversation::Action;
use crate::ui::moving::move_action;
use crate::wanted::Answer;

/// A mail button, and the keys and menu items that stand for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Button {
    Archive,
    /// Delete, which moves mail to the Trash in most mailboxes.
    Trash,
    Junk,
    ToggleStar,
    ToggleRead,
}

impl Button {
    /// The button a conversation's action stands for, if it is one.
    pub(super) fn of(action: &Action) -> Option<Button> {
        Some(match action {
            Action::Archive => Button::Archive,
            Action::Trash => Button::Trash,
            Action::Junk => Button::Junk,
            Action::ToggleStar => Button::ToggleStar,
            Action::ToggleRead => Button::ToggleRead,
            _ => return None,
        })
    }

    fn action(self) -> Action {
        match self {
            Button::Archive => Action::Archive,
            Button::Trash => Action::Trash,
            Button::Junk => Action::Junk,
            Button::ToggleStar => Action::ToggleStar,
            Button::ToggleRead => Action::ToggleRead,
        }
    }
}

/// What the person pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Press {
    /// Archive, Delete, Junk, the flag button or the read button.
    Button(Button),
    /// Mute, or unmute when every target is muted already.
    Mute,
    /// Archive now and bring back at `at`, which `when` says in words.
    Remind { at: i64, when: String },
    /// A label from the label list: `AddLabel` or `RemoveLabel`.
    Label(TriageAction),
    /// A folder from the same list, on an account that files in folders:
    /// its server id and the name the toast shows.
    Move { folder: String, name: String },
    /// Rows dragged onto a mailbox in the sidebar.
    Drop(Mailbox),
    /// A flag in this colour, or no flag.
    Flag(Option<FlagColor>),
    /// Dismiss Follow-Up from the menu.
    DismissFollowUp,
}

/// How much of the thread a press covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    /// What the view shows: the open conversation or the selected rows.
    Shown,
    /// Rows that need not be the ones on screen, as a drag carries.
    Carried {
        /// The open conversation is among them.
        open: bool,
    },
    /// One message, from its own menu.
    Message {
        /// The thread holds no other message, so it goes where the
        /// message goes.
        alone: bool,
    },
}

/// One account a press reaches, and what its server does with mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Reached {
    pub account_id: AccountId,
    /// The account's address, which the question before Delete Forever
    /// names when this account keeps its mail.
    pub email: String,
    /// Who serves its mail, as people know it: Gmail, Fastmail.
    pub provider: String,
    /// Its server can delete mail for good.
    pub erases: bool,
    /// Its server keeps a message in one folder, so mail moves between
    /// folders rather than gaining labels.
    pub moves: bool,
}

/// The accounts `targets` belong to, each once, in the order they first
/// appear, with what `offers` says each one's server does. `account`
/// names each; an account the window no longer knows keeps empty names.
pub(super) fn reached(
    targets: &[Target],
    account: impl Fn(AccountId) -> Option<Account>,
    offers: impl Fn(AccountId) -> Offers,
) -> Vec<Reached> {
    let mut reached: Vec<Reached> = Vec::new();
    for target in targets {
        let id = target.account_id;
        if reached.iter().any(|r| r.account_id == id) {
            continue;
        }
        let offered = offers(id);
        let (email, provider) = account(id).map_or_else(
            || (String::new(), String::new()),
            |a| (a.email.clone(), a.provider_name().to_string()),
        );
        reached.push(Reached {
            account_id: id,
            email,
            provider,
            erases: offered.delete_forever,
            moves: !offered.labels,
        });
    }
    reached
}

/// A press and what it was made on.
#[derive(Debug, Clone)]
pub(super) struct Pressed {
    pub press: Press,
    pub reach: Reach,
    pub scope: Scope,
    /// The colour the flag button flags in.
    pub flag_color: FlagColor,
    /// The list shows conversations, not messages, which the words count.
    pub threaded: bool,
    /// The accounts the targets belong to, from [`reached`].
    pub accounts: Vec<Reached>,
}

/// The question before a step nothing brings back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Question {
    pub heading: String,
    pub body: String,
    pub verb: String,
}

/// What a press comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Step {
    /// Nothing: no mail is reached, or the press changes none.
    Nothing,
    /// The press cannot be done, and the toast says why.
    Refuse(String),
    /// Run `action`.
    Act {
        action: MailAction,
        history: History,
        /// Step past the mail before Gmail answers, since it leaves the
        /// list.
        move_on: bool,
        /// The Undo toast's words in place of the usual ones.
        words: Option<String>,
        /// Said at once, for an action with no Undo toast.
        said: Option<String>,
    },
    /// Ask, and erase the mail on a yes.
    Erase(Question),
    /// Call off what a mailbox of queued mail holds.
    Cancel(Cancel),
}

/// A press's targets and what it comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Plan {
    pub targets: Vec<Target>,
    pub step: Step,
}

impl Plan {
    /// Whether the press does anything, which is what a drop answers.
    pub(super) fn taken(&self) -> bool {
        !matches!(self.step, Step::Nothing | Step::Refuse(_))
    }
}

/// What `pressed` comes to.
pub(super) fn plan(pressed: Pressed) -> Plan {
    let Pressed {
        press,
        reach,
        scope,
        flag_color,
        threaded,
        accounts,
    } = pressed;
    let mut targets = reach.targets;
    if targets.is_empty() {
        return Plan {
            targets,
            step: Step::Nothing,
        };
    }
    let mailbox = &reach.mailbox;
    let act = |action: MailAction, history: History, words: Option<String>| {
        let move_on = match scope {
            Scope::Shown => true,
            Scope::Carried { open } => open,
            Scope::Message { alone } => alone,
        } && aftermath::leaves(&action, mailbox);
        Step::Act {
            action,
            history,
            move_on,
            words,
            said: None,
        }
    };
    let erases = accounts.iter().any(|a| a.erases);
    let step = match press {
        Press::Button(button) => match decide(&button.action(), mailbox, reach.marks, erases) {
            None => Step::Nothing,
            Some(Decision::Triage(triage)) => {
                act(MailAction::Triage(triage), History::Record, None)
            }
            Some(Decision::Flag(on)) => act(
                MailAction::Flag(on.then_some(flag_color)),
                History::Record,
                None,
            ),
            Some(Decision::DeleteForever) => {
                targets.retain(|target| {
                    accounts
                        .iter()
                        .find(|a| a.account_id == target.account_id)
                        .is_none_or(|a| a.erases)
                });
                let mut erasers: Vec<&str> = Vec::new();
                for account in accounts.iter().filter(|a| a.erases && !a.provider.is_empty()) {
                    if !erasers.contains(&account.provider.as_str()) {
                        erasers.push(account.provider.as_str());
                    }
                }
                let kept: Vec<&str> = accounts
                    .iter()
                    .filter(|a| !a.erases)
                    .map(|a| a.email.as_str())
                    .collect();
                Step::Erase(erase_question(targets.len(), threaded, &erasers, &kept))
            }
            // A message's own menu leaves this item out; a key that
            // reaches it anyway calls off nothing.
            Some(Decision::Cancel(_)) if matches!(scope, Scope::Message { .. }) => Step::Nothing,
            Some(Decision::Cancel(Cancel::Reminder)) => {
                let mut step = act(MailAction::CancelReminder, History::Skip, None);
                if let Step::Act { said, .. } = &mut step {
                    *said = Some(gettext("Back in the Inbox"));
                }
                step
            }
            Some(Decision::Cancel(Cancel::FollowUp)) => {
                act(MailAction::DismissFollowUp, History::Record, None)
            }
            Some(Decision::Cancel(cancel)) => Step::Cancel(cancel),
        },
        Press::Mute => act(
            MailAction::Mute {
                muted: !reach.muted,
            },
            History::Record,
            None,
        ),
        Press::Remind { at, when } => act(
            MailAction::Remind { at },
            History::Record,
            Some(fill(&gettext("Will remind you {when}"), &[("when", &when)])),
        ),
        Press::Label(triage) => act(MailAction::Triage(triage), History::Record, None),
        Press::Move { folder, name } => act(
            MailAction::Triage(TriageAction::MoveTo(folder)),
            History::Record,
            Some(moved_to(&name)),
        ),
        Press::Flag(color) => act(MailAction::Flag(color), History::Record, None),
        Press::DismissFollowUp => act(MailAction::DismissFollowUp, History::Record, None),
        Press::Drop(to) => dropped(&targets, mailbox, &to, &accounts, act),
    };
    Plan { targets, step }
}

/// What dropping `targets`, listed in `from`, on `to` comes to. `accounts`
/// are the accounts the targets belong to.
fn dropped(
    targets: &[Target],
    from: &Mailbox,
    to: &Mailbox,
    accounts: &[Reached],
    act: impl Fn(MailAction, History, Option<String>) -> Step,
) -> Step {
    if let Mailbox::Label { account_id, .. } | Mailbox::Standard { account_id, .. } = to
        && targets.iter().any(|t| t.account_id != *account_id)
    {
        return Step::Refuse(gettext("Drop mail on its own account's mailboxes"));
    }
    if let Mailbox::Flag(color) = to {
        return act(MailAction::Flag(Some(*color)), History::Record, None);
    }
    let triage = match move_action(from, to) {
        Ok(triage) => triage,
        Err(reason) => return Step::Refuse(reason),
    };
    // A folder account keeps a message in one folder, so mail dropped on
    // a folder moves there from wherever the list showed it, Flagged and
    // a search included, where a label account only adds the label.
    let triage = match (triage, to) {
        (TriageAction::Relabel { .. }, Mailbox::Label { account_id, label_id, .. })
            if accounts.iter().any(|a| a.account_id == *account_id && a.moves) =>
        {
            TriageAction::MoveTo(label_id.clone())
        }
        (triage, _) => triage,
    };
    let words = matches!(
        triage,
        TriageAction::AddLabel(_)
            | TriageAction::RemoveLabel(_)
            | TriageAction::MoveTo(_)
            | TriageAction::Relabel { .. }
    )
    .then(|| moved_to(&to.title()));
    act(MailAction::Triage(triage), History::Record, words)
}

/// The toast after mail went into the mailbox named `name`.
fn moved_to(name: &str) -> String {
    fill(&gettext("Moved to {mailbox}"), &[("mailbox", name)])
}

/// The question before Delete Forever, which names how much goes and who
/// erases it. `count` is the mail that goes, `erasers` the providers of
/// the accounts it goes from, and `kept` the addresses of the accounts
/// whose server cannot erase, whose mail stays in the Trash. Every count
/// writes its own sentence: a language decides for itself where the
/// number goes and which form the noun takes beside it.
fn erase_question(count: usize, threaded: bool, erasers: &[&str], kept: &[&str]) -> Question {
    let number = count.to_string();
    let values = [("count", number.as_str())];
    let heading = match (threaded, count) {
        (true, 1) => gettext("Delete This Conversation Forever?"),
        (true, _) => fill_plural(
            "Delete {count} Conversation Forever?",
            "Delete {count} Conversations Forever?",
            count,
            &values,
        ),
        (false, 1) => gettext("Delete This Message Forever?"),
        (false, _) => fill_plural(
            "Delete {count} Message Forever?",
            "Delete {count} Messages Forever?",
            count,
            &values,
        ),
    };
    let body = match (erasers, count) {
        ([provider], 1) => fill(
            &gettext("{provider} deletes it from every device and cannot bring it back."),
            &[("provider", *provider)],
        ),
        ([provider], _) => fill(
            &gettext("{provider} deletes them from every device and cannot bring them back."),
            &[("provider", *provider)],
        ),
        (_, 1) => gettext("The server deletes it from every device and cannot bring it back."),
        _ => gettext(
            "Each account's server deletes them from every device and cannot bring them back.",
        ),
    };
    let body = match kept {
        [] => body,
        _ => {
            let accounts = kept.join(", ");
            let stays = fill_plural(
                "Mail in {accounts} stays in the Trash, since its server cannot delete mail for good.",
                "Mail in {accounts} stays in the Trash, since their servers cannot delete mail for good.",
                kept.len(),
                &[("accounts", &accounts)],
            );
            format!("{body} {stays}")
        }
    };
    Question {
        heading,
        body,
        verb: gettext("Delete Forever"),
    }
}

/// What the Delete button and its menu item say in a mailbox where they
/// do not move mail to the Trash, with the tooltip and its key. `None`
/// where the folder's own words hold: Move to Trash, or Delete Forever in
/// the Trash.
pub(super) fn trash_words(mailbox: &Mailbox) -> Option<(String, String)> {
    let cancel = match decide(&Action::Trash, mailbox, Default::default(), true)? {
        Decision::Cancel(cancel) => cancel,
        _ => return None,
    };
    Some(match cancel {
        Cancel::Scheduled => (gettext("Cancel Send"), gettext("Cancel Send (Delete)")),
        Cancel::Queued => (
            gettext("Delete from Outbox"),
            gettext("Delete from Outbox (Delete)"),
        ),
        Cancel::Reminder => (
            gettext("Cancel Reminder"),
            gettext("Cancel Reminder (Delete)"),
        ),
        Cancel::FollowUp => (
            gettext("Dismiss Follow-Up"),
            gettext("Dismiss Follow-Up (Delete)"),
        ),
    })
}

/// How a plan changes the window. The main window is one adapter and the
/// tests are another.
pub(super) trait PressEffects {
    /// Steps past the mail the view shows.
    fn move_on(&self);
    /// Asks `question`; true on the verb.
    fn confirm(&self, question: Question) -> Answer<'_, bool>;
    /// Runs the action, with an Undo toast when `history` records it.
    fn act(&self, targets: Vec<Target>, action: MailAction, history: History, words: Option<String>);
    /// Erases the targets.
    fn erase(&self, targets: Vec<Target>);
    /// Calls off what a mailbox of queued mail holds.
    fn cancel(&self, cancel: Cancel, targets: Vec<Target>);
    fn toast(&self, text: String);
}

/// Makes `plan` happen: the move on first, so the next row opens without
/// waiting on Gmail, then the action.
pub(super) async fn carry_out(plan: Plan, effects: &dyn PressEffects) {
    let Plan { targets, step } = plan;
    match step {
        Step::Nothing => {}
        Step::Refuse(reason) => effects.toast(reason),
        Step::Act {
            action,
            history,
            move_on,
            words,
            said,
        } => {
            if move_on {
                effects.move_on();
            }
            effects.act(targets, action, history, words);
            if let Some(said) = said {
                effects.toast(said);
            }
        }
        Step::Erase(question) => {
            if effects.confirm(question).await {
                effects.erase(targets);
            }
        }
        Step::Cancel(cancel) => effects.cancel(cancel, targets),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use mailrs_domain::{Folder, MailSet};

    use super::super::triage::Marks;
    use super::*;
    use crate::ui::Standard;

    fn inbox() -> Mailbox {
        Mailbox::Unified(Standard::Inbox)
    }

    fn label(id: &str) -> Mailbox {
        Mailbox::Label {
            account_id: 1,
            label_id: id.into(),
            name: id.into(),
        }
    }

    fn folder(folder: Folder) -> Mailbox {
        Mailbox::Folder {
            account_id: None,
            folder,
        }
    }

    fn gmail(id: AccountId) -> Reached {
        Reached {
            account_id: id,
            email: format!("me{id}@gmail.com"),
            provider: "Gmail".into(),
            erases: true,
            moves: false,
        }
    }

    fn pressed(press: Press, mailbox: Mailbox, scope: Scope) -> Pressed {
        Pressed {
            press,
            reach: Reach {
                targets: vec![Target::thread(1, "t1"), Target::thread(1, "t2")],
                marks: Marks::default(),
                muted: false,
                mailbox,
            },
            scope,
            flag_color: FlagColor::Orange,
            threaded: true,
            accounts: vec![gmail(1)],
        }
    }

    fn step(press: Press, mailbox: Mailbox, scope: Scope) -> Step {
        plan(pressed(press, mailbox, scope)).step
    }

    fn moves_on(step: &Step) -> bool {
        matches!(step, Step::Act { move_on: true, .. })
    }

    #[test]
    fn a_move_into_a_folder_names_the_folder_in_its_toast() {
        let press = Press::Move {
            folder: "Label_5".into(),
            name: "Receipts".into(),
        };
        let Step::Act {
            action,
            move_on,
            words,
            ..
        } = step(press, inbox(), Scope::Shown)
        else {
            panic!("the move is taken");
        };
        assert_eq!(action, MailAction::Triage(TriageAction::MoveTo("Label_5".into())));
        assert!(move_on, "the inbox no longer lists the mail");
        assert_eq!(words.as_deref(), Some("Moved to Receipts"));
    }

    #[test]
    fn nothing_reached_does_nothing() {
        let mut nothing = pressed(Press::Button(Button::Archive), inbox(), Scope::Shown);
        nothing.reach.targets.clear();
        assert_eq!(plan(nothing).step, Step::Nothing);
    }

    #[test]
    fn archiving_in_the_inbox_moves_on_first_and_keeps_an_undo() {
        let step = step(Press::Button(Button::Archive), inbox(), Scope::Shown);
        assert_eq!(
            step,
            Step::Act {
                action: MailAction::Triage(TriageAction::Archive),
                history: History::Record,
                move_on: true,
                words: None,
                said: None,
            }
        );
    }

    #[test]
    fn the_flag_button_flags_in_the_colour_last_chosen() {
        let step = step(Press::Button(Button::ToggleStar), inbox(), Scope::Shown);
        assert!(matches!(
            step,
            Step::Act {
                action: MailAction::Flag(Some(FlagColor::Orange)),
                move_on: false,
                ..
            }
        ));
    }

    #[test]
    fn delete_in_the_trash_asks_before_erasing() {
        let Step::Erase(question) = step(
            Press::Button(Button::Trash),
            folder(Folder::Trash),
            Scope::Shown,
        ) else {
            panic!("the Trash erases");
        };
        assert_eq!(question.heading, "Delete 2 Conversations Forever?");
        assert_eq!(question.verb, "Delete Forever");
    }

    #[test]
    fn delete_in_a_trash_that_cannot_erase_asks_nothing_and_does_nothing() {
        let pressed = Pressed {
            accounts: vec![Reached {
                erases: false,
                ..gmail(1)
            }],
            ..pressed(Press::Button(Button::Trash), folder(Folder::Trash), Scope::Shown)
        };
        assert_eq!(plan(pressed).step, Step::Nothing);
    }

    #[test]
    fn mail_dropped_on_a_folder_of_a_folder_account_moves_there_from_anywhere() {
        let folders = Reached {
            moves: true,
            ..gmail(1)
        };
        let search = Mailbox::Search {
            query: "x".into(),
            account_id: None,
        };
        for from in [Mailbox::Unified(Standard::Flagged), search, label("Work")] {
            let drop = Pressed {
                accounts: vec![folders.clone()],
                ..pressed(
                    Press::Drop(label("Receipts")),
                    from.clone(),
                    Scope::Carried { open: false },
                )
            };
            let Step::Act { action, words, .. } = plan(drop).step else {
                panic!("the drop from {from:?} is taken");
            };
            assert_eq!(
                action,
                MailAction::Triage(TriageAction::MoveTo("Receipts".into())),
                "from {from:?}"
            );
            assert_eq!(words.as_deref(), Some("Moved to Receipts"));
        }
    }

    #[test]
    fn each_account_a_press_reaches_counts_once() {
        let targets = [
            Target::thread(1, "a"),
            Target::thread(2, "b"),
            Target::thread(1, "c"),
        ];
        let offers = |id| match id {
            2 => Offers {
                labels: false,
                delete_forever: false,
                ..Offers::EVERYTHING
            },
            _ => Offers::EVERYTHING,
        };
        let unnamed = |id| Reached {
            email: String::new(),
            provider: String::new(),
            ..gmail(id)
        };
        assert_eq!(
            reached(&targets, |_| None, offers),
            [
                unnamed(1),
                Reached {
                    erases: false,
                    moves: true,
                    ..unnamed(2)
                },
            ]
        );
    }

    fn trash_of(targets: Vec<Target>, accounts: Vec<Reached>) -> Plan {
        let mut pressed = pressed(Press::Button(Button::Trash), folder(Folder::Trash), Scope::Shown);
        pressed.reach.targets = targets;
        pressed.accounts = accounts;
        plan(pressed)
    }

    #[test]
    fn delete_forever_names_the_provider_that_erases() {
        let fastmail = Reached {
            provider: "Fastmail".into(),
            ..gmail(1)
        };
        let Step::Erase(one) = trash_of(vec![Target::thread(1, "t1")], vec![fastmail.clone()]).step
        else {
            panic!("the Trash erases");
        };
        assert_eq!(
            one.body,
            "Fastmail deletes it from every device and cannot bring it back."
        );
        let two = vec![Target::thread(1, "t1"), Target::thread(1, "t2")];
        let Step::Erase(many) = trash_of(two, vec![fastmail]).step else {
            panic!("the Trash erases");
        };
        assert_eq!(
            many.body,
            "Fastmail deletes them from every device and cannot bring them back."
        );
    }

    #[test]
    fn delete_forever_erases_where_it_can_and_names_the_accounts_that_cannot() {
        let keeps = Reached {
            erases: false,
            ..gmail(2)
        };
        let targets = vec![
            Target::thread(1, "t1"),
            Target::thread(2, "t2"),
            Target::thread(1, "t3"),
        ];
        let plan = trash_of(targets, vec![gmail(1), keeps]);
        assert_eq!(
            plan.targets,
            [Target::thread(1, "t1"), Target::thread(1, "t3")],
            "only the mail of the account that can erase goes"
        );
        let Step::Erase(question) = plan.step else {
            panic!("account 1 erases");
        };
        assert_eq!(question.heading, "Delete 2 Conversations Forever?");
        assert_eq!(
            question.body,
            "Gmail deletes them from every device and cannot bring them back. \
             Mail in me2@gmail.com stays in the Trash, since its server cannot \
             delete mail for good."
        );
    }

    #[test]
    fn delete_forever_across_providers_names_each_accounts_server() {
        let fastmail = Reached {
            provider: "Fastmail".into(),
            ..gmail(2)
        };
        let targets = vec![Target::thread(1, "t1"), Target::thread(2, "t2")];
        let Step::Erase(question) = trash_of(targets, vec![gmail(1), fastmail]).step else {
            panic!("both erase");
        };
        assert_eq!(
            question.body,
            "Each account's server deletes them from every device and cannot bring them back."
        );
    }

    #[test]
    fn delete_calls_off_a_reminder_and_says_where_the_mail_went() {
        let step = step(Press::Button(Button::Trash), Mailbox::Reminders, Scope::Shown);
        assert_eq!(
            step,
            Step::Act {
                action: MailAction::CancelReminder,
                history: History::Skip,
                move_on: true,
                words: None,
                said: Some("Back in the Inbox".into()),
            }
        );
        assert_eq!(
            self::step(Press::Button(Button::Trash), Mailbox::Scheduled, Scope::Shown),
            Step::Cancel(Cancel::Scheduled)
        );
    }

    #[test]
    fn a_message_menu_calls_nothing_off() {
        let scope = Scope::Message { alone: true };
        assert_eq!(
            step(Press::Button(Button::Trash), Mailbox::Outbox, scope),
            Step::Nothing
        );
    }

    #[test]
    fn one_message_of_a_longer_thread_leaves_the_reader_where_they_are() {
        let alone = step(
            Press::Button(Button::Archive),
            inbox(),
            Scope::Message { alone: true },
        );
        let one_of_many = step(
            Press::Button(Button::Archive),
            inbox(),
            Scope::Message { alone: false },
        );
        assert!(moves_on(&alone));
        assert!(!moves_on(&one_of_many));
    }

    #[test]
    fn mute_unmutes_what_is_muted_already() {
        let mut muted = pressed(Press::Mute, inbox(), Scope::Shown);
        muted.reach.muted = true;
        assert!(matches!(
            plan(muted).step,
            Step::Act {
                action: MailAction::Mute { muted: false },
                move_on: false,
                ..
            }
        ));
    }

    #[test]
    fn a_reminder_says_when_it_comes_back() {
        let press = Press::Remind {
            at: 5,
            when: "tomorrow".into(),
        };
        let step = step(press, inbox(), Scope::Shown);
        assert!(moves_on(&step));
        let Step::Act { words, .. } = step else {
            panic!("a reminder acts");
        };
        assert_eq!(words.as_deref(), Some("Will remind you tomorrow"));
    }

    #[test]
    fn a_label_added_from_sent_leaves_the_reader_on_mail_still_listed() {
        let sent = Mailbox::Unified(Standard::Sent);
        let drop = step(
            Press::Drop(label("Travel")),
            sent,
            Scope::Carried { open: true },
        );
        let Step::Act {
            action,
            move_on,
            words,
            ..
        } = drop
        else {
            panic!("the drop is taken");
        };
        assert_eq!(
            action,
            MailAction::Triage(TriageAction::Relabel {
                add: vec![MailSet::Mailbox("Travel".into())],
                remove: vec![],
            })
        );
        assert!(!move_on, "Sent still lists the mail");
        assert_eq!(words.as_deref(), Some("Moved to Travel"));
    }

    #[test]
    fn a_drop_that_takes_the_open_conversation_out_moves_on() {
        let drop = |open| {
            step(
                Press::Drop(label("Travel")),
                label("Work"),
                Scope::Carried { open },
            )
        };
        assert!(moves_on(&drop(true)));
        assert!(!moves_on(&drop(false)), "the reader is on other mail");
        let starred = step(
            Press::Drop(Mailbox::Unified(Standard::Flagged)),
            inbox(),
            Scope::Carried { open: true },
        );
        assert!(!moves_on(&starred), "flagged mail stays in the inbox");
    }

    #[test]
    fn a_drop_on_another_accounts_label_or_where_the_mail_is_is_refused() {
        let other = Mailbox::Label {
            account_id: 2,
            label_id: "Travel".into(),
            name: "Travel".into(),
        };
        assert_eq!(
            step(Press::Drop(other), inbox(), Scope::Carried { open: false }),
            Step::Refuse("Drop mail on its own account's mailboxes".into())
        );
        let other_inbox = Mailbox::Standard {
            account_id: 2,
            which: Standard::Inbox,
        };
        assert_eq!(
            step(Press::Drop(other_inbox), inbox(), Scope::Carried { open: false }),
            Step::Refuse("Drop mail on its own account's mailboxes".into())
        );
        assert!(matches!(
            step(Press::Drop(inbox()), inbox(), Scope::Carried { open: false }),
            Step::Refuse(_)
        ));
        let plan = plan(pressed(
            Press::Drop(inbox()),
            inbox(),
            Scope::Carried { open: false },
        ));
        assert!(!plan.taken());
    }

    #[test]
    fn a_drop_on_a_flag_colour_flags_in_it() {
        let step = step(
            Press::Drop(Mailbox::Flag(FlagColor::Green)),
            inbox(),
            Scope::Carried { open: true },
        );
        assert!(matches!(
            step,
            Step::Act {
                action: MailAction::Flag(Some(FlagColor::Green)),
                ..
            }
        ));
    }

    #[test]
    fn delete_says_what_it_does_in_each_mailbox() {
        assert_eq!(trash_words(&inbox()), None);
        assert_eq!(trash_words(&folder(Folder::Trash)), None);
        let word = |mailbox| trash_words(&mailbox).map(|(word, _)| word);
        assert_eq!(word(Mailbox::Scheduled).as_deref(), Some("Cancel Send"));
        assert_eq!(word(Mailbox::Outbox).as_deref(), Some("Delete from Outbox"));
        assert_eq!(word(Mailbox::Reminders).as_deref(), Some("Cancel Reminder"));
        assert_eq!(word(Mailbox::FollowUp).as_deref(), Some("Dismiss Follow-Up"));
        let (_, tip) = trash_words(&Mailbox::Reminders).unwrap();
        assert_eq!(tip, "Cancel Reminder (Delete)");
    }

    /// A window that records what a plan did to it, in order, and answers
    /// every question with `yes`.
    struct Window {
        yes: bool,
        did: RefCell<Vec<String>>,
    }

    impl Window {
        fn answering(yes: bool) -> Window {
            Window {
                yes,
                did: RefCell::new(Vec::new()),
            }
        }

        fn did(&self, what: impl Into<String>) {
            self.did.borrow_mut().push(what.into());
        }
    }

    impl PressEffects for Window {
        fn move_on(&self) {
            self.did("move on");
        }

        fn confirm(&self, question: Question) -> Answer<'_, bool> {
            self.did(format!("ask {}", question.heading));
            Box::pin(std::future::ready(self.yes))
        }

        fn act(&self, targets: Vec<Target>, action: MailAction, _: History, _: Option<String>) {
            self.did(format!("act {action:?} on {}", targets.len()));
        }

        fn erase(&self, targets: Vec<Target>) {
            self.did(format!("erase {}", targets.len()));
        }

        fn cancel(&self, cancel: Cancel, _: Vec<Target>) {
            self.did(format!("cancel {cancel:?}"));
        }

        fn toast(&self, text: String) {
            self.did(format!("toast {text}"));
        }
    }

    async fn run(pressed: Pressed, yes: bool) -> Vec<String> {
        let window = Window::answering(yes);
        carry_out(plan(pressed), &window).await;
        window.did.into_inner()
    }

    #[tokio::test]
    async fn the_window_moves_on_before_the_action_goes_out() {
        let did = run(
            pressed(Press::Button(Button::Archive), inbox(), Scope::Shown),
            true,
        )
        .await;
        assert_eq!(did, ["move on", "act Triage(Archive) on 2"]);
    }

    #[tokio::test]
    async fn mail_is_erased_only_on_a_yes() {
        let trash = || pressed(Press::Button(Button::Trash), folder(Folder::Trash), Scope::Shown);
        let no = run(trash(), false).await;
        assert_eq!(no, ["ask Delete 2 Conversations Forever?"]);
        let yes = run(trash(), true).await;
        assert_eq!(yes, ["ask Delete 2 Conversations Forever?", "erase 2"]);
    }

    #[tokio::test]
    async fn a_cancelled_reminder_says_so_after_the_action() {
        let did = run(
            pressed(Press::Button(Button::Trash), Mailbox::Reminders, Scope::Shown),
            true,
        )
        .await;
        assert_eq!(
            did,
            ["move on", "act CancelReminder on 2", "toast Back in the Inbox"]
        );
    }

    #[tokio::test]
    async fn a_refused_drop_only_says_why() {
        let did = run(
            pressed(Press::Drop(inbox()), inbox(), Scope::Carried { open: true }),
            true,
        )
        .await;
        assert_eq!(did, ["toast The mail is already there"]);
    }

    #[tokio::test]
    async fn the_send_later_list_hands_its_mail_to_the_outbox_to_call_off() {
        let did = run(
            pressed(Press::Button(Button::Trash), Mailbox::Scheduled, Scope::Shown),
            true,
        )
        .await;
        assert_eq!(did, ["cancel Scheduled"]);
    }
}
