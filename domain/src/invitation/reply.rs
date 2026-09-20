//! Writing the iCalendar object that answers an invitation.
//!
//! RFC 5546 calls this iTIP: the answer to an invitation is a mail back to
//! the organizer carrying a `METHOD:REPLY` calendar object, and a proposal
//! for another time is a `METHOD:COUNTER`. No calendar server takes part,
//! which is why an answer written here reaches an organizer on Exchange,
//! on Outlook.com or on a mailing list that Google has never heard of.
//!
//! What an organizer's mailer matches the answer against is the UID, the
//! sequence and, for one occurrence of a repeating event, the
//! `RECURRENCE-ID`, so all three are copied over as they arrived. The
//! object names exactly one attendee, the person answering: an answer
//! carrying the rest of the guest list reads as the organizer's own word
//! on who is coming, and Outlook drops it.

use chrono::{DateTime, Duration, Utc};

use crate::{Address, EpochMillis};

use super::{Answer, Invitation, When};

/// The name a reader sees against the answer.
const PRODUCT: &str = "-//Penguin Mail//Penguin Mail//EN";

/// The longest a line may be, in octets, before it is folded. RFC 5545
/// counts the octets rather than the characters, and forbids a fold in the
/// middle of one.
const LINE_OCTETS: usize = 75;

/// Whether an answer covers the one occurrence the invitation names or
/// every occurrence of the series. It means nothing for an invitation to a
/// single event, which is [`Scope::Series`] either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// This occurrence alone. The object carries the `RECURRENCE-ID` the
    /// organizer sent.
    Occurrence,
    /// Every occurrence. The object leaves the `RECURRENCE-ID` out, which
    /// is how iTIP names the series.
    Series,
}

/// The `METHOD:REPLY` object that tells the organizer what the user said.
pub fn reply(
    invitation: &Invitation,
    me: &Address,
    answer: Answer,
    scope: Scope,
    now: EpochMillis,
) -> String {
    let mut out = object(invitation, me, "REPLY", answer.partstat(), scope, now);
    if let Some(when) = &invitation.when {
        times(&mut out, when);
    }
    // 2.0 is iTIP's "the request was handled". Exchange files a reply
    // without one, and reports one that carries it as handled rather than
    // as delivered.
    line(&mut out, "REQUEST-STATUS:2.0;Success");
    close(&mut out, invitation);
    out
}

/// The `METHOD:COUNTER` object that proposes `when` instead of the time
/// the organizer asked for.
pub fn counter(
    invitation: &Invitation,
    me: &Address,
    when: &When,
    scope: Scope,
    now: EpochMillis,
) -> String {
    // A counter proposal says nothing about whether the user is coming, so
    // the attendee stays tentative until the organizer answers it. That is
    // what Outlook sends, and what its own reader expects back.
    let mut out = object(invitation, me, "COUNTER", "TENTATIVE", scope, now);
    times(&mut out, when);
    close(&mut out, invitation);
    out
}

/// Everything both objects open with, up to the times they carry.
fn object(
    invitation: &Invitation,
    me: &Address,
    method: &str,
    partstat: &str,
    scope: Scope,
    now: EpochMillis,
) -> String {
    let mut out = String::with_capacity(512);
    line(&mut out, "BEGIN:VCALENDAR");
    line(&mut out, &format!("PRODID:{PRODUCT}"));
    line(&mut out, "VERSION:2.0");
    line(&mut out, "CALSCALE:GREGORIAN");
    line(&mut out, &format!("METHOD:{method}"));
    line(&mut out, "BEGIN:VEVENT");
    line(&mut out, &format!("UID:{}", escape(&invitation.uid)));
    line(&mut out, &format!("SEQUENCE:{}", invitation.sequence));
    if let Some(stamp) = utc(now) {
        line(&mut out, &format!("DTSTAMP:{stamp}"));
    }
    if let Some(organizer) = &invitation.organizer {
        line(&mut out, &format!("ORGANIZER{}", address(organizer)));
    }
    line(
        &mut out,
        &format!("ATTENDEE;PARTSTAT={partstat}{}", address(me)),
    );
    if let (Scope::Occurrence, Some(occurrence)) = (scope, &invitation.occurrence) {
        line(&mut out, &format!("RECURRENCE-ID{}", occurrence.written));
    }
    out
}

/// The summary and the ends of the two blocks.
fn close(out: &mut String, invitation: &Invitation) {
    if !invitation.summary.is_empty() {
        line(out, &format!("SUMMARY:{}", escape(&invitation.summary)));
    }
    line(out, "END:VEVENT");
    line(out, "END:VCALENDAR");
}

/// `DTSTART` and `DTEND` for the time the object is about. A timed event
/// answers in UTC, whichever zone the invitation named it in, since the
/// instant is the same one and no `VTIMEZONE` has to travel with it.
fn times(out: &mut String, when: &When) {
    match when {
        When::At { starts_at, ends_at } => {
            if let Some(start) = utc(*starts_at) {
                line(out, &format!("DTSTART:{start}"));
            }
            if let Some(end) = ends_at.and_then(utc) {
                line(out, &format!("DTEND:{end}"));
            }
        }
        When::Days { first, last } => {
            line(
                out,
                &format!("DTSTART;VALUE=DATE:{}", first.format("%Y%m%d")),
            );
            // iCalendar's DTEND for a date stops before the last day.
            line(
                out,
                &format!(
                    "DTEND;VALUE=DATE:{}",
                    (*last + Duration::days(1)).format("%Y%m%d")
                ),
            );
        }
    }
}

/// The `CN` parameter and `mailto:` value of an `ORGANIZER` or `ATTENDEE`,
/// from the semicolon or colon on.
fn address(who: &Address) -> String {
    let mut out = String::new();
    if let Some(name) = who.name.as_deref().filter(|name| !name.trim().is_empty()) {
        out.push_str(&format!(";CN={}", parameter(name)));
    }
    out.push_str(&format!(":mailto:{}", who.email.trim()));
    out
}

/// A parameter value, quoted when it holds a character that would end it.
/// A quote of its own cannot be written inside one at all, so it goes.
pub(super) fn parameter(value: &str) -> String {
    let value: String = value
        .trim()
        .chars()
        .filter(|c| *c != '"' && !c.is_control())
        .collect();
    match value.contains([';', ':', ',']) {
        true => format!("\"{value}\""),
        false => value,
    }
}

/// A text value with the characters iCalendar reads as punctuation put out
/// of reach.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// An instant in the form iCalendar writes UTC in.
fn utc(at: EpochMillis) -> Option<String> {
    let at: DateTime<Utc> = DateTime::from_timestamp_millis(at)?;
    Some(at.format("%Y%m%dT%H%M%SZ").to_string())
}

/// Adds one property, folded at [`LINE_OCTETS`] and ended with CRLF. A
/// continuation opens with a space, which counts against its own line.
fn line(out: &mut String, property: &str) {
    let mut octets = 0;
    for ch in property.chars() {
        let width = ch.len_utf8();
        if octets + width > LINE_OCTETS {
            out.push_str("\r\n ");
            octets = 1;
        }
        out.push(ch);
        octets += width;
    }
    out.push_str("\r\n");
}
