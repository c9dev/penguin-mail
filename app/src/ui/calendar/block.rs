//! `EventBlock`, the tinted button that draws one occurrence on the time
//! grid: a coloured bar, its title and time, dashed while nobody has
//! answered it and struck through once the reader declined it. Named
//! `EventBlock` rather than `EventCard` because `EventCard` already
//! names the invitation card a message shows (`CONTEXT.md`).

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};
use mailrs_domain::{AccountId, EpochMillis};
use mailrs_domain::calendar::{Event, Occurrence};
use mailrs_domain::invitation::Answer;
use mailrs_domain::translate::{date_locale, fill, gettext};

use crate::ui;
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

/// Which event a block draws: its account, calendar and id. The view
/// finds a block by it to point a popover at an event it opens by name.
pub type EventKey = (AccountId, String, String);

/// The key of the event `o` is an occurrence of.
pub fn key_of(o: &Occurrence) -> EventKey {
    (o.account_id, o.event.calendar.clone(), o.event.id.clone())
}

pub struct EventBlock {
    pub widget: gtk::Button,
    title: gtk::Label,
}

impl EventBlock {
    /// `calendar_colour` and `calendar_name` come from the calendar
    /// `o.event.calendar` names; `compact` puts the time beside the
    /// title rather than under it, which [`is_compact`] decides from the
    /// occurrence's own length. `day` is the day a view of several days
    /// shows the block under, which its spoken name then says.
    pub fn new(
        o: &Occurrence,
        calendar_colour: &str,
        calendar_name: &str,
        compact: bool,
        day: Option<NaiveDate>,
        zone: &chrono::Local,
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
        if event.all_day {
            text.append(&title);
        } else {
            let clock = time_label(o, compact, zone);
            if compact {
                button.add_css_class("compact");
                // One line centred on a block that may be shorter than
                // the line, as the mockup's 15-minute Stand-up is.
                text.set_valign(gtk::Align::Center);
                title.set_hexpand(true);
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
                row.append(&title);
                row.append(&clock);
                text.append(&row);
            } else {
                text.append(&title);
                text.append(&clock);
            }
        }

        // The bar sits 1 px in and the text 11 px in, as the mockup has
        // them.
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&bar);
        content.append(&text);

        if event.pending {
            // The clock the block draws in its top right corner; the
            // title stops short of it.
            block.imp().pending.set(true);
            text.set_margin_end(14);
        }

        button.set_child(Some(&content));

        let name = accessible_name(o, calendar_name, day, zone);
        ui::describe(&button, &name, &description(event));
        button.set_tooltip_text(Some(&name));

        EventBlock { widget: button, title }
    }

    /// Lets the title wrap onto up to `lines` lines, for a block tall
    /// enough to hold them above its time.
    pub fn set_title_lines(&self, lines: i32) {
        self.title.set_lines(lines.max(1));
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
                // The mockup's stroke sits on the block's edge, half in
                // and half out, as an SVG stroke does.
                let bounds = graphene::Rect::new(0.0, 0.0, width, height);
                let path = gsk::PathBuilder::new();
                path.add_rounded_rect(&gsk::RoundedRect::from_rect(bounds, CORNER));
                let stroke = gsk::Stroke::new(DASH_WIDTH);
                stroke.set_dash(&DASH);
                snapshot.append_stroke(&path.to_path(), &stroke, &colour);
            }
            if self.pending.get() {
                // A clock of radius 5.5 and 1.4 px lines, 14 px in from
                // the top right corner, in the dimmed text colour.
                let mut dim = widget.color();
                dim.set_alpha(dim.alpha() * 0.64);
                let (x, y) = (width - 14.0, 14.0);
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

/// Whether the occurrence's length puts its time beside the title rather
/// than under it.
pub fn is_compact(start: EpochMillis, end: EpochMillis) -> bool {
    end - start < COMPACT_MS
}

/// Whether nobody has answered `event` yet, or the reader declined it,
/// for the dashed outline and the strike-through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnswerState {
    Unanswered,
    Declined,
    Answered,
}

/// Unanswered: the account is a guest, not the organizer, and has not
/// answered. Declined: the account's own answer was No.
fn answer_state(event: &Event) -> AnswerState {
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
fn description(event: &Event) -> String {
    if event.pending {
        return gettext("Waiting to be sent");
    }
    match answer_state(event) {
        AnswerState::Unanswered => gettext("Not answered yet"),
        AnswerState::Declined => gettext("Declined"),
        AnswerState::Answered => String::new(),
    }
}

/// "10:00" in `zone`'s local time, the same pattern the rest of the app
/// clocks a moment with.
fn clock<Z: TimeZone>(at: EpochMillis, zone: &Z) -> String
where
    Z::Offset: std::fmt::Display,
{
    DateTime::<Utc>::from_timestamp_millis(at)
        .map(|utc| {
            utc.with_timezone(zone)
                .format_localized(&gettext("%H:%M"), date_locale())
                .to_string()
        })
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
    // a compact block does, rather than cutting the end time short. The
    // overlay learns its width as it lays out, which is when it chooses.
    let short = label(&start);
    let time = gtk::Overlay::builder().child(&full).build();
    time.add_overlay(&short);
    let chosen = full.clone();
    time.connect_get_child_position(move |time, short| {
        let fits = chosen.measure(gtk::Orientation::Horizontal, -1).1 <= time.width();
        chosen.set_child_visible(fits);
        short.set_child_visible(!fits);
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
    let title = match day {
        Some(day) => fill(
            &gettext("{title}, {day}"),
            &[("title", &o.event.title), ("day", &super::words::day_words(day))],
        ),
        None => o.event.title.clone(),
    };
    if o.event.all_day {
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
