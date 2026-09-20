//! The event card: an invitation as a card above the message, with the
//! answer buttons in it.
//!
//! The message body itself goes on being drawn in the WebView below, so a
//! user who declines the calendar permission still has Google's own Yes,
//! No and Maybe links there.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use chrono::Local;
use mailrs_domain::Address;
use mailrs_domain::invitation::{Answer, Invitation, Method};
use mailrs_sync::Change;

use crate::format::{event_moved_from, event_tile, event_when};

/// What the card asks the window to do.
pub enum Action {
    /// Send this answer to the organizer.
    Answer(Answer),
    /// Hand the `.ics` to the desktop, which files it in GNOME Calendar.
    AddToCalendar,
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
    /// Where the last answer went, under the buttons that sent it.
    went: gtk::Label,
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
            .build();
        let mut buttons = Vec::new();
        for answer in Answer::ALL {
            let button = gtk::ToggleButton::builder().label(answer.label()).build();
            answers.append(&button);
            buttons.push((answer, button));
        }
        let add = gtk::Button::builder()
            .label("Add to Calendar")
            .css_classes(["flat"])
            .build();
        let actions = gtk::Box::builder().spacing(8).margin_top(6).build();
        actions.append(&answers);
        let spacer = gtk::Box::builder().hexpand(true).build();
        actions.append(&spacer);
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

        let inside = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .css_classes(["card", "invitation-card"])
            .build();
        inside.append(&news);
        inside.append(&head);
        inside.append(&organizer);
        inside.append(&guests);
        inside.append(&actions);
        inside.append(&went);

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
            went,
            showing: RefCell::new(None),
            filling: Cell::new(false),
        });

        for (answer, button) in &card.buttons {
            let (answer, act, weak) = (*answer, Rc::clone(&on_action), Rc::downgrade(&card));
            button.connect_toggled(move |button| {
                let Some(card) = weak.upgrade() else { return };
                if card.filling.get() {
                    return;
                }
                if button.is_active() {
                    card.mark(Some(answer));
                    act(Action::Answer(answer));
                } else {
                    // Pressing the answer already given keeps it: taking an
                    // answer back is not something Google Calendar does.
                    card.mark(Some(answer));
                }
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

    /// Says what else the user has on while this event runs. The answer
    /// arrives after the card is already up, and the user may have moved
    /// on to another message by then, so it names the invitation it
    /// belongs to and a late answer to an old question is dropped.
    pub fn set_busy(&self, uid: &str, busy: &[String]) {
        let mine = self
            .showing
            .borrow()
            .as_ref()
            .is_some_and(|showing| showing.invitation.uid == uid);
        if mine {
            set_line(&self.clash, clash(busy));
        }
    }

    /// Says under the buttons where the answer went, or takes the line
    /// away while one is on its way.
    pub fn set_went(&self, went: Option<String>) {
        set_line(&self.went, went);
    }

    /// Puts the card back where an answer left it: on the one that went
    /// through, or on the one it showed before an answer that did not.
    pub fn set_answer(&self, answer: Option<Answer>) {
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
        self.title.set_text(if event.summary.is_empty() {
            "Untitled event"
        } else {
            &event.summary
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
        set_line(&self.news, news(showing, now));
        self.news
            .set_css_classes(&["invitation-news", news_tone(showing)]);

        // A cancellation and a reply from somebody else are news, not a
        // question, so neither gets answer buttons.
        let answerable = event.method == Method::Request && !event.cancelled();
        self.answers.set_visible(answerable);
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
                    None => "No reply yet",
                })
                .build();
            row.append(&name);
            row.append(&said);
            self.guest_list.append(&row);
        }
    }
}

/// "You have Design crit then", for an event the user already has while
/// this one runs. Two clashes name both; more than two name the first and
/// count the rest, since the point is that the hour is taken.
fn clash(busy: &[String]) -> Option<String> {
    Some(match busy {
        [] => return None,
        [one] => format!("You have {one} then"),
        [one, two] => format!("You have {one} and {two} then"),
        [one, rest @ ..] => format!("You have {one} and {} more then", rest.len()),
    })
}

/// "Invitation from Priya Raman", or nothing when the organizer is missing.
fn organizer_line(event: &Invitation) -> Option<String> {
    let who = event.organizer.as_ref()?;
    Some(match event.method {
        Method::Reply => format!("Reply to the invitation from {}", who.display()),
        _ => format!("Invitation from {}", who.display()),
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
                    "You".to_string()
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
    let mut parts = Vec::new();
    for (answer, word) in [
        (Answer::Yes, "yes"),
        (Answer::No, "no"),
        (Answer::Maybe, "maybe"),
    ] {
        let n = count(Some(answer));
        if n > 0 {
            parts.push(format!("{n} {word}"));
        }
    }
    let waiting = count(None);
    if waiting > 0 {
        parts.push(format!("{waiting} awaiting"));
    }
    let noun = if guests.len() == 1 { "guest" } else { "guests" };
    format!("{} {noun} · {}", guests.len(), parts.join(", "))
}

/// The line above the card: what this message does to an event the user
/// already has, or that the organizer called it off.
fn news(showing: &Showing, now: chrono::DateTime<Local>) -> Option<String> {
    match showing.change {
        Some(Change::Moved { was, all_day }) => Some(format!(
            "This meeting moved from {}",
            event_moved_from(was, all_day, now)
        )),
        Some(Change::Updated) => Some("The organizer changed this meeting".into()),
        Some(Change::Cancelled) => Some("The organizer canceled this meeting".into()),
        None if showing.invitation.cancelled() => Some("This meeting is canceled".into()),
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
