//! The event card: an invitation as a card above the message, with the
//! answer buttons in it.
//!
//! The message body itself goes on being drawn in the WebView below, so a
//! user who declines the calendar permission still has Google's own Yes,
//! No and Maybe links there.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use chrono::{DateTime, Days, Local, TimeDelta};
use mailrs_domain::invitation::{Answer, Invitation, Method, Scope, When};
use mailrs_domain::{Address, EpochMillis};
use mailrs_sync::Change;

use crate::format::{event_moved_from, event_tile, event_when};
use crate::ui::name;
use mailrs_domain::translate::{fill, fill_plural, gettext};

/// What the card asks the window to do.
pub enum Action {
    /// Send this answer to the organizer, for the one occurrence the
    /// invitation names or for the whole series.
    Answer(Answer, Scope),
    /// Ask the organizer for another time.
    Propose(Proposal),
    /// Hand the `.ics` to the desktop, which files it in GNOME Calendar.
    AddToCalendar,
    /// Answer the offer to add this account to GNOME Online Accounts.
    /// Either way the offer is over.
    OnlineAccounts { open: bool },
}

/// Which time to propose to the organizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proposal {
    /// One of the times the card offered, worked out from the one the
    /// organizer asked for.
    At(EpochMillis),
    /// Whatever the user picks from a calendar.
    Pick,
}

/// One invitation as the card shows it.
#[derive(Clone)]
pub struct Showing {
    pub invitation: Invitation,
    pub change: Option<Change>,
    /// The answer the user already sent, if any. It wins over the guest
    /// list the organizer sent, which was written before the user answered.
    pub answer: Option<Answer>,
    /// The addresses of the account the message arrived in, so the card can
    /// find the user among the guests and call them "You".
    pub me: Vec<String>,
}

impl Showing {
    /// The address an answer goes out as: the one the invitation reached,
    /// under the name the organizer put on the guest list, so a reply
    /// matches the guest it answers for. An invitation that lists none of
    /// the account's addresses answers as the account itself.
    pub fn answering_as(&self) -> Option<Address> {
        let guest = self.invitation.me(&self.me);
        Some(Address {
            name: guest.and_then(|guest| guest.who.name.clone()),
            email: match guest {
                Some(guest) => guest.who.email.clone(),
                None => self.me.first()?.clone(),
            },
        })
    }
}

/// One line of the guest list.
struct Attending {
    name: String,
    answer: Option<Answer>,
}

pub struct EventCard {
    pub widget: gtk::Box,
    news: gtk::Label,
    month: gtk::Label,
    day: gtk::Label,
    title: gtk::Label,
    when: gtk::Label,
    repeats: gtk::Label,
    /// What else the user has on while the event runs.
    clash: gtk::Label,
    location: gtk::Label,
    organizer: gtk::Label,
    guests: gtk::Expander,
    guest_list: gtk::Box,
    answers: gtk::Box,
    buttons: Vec<(Answer, gtk::ToggleButton)>,
    add: gtk::Button,
    /// The button that opens the other times to ask the organizer for,
    /// and the list inside it, which is rebuilt for each invitation. It
    /// appears only for an invitation with a time to move.
    propose: gtk::MenuButton,
    proposals: gtk::Box,
    /// What the card's buttons ask the window for. The propose list is
    /// built as each invitation goes up, so the card keeps it.
    act: Rc<dyn Fn(Action)>,
    /// The row that asks whether an answer covers this occurrence or the
    /// series. It appears for an invitation to one occurrence of a
    /// repeating event and stays hidden for every other.
    reach: gtk::Box,
    /// Which of the two the answer buttons will send. It opens on this
    /// occurrence, which is what the organizer asked about.
    scope: Cell<Scope>,
    /// Where the last answer went, under the buttons that sent it.
    went: gtk::Label,
    /// The line offering this account to GNOME Online Accounts, which the
    /// window puts up once an account and never again.
    gnome: gtk::Box,
    /// What the card shows now. The window reads it back to answer the
    /// invitation, so the card is the one place that holds it.
    showing: RefCell<Option<Showing>>,
    /// Set while the card fills its buttons in, so setting one does not
    /// look like the user pressing it.
    filling: Cell<bool>,
}

impl EventCard {
    pub fn new(on_action: impl Fn(Action) + 'static) -> Rc<EventCard> {
        let on_action: Rc<dyn Fn(Action)> = Rc::new(on_action);
        let label = |classes: &[&str]| {
            gtk::Label::builder()
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .css_classes(classes.to_vec())
                .build()
        };

        let month = gtk::Label::builder()
            .css_classes(["invitation-month"])
            .build();
        let day = gtk::Label::builder()
            .css_classes(["invitation-day"])
            .build();
        let tile = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Start)
            .css_classes(["invitation-tile"])
            .build();
        tile.append(&month);
        tile.append(&day);

        let title = label(&["title-3"]);
        let when = label(&["invitation-when"]);
        let repeats = label(&["dim-label", "caption"]);
        let clash = label(&["invitation-clash", "caption"]);
        let location = label(&["dim-label"]);
        let organizer = label(&["dim-label", "caption"]);
        let details = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .build();
        for widget in [&title, &when, &repeats, &clash, &location] {
            details.append(widget);
        }

        let head = gtk::Box::builder().spacing(14).build();
        head.append(&tile);
        head.append(&details);

        let guest_list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_top(6)
            .margin_start(4)
            .build();
        let guests = gtk::Expander::builder()
            .use_markup(false)
            .child(&guest_list)
            .build();

        let answers = gtk::Box::builder()
            .spacing(0)
            .margin_top(4)
            .css_classes(["linked"])
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        name(&answers, &gettext("Answer"));
        let mut buttons = Vec::new();
        for answer in Answer::ALL {
            let button = gtk::ToggleButton::builder().label(answer.label()).build();
            answers.append(&button);
            buttons.push((answer, button));
        }
        let reach = gtk::Box::builder()
            .spacing(0)
            .visible(false)
            .css_classes(["linked"])
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        name(&reach, &gettext("What the answer covers"));
        let this_one = gtk::ToggleButton::builder()
            .label(gettext("This Event"))
            .active(true)
            .css_classes(["flat"])
            .build();
        let every = gtk::ToggleButton::builder()
            .label(gettext("All Events"))
            .group(&this_one)
            .css_classes(["flat"])
            .build();
        reach.append(&this_one);
        reach.append(&every);

        let proposals = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .build();
        let propose = gtk::MenuButton::builder()
            .label(gettext("Propose New Time"))
            .css_classes(["flat"])
            .popover(
                &gtk::Popover::builder()
                    .child(&proposals)
                    .has_arrow(true)
                    .build(),
            )
            .build();
        let add = gtk::Button::builder()
            .label(gettext("Add to Calendar"))
            .css_classes(["flat"])
            .build();
        let actions = gtk::Box::builder().spacing(8).margin_top(6).build();
        actions.append(&answers);
        actions.append(&reach);
        let spacer = gtk::Box::builder().hexpand(true).build();
        actions.append(&spacer);
        actions.append(&propose);
        actions.append(&add);

        let news = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["invitation-news"])
            .build();
        let went = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["dim-label", "caption"])
            .build();

        // GNOME Calendar shows nothing about an account GNOME has never
        // been told about, and adding it there is a job for Settings.
        let gnome = gtk::Box::builder()
            .spacing(8)
            .margin_top(4)
            .visible(false)
            .build();
        let told = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["dim-label", "caption"])
            .label(gettext(
                "Add this account to GNOME Online Accounts and Calendar shows these \
                 events too.",
            ))
            .build();
        let open_settings = gtk::Button::builder()
            .label(gettext("Open Settings"))
            .css_classes(["flat"])
            .build();
        let not_now = gtk::Button::builder()
            .label(gettext("Not Now"))
            .css_classes(["flat"])
            .build();
        gnome.append(&told);
        gnome.append(&not_now);
        gnome.append(&open_settings);

        let inside = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .css_classes(["card", "invitation-card"])
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        name(&inside, &gettext("Invitation"));
        inside.append(&news);
        inside.append(&head);
        inside.append(&organizer);
        inside.append(&guests);
        inside.append(&actions);
        inside.append(&went);
        inside.append(&gnome);

        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .visible(false)
            .css_classes(["invitation-area"])
            .build();
        widget.append(&inside);

        let card = Rc::new(EventCard {
            widget,
            news,
            month,
            day,
            title,
            when,
            repeats,
            clash,
            location,
            organizer,
            guests,
            guest_list,
            answers,
            buttons,
            add,
            propose,
            proposals,
            act: Rc::clone(&on_action),
            gnome,
            reach,
            scope: Cell::new(Scope::Occurrence),
            went,
            showing: RefCell::new(None),
            filling: Cell::new(false),
        });

        for (scope, button) in [(Scope::Occurrence, &this_one), (Scope::Series, &every)] {
            let weak = Rc::downgrade(&card);
            button.connect_toggled(move |button| {
                if let (true, Some(card)) = (button.is_active(), weak.upgrade()) {
                    card.scope.set(scope);
                }
            });
        }

        for (answer, button) in &card.buttons {
            let (answer, act, weak) = (*answer, Rc::clone(&on_action), Rc::downgrade(&card));
            button.connect_toggled(move |button| {
                let Some(card) = weak.upgrade() else { return };
                if card.filling.get() {
                    return;
                }
                if button.is_active() {
                    card.mark(Some(answer));
                    act(Action::Answer(answer, card.scope.get()));
                } else {
                    // Pressing the answer already given keeps it: taking an
                    // answer back is not something Google Calendar does.
                    card.mark(Some(answer));
                }
            });
        }
        for (open, button) in [(true, &open_settings), (false, &not_now)] {
            let (act, weak) = (Rc::clone(&on_action), Rc::downgrade(&card));
            button.connect_clicked(move |_| {
                if let Some(card) = weak.upgrade() {
                    card.gnome.set_visible(false);
                }
                act(Action::OnlineAccounts { open });
            });
        }

        card.add
            .connect_clicked(move |_| on_action(Action::AddToCalendar));
        card
    }

    /// Fills the card from an invitation and shows it.
    pub fn show(&self, showing: Showing) {
        self.draw(&showing);
        *self.showing.borrow_mut() = Some(showing);
    }

    pub fn hide(&self) {
        self.widget.set_visible(false);
        *self.showing.borrow_mut() = None;
    }

    /// Reads what the card shows. `None` means no invitation is on screen.
    pub fn with_showing<R>(&self, f: impl FnOnce(&Showing) -> R) -> Option<R> {
        self.showing.borrow().as_ref().map(f)
    }

    /// Which of the two the chooser is on, for an invitation that shows
    /// one. A proposal reaches as far as an answer would.
    pub fn scope(&self) -> Scope {
        self.scope.get()
    }

    /// Whether the card still shows the invitation `uid` names. Every
    /// setter an answer comes back to asks this first: the answer arrives
    /// after the card is already up, and the user may have opened another
    /// message by then.
    fn shows(&self, uid: &str) -> bool {
        let held = self.showing.borrow();
        let on_screen = held.as_ref().map(|showing| showing.invitation.uid.as_str());
        still_showing(on_screen, uid)
    }

    /// Says what else the user has on while this event runs.
    pub fn set_busy(&self, uid: &str, busy: &[String]) {
        if self.shows(uid) {
            set_line(&self.clash, clash(busy));
        }
    }

    /// Puts up the offer to add this account to GNOME Online Accounts.
    pub fn offer_gnome(&self) {
        self.gnome.set_visible(true);
    }

    /// Says under the buttons where the answer went, or takes the line
    /// away while one is on its way.
    pub fn set_went(&self, uid: &str, went: Option<String>) {
        if self.shows(uid) {
            set_line(&self.went, went);
        }
    }

    /// Puts the card back where an answer left it: on the one that went
    /// through, or on the one it showed before an answer that did not.
    pub fn set_answer(&self, uid: &str, answer: Option<Answer>) {
        if !self.shows(uid) {
            return;
        }
        let updated = {
            let mut held = self.showing.borrow_mut();
            let Some(showing) = held.as_mut() else {
                return;
            };
            showing.answer = answer;
            showing.clone()
        };
        self.draw(&updated);
    }

    fn draw(&self, showing: &Showing) {
        let now = Local::now();
        let event = &showing.invitation;
        self.title.set_text(&if event.summary.is_empty() {
            gettext("Untitled event")
        } else {
            event.summary.clone()
        });
        match &event.when {
            Some(when) => {
                let (month, day) = event_tile(when);
                self.month.set_text(&month);
                self.day.set_text(&day);
                self.when.set_text(&event_when(when, now));
                self.when.set_visible(true);
            }
            None => {
                self.month.set_text("");
                self.day.set_text("?");
                self.when.set_visible(false);
            }
        }
        set_line(&self.repeats, event.repeats.clone());
        self.clash.set_visible(false);
        set_line(&self.location, event.location.clone());
        set_line(&self.organizer, organizer_line(event));
        self.fill_guests(showing);
        self.went.set_visible(false);
        self.gnome.set_visible(false);
        set_line(&self.news, news(showing, now));
        self.news
            .set_css_classes(&["invitation-news", news_tone(showing)]);

        // A cancellation and a reply from somebody else are news, not a
        // question, so neither gets answer buttons.
        let answerable = event.method == Method::Request && !event.cancelled();
        self.answers.set_visible(answerable);
        // Only an invitation to one occurrence of a series leaves the
        // question open; an answer to anything else covers the lot.
        self.reach
            .set_visible(answerable && event.occurrence.is_some());
        self.fill_proposals(showing, answerable);
        self.mark(showing.answer);
        self.widget.set_visible(true);
    }

    /// Puts the pressed look on one answer and takes it off the others.
    fn mark(&self, answer: Option<Answer>) {
        self.filling.set(true);
        for (button_answer, button) in &self.buttons {
            let chosen = Some(*button_answer) == answer;
            button.set_active(chosen);
            button.remove_css_class("suggested-action");
            if chosen {
                button.add_css_class("suggested-action");
            }
        }
        self.filling.set(false);
    }

    /// Fills the propose list with times near the one the organizer asked
    /// for, and a way to pick any other. An all-day event and one with no
    /// time at all have nothing to move, so they get no button.
    fn fill_proposals(&self, showing: &Showing, answerable: bool) {
        while let Some(child) = self.proposals.first_child() {
            self.proposals.remove(&child);
        }
        let starts_at = match showing.invitation.when {
            Some(When::At { starts_at, .. }) if answerable => starts_at,
            _ => {
                self.propose.set_visible(false);
                return;
            }
        };
        let mut offered: Vec<(String, Proposal)> = nearby(starts_at)
            .into_iter()
            .map(|(label, at)| (label, Proposal::At(at)))
            .collect();
        offered.push((gettext("Pick a Time…"), Proposal::Pick));
        for (label, proposal) in offered {
            let button = gtk::Button::builder()
                .label(&label)
                .css_classes(["flat"])
                .build();
            let (act, popover) = (Rc::clone(&self.act), self.propose.popover());
            button.connect_clicked(move |_| {
                if let Some(popover) = &popover {
                    popover.popdown();
                }
                act(Action::Propose(proposal));
            });
            self.proposals.append(&button);
        }
        self.propose.set_visible(true);
    }

    fn fill_guests(&self, showing: &Showing) {
        while let Some(child) = self.guest_list.first_child() {
            self.guest_list.remove(&child);
        }
        let attending = attending(showing);
        if attending.is_empty() {
            self.guests.set_visible(false);
            return;
        }
        self.guests.set_visible(true);
        self.guests.set_label(Some(&guest_summary(&attending)));
        for guest in &attending {
            let row = gtk::Box::builder().spacing(8).build();
            let name = gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .label(&guest.name)
                .build();
            let said = gtk::Label::builder()
                .css_classes(["dim-label", "caption"])
                .label(match guest.answer {
                    Some(answer) => answer.said(),
                    None => gettext("No reply yet"),
                })
                .build();
            row.append(&name);
            row.append(&said);
            self.guest_list.append(&row);
        }
    }
}

/// Whether an answer that names `uid` belongs to the invitation on
/// screen, `on_screen` being the UID the card holds and `None` meaning no
/// invitation is up. An answer to one invitation must never land on
/// another, and an invitation the organizer gave no UID answers for
/// nothing, since there is no telling it from the next one.
fn still_showing(on_screen: Option<&str>, uid: &str) -> bool {
    !uid.trim().is_empty() && on_screen == Some(uid)
}

/// The times the propose list offers, each the same meeting moved whole.
/// A day and a week go through the local calendar rather than through
/// arithmetic on the instant, so the hour stays put across a clock change.
fn nearby(starts_at: EpochMillis) -> Vec<(String, EpochMillis)> {
    let Some(start) = DateTime::from_timestamp_millis(starts_at).map(|at| at.with_timezone(&Local))
    else {
        return Vec::new();
    };
    [
        (
            gettext("Half an Hour Later"),
            Some(start + TimeDelta::minutes(30)),
        ),
        (gettext("An Hour Later"), Some(start + TimeDelta::hours(1))),
        (
            gettext("Same Time Tomorrow"),
            start.checked_add_days(Days::new(1)),
        ),
        (
            gettext("Same Time Next Week"),
            start.checked_add_days(Days::new(7)),
        ),
    ]
    .into_iter()
    .filter_map(|(label, at)| Some((label, at?.timestamp_millis())))
    .collect()
}

/// "You have Design crit then", for an event the user already has while
/// this one runs. Two clashes name both; more than two name the first and
/// count the rest, since the point is that the hour is taken.
fn clash(busy: &[String]) -> Option<String> {
    Some(match busy {
        [] => return None,
        [one] => fill(&gettext("You have {event} then"), &[("event", one)]),
        [one, two] => fill(
            &gettext("You have {event} and {other} then"),
            &[("event", one), ("other", two)],
        ),
        [one, rest @ ..] => fill_plural(
            "You have {event} and {count} more then",
            "You have {event} and {count} more then",
            rest.len(),
            &[("event", one), ("count", &rest.len().to_string())],
        ),
    })
}

/// "Invitation from Priya Raman", or nothing when the organizer is missing.
fn organizer_line(event: &Invitation) -> Option<String> {
    let who = event.organizer.as_ref()?;
    let values = [("organizer", who.display())];
    Some(match event.method {
        Method::Reply => fill(
            &gettext("Reply to the invitation from {organizer}"),
            &values,
        ),
        _ => fill(&gettext("Invitation from {organizer}"), &values),
    })
}

/// The guest list as the card shows it: the user's own row reads "You"
/// and carries the answer they sent, which the organizer's copy of the
/// list predates.
fn attending(showing: &Showing) -> Vec<Attending> {
    let me = showing
        .invitation
        .me(&showing.me)
        .map(|guest| guest.who.email.clone());
    showing
        .invitation
        .guests
        .iter()
        .map(|guest| {
            let mine = me.as_deref() == Some(guest.who.email.as_str());
            Attending {
                name: if mine {
                    gettext("You")
                } else {
                    guest.who.display().to_string()
                },
                answer: if mine {
                    showing.answer.or(guest.answer)
                } else {
                    guest.answer
                },
            }
        })
        .collect()
}

/// "4 guests · 2 yes, 1 maybe, 1 awaiting".
fn guest_summary(guests: &[Attending]) -> String {
    let count = |wanted: Option<Answer>| guests.iter().filter(|g| g.answer == wanted).count();
    let (yes, no, maybe) = (
        count(Some(Answer::Yes)),
        count(Some(Answer::No)),
        count(Some(Answer::Maybe)),
    );
    let waiting = count(None);
    let mut parts = Vec::new();
    if yes > 0 {
        parts.push(fill_plural(
            "{count} yes",
            "{count} yes",
            yes,
            &[("count", &yes.to_string())],
        ));
    }
    if no > 0 {
        parts.push(fill_plural(
            "{count} no",
            "{count} no",
            no,
            &[("count", &no.to_string())],
        ));
    }
    if maybe > 0 {
        parts.push(fill_plural(
            "{count} maybe",
            "{count} maybe",
            maybe,
            &[("count", &maybe.to_string())],
        ));
    }
    if waiting > 0 {
        parts.push(fill_plural(
            "{count} awaiting",
            "{count} awaiting",
            waiting,
            &[("count", &waiting.to_string())],
        ));
    }
    let all = guests.len();
    let counted = fill_plural(
        "{count} guest",
        "{count} guests",
        all,
        &[("count", &all.to_string())],
    );
    fill(
        &gettext("{guests} · {answers}"),
        &[("guests", &counted), ("answers", &parts.join(", "))],
    )
}

/// The line above the card: what this message does to an event the user
/// already has, or that the organizer called it off.
fn news(showing: &Showing, now: chrono::DateTime<Local>) -> Option<String> {
    match showing.change {
        Some(Change::Moved { was, all_day }) => Some(fill(
            &gettext("This meeting moved from {when}"),
            &[("when", &event_moved_from(was, all_day, now))],
        )),
        Some(Change::Updated) => Some(gettext("The organizer changed this meeting")),
        Some(Change::Cancelled) => Some(gettext("The organizer canceled this meeting")),
        None if showing.invitation.cancelled() => Some(gettext("This meeting is canceled")),
        None if showing.invitation.method == Method::Reply => None,
        None => None,
    }
}

/// The colour the news line takes: a cancellation reads as a warning, an
/// update as a note.
fn news_tone(showing: &Showing) -> &'static str {
    match showing.change {
        Some(Change::Cancelled) => "cancelled",
        _ if showing.invitation.cancelled() => "cancelled",
        _ => "changed",
    }
}

/// Shows a label with `text`, or hides it when there is none.
fn set_line(label: &gtk::Label, text: Option<String>) {
    match text {
        Some(text) if !text.trim().is_empty() => {
            label.set_text(&text);
            label.set_visible(true);
        }
        _ => label.set_visible(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One event, written as a mail client writes it. `extra` holds the
    /// attendees and whatever else a test needs in the `VEVENT`.
    fn ics(method: &str, extra: &[&str]) -> String {
        let mut lines = vec![
            "BEGIN:VCALENDAR".to_string(),
            format!("METHOD:{method}"),
            "BEGIN:VEVENT".to_string(),
            "UID:design@example.com".to_string(),
            "SUMMARY:Design review".to_string(),
            "DTSTART:20240610T090000Z".to_string(),
            "ORGANIZER;CN=Priya Raman:mailto:priya@example.com".to_string(),
        ];
        lines.extend(extra.iter().map(|line| line.to_string()));
        lines.push("END:VEVENT".to_string());
        lines.push("END:VCALENDAR".to_string());
        lines.push(String::new());
        lines.join("\r\n")
    }

    fn showing(method: &str, extra: &[&str]) -> Showing {
        Showing {
            invitation: mailrs_domain::invitation::read(&ics(method, extra))
                .expect("the part holds an event"),
            change: None,
            answer: None,
            me: vec!["me@example.com".to_string()],
        }
    }

    #[test]
    fn an_answer_lands_only_on_the_invitation_it_answers() {
        assert!(still_showing(
            Some("design@example.com"),
            "design@example.com"
        ));
        assert!(!still_showing(
            Some("budget@example.com"),
            "design@example.com"
        ));
        assert!(!still_showing(None, "design@example.com"));
    }

    #[test]
    fn an_invitation_with_no_uid_answers_for_nothing() {
        assert!(!still_showing(Some(""), ""));
        assert!(!still_showing(Some("   "), "   "));
        assert!(!still_showing(Some("design@example.com"), ""));
    }

    #[test]
    fn a_taken_hour_names_what_takes_it() {
        assert_eq!(clash(&[]), None);
        assert_eq!(
            clash(&["Design crit".to_string()]).as_deref(),
            Some("You have Design crit then")
        );
        assert_eq!(
            clash(&["Design crit".to_string(), "Standup".to_string()]).as_deref(),
            Some("You have Design crit and Standup then")
        );
        assert_eq!(
            clash(&[
                "Design crit".to_string(),
                "Standup".to_string(),
                "One to one".to_string(),
            ])
            .as_deref(),
            Some("You have Design crit and 2 more then")
        );
    }

    #[test]
    fn the_organizer_line_follows_who_is_speaking() {
        assert_eq!(
            organizer_line(&showing("REQUEST", &[]).invitation).as_deref(),
            Some("Invitation from Priya Raman")
        );
        assert_eq!(
            organizer_line(&showing("REPLY", &[]).invitation).as_deref(),
            Some("Reply to the invitation from Priya Raman")
        );
        let mut anonymous = showing("REQUEST", &[]).invitation;
        anonymous.organizer = None;
        assert_eq!(organizer_line(&anonymous), None);
    }

    #[test]
    fn the_guest_list_calls_the_user_you_and_carries_their_answer() {
        let mut showing = showing(
            "REQUEST",
            &[
                "ATTENDEE;PARTSTAT=ACCEPTED;CN=Ann Lee:mailto:ann@example.com",
                "ATTENDEE;PARTSTAT=NEEDS-ACTION;CN=Me:mailto:me@example.com",
            ],
        );
        showing.answer = Some(Answer::Yes);
        let guests = attending(&showing);
        assert_eq!(guests[0].name, "Ann Lee");
        assert_eq!(guests[0].answer, Some(Answer::Yes));
        assert_eq!(guests[1].name, "You");
        assert_eq!(guests[1].answer, Some(Answer::Yes));
    }

    #[test]
    fn the_guest_summary_counts_each_answer() {
        let showing = showing(
            "REQUEST",
            &[
                "ATTENDEE;PARTSTAT=ACCEPTED;CN=Ann Lee:mailto:ann@example.com",
                "ATTENDEE;PARTSTAT=ACCEPTED;CN=Bo Chen:mailto:bo@example.com",
                "ATTENDEE;PARTSTAT=TENTATIVE;CN=Cal Diaz:mailto:cal@example.com",
                "ATTENDEE;PARTSTAT=NEEDS-ACTION;CN=Me:mailto:me@example.com",
            ],
        );
        assert_eq!(
            guest_summary(&attending(&showing)),
            "4 guests · 2 yes, 1 maybe, 1 awaiting"
        );
    }

    #[test]
    fn the_news_line_says_what_the_message_did_to_the_event() {
        let now = Local::now();
        let mut showing = showing("REQUEST", &[]);
        assert_eq!(news(&showing, now), None);
        showing.change = Some(Change::Updated);
        assert_eq!(
            news(&showing, now).as_deref(),
            Some("The organizer changed this meeting")
        );
        assert_eq!(news_tone(&showing), "changed");
        showing.change = Some(Change::Moved {
            was: 1_717_924_800_000,
            all_day: false,
        });
        assert!(
            news(&showing, now).is_some_and(|line| line.starts_with("This meeting moved from "))
        );
        showing.change = Some(Change::Cancelled);
        assert_eq!(
            news(&showing, now).as_deref(),
            Some("The organizer canceled this meeting")
        );
        assert_eq!(news_tone(&showing), "cancelled");
    }

    #[test]
    fn a_cancellation_reads_as_one_without_a_change_behind_it() {
        let showing = showing("CANCEL", &[]);
        assert_eq!(
            news(&showing, Local::now()).as_deref(),
            Some("This meeting is canceled")
        );
        assert_eq!(news_tone(&showing), "cancelled");
    }

    #[test]
    fn the_times_offered_move_the_meeting_whole() {
        let starts_at = 1_717_924_800_000;
        let offered = nearby(starts_at);
        assert_eq!(offered.len(), 4);
        assert_eq!(offered[0].1, starts_at + 30 * 60 * 1_000);
        assert_eq!(offered[1].1, starts_at + 60 * 60 * 1_000);
        assert!(offered[2].1 > offered[1].1);
        assert!(offered[3].1 > offered[2].1);
        assert!(nearby(EpochMillis::MAX).is_empty());
    }
}
