//! The automatic reply, from Graph's automatic replies setting. Graph
//! keeps two messages, one for the organization and one for everyone
//! else; the app edits one body and writes it to both. Graph has no
//! subject, so one given is dropped and the dialog hides the field for a
//! Microsoft account.
//!
//! Graph answers 200 to a write it does not keep. An Outlook.com mailbox
//! leaves an `alwaysEnabled` reply off and keeps only `scheduled` ones,
//! and a scheduled reply sent `disabled` with no dates stays scheduled.
//! So each write is checked against what the mailbox kept, and these two
//! cases get a second write in the form the mailbox takes.

use mailrs_domain::Vacation;
use mailrs_domain::translate::gettext;
use mailrs_graph::{AutomaticReplies, DateTimeZone};
use mailrs_mime::html::html_to_text;

use super::{GraphApi, Microsoft, Service};
use crate::BackendError;
use crate::services::AutoReplyService;

/// How far ahead an automatic reply with a start and no end runs: Graph
/// asks for both.
const OPEN_ENDED: i64 = 10 * 365 * 24 * 60 * 60 * 1000;

/// A schedule at least this long was written for a reply with no end.
const FAR: i64 = 5 * 365 * 24 * 60 * 60 * 1000;

fn at(ms: i64) -> DateTimeZone {
    let when = chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default();
    DateTimeZone { date_time: when.format("%Y-%m-%dT%H:%M:%S").to_string(), time_zone: "UTC".into() }
}

fn millis(at: &DateTimeZone) -> Option<i64> {
    chrono::NaiveDateTime::parse_from_str(at.date_time.get(..19)?, "%Y-%m-%dT%H:%M:%S")
        .ok()
        .map(|t| t.and_utc().timestamp_millis())
}

/// Plain text as the HTML Graph keeps: escaped, a `<br>` per line.
fn html_of(text: &str) -> String {
    let escaped = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!("<p>{}</p>", escaped.lines().collect::<Vec<_>>().join("<br>"))
}

/// The reply as the dialog shows it. A schedule that runs years past its
/// start is one this module wrote for a reply with no end, and reads as
/// having none; once it has started it reads as having no dates at all.
fn vacation_of(replies: &AutomaticReplies, now: i64) -> Vacation {
    let enabled = replies.status != "disabled";
    let scheduled = replies.status == "scheduled";
    let mut start = scheduled.then(|| replies.scheduled_start_date_time.as_ref().and_then(millis)).flatten();
    let mut end = scheduled.then(|| replies.scheduled_end_date_time.as_ref().and_then(millis)).flatten();
    if let (Some(from), Some(to)) = (start, end)
        && to - from >= FAR
    {
        end = None;
        start = start.filter(|&from| from > now);
    }
    Vacation {
        enabled,
        subject: String::new(),
        body: html_to_text(&replies.internal_reply_message).trim().to_string(),
        contacts_only: replies.external_audience == "contactsOnly",
        // A mailbox whose reply is off still names an audience, and a new
        // Outlook.com one names `none`. The dialog does not show this
        // setting and says the reply goes outside the organization, so
        // only a reply that is on keeps it.
        domain_only: enabled && replies.external_audience == "none",
        start,
        end,
    }
}

fn replies_of(vacation: &Vacation, now: i64) -> AutomaticReplies {
    let scheduled = vacation.enabled && (vacation.start.is_some() || vacation.end.is_some());
    let start = vacation.start.unwrap_or(now);
    let end = vacation.end.unwrap_or(start + OPEN_ENDED);
    let body = html_of(&vacation.body);
    AutomaticReplies {
        status: match (vacation.enabled, scheduled) {
            (false, _) => "disabled",
            (true, true) => "scheduled",
            (true, false) => "alwaysEnabled",
        }
        .into(),
        external_audience: match (vacation.domain_only, vacation.contacts_only) {
            (true, _) => "none",
            (false, true) => "contactsOnly",
            (false, false) => "all",
        }
        .into(),
        scheduled_start_date_time: scheduled.then(|| at(start)),
        scheduled_end_date_time: scheduled.then(|| at(end)),
        internal_reply_message: body.clone(),
        external_reply_message: body,
    }
}

/// The words of a reply message, whatever HTML Graph wraps them in.
fn words(html: &str) -> String {
    html_to_text(html).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether the mailbox kept the status and, for a reply that is on, the
/// message it was sent.
fn kept_as_sent(sent: &AutomaticReplies, kept: &AutomaticReplies) -> bool {
    sent.status == kept.status
        && (sent.status == "disabled"
            || words(&sent.internal_reply_message) == words(&kept.internal_reply_message))
}

/// The write to try when the mailbox did not keep `sent`, in the form an
/// Outlook.com mailbox takes: a reply with no dates as one scheduled from
/// now for [`OPEN_ENDED`], and a reply turned off with its dates cleared
/// to the epoch (Graph refuses null there).
fn second_try(sent: &AutomaticReplies, now: i64) -> Option<AutomaticReplies> {
    let (start, end) = match sent.status.as_str() {
        "alwaysEnabled" => (now, now + OPEN_ENDED),
        "disabled" => (0, 0),
        _ => return None,
    };
    Some(AutomaticReplies {
        status: if start == 0 { "disabled" } else { "scheduled" }.into(),
        scheduled_start_date_time: Some(at(start)),
        scheduled_end_date_time: Some(at(end)),
        ..sent.clone()
    })
}

impl<G: GraphApi> Microsoft<G> {
    /// Writes `replies` and says whether the mailbox kept them.
    async fn write_replies(&self, replies: &AutomaticReplies) -> Result<bool, BackendError> {
        let kept = self
            .graph()
            .set_automatic_replies(replies)
            .await
            .map_err(|e| self.service_error(Service::AutoReply, e))?;
        let same = kept_as_sent(replies, &kept);
        if !same {
            tracing::info!(sent = %replies.status, kept = %kept.status, "the mailbox did not keep the automatic reply");
        }
        Ok(same)
    }
}

impl<G: GraphApi> AutoReplyService for Microsoft<G> {
    async fn vacation(&self) -> Result<Vacation, BackendError> {
        let replies = self
            .graph()
            .automatic_replies()
            .await
            .map_err(|e| self.service_error(Service::AutoReply, e))?;
        Ok(vacation_of(&replies, crate::now_millis()))
    }

    async fn set_vacation(&self, vacation: &Vacation) -> Result<(), BackendError> {
        let now = crate::now_millis();
        let wanted = replies_of(vacation, now);
        if self.write_replies(&wanted).await? {
            return Ok(());
        }
        if let Some(again) = second_try(&wanted, now)
            && self.write_replies(&again).await?
        {
            return Ok(());
        }
        Err(BackendError::Refused(gettext(
            "Outlook answered but did not keep the automatic reply.",
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 24 * 60 * 60 * 1000;

    #[test]
    fn a_reply_with_a_later_start_and_no_end_keeps_its_start() {
        let now = 1_790_000_000_000;
        let away = Vacation { enabled: true, start: Some(now + 3 * DAY), ..Vacation::default() };
        let read = vacation_of(&replies_of(&away, now), now);
        assert_eq!((read.start, read.end), (Some(now + 3 * DAY), None));
    }
}
