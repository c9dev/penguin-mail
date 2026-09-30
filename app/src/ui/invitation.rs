//! The event card: an invitation as a card above the message, with the
//! answer buttons in it.
//!
//! The message body itself goes on being drawn in the WebView below, so a
//! user who declines the calendar permission still has Google's own Yes,
//! No and Maybe links there.

mod outline;
pub mod strip;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use chrono::{DateTime, Days, Local, TimeDelta};
use mailrs_domain::invitation::{Answer, Card, Invitation, Method, Scope, When};
use mailrs_domain::{AccountId, Address, EpochMillis};
use mailrs_sync::{Change, Spot};

use crate::format::{event_moved_from, event_when};
use crate::ui::calendar::{tint, words};
use strip::Strip;
use crate::ui::name;
use mailrs_domain::translate::{fill, fill_plural, gettext};

/// What the card asks the window to do.
pub enum Action {
    /// Send this answer to the organizer, for the one occurrence the
    /// invitation names or for the whole series, with the note the user
    /// wrote under the buttons.
    Answer(Answer, Scope, Option<String>),
    /// Ask the organizer for another time.
    Propose(Proposal),
    /// Hand the `.ics` to the desktop, which files it in GNOME Calendar.
    /// The card asks this only where it knows no calendar of the
    /// account's to add to.
    AddToCalendar,
    /// Add these events of the file to a calendar of the account.
    Import(Vec<Invitation>, AddTo),
    /// Switch the main window to the calendar, on the event's day, with
    /// its popover open.
    ShowInCalendar,
    /// Ask for the calendar permission the account withheld, so the event
    /// can show in Calendar.
    GrantAccess,
}

/// A calendar the card offers to add a file's events to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddTo {
    pub account_id: AccountId,
    /// The provider's id for the calendar.
    pub calendar: String,
    /// What the picker says: the calendar's name, with the account's
    /// address after it when the person has more than one.
    pub label: String,
    /// The account's own calendar, which the picker starts on.
    pub primary: bool,
}

/// The calendars of one account a file's events can go on: the ones it
/// can write to, its own first. `account` is added to each label when
/// the person has several accounts.
pub fn targets_of(
    account_id: AccountId,
    calendars: Vec<mailrs_domain::calendar::Calendar>,
    account: Option<&str>,
) -> Vec<AddTo> {
    let mut found: Vec<AddTo> = calendars
        .into_iter()
        .filter(|calendar| calendar.access.can_write())
        .map(|calendar| AddTo {
            account_id,
            label: match account {
                Some(account) => fill(
                    &gettext("{calendar} ({account})"),
                    &[("calendar", &calendar.name), ("account", account)],
                ),
                None => calendar.name.clone(),
            },
            primary: calendar.primary,
            calendar: calendar.id,
        })
        .collect();
    found.sort_by_key(|target| !target.primary);
    found
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
    /// The message that carries the invitation, which the card sits in.
    pub message_id: String,
    pub invitation: Invitation,
    /// The other events of the same file, for a card that adds them.
    pub also: Vec<Invitation>,
    pub change: Option<Change>,
    /// The answer the user already sent, if any. It wins over the guest
    /// list the organizer sent, which was written before the user answered.
    pub answer: Option<Answer>,
    /// The addresses of the account the message arrived in, so the card can
    /// find the user among the guests and call them "You".
    pub me: Vec<String>,
    /// Where the event sits in the calendar's copy on this computer, once
    /// the thread run has looked. `None` until then, and for an event the
    /// copy lacks.
    pub on_calendar: Option<Spot>,
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
    /// The bar in the event's calendar colour, beside the title.
    bar: gtk::Box,
    title: gtk::Label,
    /// When the event runs and where, on one line.
    when: gtk::Label,
    /// How the series runs and who organizes it, on one line.
    meta: gtk::Label,
    /// What else the user has on while the event runs.
    clash: gtk::Label,
    /// The guest count, which opens the guest list.
    guests: gtk::MenuButton,
    guests_label: gtk::Label,
    guest_list: gtk::Box,
    /// The hours around the event, which take the clash line's place once
    /// the calendar's copy has them.
    strip: gtk::Box,
    strip_heading: gtk::Label,
    strip_verdict: gtk::Label,
    strip_grid: gtk::Grid,
    answers: gtk::Box,
    buttons: Vec<(Answer, gtk::ToggleButton)>,
    /// "Add a note", above the answer buttons: words the organizer reads
    /// with the answer. Emptied when another invitation goes up.
    note: gtk::Entry,
    add: gtk::Button,
    /// The calendars a file's events can go to, and the drop-down that
    /// picks one. Empty until the window has asked the account, and for
    /// an account with no calendar, where Add to Calendar hands the file
    /// to the desktop.
    targets: RefCell<Vec<AddTo>>,
    /// Set for a card in a window of its own that has no calendar to add
    /// to, where Add to Calendar has nothing to do and stays off.
    no_add: Cell<bool>,
    picker: gtk::DropDown,
    /// One check button per event of a file that holds several.
    events: gtk::Box,
    picks: RefCell<Vec<gtk::CheckButton>>,
    /// Show in Calendar. It takes Add to Calendar's place once the event
    /// is known to be on a calendar the app shows.
    show_in_calendar: gtk::Button,
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
    /// The line offering Grant Access, which the window puts up for an
    /// account whose consent left the calendar out.
    access: gtk::Box,
    /// What the card shows now. The window reads it back to answer the
    /// invitation, so the card is the one place that holds it.
    showing: RefCell<Option<Showing>>,
    /// The hours around the event, kept so an answer can draw them again
    /// with this meeting solid.
    strip_shown: RefCell<Option<Strip>>,
    /// Set while the card fills its buttons in, so setting one does not
    /// look like the user pressing it.
    filling: Cell<bool>,
}

/// Whether a block on the strip draws dashed: the calendar's sign for a
/// meeting the person has not answered, so only this meeting, and only
/// until an answer exists.
fn dashed(this: bool, answer: Option<Answer>) -> bool {
    this && answer.is_none()
}

/// The answers in the order the card offers them.
const ANSWERS: [Answer; 3] = [Answer::Yes, Answer::Maybe, Answer::No];

/// What goes between two parts of one line on the card.
const DOT: &str = " · ";

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

        let bar = gtk::Box::builder()
            .valign(gtk::Align::Start)
            .css_classes(["invitation-bar", "cal-accent"])
            .build();

        let title = label(&["invitation-title"]);
        let when = label(&["invitation-when"]);
        // One line, as the mockup has it: a narrow card cuts the series
        // and the organizer short rather than pushing the guest count
        // under them.
        let meta = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["invitation-meta"])
            .build();
        let clash = label(&["invitation-clash"]);

        let guest_list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(8)
            .margin_end(8)
            .build();
        // A label of its own as the child keeps the menu button from
        // drawing an arrow, which the mockup's line has none of.
        let guests_label = gtk::Label::new(None);
        let guests = gtk::MenuButton::builder()
            .child(&guests_label)
            .css_classes(["flat", "invitation-guests"])
            .popover(
                &gtk::Popover::builder()
                    .child(&guest_list)
                    .has_arrow(true)
                    .build(),
            )
            .build();
        let meta_row = gtk::Box::builder()
            .spacing(4)
            .css_classes(["invitation-meta-row"])
            .build();
        meta_row.append(&meta);
        meta_row.append(&guests);

        let strip_heading = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["strip-heading"])
            .build();
        let strip_verdict = gtk::Label::builder()
            .css_classes(["strip-verdict"])
            .build();
        let strip_top = gtk::Box::builder().spacing(8).build();
        strip_top.append(&strip_heading);
        strip_top.append(&strip_verdict);
        let strip_grid = gtk::Grid::builder()
            .column_homogeneous(true)
            .overflow(gtk::Overflow::Hidden)
            .css_classes(["strip-grid"])
            .build();
        let strip = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(5)
            .visible(false)
            .css_classes(["day-strip"])
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        strip.append(&strip_top);
        strip.append(&strip_grid);

        let answers = gtk::Box::builder()
            .spacing(8)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        name(&answers, &gettext("Answer"));
        let mut buttons = Vec::new();
        for answer in ANSWERS {
            let button = gtk::ToggleButton::builder()
                .label(answer.label())
                .css_classes(["invitation-answer"])
                .build();
            answers.append(&button);
            buttons.push((answer, button));
        }
        let note = gtk::Entry::builder()
            .placeholder_text(gettext("Add a note"))
            .max_length(500)
            .css_classes(["invitation-note"])
            .build();
        crate::ui::describe(&note, &gettext("Note with your answer"), &gettext("The organizer reads it with your answer"));
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
            .css_classes(["flat", "invitation-quiet"])
            .popover(
                &gtk::Popover::builder()
                    .child(&proposals)
                    .has_arrow(true)
                    .build(),
            )
            .build();
        let add = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("penguin-mail-calendar-symbolic")
                    .label(gettext("Add to Calendar"))
                    .build(),
            )
            .css_classes(["invitation-pill"])
            .build();
        name(&add, &gettext("Add to Calendar"));
        let picker = gtk::DropDown::builder()
            .model(&gtk::StringList::new(&[]))
            .visible(false)
            .css_classes(["invitation-picker"])
            .build();
        crate::ui::describe(
            &picker,
            &gettext("Calendar to add to"),
            &gettext("The calendar the events go on"),
        );
        let events = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .visible(false)
            .css_classes(["invitation-events"])
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        name(&events, &gettext("Events to add"));
        let show_in_calendar = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("penguin-mail-calendar-symbolic")
                    .label(gettext("Show in Calendar"))
                    .build(),
            )
            .css_classes(["invitation-pill"])
            .visible(false)
            .build();
        name(&show_in_calendar, &gettext("Show in Calendar"));
        let actions = gtk::Box::builder()
            .spacing(8)
            .css_classes(["invitation-actions"])
            .build();
        actions.append(&answers);
        actions.append(&reach);
        let spacer = gtk::Box::builder().hexpand(true).build();
        actions.append(&spacer);
        actions.append(&propose);
        actions.append(&picker);
        actions.append(&show_in_calendar);
        actions.append(&add);

        let news = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["invitation-news"])
            .build();
        // Clear of the buttons above it, which have no gap of their own
        // below them in the column.
        let went = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .margin_top(8)
            .visible(false)
            .css_classes(["invitation-meta"])
            .build();

        // Without the calendar permission the event never reaches the
        // Calendar space, so Show in Calendar has nothing to show.
        let access = gtk::Box::builder()
            .spacing(8)
            .visible(false)
            .build();
        let told = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["invitation-meta"])
            .label(gettext("Penguin Mail needs permission to show this event in Calendar."))
            .build();
        let grant = gtk::Button::builder()
            .label(gettext("Grant Access"))
            .css_classes(["flat"])
            .build();
        access.append(&told);
        access.append(&grant);

        // The note sits over the buttons that send it. Its box keeps the
        // gap above the buttons whether the note shows or not.
        let answering = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .css_classes(["invitation-answering"])
            .build();
        answering.append(&note);
        answering.append(&actions);

        // Everything but the bar sits in one column, 12 px right of it,
        // as the mockup lines the title, the strip and the buttons up.
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .css_classes(["invitation-column"])
            .build();
        for widget in [
            title.upcast_ref::<gtk::Widget>(),
            when.upcast_ref(),
            meta_row.upcast_ref(),
            events.upcast_ref(),
            clash.upcast_ref(),
            strip.upcast_ref(),
            answering.upcast_ref(),
            went.upcast_ref(),
            access.upcast_ref(),
        ] {
            column.append(widget);
        }
        let head = gtk::Box::builder().spacing(12).build();
        head.append(&bar);
        head.append(&column);

        let inside = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .css_classes(["invitation-card"])
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        // No name of its own: the page's slot the card sits in already
        // says "Invitation", and a screen reader would say it twice.
        inside.append(&news);
        inside.append(&head);

        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .visible(false)
            .css_classes(["invitation-area"])
            .build();
        widget.append(&inside);

        let card = Rc::new(EventCard {
            widget,
            news,
            bar,
            title,
            when,
            meta,
            clash,
            guests,
            guests_label,
            guest_list,
            strip,
            strip_heading,
            strip_verdict,
            strip_grid,
            answers,
            buttons,
            note,
            add,
            targets: RefCell::new(Vec::new()),
            no_add: Cell::new(false),
            picker,
            events,
            picks: RefCell::new(Vec::new()),
            show_in_calendar,
            propose,
            proposals,
            act: Rc::clone(&on_action),
            access,
            reach,
            scope: Cell::new(Scope::Occurrence),
            went,
            showing: RefCell::new(None),
            strip_shown: RefCell::new(None),
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
                    let note = Some(card.note.text().trim().to_string()).filter(|n| !n.is_empty());
                    act(Action::Answer(answer, card.scope.get(), note));
                } else {
                    // Pressing the answer already given keeps it: taking an
                    // answer back is not something Google Calendar does.
                    card.mark(Some(answer));
                }
            });
        }
        let act = Rc::clone(&on_action);
        grant.connect_clicked(move |_| act(Action::GrantAccess));

        let act = Rc::clone(&on_action);
        card.show_in_calendar
            .connect_clicked(move |_| act(Action::ShowInCalendar));
        let weak = Rc::downgrade(&card);
        card.add.connect_clicked(move |_| {
            if let Some(card) = weak.upgrade() {
                card.add_pressed();
            }
        });
        card
    }

    /// Fills the card from an invitation and shows it.
    pub fn show(&self, showing: Showing) {
        *self.strip_shown.borrow_mut() = None;
        let same = self
            .showing
            .borrow()
            .as_ref()
            .is_some_and(|shown| shown.invitation.uid == showing.invitation.uid);
        if !same {
            self.note.set_text("");
            self.targets.borrow_mut().clear();
        }
        self.draw(&showing);
        *self.showing.borrow_mut() = Some(showing);
    }

    pub fn hide(&self) {
        self.widget.set_visible(false);
        *self.showing.borrow_mut() = None;
        *self.strip_shown.borrow_mut() = None;
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

    /// Puts the hours around the event on the card in place of the clash
    /// line: the account's other events as tinted blocks, this one
    /// dashed, hairlines on the hours, and one line saying whether the
    /// hour is free. The bar beside the title takes the colour of the
    /// calendar that holds the event.
    pub fn set_strip(&self, uid: &str, strip: &Strip) {
        if !self.shows(uid) {
            return;
        }
        *self.strip_shown.borrow_mut() = Some(strip.clone());
        let answer = self.with_showing(|showing| showing.answer).flatten();
        self.draw_strip(strip, answer);
    }

    /// Draws `strip`, with this meeting dashed until `answer` holds one,
    /// as the calendar draws an event the person has not answered.
    fn draw_strip(&self, strip: &Strip, answer: Option<Answer>) {
        self.clash.set_visible(false);
        self.strip_heading.set_label(&strip.heading);
        name(&self.strip, &strip.heading);
        self.strip_verdict.set_label(&strip.verdict.words());
        self.strip_verdict
            .set_css_classes(&["strip-verdict", strip.verdict.tone()]);
        while let Some(child) = self.strip_grid.first_child() {
            self.strip_grid.remove(&child);
        }
        let rows = strip.lanes as i32;
        let marks: Vec<i32> = strip
            .hours
            .iter()
            .map(|(at, _)| strip::columns(*at, *at).0)
            .collect();
        // One cell per quarter hour holds the grid's width where no event
        // sits; a cell that starts an hour draws its hairline.
        for column in 0..strip::COLUMNS {
            let cell = gtk::Box::builder()
                .hexpand(true)
                .css_classes(["strip-cell"])
                .build();
            if marks.contains(&column) {
                cell.add_css_class("hour");
            }
            self.strip_grid.attach(&cell, column, 0, 1, rows + 1);
        }
        for ((_, hour), column) in strip.hours.iter().zip(&marks) {
            let label = gtk::Label::builder()
                .label(hour)
                .xalign(0.0)
                .valign(gtk::Align::End)
                .css_classes(["strip-hour"])
                .build();
            self.strip_grid.attach(&label, *column, rows, 4, 1);
        }
        let mut colours = Vec::new();
        for block in &strip.blocks {
            let (column, span) = strip::columns(block.from, block.to);
            colours.push(block.colour.clone());
            let is_dashed = dashed(block.this, answer);
            let outline = is_dashed.then(|| outline::colour_of(&block.colour));
            let card = outline::Outlined::new(6, outline);
            card.set_css_classes(&["event-block", "strip-block", &tint::css_class(&block.colour)]);
            if block.lane == 0 {
                card.add_css_class("first");
            }
            if is_dashed {
                card.add_css_class("unanswered");
            } else {
                card.append(&gtk::Box::builder().css_classes(["bar"]).build());
            }
            card.append(
                &gtk::Label::builder()
                    .label(&block.title)
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .single_line_mode(true)
                    .build(),
            );
            self.strip_grid
                .attach(&card, column, block.lane as i32, span, 1);
            if block.this {
                self.bar
                    .set_css_classes(&["invitation-bar", &tint::css_class(&block.colour)]);
            }
        }
        tints(&colours);
        self.strip.set_visible(true);
    }

    /// Says how the series runs, in the line a repeating invitation's rule
    /// fills. An invitation to one occurrence has no rule of its own, so
    /// the line comes from the calendar after the card is up.
    /// The line goes into the invitation the card holds as well, so a
    /// redraw after an answer keeps it.
    pub fn set_series(&self, uid: &str, line: String) {
        if !self.shows(uid) {
            return;
        }
        let updated = {
            let mut held = self.showing.borrow_mut();
            let Some(showing) = held.as_mut() else {
                return;
            };
            showing.invitation.repeats = Some(line);
            showing.clone()
        };
        self.fill_meta(&updated);
    }

    /// Offers Show in Calendar for the invitation `uid`, now that the
    /// calendar's copy is known to hold its event. The place goes into
    /// the invitation the card holds, so a redraw after an answer keeps
    /// the button.
    pub fn set_on_calendar(&self, uid: &str, spot: Spot) {
        if !self.shows(uid) {
            return;
        }
        let updated = {
            let mut held = self.showing.borrow_mut();
            let Some(showing) = held.as_mut() else {
                return;
            };
            showing.on_calendar = Some(spot);
            showing.clone()
        };
        self.place_calendar_button(&updated);
    }

    fn place_calendar_button(&self, showing: &Showing) {
        let button = calendar_button(showing);
        self.show_in_calendar.set_visible(button == CalendarButton::Show);
        self.add.set_visible(button == CalendarButton::Add && !self.no_add.get());
        // Where the events go is asked only while Add to Calendar is the
        // button on offer, for a card that adds them.
        let adding = button == CalendarButton::Add && showing.invitation.card() == Card::Add;
        self.picker
            .set_visible(adding && !self.targets.borrow().is_empty());
        // The list stays once the events are on the calendar, so the card
        // still says which ones went; only the choosing ends.
        self.events.set_visible(showing.invitation.card() == Card::Add && !showing.also.is_empty());
        self.events.set_sensitive(adding);
    }

    /// One check button per event when the file holds several, all
    /// checked: Add to Calendar adds the checked ones.
    fn fill_events(&self, showing: &Showing) {
        while let Some(child) = self.events.first_child() {
            self.events.remove(&child);
        }
        let mut picks = self.picks.borrow_mut();
        picks.clear();
        if showing.also.is_empty() {
            return;
        }
        let now = Local::now();
        for event in std::iter::once(&showing.invitation).chain(&showing.also) {
            let title = match event.summary.is_empty() {
                true => gettext("Untitled event"),
                false => event.summary.clone(),
            };
            let when = event.when.as_ref().map(|when| event_when(when, now));
            let check = gtk::CheckButton::builder()
                .label(joined(&[Some(title), when]))
                .active(true)
                .build();
            check.connect_toggled({
                let add = self.add.clone();
                let events = self.events.clone();
                move |_| add.set_sensitive(any_checked(&events))
            });
            self.events.append(&check);
            picks.push(check);
        }
        self.add.set_sensitive(true);
    }

    /// Takes Add to Calendar off the card, for a window that has no
    /// calendar to add the events to.
    pub fn cannot_add(&self) {
        self.no_add.set(true);
        self.add.set_visible(false);
        self.picker.set_visible(false);
    }

    /// Offers these calendars for the file's events, starting on the
    /// account's own. The card keeps them only while it still shows the
    /// invitation `uid` names. With none, Add to Calendar goes on handing
    /// the file to the desktop.
    pub fn set_targets(&self, uid: &str, targets: Vec<AddTo>) {
        if !self.shows(uid) {
            return;
        }
        let labels: Vec<&str> = targets.iter().map(|t| t.label.as_str()).collect();
        self.picker.set_model(Some(&gtk::StringList::new(&labels)));
        let start = targets.iter().position(|t| t.primary).unwrap_or(0);
        self.picker.set_selected(start as u32);
        *self.targets.borrow_mut() = targets;
        let showing = self.showing.borrow().clone();
        if let Some(showing) = showing {
            self.place_calendar_button(&showing);
        }
    }

    /// Says the events went on `calendar` and turns Add to Calendar into
    /// Show in Calendar, which opens the first of them. `calendar` is
    /// empty when the account never named it.
    pub fn set_added(&self, uid: &str, calendar: &str, spots: &[Spot]) {
        if !self.shows(uid) {
            return;
        }
        let Some(first) = spots.first() else {
            return;
        };
        let line = match (calendar.is_empty(), spots.len()) {
            (true, 1) => gettext("Added to your calendar"),
            (true, count) => fill_plural(
                "Added {count} events to your calendar",
                "Added {count} events to your calendar",
                count,
                &[("count", &count.to_string())],
            ),
            (false, 1) => fill(&gettext("Added to {calendar}"), &[("calendar", calendar)]),
            (false, count) => fill_plural(
                "Added {count} events to {calendar}",
                "Added {count} events to {calendar}",
                count,
                &[("count", &count.to_string()), ("calendar", calendar)],
            ),
        };
        set_line(&self.went, Some(line));
        self.set_on_calendar(uid, first.clone());
    }

    /// Add to Calendar: into the calendar the picker names when the card
    /// knows one, and to the desktop otherwise.
    fn add_pressed(&self) {
        let picked = {
            let showing = self.showing.borrow();
            let Some(showing) = showing.as_ref() else {
                return;
            };
            let picks = self.picks.borrow();
            let all = std::iter::once(&showing.invitation).chain(&showing.also);
            match picks.is_empty() {
                true => all.cloned().collect::<Vec<_>>(),
                false => all
                    .zip(picks.iter())
                    .filter(|(_, check)| check.is_active())
                    .map(|(event, _)| event.clone())
                    .collect(),
            }
        };
        let target = self.targets.borrow().get(self.picker.selected() as usize).cloned();
        match target {
            Some(target) if showing_adds(&self.showing) => (self.act)(Action::Import(picked, target)),
            _ => (self.act)(Action::AddToCalendar),
        }
    }

    /// Puts up the line offering Grant Access for the calendar.
    pub fn offer_calendar_access(&self) {
        self.access.set_visible(true);
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
        let strip = self.strip_shown.borrow().clone();
        if let Some(strip) = strip {
            self.draw_strip(&strip, answer);
        }
    }

    fn draw(&self, showing: &Showing) {
        let now = Local::now();
        let event = &showing.invitation;
        self.title.set_text(&if event.summary.is_empty() {
            gettext("Untitled event")
        } else {
            event.summary.clone()
        });
        let when = event.when.as_ref().map(|when| event_when(when, now));
        set_line(&self.when, Some(joined(&[when, event.location.clone()])));
        if !showing.also.is_empty() {
            // A file with several events names none of them on the card's
            // head: the list below does, one row each.
            self.title.set_text(&fill_plural(
                "{count} events",
                "{count} events",
                showing.also.len() + 1,
                &[("count", &(showing.also.len() + 1).to_string())],
            ));
            self.when.set_visible(false);
        }
        self.fill_events(showing);
        self.bar.set_css_classes(&["invitation-bar", "cal-accent"]);
        self.clash.set_visible(false);
        self.strip.set_visible(false);
        self.fill_meta(showing);
        self.went.set_visible(false);
        self.access.set_visible(false);
        set_line(&self.news, news(showing, now));
        self.news
            .set_css_classes(&["invitation-news", news_tone(showing)]);

        // A cancellation and a reply from somebody else are news, not a
        // question, so neither gets answer buttons.
        let answerable = event.card() == Card::Answer;
        self.answers.set_visible(answerable);
        self.note.set_visible(answerable);
        // Only an invitation to one occurrence of a series leaves the
        // question open; an answer to anything else covers the lot.
        self.reach
            .set_visible(answerable && event.occurrence.is_some());
        self.fill_proposals(showing, answerable);
        self.place_calendar_button(showing);
        self.mark(showing.answer);
        self.widget.set_visible(true);
    }

    /// The line under the time: how the series runs, who organizes it,
    /// and how many guests said yes, which opens the guest list.
    fn fill_meta(&self, showing: &Showing) {
        let mut line = joined(&[
            showing.invitation.repeats.clone(),
            organizer_line(&showing.invitation),
        ]);
        if self.fill_guests(showing) && !line.is_empty() {
            line.push_str(DOT.trim_end());
        }
        set_line(&self.meta, Some(line));
    }

    /// Puts the pressed look on one answer and takes it off the others.
    fn mark(&self, answer: Option<Answer>) {
        self.filling.set(true);
        for (button_answer, button) in &self.buttons {
            button.set_active(Some(*button_answer) == answer);
            button.remove_css_class("suggested-action");
            if words::answer_filled(*button_answer, answer) {
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

    /// Fills the guest list, and answers whether there is one to offer.
    fn fill_guests(&self, showing: &Showing) -> bool {
        while let Some(child) = self.guest_list.first_child() {
            self.guest_list.remove(&child);
        }
        let attending = attending(showing);
        if attending.is_empty() {
            self.guests.set_visible(false);
            return false;
        }
        self.guests.set_visible(true);
        let yes = said_yes(&attending);
        self.guests_label.set_label(&yes);
        name(&self.guests, &yes);
        self.guests.set_tooltip_text(Some(&guest_summary(&attending)));
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
        true
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

/// Whether the card holds a file that only describes events, which Add
/// to Calendar puts on a calendar of the account's own.
fn showing_adds(showing: &RefCell<Option<Showing>>) -> bool {
    showing
        .borrow()
        .as_ref()
        .is_some_and(|showing| showing.invitation.card() == Card::Add)
}

/// Whether any check button in the list of events is on.
fn any_checked(events: &gtk::Box) -> bool {
    let mut child = events.first_child();
    while let Some(widget) = child {
        if widget
            .downcast_ref::<gtk::CheckButton>()
            .is_some_and(|check| check.is_active())
        {
            return true;
        }
        child = widget.next_sibling();
    }
    false
}

/// Which calendar button the card offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CalendarButton {
    /// Show in Calendar: the event is on a calendar the app shows.
    Show,
    /// Add to Calendar, which hands the `.ics` to the desktop.
    Add,
}

fn calendar_button(showing: &Showing) -> CalendarButton {
    match showing.on_calendar {
        Some(_) => CalendarButton::Show,
        None => CalendarButton::Add,
    }
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

/// "Priya Raman, organizer", or nothing when the organizer is missing.
fn organizer_line(event: &Invitation) -> Option<String> {
    let who = event.organizer.as_ref()?;
    Some(match event.method {
        Method::Reply => fill(
            &gettext("Reply to the invitation from {organizer}"),
            &[("organizer", who.display())],
        ),
        // The calendar's popover words its people line the same way.
        _ => fill(&gettext("{name}, organizer"), &[("name", who.display())]),
    })
}

/// The parts of one line on the card that have words, with a dot
/// between each two.
fn joined(parts: &[Option<String>]) -> String {
    parts
        .iter()
        .flatten()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(DOT)
}

/// "4 of 6 said yes", which opens the guest list, in the words the
/// calendar's event popover uses for the same event.
fn said_yes(guests: &[Attending]) -> String {
    crate::ui::calendar::words::said_yes_words(guests.iter().map(|g| g.answer))
}

thread_local! {
    /// One stylesheet for the calendar colours of every card's strip, in
    /// every window, and the colours it holds. A provider per card would
    /// stay on the display for the rest of the run.
    static TINTS: RefCell<Option<(gtk::CssProvider, Vec<String>)>> = const { RefCell::new(None) };
}

/// Makes sure the shared stylesheet holds a rule set for each colour in
/// `colours`. It grows by the calendar colours the strips have shown,
/// which are few.
fn tints(colours: &[String]) {
    TINTS.with(|held| {
        let mut held = held.borrow_mut();
        if held.is_none() {
            let provider = gtk::CssProvider::new();
            if let Some(display) = gtk::gdk::Display::default() {
                gtk::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
            }
            *held = Some((provider, Vec::new()));
        }
        let Some((provider, known)) = held.as_mut() else {
            return;
        };
        let before = known.len();
        for colour in colours {
            let class = tint::css_class(colour);
            if !known.iter().any(|k| tint::css_class(k) == class) {
                known.push(colour.clone());
            }
        }
        if known.len() != before {
            provider.load_from_string(&tint::stylesheet(known));
        }
    });
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
            message_id: "m1".to_string(),
            invitation: mailrs_domain::invitation::read(&ics(method, extra))
                .expect("the part holds an event"),
            also: Vec::new(),
            change: None,
            answer: None,
            me: vec!["me@example.com".to_string()],
            on_calendar: None,
        }
    }

    #[test]
    fn this_meeting_is_dashed_until_answered() {
        assert!(dashed(true, None));
        for answer in Answer::ALL {
            assert!(!dashed(true, Some(answer)), "{answer:?}");
        }
    }

    #[test]
    fn other_events_are_never_dashed() {
        assert!(!dashed(false, None));
        assert!(!dashed(false, Some(Answer::Yes)));
    }

    #[test]
    fn an_event_on_the_calendar_offers_show_in_place_of_add() {
        let mut showing = showing("REQUEST", &[]);
        assert_eq!(calendar_button(&showing), CalendarButton::Add);
        showing.on_calendar = Some(Spot {
            account_id: 1,
            calendar: "primary".to_string(),
            id: "design".to_string(),
            start: 1_718_010_000_000,
        });
        assert_eq!(calendar_button(&showing), CalendarButton::Show);
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
            Some("Priya Raman, organizer")
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
    fn the_guest_line_counts_who_said_yes() {
        let showing = showing(
            "REQUEST",
            &[
                "ATTENDEE;PARTSTAT=ACCEPTED;CN=Ann Lee:mailto:ann@example.com",
                "ATTENDEE;PARTSTAT=ACCEPTED;CN=Bo Chen:mailto:bo@example.com",
                "ATTENDEE;PARTSTAT=TENTATIVE;CN=Cal Diaz:mailto:cal@example.com",
                "ATTENDEE;PARTSTAT=NEEDS-ACTION;CN=Me:mailto:me@example.com",
            ],
        );
        assert_eq!(said_yes(&attending(&showing)), "2 of 4 said yes");
    }

    #[test]
    fn the_second_line_gives_the_time_then_the_place() {
        let mut event = showing("REQUEST", &[]).invitation;
        event.location = Some("Room 2.04".to_string());
        assert_eq!(joined(&[Some("Wednesday".to_string()), event.location.clone()]), "Wednesday · Room 2.04");
        assert_eq!(joined(&[Some("Wednesday".to_string()), None]), "Wednesday");
        assert_eq!(joined(&[None, Some(" ".to_string())]), "");
    }

    #[test]
    fn the_answers_run_yes_maybe_no_and_only_the_one_given_is_filled() {
        let order: Vec<Answer> = ANSWERS.to_vec();
        assert_eq!(order, [Answer::Yes, Answer::Maybe, Answer::No]);
        for answer in ANSWERS {
            assert!(!words::answer_filled(answer, None), "{answer:?} before any answer");
        }
        assert!(words::answer_filled(Answer::Maybe, Some(Answer::Maybe)));
        assert!(!words::answer_filled(Answer::Yes, Some(Answer::Maybe)));
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
