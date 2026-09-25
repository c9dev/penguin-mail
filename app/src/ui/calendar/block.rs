//! `EventBlock`, the tinted button that draws one occurrence on the time
//! grid: a coloured bar, its title and time, dashed while nobody has
//! answered it and struck through once the reader declined it. Named
//! `EventBlock` rather than `EventCard` because `EventCard` already
//! names the invitation card a message shows (`CONTEXT.md`).

use chrono::{DateTime, TimeZone, Utc};
use gtk::pango;
use gtk::prelude::*;
use mailrs_domain::EpochMillis;
use mailrs_domain::calendar::{Event, Occurrence};
use mailrs_domain::invitation::Answer;
use mailrs_domain::translate::{date_locale, fill, gettext};

use crate::ui;
use crate::ui::calendar::tint;

/// An occurrence under this many milliseconds puts its time on the
/// title's own line, right-aligned, rather than a line under it.
const COMPACT_MS: EpochMillis = 45 * 60_000;

/// The button GTK draws one occurrence of an event as.
pub struct EventBlock {
    pub widget: gtk::Button,
}

impl EventBlock {
    /// `calendar_colour` and `calendar_name` come from the calendar
    /// `o.event.calendar` names; `compact` puts the time beside the
    /// title rather than under it, which [`is_compact`] decides from the
    /// occurrence's own length.
    pub fn new(
        o: &Occurrence,
        calendar_colour: &str,
        calendar_name: &str,
        compact: bool,
        zone: &chrono::Local,
    ) -> EventBlock {
        let event = &o.event;
        let colour = event.color.as_deref().unwrap_or(calendar_colour);

        let button = gtk::Button::builder()
            .css_classes(["event-block", &tint::css_class(colour)])
            .build();
        match answer_state(event) {
            AnswerState::Unanswered => button.add_css_class("unanswered"),
            AnswerState::Declined => button.add_css_class("declined"),
            AnswerState::Answered => {}
        }

        let bar = gtk::Box::builder().css_classes(["bar"]).build();

        let title = gtk::Label::builder()
            .label(&event.title)
            .css_classes(["title"])
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .single_line_mode(true)
            .build();

        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        if event.all_day {
            text.append(&title);
        } else {
            let clock = time_label(o, compact, zone);
            if compact {
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

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&bar);
        content.append(&text);

        if event.pending {
            let waiting = gtk::Image::builder()
                .icon_name("content-loading-symbolic")
                .pixel_size(12)
                .build();
            waiting.set_tooltip_text(Some(&gettext("Waiting to be saved")));
            content.append(&waiting);
        }

        button.set_child(Some(&content));

        let name = accessible_name(o, calendar_name, zone);
        ui::describe(&button, &name, &description(event));
        button.set_tooltip_text(Some(&name));

        EventBlock { widget: button }
    }
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
/// `compact` puts it beside the title and the card has no room for both.
fn time_label<Z: TimeZone>(o: &Occurrence, compact: bool, zone: &Z) -> gtk::Label
where
    Z::Offset: std::fmt::Display,
{
    let text = if compact {
        clock(o.start, zone)
    } else {
        fill(
            &gettext("{start}–{end}"),
            &[
                ("start", &clock(o.start, zone)),
                ("end", &clock(o.end, zone)),
            ],
        )
    };
    gtk::Label::builder()
        .label(&text)
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
}

/// What a screen reader says for the block: the title, the time and the
/// calendar, or "all day" in place of the time for an all-day event.
fn accessible_name<Z: TimeZone>(o: &Occurrence, calendar_name: &str, zone: &Z) -> String
where
    Z::Offset: std::fmt::Display,
{
    if o.event.all_day {
        fill(
            &gettext("{title}, all day, {calendar}"),
            &[("title", &o.event.title), ("calendar", calendar_name)],
        )
    } else {
        fill(
            &gettext("{title}, {start} to {end}, {calendar}"),
            &[
                ("title", &o.event.title),
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
            accessible_name(&o, "Design team", &Utc),
            "Quarterly review, 15:00 to 16:00, Design team"
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
            accessible_name(&o, "Family", &Utc),
            "Quarterly review, all day, Family"
        );
    }
}
