//! `EventPopover`, the small window an event block opens: what it is,
//! when it runs, where, who else is coming, and Yes/Maybe/No for a
//! guest. One popover serves the whole view (reconcile.md Task 5 item
//! 4): parenting a popover to a block a reload later destroys would
//! leave it dangling, so the view keeps one, parented to itself, and
//! points it at whichever block was pressed with `set_pointing_to`.
//! Edit and Delete wait for stage 3 (reconcile.md item 3).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, pango};
use mailrs_domain::calendar::{Event, Guest, Occurrence};
use mailrs_domain::invitation::Answer;
use mailrs_domain::translate::gettext;

use super::tint;
use super::words;

/// Guest names past this many collapse into "and N more" (the brief's
/// "collapsed after 5").
const MOST_GUESTS_SHOWN: usize = 5;

/// The order the mockup answers in: Yes, Maybe, No (reconcile.md Task 5
/// item 7; `Answer::ALL` orders Yes, No, Maybe, for the invitation card).
const ANSWER_ORDER: [Answer; 3] = [Answer::Yes, Answer::Maybe, Answer::No];

/// Whether the account is a guest worth asking: it holds a guest row of
/// its own and did not organize the event, mirroring `block::answer_state`'s
/// "unanswered" guard.
fn is_guest(guests: &[Guest]) -> bool {
    guests.iter().any(|guest| guest.me && !guest.organizer)
}

/// Who organized the event, by name where a guest row gives one,
/// otherwise the bare organizer address the event carries.
fn organizer_name(event: &Event) -> Option<String> {
    if let Some(guest) = event.guests.iter().find(|guest| guest.organizer) {
        return Some(guest.name.clone().unwrap_or_else(|| guest.email.clone()));
    }
    event.organizer.clone()
}

/// Up to `limit` guest names, and how many more there are past it.
fn guest_names(guests: &[Guest], limit: usize) -> (Vec<String>, usize) {
    let names: Vec<String> = guests
        .iter()
        .map(|guest| guest.name.clone().unwrap_or_else(|| guest.email.clone()))
        .collect();
    if names.len() > limit {
        (names[..limit].to_vec(), names.len() - limit)
    } else {
        (names, 0)
    }
}

type OnAnswer = dyn Fn(Answer);

pub struct EventPopover {
    popover: gtk::Popover,
    parent: gtk::Widget,
    bar: gtk::Box,
    title: gtk::Label,
    when: gtk::Label,
    calendar_label: gtk::Label,
    place_row: gtk::Box,
    place_label: gtk::Label,
    place_url: RefCell<String>,
    organizer_row: gtk::Box,
    organizer_label: gtk::Label,
    answers_label: gtk::Label,
    join: gtk::Button,
    conference_url: RefCell<Option<String>>,
    guests_row: gtk::Box,
    guests_label: gtk::Label,
    answer_box: gtk::Box,
    answer_buttons: Vec<(Answer, gtk::Button)>,
    on_answer: RefCell<Option<Box<OnAnswer>>>,
}

impl EventPopover {
    /// Parents the one popover this view will ever open to `parent`, so
    /// it survives every reload; `show` repositions it at whichever
    /// block was pressed.
    pub fn new(parent: &impl IsA<gtk::Widget>) -> Rc<EventPopover> {
        let bar = gtk::Box::builder()
            .css_classes(["popover-bar"])
            .width_request(3)
            .vexpand(true)
            .build();
        let title = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["popover-title"])
            .hexpand(true)
            .build();
        let head = gtk::Box::builder().spacing(8).build();
        head.append(&bar);
        head.append(&title);

        let when = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();

        let calendar_label = gtk::Label::builder().xalign(0.0).build();
        let calendar_row = icon_row("x-office-calendar-symbolic", &calendar_label);

        let place_label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .single_line_mode(true)
            .build();
        let maps_link = gtk::Button::builder()
            .css_classes(["flat", "link"])
            .label(gettext("Open in Maps"))
            .valign(gtk::Align::Center)
            .build();
        let place_row = gtk::Box::builder().spacing(8).build();
        place_row.append(&gtk::Image::from_icon_name("mark-location-symbolic"));
        place_row.append(&place_label);
        place_row.append(&maps_link);

        let organizer_label = gtk::Label::builder().xalign(0.0).build();
        let answers_label = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["dim-label", "caption"])
            .build();
        let organizer_text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .build();
        organizer_text.append(&organizer_label);
        organizer_text.append(&answers_label);
        let organizer_row = gtk::Box::builder().spacing(8).build();
        organizer_row.append(&gtk::Image::from_icon_name("avatar-default-symbolic"));
        organizer_row.append(&organizer_text);

        let join = gtk::Button::builder()
            .css_classes(["pill"])
            .hexpand(true)
            .build();

        let guests_label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label", "caption"])
            .build();
        let guests_row = gtk::Box::builder().spacing(8).build();
        guests_row.append(&gtk::Image::from_icon_name("system-users-symbolic"));
        guests_row.append(&guests_label);

        let answer_box = gtk::Box::builder()
            .spacing(0)
            .css_classes(["linked"])
            .homogeneous(true)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        crate::ui::name(&answer_box, &gettext("Answer"));
        let answer_buttons: Vec<(Answer, gtk::Button)> = ANSWER_ORDER
            .iter()
            .map(|&answer| {
                let button = gtk::Button::builder().label(answer.label()).build();
                answer_box.append(&button);
                (answer, button)
            })
            .collect();

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_top(14)
            .margin_bottom(14)
            .margin_start(14)
            .margin_end(14)
            .width_request(280)
            .build();
        for widget in [
            head.upcast_ref::<gtk::Widget>(),
            when.upcast_ref(),
            calendar_row.upcast_ref(),
            place_row.upcast_ref(),
            organizer_row.upcast_ref(),
            join.upcast_ref(),
            guests_row.upcast_ref(),
            answer_box.upcast_ref(),
        ] {
            content.append(widget);
        }

        let popover = gtk::Popover::builder()
            .has_arrow(true)
            .position(gtk::PositionType::Right)
            .child(&content)
            .build();
        popover.set_parent(parent);

        let this = Rc::new(EventPopover {
            popover,
            parent: parent.as_ref().clone(),
            bar,
            title,
            when,
            calendar_label,
            place_row,
            place_label,
            place_url: RefCell::new(String::new()),
            organizer_row,
            organizer_label,
            answers_label,
            join,
            conference_url: RefCell::new(None),
            guests_row,
            guests_label,
            answer_box,
            answer_buttons,
            on_answer: RefCell::new(None),
        });

        let weak = Rc::downgrade(&this);
        maps_link.connect_clicked(move |button| {
            let Some(this) = weak.upgrade() else { return };
            open(&this.place_url.borrow(), button);
        });
        let weak = Rc::downgrade(&this);
        this.join.connect_clicked(move |button| {
            let Some(this) = weak.upgrade() else { return };
            if let Some(link) = this.conference_url.borrow().as_deref() {
                open(link, button);
            }
        });
        for (answer, button) in &this.answer_buttons {
            let weak = Rc::downgrade(&this);
            let answer = *answer;
            button.connect_clicked(move |_| {
                let Some(this) = weak.upgrade() else { return };
                if let Some(f) = this.on_answer.borrow().as_ref() {
                    f(answer);
                }
                this.popover.popdown();
            });
        }

        this
    }

    /// Shows the popover for `o`, pointed at `anchor` (the block or "N
    /// more" button pressed). `on_answer` runs when a guest picks Yes,
    /// Maybe or No; it covers the whole series (ruling R3), which the
    /// caller's `on_answer` (Task 6) sends through
    /// `Invitations::answer_event`.
    pub fn show(
        self: &Rc<Self>,
        anchor: &gtk::Widget,
        o: &Occurrence,
        calendar: &mailrs_domain::calendar::Calendar,
        on_answer: impl Fn(Answer) + 'static,
    ) {
        let event = &o.event;
        let colour = event.color.as_deref().unwrap_or(calendar.color.as_str());
        self.bar
            .set_css_classes(&["popover-bar", &tint::css_class(colour)]);
        self.title.set_label(&event.title);
        self.when.set_label(&words::when_words(o, &chrono::Local));
        self.calendar_label.set_label(&calendar.name);

        let place_visible = !event.place.is_empty();
        self.place_row.set_visible(place_visible);
        if place_visible {
            self.place_label.set_label(&event.place);
            self.place_url.replace(words::maps_url(&event.place));
        }

        let organizer = organizer_name(event);
        self.organizer_row
            .set_visible(organizer.is_some() || !event.guests.is_empty());
        match &organizer {
            Some(name) => {
                self.organizer_label.set_visible(true);
                self.organizer_label
                    .set_label(&words::organizer_words(name));
            }
            None => self.organizer_label.set_visible(false),
        }
        let answers_visible = !event.guests.is_empty();
        self.answers_label.set_visible(answers_visible);
        if answers_visible {
            self.answers_label
                .set_label(&words::answers_words(&event.guests));
        }

        let conference = event
            .conference
            .as_deref()
            .filter(|link| link.starts_with("https://"));
        self.join.set_visible(conference.is_some());
        if let Some(link) = conference {
            self.join.set_label(&words::join_words(link));
            crate::ui::name(&self.join, &words::join_words(link));
        }
        self.conference_url.replace(conference.map(str::to_string));

        let (names, more) = guest_names(&event.guests, MOST_GUESTS_SHOWN);
        self.guests_row.set_visible(!names.is_empty());
        if !names.is_empty() {
            let mut text = names.join(", ");
            if more > 0 {
                text.push_str(", ");
                text.push_str(&words::more_guests_words(more));
            }
            self.guests_label.set_label(&text);
        }

        let guest = is_guest(&event.guests);
        self.answer_box.set_visible(guest);
        for (answer, button) in &self.answer_buttons {
            button.remove_css_class("suggested-action");
            if guest && Some(*answer) == event.my_answer {
                button.add_css_class("suggested-action");
            }
        }

        self.on_answer.replace(Some(Box::new(on_answer)));

        if let Some(bounds) = anchor.compute_bounds(&self.parent) {
            let rect = gdk::Rectangle::new(
                bounds.x().round() as i32,
                bounds.y().round() as i32,
                bounds.width().round().max(1.0) as i32,
                bounds.height().round().max(1.0) as i32,
            );
            self.popover.set_pointing_to(Some(&rect));
        }
        self.popover.popup();
    }

    /// Closes the popover, such as when the view's range changes under
    /// it.
    pub fn hide(&self) {
        self.popover.popdown();
    }
}

/// Opens `link` in the browser, only when it is `https:`: a calendar
/// event's link is whatever the organizer typed (reconcile.md Task 5
/// item 5).
fn open(link: &str, from: &impl IsA<gtk::Widget>) {
    if !link.starts_with("https://") {
        return;
    }
    let window = from.as_ref().root().and_downcast::<gtk::Window>();
    gtk::UriLauncher::new(link).launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
}

fn icon_row(icon: &str, label: &gtk::Label) -> gtk::Box {
    let row = gtk::Box::builder().spacing(8).build();
    row.append(&gtk::Image::from_icon_name(icon));
    label.set_hexpand(true);
    row.append(label);
    row
}
