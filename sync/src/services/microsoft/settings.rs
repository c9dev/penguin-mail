//! The automatic reply, from Graph's automatic replies setting. Graph
//! keeps two messages, one for the organization and one for everyone
//! else; the app edits one body and writes it to both. Graph has no
//! subject, so one given is dropped and the dialog hides the field for a
//! Microsoft account.

use mailrs_domain::Vacation;
use mailrs_graph::{AutomaticReplies, DateTimeZone};
use mailrs_mime::html::html_to_text;

use super::{GraphApi, Microsoft, Service};
use crate::BackendError;
use crate::services::AutoReplyService;

/// How far ahead an automatic reply with a start and no end runs: Graph
/// asks for both.
const OPEN_ENDED: i64 = 10 * 365 * 24 * 60 * 60 * 1000;

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

fn vacation_of(replies: &AutomaticReplies) -> Vacation {
    let scheduled = replies.status == "scheduled";
    Vacation {
        enabled: replies.status != "disabled",
        subject: String::new(),
        body: html_to_text(&replies.internal_reply_message).trim().to_string(),
        contacts_only: replies.external_audience == "contactsOnly",
        domain_only: replies.external_audience == "none",
        start: scheduled.then(|| replies.scheduled_start_date_time.as_ref().and_then(millis)).flatten(),
        end: scheduled.then(|| replies.scheduled_end_date_time.as_ref().and_then(millis)).flatten(),
    }
}

fn replies_of(vacation: &Vacation) -> AutomaticReplies {
    let scheduled = vacation.enabled && (vacation.start.is_some() || vacation.end.is_some());
    let start = vacation.start.unwrap_or_else(crate::now_millis);
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

impl<G: GraphApi> AutoReplyService for Microsoft<G> {
    async fn vacation(&self) -> Result<Vacation, BackendError> {
        let replies = self
            .graph()
            .automatic_replies()
            .await
            .map_err(|e| self.service_error(Service::AutoReply, e))?;
        Ok(vacation_of(&replies))
    }

    async fn set_vacation(&self, vacation: &Vacation) -> Result<(), BackendError> {
        self.graph()
            .set_automatic_replies(&replies_of(vacation))
            .await
            .map_err(|e| self.service_error(Service::AutoReply, e))
    }
}
