//! `EventPopover`, the small window an event block opens: what it is,
//! when it runs, where, who else is coming, and Yes/Maybe/No for a
//! guest. One popover serves the whole view: parenting a popover to a
//! block a reload later destroys would leave it dangling, so the view
//! keeps one, parented to itself, and points it at whichever block was
//! pressed with `set_pointing_to`. It has no Edit or Delete yet, since
//! the calendar cannot change events yet.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use mailrs_domain::calendar::{Event, Guest, Occurrence};
use mailrs_domain::invitation::Answer;
use mailrs_domain::translate::gettext;

use super::shown::{self, Refocus};
use super::tint;
use super::words;

/// Guest names past this many collapse into "and N more", so a meeting
/// of forty does not fill the tooltip.
const MOST_GUESTS_SHOWN: usize = 5;

/// The order the approved design answers in: Yes, Maybe, No.
/// `Answer::ALL` orders Yes, No, Maybe, for the invitation card.
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
    place_row: gtk::Button,
    place_label: gtk::Label,
    place_url: RefCell<String>,
    /// Who organized the event and how many said yes; the guests' names
    /// are its tooltip.
    people_row: gtk::Box,
    people_label: gtk::Label,
    join: gtk::Button,
    conference_url: RefCell<Option<String>>,
    answer_box: gtk::Box,
    answer_buttons: Vec<(Answer, gtk::Button)>,
    on_answer: RefCell<Option<Box<OnAnswer>>>,
    /// The block the popover points at, which takes the focus back when
    /// it closes.
    anchor: glib::WeakRef<gtk::Widget>,
}

impl EventPopover {
    /// Parents the one popover this view will ever open to `parent`, so
    /// it survives every reload; `show` repositions it at whichever
    /// block was pressed. `fallback` takes the focus when the popover
    /// closes after its block has gone.
    pub fn new(parent: &impl IsA<gtk::Widget>, fallback: &impl IsA<gtk::Widget>) -> Rc<EventPopover> {
        let bar = gtk::Box::builder()
            .css_classes(["popover-bar"])
            .width_request(4)
            .vexpand(true)
            .build();
        let title = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["popover-title"])
            .hexpand(true)
            .build();
        let head = gtk::Box::builder().spacing(10).build();
        head.append(&bar);

        let when = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["popover-when"])
            .build();
        // The bar runs beside both the title and the time, as the
        // mockup draws it.
        let heading = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .hexpand(true)
            .build();
        heading.append(&title);
        heading.append(&when);
        head.append(&heading);

        let calendar_label = gtk::Label::builder().xalign(0.0).build();
        let calendar_row = icon_row("penguin-mail-calendar-symbolic", &calendar_label);

        let place_label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .single_line_mode(true)
            .build();
        // The whole place row opens the map, so the popover draws it as
        // the mockup does, with no link beside it.
        let place_row = gtk::Button::builder()
            .child(&icon_row("folder-symbolic", &place_label))
            .css_classes(["flat", "popover-place"])
            .tooltip_text(gettext("Open in Maps"))
            .build();

        let people_label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .build();
        let people_row = icon_row("penguin-mail-people-symbolic", &people_label);

        let join = gtk::Button::builder()
            .css_classes(["popover-join"])
            .hexpand(true)
            .build();

        let answer_box = gtk::Box::builder()
            .spacing(8)
            .homogeneous(true)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        crate::ui::name(&answer_box, &gettext("Answer"));
        let answer_buttons: Vec<(Answer, gtk::Button)> = ANSWER_ORDER
            .iter()
            .map(|&answer| {
                let button = gtk::Button::builder()
                    .label(answer.label())
                    .css_classes(["popover-answer"])
                    .build();
                answer_box.append(&button);
                (answer, button)
            })
            .collect();

        let rows = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(8)
            .build();
        rows.append(&calendar_row);
        rows.append(&place_row);
        rows.append(&people_row);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(20)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .width_request(300)
            .build();
        for widget in [
            head.upcast_ref::<gtk::Widget>(),
            rows.upcast_ref(),
            join.upcast_ref(),
            answer_box.upcast_ref(),
        ] {
            content.append(widget);
        }

        let popover = gtk::Popover::builder()
            .css_classes(["event-popover"])
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
            people_row,
            people_label,
            join,
            conference_url: RefCell::new(None),
            answer_box,
            answer_buttons,
            on_answer: RefCell::new(None),
            anchor: glib::WeakRef::new(),
        });

        // A popover gives the focus back to nothing when it closes, which
        // left a keyboard user at the top of the window without the
        // calendar's keys.
        let weak = Rc::downgrade(&this);
        let fallback = fallback.as_ref().downgrade();
        this.popover.connect_closed(move |_| {
            let Some(this) = weak.upgrade() else { return };
            let anchor = this.anchor.upgrade();
            let back = match shown::after_popover(anchor.as_ref().is_some_and(|a| a.is_mapped())) {
                Refocus::Anchor => anchor.is_some_and(|a| a.grab_focus()),
                _ => false,
            };
            if !back && let Some(fallback) = fallback.upgrade() {
                fallback.grab_focus();
            }
        });

        let weak = Rc::downgrade(&this);
        this.place_row.connect_clicked(move |button| {
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
    /// Maybe or No; it covers the whole series, since Google answers a
    /// series by its uid, and the caller sends it through
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
            crate::ui::name(&self.place_row, &words::open_place_words(&event.place));
        }

        let organizer = organizer_name(event);
        let people = words::people_words(organizer.as_deref(), &event.guests);
        self.people_row.set_visible(!people.is_empty());
        self.people_label.set_label(&people);
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
        let mut guests = names.join(", ");
        if more > 0 {
            guests.push_str(", ");
            guests.push_str(&words::more_guests_words(more));
        }
        self.people_row
            .set_tooltip_text((!guests.is_empty()).then_some(guests.as_str()));
        if !guests.is_empty() {
            crate::ui::describe(&self.people_label, &people, &guests);
        }

        let guest = is_guest(&event.guests);
        self.answer_box.set_visible(guest);
        // The current answer is filled; with none yet, Yes is, as the
        // mockup draws an invitation still waiting. A screen reader hears
        // which one is the answer, or that there is none yet.
        let filled = event.my_answer.unwrap_or(Answer::Yes);
        let answered = event.my_answer.is_some();
        let waiting = match answered {
            true => String::new(),
            false => gettext("Not answered yet"),
        };
        crate::ui::describe(&self.answer_box, &gettext("Answer"), &waiting);
        let mut first = None;
        for (answer, button) in &self.answer_buttons {
            button.remove_css_class("suggested-action");
            let current = guest && *answer == filled;
            if current {
                button.add_css_class("suggested-action");
                first = Some(button.clone().upcast::<gtk::Widget>());
            }
            let said = match current && answered {
                true => gettext("Your answer"),
                false => String::new(),
            };
            crate::ui::describe(button, &answer.label(), &said);
        }

        self.on_answer.replace(Some(Box::new(on_answer)));
        self.anchor.set(Some(anchor));

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
        // A guest opens the popover to answer, so the focus starts on the
        // current answer; anyone else starts on the first row they can
        // press.
        let first = first.or_else(|| {
            [self.place_row.clone().upcast::<gtk::Widget>(), self.join.clone().upcast()]
                .into_iter()
                .find(|w| w.is_visible())
        });
        if let Some(first) = first {
            first.grab_focus();
        }
    }

    /// Closes the popover, such as when the view's range changes under
    /// it.
    pub fn hide(&self) {
        self.popover.popdown();
    }
}

/// Opens `link` in the browser, only when it is `https:`: a calendar
/// event's link is whatever the organizer typed, and a `file:` or
/// custom scheme must not open from a click on a meeting.
fn open(link: &str, from: &impl IsA<gtk::Widget>) {
    if !link.starts_with("https://") {
        return;
    }
    let window = from.as_ref().root().and_downcast::<gtk::Window>();
    gtk::UriLauncher::new(link).launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
}

fn icon_row(icon: &str, label: &gtk::Label) -> gtk::Box {
    let row = gtk::Box::builder().spacing(8).build();
    let image = gtk::Image::from_icon_name(icon);
    image.add_css_class("dim-label");
    row.append(&image);
    label.set_hexpand(true);
    row.append(label);
    row
}
