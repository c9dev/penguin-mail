//! `EventBlock`, the tinted button that draws one occurrence on the time
//! grid: a coloured bar, its title and time, dashed while nobody has
//! answered it and struck through once the reader declined it. Named
//! `EventBlock` rather than `EventCard` because `EventCard` already
//! names the invitation card a message shows (`CONTEXT.md`).

use std::rc::Rc;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_domain::calendar::{Event, Occurrence};
use mailrs_domain::invitation::Answer;
use mailrs_domain::translate::{fill, gettext};

use crate::ui;
use crate::ui::calendar::drag;
use crate::ui::calendar::kinds;
use crate::ui::calendar::tint;

/// An occurrence under this many milliseconds puts its time on the
/// title's own line, right-aligned, rather than a line under it.
const COMPACT_MS: EpochMillis = 45 * 60_000;

/// The dashed outline of an invitation not answered yet, as the mockup
/// strokes it: 1.5 px, dashes of 5 and gaps of 4, on the block's own
/// 8 px corners. CSS draws `dashed` borders with short dashes and 1 px
/// gaps, so the block strokes the outline itself.
const DASH_WIDTH: f32 = 1.5;
const DASH: [f32; 2] = [5.0, 4.0];
const CORNER: f32 = 8.0;

/// The pending clock's centre sits 14 px from the content's right edge
/// with a radius of 5.5 and a 1.4 px line, so its left edge is about
/// 20 px in. The text keeps 23 px clear of that edge.
const PENDING_ROOM: i32 = 23;

/// Which event a block draws: its account, calendar and id. The view
/// finds a block by it to point a popover at an event it opens by name.
pub type EventKey = (AccountId, String, String);

/// The key of the event `o` is an occurrence of.
pub fn key_of(o: &Occurrence) -> EventKey {
    (o.account_id, o.event.calendar.clone(), o.event.id.clone())
}

pub struct EventBlock {
    pub widget: gtk::Button,
    title: TitleRow,
}

impl EventBlock {
    /// `calendar_colour` and `calendar_name` come from the calendar
    /// `o.event.calendar` names; `compact` puts the time beside the
    /// title rather than under it, which [`is_compact`] decides from the
    /// occurrence's own length. `day` is the day a view of several days
    /// shows the block under, which its spoken name then says. `on_edit`
    /// runs on a double click or Enter, which opens the editor over the
    /// popover a single click or Space still opens.
    pub fn new(
        o: &Occurrence,
        calendar_colour: &str,
        calendar_name: &str,
        compact: bool,
        day: Option<NaiveDate>,
        zone: &chrono::Local,
        on_edit: Rc<dyn Fn()>,
    ) -> EventBlock {
        let event = &o.event;
        let colour = event.color.as_deref().unwrap_or(calendar_colour);

        let block: BlockButton = glib::Object::new();
        let button = block.clone().upcast::<gtk::Button>();
        button.set_css_classes(&["event-block", &tint::css_class(colour)]);
        match answer_state(event) {
            AnswerState::Unanswered => {
                button.add_css_class("unanswered");
                block.imp().dashed.set(Some(outline_colour(colour)));
            }
            AnswerState::Declined => button.add_css_class("declined"),
            AnswerState::Answered => {}
        }
        let look = kinds::look(&event.kind);
        if let Some(class) = look.css_class() {
            button.add_css_class(class);
        }

        let bar = gtk::Box::builder().css_classes(["bar"]).build();

        let title = gtk::Label::builder()
            .label(&event.title)
            .css_classes(["title"])
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .wrap(true)
            .wrap_mode(pango::WrapMode::Word)
            .lines(1)
            .build();

        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // A whole-day out of office comes as midnight to midnight and
        // sits with the all-day entries, where "00:00" says nothing.
        let whole_days = event.all_day || super::layout::whole_days(o.start, o.end, zone).is_some();
        let row = if whole_days {
            let row = TitleRow::new(&title, None);
            text.append(&row);
            row
        } else {
            let clock = time_label(o, compact, zone);
            if compact {
                button.add_css_class("compact");
                // One line centred on a block that may be shorter than
                // the line, as the mockup's 15-minute Stand-up is.
                text.set_valign(gtk::Align::Center);
                let row = TitleRow::new(&title, Some(&clock));
                text.append(&row);
                row
            } else {
                let row = TitleRow::new(&title, None);
                text.append(&row);
                text.append(&clock);
                row
            }
        };

        // The bar sits 1 px in and the text 11 px in, as the mockup has
        // them. Focus time and a birthday put their icon between the two,
        // level with the title's first line.
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&bar);
        if let Some(icon) = look.icon() {
            let image = gtk::Image::builder()
                .icon_name(icon)
                .pixel_size(12)
                .valign(if whole_days || compact { gtk::Align::Center } else { gtk::Align::Start })
                .css_classes(["kind-icon"])
                .accessible_role(gtk::AccessibleRole::Presentation)
                .build();
            content.append(&image);
            content.set_spacing(5);
        }
        content.append(&text);

        if event.pending {
            // The clock the block draws in its top right corner; the
            // title and a compact block's time stop 3 px short of it.
            block.imp().pending.set(true);
            text.set_margin_end(PENDING_ROOM);
        }

        button.set_child(Some(&content));
        // Whatever still does not fit, in a lane narrower than the bar
        // and the padding, stops at the block's rounded edge.
        button.set_overflow(gtk::Overflow::Hidden);

        let name = accessible_name(o, calendar_name, day, zone);
        ui::describe(&button, &name, &description(event));
        button.set_tooltip_text(Some(&name));

        // Capture phase, so this sees the second press before the
        // button's own click gesture does, and claims it. The release
        // still fires the button's own `clicked` a second time, so the
        // calendar view refuses to open the popover while the editor
        // this gesture opens is on its way or on screen.
        let double = gtk::GestureClick::builder()
            .button(gdk::BUTTON_PRIMARY)
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        let on_double = Rc::clone(&on_edit);
        double.connect_pressed(move |gesture, n_press, _, _| {
            if drag::opens_editor(n_press) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                on_double();
            }
        });
        button.add_controller(double);

        // GTK activates a button on Enter from its own key binding, in
        // the bubble phase, so a capture-phase controller is what sees
        // Enter first; Space still falls through to activate the button.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(move |_, key, _, modifiers| match key {
            gdk::Key::Return | gdk::Key::KP_Enter if modifiers.is_empty() => {
                on_edit();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        button.add_controller(keys);

        EventBlock { widget: button, title: row }
    }

    /// Lets the title wrap onto up to `lines` lines, for a block tall
    /// enough to hold them above its time.
    pub fn set_title_lines(&self, lines: i32) {
        self.title.imp().lines.set(lines.max(1));
        self.title.queue_resize();
    }
}

/// The colour an unanswered block's outline takes: its own, or the
/// accent for a colour [`tint::parse_hex`] cannot read, as `.cal-accent`
/// does in the stylesheet.
fn outline_colour(colour: &str) -> gdk::RGBA {
    match tint::parse_hex(colour) {
        Some((r, g, b)) => gdk::RGBA::new(
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            1.0,
        ),
        None => adw::StyleManager::default().accent_color_rgba(),
    }
}

mod imp {
    use std::cell::Cell;

    use super::*;

    /// A button that strokes the outlines CSS cannot draw as the mockup
    /// does.
    #[derive(Default)]
    pub struct BlockButton {
        /// The colour of the dashed outline of an invitation not
        /// answered yet.
        pub dashed: Cell<Option<gdk::RGBA>>,
        /// Whether a change to the event waits to be sent, which a clock
        /// in the top right corner says.
        pub pending: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BlockButton {
        const NAME: &'static str = "MailrsEventBlock";
        type Type = super::BlockButton;
        type ParentType = gtk::Button;
    }

    impl ObjectImpl for BlockButton {}

    impl WidgetImpl for BlockButton {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.parent_snapshot(snapshot);
            let widget = self.obj();
            let (width, height) = (widget.width() as f32, widget.height() as f32);
            if let Some(colour) = self.dashed.get() {
                // The stroke runs just inside the block's edge. The
                // snapshot starts at the content box, which a compact
                // block's 7 px of padding puts inside that edge, so the
                // outline follows the widget's own bounds; drawn on the
                // content box, it ran through the time at the end of the
                // line. It keeps half its width in from the bounds, since
                // the block clips what lies outside them.
                let half = DASH_WIDTH / 2.0;
                let bounds = widget
                    .compute_bounds(&*widget)
                    .unwrap_or_else(|| graphene::Rect::new(0.0, 0.0, width, height))
                    .inset_r(half, half);
                let path = gsk::PathBuilder::new();
                path.add_rounded_rect(&gsk::RoundedRect::from_rect(bounds, CORNER - half));
                let stroke = gsk::Stroke::new(DASH_WIDTH);
                stroke.set_dash(&DASH);
                snapshot.append_stroke(&path.to_path(), &stroke, &colour);
            }
            if self.pending.get() {
                // A clock of radius 5.5 and 1.4 px lines, 14 px in from
                // the top right corner, in the dimmed text colour; centred
                // on a block of one line, which is too short for that.
                let mut dim = widget.color();
                dim.set_alpha(dim.alpha() * 0.64);
                let (x, y) = (width - 14.0, 14.0_f32.min(height / 2.0));
                let path = gsk::PathBuilder::new();
                path.add_circle(&graphene::Point::new(x, y), 5.5);
                path.move_to(x, y);
                path.line_to(x, y - 3.0);
                path.move_to(x, y);
                path.line_to(x + 2.5, y);
                let stroke = gsk::Stroke::new(1.4);
                stroke.set_line_cap(gsk::LineCap::Round);
                snapshot.append_stroke(&path.to_path(), &stroke, &dim);
            }
        }
    }

    impl ButtonImpl for BlockButton {}
}

glib::wrapper! {
    pub struct BlockButton(ObjectSubclass<imp::BlockButton>)
        @extends gtk::Button, gtk::Widget,
        @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

mod title_row {
    use std::cell::{Cell, RefCell};

    use super::*;

    /// A block's title, and for a short block its start time on the same
    /// line against the right edge. It picks what shows at the width it
    /// is given, through [`super::title_lines`] and
    /// [`super::beside_title`], and asks for no width of its own: a
    /// `gtk::Box` hands each label at least its minimum and runs past a
    /// lane narrower than that.
    pub struct TitleRow {
        pub title: RefCell<Option<gtk::Label>>,
        pub clock: RefCell<Option<gtk::Widget>>,
        /// The lines the block is tall enough for.
        pub lines: Cell<i32>,
    }

    impl Default for TitleRow {
        fn default() -> Self {
            TitleRow { title: RefCell::default(), clock: RefCell::default(), lines: Cell::new(1) }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TitleRow {
        const NAME: &'static str = "MailrsTitleRow";
        type Type = super::TitleRow;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for TitleRow {
        fn dispose(&self) {
            self.title.take();
            self.clock.take();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for TitleRow {
        /// A wrapping title is taller the narrower it is.
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let title = self.title.borrow().clone();
            let clock = self.clock.borrow().clone();
            let Some(title) = title else { return (0, 0, -1, -1) };
            let clock_nat = clock.as_ref().map_or(0, |c| c.measure(gtk::Orientation::Horizontal, -1).1);
            match orientation {
                gtk::Orientation::Horizontal => {
                    let (_, title_nat, _, _) = title.measure(orientation, for_size);
                    let beside = if clock_nat > 0 { BESIDE_GAP + clock_nat } else { 0 };
                    (0, title_nat + beside, -1, -1)
                }
                _ => {
                    // The height depends on how many lines the title
                    // keeps at this width, so that is chosen here, before
                    // the label measures; a change made while allocating
                    // would come after the height was already settled.
                    let title_nat = title.measure(gtk::Orientation::Horizontal, -1).1;
                    let room = if for_size >= 0 { self.room(for_size, clock_nat, title_nat) } else { -1 };
                    if room >= 0 {
                        self.fit_lines(&title, room);
                    }
                    let (title_min, title_nat, _, _) = title.measure(orientation, room);
                    let clock_height = clock.map_or(0, |c| c.measure(orientation, -1).1);
                    (title_min.max(clock_height), title_nat.max(clock_height), -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let title = self.title.borrow().clone();
            let clock = self.clock.borrow().clone();
            let Some(title) = title else { return };
            let clock_width = clock.as_ref().map_or(0, |c| c.measure(gtk::Orientation::Horizontal, -1).1);
            let title_width = title.measure(gtk::Orientation::Horizontal, -1).1;
            let beside = clock.is_some() && super::beside_title(width, clock_width, title_width) == TimeShown::Start;
            let room = self.room(width, clock_width, title_width);
            self.fit_lines(&title, room);
            title.allocate(room, height, baseline, None);
            if let Some(clock) = clock {
                clock.set_child_visible(beside);
                if beside {
                    let at = gsk::Transform::new().translate(&graphene::Point::new((width - clock_width) as f32, 0.0));
                    clock.allocate(clock_width, height, baseline, Some(at));
                }
            }
        }
    }

    impl TitleRow {
        /// The title's own width in a row `width` wide, beside a clock
        /// `clock` wide when the row has one and it fits a title whose
        /// natural width is `title`.
        fn room(&self, width: i32, clock: i32, title: i32) -> i32 {
            let beside = self.clock.borrow().is_some() && super::beside_title(width, clock, title) == TimeShown::Start;
            if beside { width - clock - BESIDE_GAP } else { width }
        }

        fn fit_lines(&self, title: &gtk::Label, room: i32) {
            let lines = super::title_lines(self.lines.get(), widest_word(title), room);
            if title.lines() != lines {
                title.set_lines(lines);
            }
        }
    }

    /// The width of the widest word of `title`'s text, in its own font.
    fn widest_word(title: &gtk::Label) -> i32 {
        let text = title.text();
        text.split_whitespace()
            .map(|word| title.create_pango_layout(Some(word)).pixel_size().0)
            .max()
            .unwrap_or(0)
    }
}

glib::wrapper! {
    pub struct TitleRow(ObjectSubclass<title_row::TitleRow>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TitleRow {
    fn new(title: &gtk::Label, clock: Option<&gtk::Widget>) -> TitleRow {
        let row: TitleRow = glib::Object::new();
        title.set_parent(&row);
        if let Some(clock) = clock {
            clock.set_parent(&row);
        }
        row.imp().title.replace(Some(title.clone()));
        row.imp().clock.replace(clock.cloned());
        row
    }
}

/// How much of its time a block shows, from the room its lane leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeShown {
    /// "10:00–11:30".
    Full,
    /// "10:00".
    Start,
    /// No time: the block keeps its room for the title, and the
    /// accessible name and the tooltip still carry the time.
    Nothing,
}

/// The gap between a short block's title and its time.
const BESIDE_GAP: i32 = 4;
/// The least room a short block keeps for its title before it gives the
/// time any: about two letters and the ellipsis.
const TITLE_FLOOR: i32 = 32;

/// The time line under a block's title, `width` pixels wide: the whole
/// span when `full` pixels fit, the start alone when `start` pixels do,
/// and nothing when neither does, rather than a time cut off mid-digit.
pub fn time_shown(width: i32, full: i32, start: i32) -> TimeShown {
    if full <= width {
        TimeShown::Full
    } else if start <= width {
        TimeShown::Start
    } else {
        TimeShown::Nothing
    }
}

/// A short block's time, beside its title on one line `width` pixels
/// wide, is the start (`clock` pixels) only when the title keeps
/// [`TITLE_FLOOR`] pixels of its own, or all of itself when its natural
/// width `title` is less; otherwise the title takes the line. A title
/// such as "Gym" is narrower than the floor, and the row that holds it
/// gets only its natural width, so asking for the whole floor would drop
/// its time with room to spare.
pub fn beside_title(width: i32, clock: i32, title: i32) -> TimeShown {
    match time_shown(width - TITLE_FLOOR.min(title) - BESIDE_GAP, clock, clock) {
        TimeShown::Nothing => TimeShown::Nothing,
        _ => TimeShown::Start,
    }
}

/// How many lines a title wraps onto, out of the `lines` its block is
/// tall enough for. A word `widest` pixels wide on a line `room` pixels
/// wide cannot wrap, and Pango lets it run past the edge on any line but
/// the last, which alone ends in "…". So such a title keeps one line.
pub fn title_lines(lines: i32, widest: i32, room: i32) -> i32 {
    if widest > room { 1 } else { lines.max(1) }
}

/// Whether the occurrence's length puts its time beside the title rather
/// than under it.
pub fn is_compact(start: EpochMillis, end: EpochMillis) -> bool {
    end - start < COMPACT_MS
}

/// Whether nobody has answered `event` yet, or the reader declined it,
/// for the dashed outline and the strike-through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AnswerState {
    Unanswered,
    Declined,
    Answered,
}

/// Unanswered: the account is a guest, not the organizer, and has not
/// answered. Declined: the account's own answer was No.
pub(super) fn answer_state(event: &Event) -> AnswerState {
    if event.my_answer == Some(Answer::No) {
        return AnswerState::Declined;
    }
    let unanswered = event
        .guests
        .iter()
        .any(|guest| guest.me && !guest.organizer && guest.answer.is_none());
    if unanswered {
        AnswerState::Unanswered
    } else {
        AnswerState::Answered
    }
}

/// The line a screen reader adds after the block's name: what the dashed
/// outline, the strike-through or the loading icon already say to a
/// sighted reader. A pending change takes the word over an answer state,
/// since it is the account's own event most of the time a card is both.
pub(super) fn description(event: &Event) -> String {
    if event.pending {
        return gettext("Waiting to be sent");
    }
    match answer_state(event) {
        AnswerState::Unanswered => gettext("Not answered yet"),
        AnswerState::Declined => gettext("Declined"),
        AnswerState::Answered => String::new(),
    }
}

/// "10:00" or "10:00 AM" in `zone`'s local time, in the clock
/// [`crate::clock_format::current`] names.
fn clock<Z: TimeZone>(at: EpochMillis, zone: &Z) -> String
where
    Z::Offset: std::fmt::Display,
{
    DateTime::<Utc>::from_timestamp_millis(at)
        .map(|utc| crate::clock_format::time_text(utc.with_timezone(zone).time()))
        .unwrap_or_default()
}

/// The card's own time line: "10:00–11:30", or just the start when
/// `compact` puts it beside the title or the lane has no room for both.
fn time_label<Z: TimeZone>(o: &Occurrence, compact: bool, zone: &Z) -> gtk::Widget
where
    Z::Offset: std::fmt::Display,
{
    let start = clock(o.start, zone);
    let label = |text: &str| {
        gtk::Label::builder()
            .label(text)
            .css_classes(["time"])
            .xalign(if compact { 1.0 } else { 0.0 })
            .halign(if compact {
                gtk::Align::End
            } else {
                gtk::Align::Start
            })
            .ellipsize(pango::EllipsizeMode::End)
            .single_line_mode(true)
            .build()
    };
    if compact {
        return label(&start).upcast();
    }
    let full = label(&fill(
        &gettext("{start}–{end}"),
        &[("start", &start), ("end", &clock(o.end, zone))],
    ));
    // A block in a lane too narrow for "10:00–11:30" shows "10:00", as
    // a compact block does, and one too narrow for that shows no time,
    // rather than cutting a time short. The overlay learns its width as
    // it lays out, which is when it chooses.
    let short = label(&start);
    let time = gtk::Overlay::builder().child(&full).build();
    time.add_overlay(&short);
    let chosen = full.clone();
    time.connect_get_child_position(move |time, short| {
        let natural = |label: &gtk::Widget| label.measure(gtk::Orientation::Horizontal, -1).1;
        let shown = time_shown(time.width(), natural(chosen.upcast_ref()), natural(short));
        chosen.set_child_visible(shown == TimeShown::Full);
        short.set_child_visible(shown == TimeShown::Start);
        Some(gdk::Rectangle::new(0, 0, time.width(), time.height()))
    });
    time.upcast()
}

/// What a screen reader says for the block: the title, the day when a
/// view shows several, the time and the calendar, or "all day" in place
/// of the time for an all-day event. Without the day, a week's five
/// Stand-ups would read alike.
fn accessible_name<Z: TimeZone>(
    o: &Occurrence,
    calendar_name: &str,
    day: Option<NaiveDate>,
    zone: &Z,
) -> String
where
    Z::Offset: std::fmt::Display,
{
    // Out of office, focus time and a birthday say so after the title,
    // since the stripes and the icon that show it are not spoken. A
    // title that is already the type's name says it once.
    let named = match super::kinds::kind_words(&o.event.kind) {
        Some(kind) if !kind.eq_ignore_ascii_case(o.event.title.trim()) => {
            fill(&gettext("{title}, {kind}"), &[("title", &o.event.title), ("kind", &kind)])
        }
        _ => o.event.title.clone(),
    };
    let title = match day {
        Some(day) => fill(
            &gettext("{title}, {day}"),
            &[("title", &named), ("day", &super::words::day_words(day))],
        ),
        None => named,
    };
    if o.event.all_day || super::layout::whole_days(o.start, o.end, zone).is_some() {
        fill(
            &gettext("{title}, all day, {calendar}"),
            &[("title", &title), ("calendar", calendar_name)],
        )
    } else {
        fill(
            &gettext("{title}, {start} to {end}, {calendar}"),
            &[
                ("title", &title),
                ("start", &clock(o.start, zone)),
                ("end", &clock(o.end, zone)),
                ("calendar", calendar_name),
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(all_day: bool, my_answer: Option<Answer>) -> Event {
        Event {
            title: "Quarterly review".into(),
            all_day,
            my_answer,
            ..Event::default()
        }
    }

    #[test]
    fn a_lane_wide_enough_shows_the_whole_time() {
        assert_eq!(time_shown(80, 70, 30), TimeShown::Full);
    }

    #[test]
    fn a_lane_too_narrow_for_the_end_shows_the_start() {
        assert_eq!(time_shown(50, 70, 30), TimeShown::Start);
    }

    #[test]
    fn a_lane_too_narrow_for_the_start_drops_the_time() {
        assert_eq!(time_shown(20, 70, 30), TimeShown::Nothing);
    }

    #[test]
    fn a_short_block_keeps_room_for_its_title_before_the_time() {
        // 30 px of clock, the gap and the title's floor need 30 + 4 + 32.
        assert_eq!(beside_title(66, 30, 100), TimeShown::Start);
        assert_eq!(beside_title(65, 30, 100), TimeShown::Nothing);
    }

    #[test]
    fn a_title_shorter_than_the_floor_keeps_its_time_at_its_own_width() {
        // "Gym" is 25 px: its row is 25 + 4 + 26 wide and shows 16:00.
        assert_eq!(beside_title(55, 26, 25), TimeShown::Start);
        assert_eq!(beside_title(54, 26, 25), TimeShown::Nothing);
    }

    #[test]
    fn a_title_whose_words_fit_wraps_onto_the_lines_it_has() {
        assert_eq!(title_lines(3, 40, 50), 3);
    }

    #[test]
    fn a_word_wider_than_the_lane_puts_the_title_on_one_ellipsized_line() {
        assert_eq!(title_lines(3, 60, 50), 1);
    }

    #[test]
    fn an_occurrence_under_forty_five_minutes_is_compact() {
        assert!(is_compact(0, 44 * 60_000));
        assert!(!is_compact(0, 45 * 60_000));
    }

    #[test]
    fn a_guest_with_no_answer_is_unanswered() {
        let mut event = event(false, None);
        event.guests.push(mailrs_domain::calendar::Guest {
            me: true,
            organizer: false,
            answer: None,
            ..Default::default()
        });
        assert_eq!(answer_state(&event), AnswerState::Unanswered);
        assert_eq!(description(&event), "Not answered yet");
    }

    #[test]
    fn declining_wins_over_an_empty_guest_answer() {
        let event = event(false, Some(Answer::No));
        assert_eq!(answer_state(&event), AnswerState::Declined);
        assert_eq!(description(&event), "Declined");
    }

    #[test]
    fn an_event_the_account_organizes_needs_no_answer() {
        let mut event = event(false, None);
        event.guests.push(mailrs_domain::calendar::Guest {
            me: true,
            organizer: true,
            answer: None,
            ..Default::default()
        });
        assert_eq!(answer_state(&event), AnswerState::Answered);
        assert_eq!(description(&event), "");
    }

    #[test]
    fn a_pending_change_is_named_over_an_answer_state() {
        let mut event = event(false, None);
        event.pending = true;
        event.guests.push(mailrs_domain::calendar::Guest {
            me: true,
            organizer: false,
            answer: None,
            ..Default::default()
        });
        assert_eq!(description(&event), "Waiting to be sent");
    }

    #[test]
    fn the_accessible_name_reads_the_time_and_the_calendar() {
        mailrs_domain::translate::set_date_locale("en_US");
        let event = std::sync::Arc::new(event(false, None));
        let o = Occurrence {
            account_id: 1,
            event,
            start: 15 * 3_600_000,
            end: 16 * 3_600_000,
        };
        assert_eq!(
            accessible_name(&o, "Design team", None, &Utc),
            "Quarterly review, 15:00 to 16:00, Design team"
        );
    }

    #[test]
    fn a_block_in_a_week_names_its_day() {
        mailrs_domain::translate::set_date_locale("en_US");
        let event = std::sync::Arc::new(event(false, None));
        let o = Occurrence {
            account_id: 1,
            event,
            start: 15 * 3_600_000,
            end: 16 * 3_600_000,
        };
        let monday = chrono::NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();
        assert_eq!(
            accessible_name(&o, "Design team", Some(monday), &Utc),
            "Quarterly review, Monday 21, 15:00 to 16:00, Design team"
        );
    }

    #[test]
    fn an_all_day_block_in_a_week_names_its_day() {
        mailrs_domain::translate::set_date_locale("en_US");
        let event = std::sync::Arc::new(event(true, None));
        let o = Occurrence {
            account_id: 1,
            event,
            start: 0,
            end: 24 * 3_600_000,
        };
        let monday = chrono::NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();
        assert_eq!(
            accessible_name(&o, "Family", Some(monday), &Utc),
            "Quarterly review, Monday 21, all day, Family"
        );
    }

    #[test]
    fn an_out_of_office_block_says_what_it_is() {
        use mailrs_domain::calendar::{Decline, Kind};
        mailrs_domain::translate::set_date_locale("en_US");
        let event = Event { kind: Kind::OutOfOffice(Decline::default()), ..event(false, None) };
        let o = Occurrence { account_id: 1, event: std::sync::Arc::new(event), start: 9 * 3_600_000, end: 17 * 3_600_000 };
        assert_eq!(
            accessible_name(&o, "Work", None, &Utc),
            "Quarterly review, Out of office, 09:00 to 17:00, Work"
        );
    }

    #[test]
    fn a_block_titled_with_its_type_says_it_once() {
        use mailrs_domain::calendar::{Decline, Kind};
        mailrs_domain::translate::set_date_locale("en_US");
        let event = Event { kind: Kind::Focus(Decline::default()), title: "Focus time".into(), ..event(false, None) };
        let o = Occurrence { account_id: 1, event: std::sync::Arc::new(event), start: 9 * 3_600_000, end: 11 * 3_600_000 };
        assert_eq!(accessible_name(&o, "Work", None, &Utc), "Focus time, 09:00 to 11:00, Work");
    }

    #[test]
    fn a_birthday_chip_says_it_is_a_birthday() {
        use mailrs_domain::calendar::Kind;
        mailrs_domain::translate::set_date_locale("en_US");
        let event = Event { kind: Kind::Birthday, title: "Ana".into(), ..event(true, None) };
        let o = Occurrence { account_id: 1, event: std::sync::Arc::new(event), start: 0, end: 24 * 3_600_000 };
        assert_eq!(accessible_name(&o, "Birthdays", None, &Utc), "Ana, Birthday, all day, Birthdays");
    }

    #[test]
    fn an_out_of_office_from_midnight_to_midnight_reads_all_day() {
        use mailrs_domain::calendar::{Decline, Kind};
        let event = Event { kind: Kind::OutOfOffice(Decline::default()), title: "Out of office".into(), ..event(false, None) };
        let o = Occurrence { account_id: 1, event: std::sync::Arc::new(event), start: 0, end: 24 * 3_600_000 };
        assert_eq!(accessible_name(&o, "Work", None, &Utc), "Out of office, all day, Work");
    }

    #[test]
    fn an_all_day_event_reads_all_day() {
        mailrs_domain::translate::set_date_locale("en_US");
        let event = std::sync::Arc::new(event(true, None));
        let o = Occurrence {
            account_id: 1,
            event,
            start: 0,
            end: 24 * 3_600_000,
        };
        assert_eq!(
            accessible_name(&o, "Family", None, &Utc),
            "Quarterly review, all day, Family"
        );
    }
}
