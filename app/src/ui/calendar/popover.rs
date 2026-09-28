//! `EventPopover`, the small window an event block opens: what it is,
//! when it runs, where, who else is coming, and Yes/Maybe/No for a
//! guest. One popover serves the whole view: parenting a popover to a
//! block a reload later destroys would leave it dangling, so the view
//! keeps one, parented to itself, and points it at whichever block was
//! pressed with `set_pointing_to`. Edit and Delete sit in the title row
//! for an event the account may change as a whole. An invitation gets
//! Edit, for the guest's own reminders, colour and busy, and Remove,
//! which takes it off this account's calendar alone.
//!
//! The popover does not auto-hide: a second click of a double click must
//! reach the card behind it rather than be swallowed as the click that
//! dismisses the popover. `new` closes it on Escape and on a press
//! anywhere else in the window instead.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use mailrs_domain::AccountId;
use mailrs_domain::calendar::{Event, Guest, Occurrence};
use mailrs_domain::invitation::Answer;
use mailrs_domain::translate::{fill, gettext};

use super::draft;
use super::kinds;
use super::shown::{self, Refocus};
use super::tint;
use super::words;

/// Guests past this many collapse behind "Show all", so a meeting of
/// forty does not fill the popover.
const MOST_GUESTS_SHOWN: usize = 5;

/// The order the approved design answers in: Yes, Maybe, No.
/// `Answer::ALL` orders Yes, No, Maybe, for the invitation card.
const ANSWER_ORDER: [Answer; 3] = [Answer::Yes, Answer::Maybe, Answer::No];

/// Who organized the event, by name where a guest row gives one,
/// otherwise the bare organizer address the event carries.
fn organizer_name(event: &Event) -> Option<String> {
    if let Some(guest) = event.guests.iter().find(|guest| guest.organizer) {
        return Some(guest.name.clone().unwrap_or_else(|| guest.email.clone()));
    }
    event.organizer.clone()
}

/// Up to `limit` of `guests`, and how many more there are past it, for
/// the popover's own list: the first few show, and a "Show all" button
/// covers the rest.
fn guests_shown(guests: &[Guest], limit: usize) -> (&[Guest], usize) {
    if guests.len() > limit {
        (&guests[..limit], guests.len() - limit)
    } else {
        (guests, 0)
    }
}

type OnAnswer = dyn Fn(Answer, Option<String>);
type OnEdit = dyn Fn();

pub struct EventPopover {
    popover: gtk::Popover,
    parent: gtk::Widget,
    bar: gtk::Box,
    title: gtk::Label,
    when: gtk::Label,
    /// "Weekly on Wednesday", under the time; empty and hidden for an
    /// event that does not repeat.
    repeat_label: gtk::Label,
    edit_button: gtk::Button,
    delete_button: gtk::Button,
    on_edit: RefCell<Option<Box<OnEdit>>>,
    on_delete: RefCell<Option<Box<OnEdit>>>,
    calendar_label: gtk::Label,
    /// What sort of entry it is and what it declines, or that Google's
    /// own apps make it; hidden for an ordinary event.
    kind_label: gtk::Label,
    place_row: gtk::Button,
    place_label: gtk::Label,
    place_url: RefCell<String>,
    /// The notes as clickable text, under the place; hidden for an
    /// event with none. Shows [`words::notes_collapsed`] until
    /// `notes_more` is pressed, then the notes whole.
    notes_label: gtk::Label,
    /// "Show more", under the notes; visible only once
    /// [`words::notes_need_more`] says the notes overflow the cap.
    notes_more: gtk::Button,
    /// The occurrence's own notes, already turned to text, kept so
    /// `notes_more`'s handler can show them whole without `show` having
    /// run again.
    notes: RefCell<String>,
    /// Who organized the event and how many said yes; `guests_box`
    /// below it lists each one's own answer.
    people_row: gtk::Box,
    people_label: gtk::Label,
    /// One row per guest, up to [`MOST_GUESTS_SHOWN`] until
    /// `guests_more` is pressed.
    guests_box: gtk::Box,
    /// "Show all", under `guests_box`; visible only past the cap.
    guests_more: gtk::Button,
    /// The occurrence's own guests, kept so `guests_more`'s handler can
    /// rebuild the list in full without `show` having run again.
    guests: RefCell<Vec<Guest>>,
    join: gtk::Button,
    conference_url: RefCell<Option<String>>,
    answer_box: gtk::Box,
    answer_buttons: Vec<(Answer, gtk::Button)>,
    on_answer: RefCell<Option<Rc<OnAnswer>>>,
    /// "Add a note", under the answer buttons: words the organizer reads
    /// with the answer. Emptied each time the popover opens.
    note: gtk::Entry,
    /// "Propose a New Time", for a guest of a timed event with an
    /// organizer to ask.
    propose_row: gtk::Button,
    on_propose: RefCell<Option<Box<dyn Fn()>>>,
    /// "Open the invitation in Mail", shown only for an event this popover
    /// has found the mail for.
    mail_row: gtk::Button,
    on_mail: RefCell<Option<Box<dyn Fn()>>>,
    /// The account and UID `show` last opened, so a mail lookup that comes
    /// back after the popover has moved to another occurrence changes
    /// nothing.
    showing: RefCell<Option<(AccountId, String)>>,
    /// The block the popover points at, which takes the focus back when
    /// it closes.
    anchor: glib::WeakRef<gtk::Widget>,
    /// The window root and the gesture watching for a press outside the
    /// popover, added once the popover is realized and removed when it
    /// is not, since the root does not exist before then.
    root_press: RefCell<Option<(gtk::Root, gtk::GestureClick)>>,
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
        let repeat_label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["popover-when"])
            .visible(false)
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
        heading.append(&repeat_label);
        head.append(&heading);

        // Edit and Delete, right of the title, flat and icon-only, so a
        // popover that has them does not grow past the mockup's width.
        // `show` hides whichever `on_edit` or `on_delete` comes in
        // `None`, and names the second Remove for a guest.
        let edit_button = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .css_classes(["flat"])
            .valign(gtk::Align::Start)
            .visible(false)
            .tooltip_text(gettext("Edit"))
            .build();
        crate::ui::name(&edit_button, &gettext("Edit"));
        let delete_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .css_classes(["flat"])
            .valign(gtk::Align::Start)
            .visible(false)
            .tooltip_text(gettext("Delete"))
            .build();
        crate::ui::name(&delete_button, &gettext("Delete"));
        head.append(&edit_button);
        head.append(&delete_button);

        let calendar_label = gtk::Label::builder().xalign(0.0).build();
        let calendar_row = icon_row("penguin-mail-calendar-symbolic", &calendar_label);
        // Under the calendar, level with the rows' own text, as the notes
        // sit.
        let kind_label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .margin_start(24)
            .css_classes(["popover-kind"])
            .build();

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

        // The notes sit under the place, indented level with the other
        // rows' own text, with no icon of their own.
        let notes_label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .use_markup(true)
            .visible(false)
            .margin_start(24)
            .build();
        let notes_more = gtk::Button::builder()
            .label(gettext("Show more"))
            .css_classes(["flat", "popover-more"])
            .halign(gtk::Align::Start)
            .margin_start(24)
            .visible(false)
            .build();
        crate::ui::name(&notes_more, &gettext("Show more of the notes"));

        let people_label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .build();
        let people_row = icon_row("penguin-mail-people-symbolic", &people_label);
        // Each row carries its own answer icon, level with the people
        // row's own, so the list needs no extra indent of its own.
        let guests_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        let guests_more = gtk::Button::builder()
            .label(gettext("Show all"))
            .css_classes(["flat", "popover-more"])
            .halign(gtk::Align::Start)
            .margin_start(24)
            .visible(false)
            .build();
        crate::ui::name(&guests_more, &gettext("Show all guests"));

        let join = gtk::Button::builder()
            .css_classes(["popover-join"])
            .hexpand(true)
            .margin_top(5)
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

        let note = gtk::Entry::builder()
            .placeholder_text(gettext("Add a note"))
            .max_length(500)
            .css_classes(["popover-note"])
            .build();
        crate::ui::describe(&note, &gettext("Note with your answer"), &gettext("The organizer reads it with your answer"));

        let propose_label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .label(gettext("Propose a New Time"))
            .build();
        let propose_row = gtk::Button::builder()
            .child(&row_with_icon("penguin-mail-calendar-symbolic", &propose_label, false))
            .css_classes(["flat", "popover-open-mail"])
            .visible(false)
            .build();
        crate::ui::name(&propose_row, &gettext("Propose a New Time"));

        // "Open the invitation in Mail", under the answer row, only for
        // an event that arrived by mail; an icon-and-text link
        // rather than a filled pill, as the mockup draws it.
        let mail_label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .label(gettext("Open the invitation in Mail"))
            .build();
        let mail_row = gtk::Button::builder()
            .child(&row_with_icon("mail-unread-symbolic", &mail_label, false))
            .css_classes(["flat", "popover-open-mail"])
            .visible(false)
            .build();
        crate::ui::name(&mail_row, &gettext("Open the invitation in Mail"));

        // The mockup's rows sit 26 px apart, the first 31 px under the
        // time, and Join 22 px under the last.
        let rows = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(9)
            .margin_top(1)
            .build();
        rows.append(&calendar_row);
        rows.append(&kind_label);
        rows.append(&place_row);
        rows.append(&notes_label);
        rows.append(&notes_more);
        rows.append(&people_row);
        rows.append(&guests_box);
        rows.append(&guests_more);

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
            note.upcast_ref(),
            propose_row.upcast_ref(),
            mail_row.upcast_ref(),
        ] {
            content.append(widget);
        }

        let popover = gtk::Popover::builder()
            .css_classes(["event-popover"])
            .has_arrow(true)
            .position(gtk::PositionType::Right)
            .child(&content)
            // A popover that auto-hides takes the second click of a
            // double click for itself, so the card never sees it. `new`
            // closes this one on Escape and on a press elsewhere instead.
            .autohide(false)
            .build();
        popover.set_parent(parent);

        let keys = gtk::EventControllerKey::new();
        let p = popover.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                p.popdown();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        popover.add_controller(keys);

        let this = Rc::new(EventPopover {
            popover,
            parent: parent.as_ref().clone(),
            bar,
            title,
            when,
            repeat_label,
            edit_button,
            delete_button,
            on_edit: RefCell::new(None),
            on_delete: RefCell::new(None),
            calendar_label,
            kind_label,
            place_row,
            place_label,
            place_url: RefCell::new(String::new()),
            notes_label,
            notes_more,
            notes: RefCell::new(String::new()),
            people_row,
            people_label,
            guests_box,
            guests_more,
            guests: RefCell::new(Vec::new()),
            join,
            conference_url: RefCell::new(None),
            answer_box,
            answer_buttons,
            on_answer: RefCell::new(None),
            note,
            propose_row,
            on_propose: RefCell::new(None),
            mail_row,
            on_mail: RefCell::new(None),
            showing: RefCell::new(None),
            anchor: glib::WeakRef::new(),
            root_press: RefCell::new(None),
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
        // A link in the notes opens the same way the place and Join
        // rows do, not through GTK's own URI opener, which would skip
        // the `https:` check every other link in the popover keeps to.
        this.notes_label.connect_activate_link(|label, link| {
            open(link, label);
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(&this);
        this.notes_more.connect_clicked(move |button| {
            let Some(this) = weak.upgrade() else { return };
            this.notes_label.set_markup(&words::notes_markup(&this.notes.borrow()));
            button.set_visible(false);
        });
        let weak = Rc::downgrade(&this);
        this.guests_more.connect_clicked(move |_| {
            let Some(this) = weak.upgrade() else { return };
            this.rebuild_guests(usize::MAX);
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
                // Cloned out so the borrow ends before the callback runs,
                // which may open the popover again and replace it.
                let f = this.on_answer.borrow().clone();
                let note = Some(this.note.text().trim().to_string()).filter(|n| !n.is_empty());
                if let Some(f) = f {
                    f(answer, note);
                }
                this.popover.popdown();
            });
        }
        let weak = Rc::downgrade(&this);
        this.edit_button.connect_clicked(move |_| {
            let Some(this) = weak.upgrade() else { return };
            this.popover.popdown();
            let f = this.on_edit.borrow_mut().take();
            if let Some(f) = f {
                f();
            }
        });
        let weak = Rc::downgrade(&this);
        this.delete_button.connect_clicked(move |_| {
            let Some(this) = weak.upgrade() else { return };
            this.popover.popdown();
            let f = this.on_delete.borrow_mut().take();
            if let Some(f) = f {
                f();
            }
        });
        let weak = Rc::downgrade(&this);
        this.propose_row.connect_clicked(move |_| {
            let Some(this) = weak.upgrade() else { return };
            this.popover.popdown();
            let f = this.on_propose.borrow_mut().take();
            if let Some(f) = f {
                f();
            }
        });
        let weak = Rc::downgrade(&this);
        this.mail_row.connect_clicked(move |_| {
            let Some(this) = weak.upgrade() else { return };
            this.popover.popdown();
            let f = this.on_mail.borrow_mut().take();
            if let Some(f) = f {
                f();
            }
        });

        // A press anywhere else in the window closes the popover, since
        // it no longer auto-hides. Installed once, on the root the
        // popover's own realize finds, rather than per `show`.
        let outside = gtk::GestureClick::builder().propagation_phase(gtk::PropagationPhase::Capture).build();
        let weak = Rc::downgrade(&this);
        outside.connect_pressed(move |gesture, _, x, y| {
            let Some(this) = weak.upgrade() else { return };
            let Some(widget) = gesture.widget() else { return };
            let inside = widget
                .pick(x, y, gtk::PickFlags::DEFAULT)
                .is_some_and(|w| w.is_ancestor(&this.popover) || w == *this.popover.upcast_ref::<gtk::Widget>());
            if !inside {
                this.popover.popdown();
            }
        });
        let weak = Rc::downgrade(&this);
        this.popover.connect_realize(move |popover| {
            let Some(this) = weak.upgrade() else { return };
            if let Some(root) = popover.root() {
                root.add_controller(outside.clone());
                this.root_press.replace(Some((root, outside.clone())));
            }
        });
        let weak = Rc::downgrade(&this);
        this.popover.connect_unrealize(move |_| {
            let Some(this) = weak.upgrade() else { return };
            let press = this.root_press.borrow_mut().take();
            if let Some((root, outside)) = press {
                root.remove_controller(&outside);
            }
        });

        this
    }

    /// Shows the popover for `o`, pointed at `anchor` (the block or "N
    /// more" button pressed). `on_answer` runs when a guest picks Yes,
    /// Maybe or No, with the note they wrote; the caller asks which
    /// occurrences it covers and sends it through
    /// `Invitations::answer_event`. `on_propose`, when given, puts
    /// "Propose a New Time" under the answers.
    #[expect(clippy::too_many_arguments, reason = "each is a separate door the popover opens")]
    pub fn show(
        self: &Rc<Self>,
        anchor: &gtk::Widget,
        o: &Occurrence,
        calendar: &mailrs_domain::calendar::Calendar,
        on_answer: impl Fn(Answer, Option<String>) + 'static,
        on_propose: Option<Box<dyn Fn()>>,
        on_edit: Option<Box<dyn Fn()>>,
        on_delete: Option<(draft::Removal, Box<dyn Fn()>)>,
    ) {
        let event = &o.event;
        // The mail lookup this event's uid started, if any, is for the
        // occurrence the popover showed then; a late answer for it must
        // not land on whatever the popover shows now.
        self.showing
            .replace(Some((o.account_id, event.uid.clone())));
        self.mail_row.set_visible(false);
        self.on_mail.replace(None);
        let colour = event.color.as_deref().unwrap_or(calendar.color.as_str());
        self.bar
            .set_css_classes(&["popover-bar", &tint::css_class(colour)]);
        self.title.set_label(&event.title);
        let zone: chrono_tz::Tz = event.zone.parse().unwrap_or_else(|_| draft::local_zone());
        let mut when = words::when_words(o, &chrono::Local);
        // Names the event's own zone beside the desktop's when they
        // differ ("15:00–16:00, 09:00 New York"), so a meeting in
        // another zone does not read as if it ran in this one.
        if !event.all_day
            && let Some(own_zone) = words::own_zone_words(o.start, zone, draft::local_zone())
        {
            when = fill(&gettext("{when}, {zone}"), &[("when", &when), ("zone", &own_zone)]);
        }
        self.when.set_label(&when);
        let repeats = words::series_words(&event.rules, o.start, zone);
        self.repeat_label.set_visible(repeats.is_some());
        self.repeat_label.set_label(repeats.as_deref().unwrap_or_default());
        self.calendar_label.set_label(&calendar.name);
        let kind = kinds::popover_words(&event.kind);
        self.kind_label.set_visible(kind.is_some());
        self.kind_label.set_label(kind.as_deref().unwrap_or_default());

        self.edit_button.set_visible(on_edit.is_some());
        self.delete_button.set_visible(on_delete.is_some());
        let removal = on_delete.as_ref().map_or(draft::Removal::Event, |(removal, _)| *removal);
        let delete_words = match removal {
            draft::Removal::Event => gettext("Delete"),
            draft::Removal::OwnCopy => gettext("Remove from My Calendar"),
        };
        self.delete_button.set_tooltip_text(Some(&delete_words));
        crate::ui::name(&self.delete_button, &delete_words);
        self.on_edit.replace(on_edit);
        self.on_delete.replace(on_delete.map(|(_, run)| run));

        let place_visible = !event.place.is_empty();
        self.place_row.set_visible(place_visible);
        if place_visible {
            self.place_label.set_label(&event.place);
            self.place_url.replace(words::maps_url(&event.place));
            crate::ui::name(&self.place_row, &words::open_place_words(&event.place));
        }

        let notes = mailrs_mime::notes::text(&event.description);
        let notes_visible = !notes.is_empty();
        self.notes_label.set_visible(notes_visible);
        let overflows = notes_visible && words::notes_need_more(&notes);
        if notes_visible {
            self.notes_label.set_markup(&words::notes_markup(&words::notes_collapsed(&notes)));
        }
        self.notes.replace(notes);
        self.notes_more.set_visible(overflows);

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

        self.guests.replace(event.guests.clone());
        self.rebuild_guests(MOST_GUESTS_SHOWN);

        let guest = draft::limited(event);
        self.answer_box.set_visible(guest);
        self.note.set_visible(guest);
        self.note.set_text("");
        self.propose_row.set_visible(guest && on_propose.is_some());
        self.on_propose.replace(on_propose);
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

        self.on_answer.replace(Some(Rc::new(on_answer)));
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
            [
                self.place_row.clone().upcast::<gtk::Widget>(),
                self.join.clone().upcast(),
                self.edit_button.clone().upcast(),
            ]
            .into_iter()
            .find(|w| w.is_visible())
        });
        if let Some(first) = first {
            first.grab_focus();
        }
    }

    /// Rebuilds `guests_box` from `self.guests`, showing up to `limit`
    /// of them and a "Show all" button for the rest; `guests_more`'s own
    /// handler calls this again with `usize::MAX` to reveal the whole
    /// list.
    fn rebuild_guests(&self, limit: usize) {
        while let Some(child) = self.guests_box.first_child() {
            self.guests_box.remove(&child);
        }
        let guests = self.guests.borrow();
        let (shown, more) = guests_shown(&guests, limit);
        for guest in shown {
            self.guests_box.append(&guest_row(guest));
        }
        self.guests_more.set_visible(more > 0);
        if more > 0 {
            // The button reads "Show all"; a screen reader also hears how
            // many are still hidden, in the same words the tooltip used
            // to give before this list replaced it.
            crate::ui::describe(&self.guests_more, &gettext("Show all guests"), &words::more_guests_words(more));
        }
    }

    /// Closes the popover, such as when the view's range changes under
    /// it.
    pub fn hide(&self) {
        self.popover.popdown();
    }

    /// Puts "Open the invitation in Mail" on the popover, once the mail
    /// carrying `uid`'s invitation is found; `None` leaves it hidden, for
    /// an event with no mail behind it. Changes nothing once the popover
    /// has moved on to another occurrence, since the lookup that found
    /// this answers after `show` already returned.
    pub fn set_open_mail(&self, account_id: AccountId, uid: &str, open: Option<Box<dyn Fn()>>) {
        if *self.showing.borrow() != Some((account_id, uid.to_string())) {
            return;
        }
        self.mail_row.set_visible(open.is_some());
        self.on_mail.replace(open);
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
    row_with_icon(icon, label, true)
}

/// One guest's own row in the popover's list: their answer as a dim
/// icon, and their name, or their address when they gave none, with the
/// organizer marked (`words::guest_name_words`). The icon carries no
/// name of its own; the row's own accessible name and description
/// cover both.
fn guest_row(guest: &Guest) -> gtk::Box {
    let name = words::guest_name_words(guest);
    let label = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .single_line_mode(true)
        .label(&name)
        .build();
    let row = row_with_icon(words::answer_icon(guest.answer), &label, true);
    row.set_accessible_role(gtk::AccessibleRole::Group);
    crate::ui::describe(&row, &name, &words::guest_answer_words(guest));
    row
}

/// A row of an icon and `label`. The detail rows dim their icon; the
/// link into Mail keeps its icon in the link's accent colour, as the
/// mockup draws it.
fn row_with_icon(icon: &str, label: &gtk::Label, dim: bool) -> gtk::Box {
    let row = gtk::Box::builder().spacing(8).build();
    let image = gtk::Image::from_icon_name(icon);
    if dim {
        image.add_css_class("dim-label");
    }
    row.append(&image);
    label.set_hexpand(true);
    row.append(label);
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guest(email: &str) -> Guest {
        Guest { email: email.to_string(), ..Guest::default() }
    }

    #[test]
    fn guests_shown_gives_every_guest_under_the_cap() {
        let guests = vec![guest("a@example.com"), guest("b@example.com")];
        let (shown, more) = guests_shown(&guests, 5);
        assert_eq!(shown.len(), 2);
        assert_eq!(more, 0);
    }

    #[test]
    fn guests_shown_caps_the_list_and_counts_the_rest() {
        let guests: Vec<Guest> = (0..7).map(|n| guest(&format!("{n}@example.com"))).collect();
        let (shown, more) = guests_shown(&guests, 5);
        assert_eq!(shown.len(), 5);
        assert_eq!(shown[0].email, "0@example.com");
        assert_eq!(shown[4].email, "4@example.com");
        assert_eq!(more, 2);
    }

    #[test]
    fn organizer_name_prefers_the_marked_guest_over_the_bare_organizer_field() {
        let event = Event {
            organizer: Some("bare@example.com".to_string()),
            guests: vec![Guest {
                name: Some("Rita Lopes".to_string()),
                organizer: true,
                ..Guest::default()
            }],
            ..Event::default()
        };
        assert_eq!(organizer_name(&event), Some("Rita Lopes".to_string()));
    }

    #[test]
    fn organizer_name_falls_back_to_the_bare_field_with_no_guest_marked() {
        let event = Event {
            organizer: Some("bare@example.com".to_string()),
            ..Event::default()
        };
        assert_eq!(organizer_name(&event), Some("bare@example.com".to_string()));
    }
}
