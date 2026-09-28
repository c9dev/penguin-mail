//! Changes to the account's calendar list: Google's `calendars` calls for
//! the calendars the account owns, and its `calendarList` calls for the
//! account's own entry for any calendar on its list.
//!
//! Making, renaming and deleting a calendar needs
//! [`CALENDARS_SCOPE`](crate::CALENDARS_SCOPE); colouring, hiding and
//! subscribing need [`CALENDAR_LIST_WRITE_SCOPE`](crate::CALENDAR_LIST_WRITE_SCOPE).
//! Google turns a call down with a missing scope as it does any other,
//! and the refusal arrives as [`GmailError::MissingScope`].

use mailrs_domain::calendar;
use mailrs_domain::calendar::list::ListEdit;
use serde_json::{Map, Value, json};

use crate::GmailError;
use crate::calendar::{encode, google_calendar};
use crate::client::GmailClient;

impl GmailClient {
    /// Makes `edit` true of the calendar `id` on Google. An edit that puts
    /// a calendar on the list (a new one, a subscription, a public one by
    /// its id) answers the list entry as Google now holds it, whose id is
    /// Google's own; the others answer `None`, or the entry when Google
    /// sent one back.
    pub async fn edit_calendar_list(
        &self,
        id: &str,
        edit: &ListEdit,
    ) -> Result<Option<calendar::Calendar>, GmailError> {
        match edit {
            ListEdit::Create { name, color, zone } => {
                let made = self.insert_calendar(name, zone).await?;
                // Google puts a calendar it made on its owner's list with a
                // colour of its choosing; the colour the person picked goes
                // on that entry.
                self.patch_list_entry(&made, colored(color)).await.map(Some)
            }
            ListEdit::Rename { name } => {
                let url = format!("{}/calendars/{}", self.calendar_base_url, encode(id));
                let body = json!({ "summary": name });
                let _: Value = self.call_at(&url, |url| self.http().patch(url).json(&body)).await?;
                Ok(None)
            }
            ListEdit::Delete => {
                let url = format!("{}/calendars/{}", self.calendar_base_url, encode(id));
                self.call_at_empty(&url, |url| self.http().delete(url)).await?;
                Ok(None)
            }
            ListEdit::Recolor { color } => self.patch_list_entry(id, colored(color)).await.map(Some),
            ListEdit::Hide { hidden } => {
                // Google's `hidden` is what its own list's "Hide from list"
                // sets; `selected` is the tick beside a calendar. A calendar
                // shown again comes back ticked, as the copy puts it back.
                let mut body = Map::new();
                body.insert("hidden".into(), json!(hidden));
                if !hidden {
                    body.insert("selected".into(), json!(true));
                }
                self.patch_list_entry(id, Value::Object(body)).await.map(Some)
            }
            ListEdit::Subscribe { url } => self.insert_list_entry(url).await.map(Some),
            ListEdit::Add => self.insert_list_entry(id).await.map(Some),
        }
    }

    /// Google's `calendars.insert`: a new calendar the account owns.
    /// Answers its id, which Google picks.
    async fn insert_calendar(&self, name: &str, zone: &str) -> Result<String, GmailError> {
        let url = format!("{}/calendars", self.calendar_base_url);
        let mut body = Map::new();
        body.insert("summary".into(), json!(name));
        if !zone.is_empty() {
            body.insert("timeZone".into(), json!(zone));
        }
        let body = Value::Object(body);
        let made: Value = self.call_at(&url, |url| self.http().post(url).json(&body)).await?;
        made.get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| GmailError::Decode("a new calendar came back without an id".into()))
    }

    /// Google's `calendarList.insert`: puts a calendar on the account's
    /// list by its id, which for a subscription is the address of the
    /// feed. Google answers the entry under its own id.
    async fn insert_list_entry(&self, id: &str) -> Result<calendar::Calendar, GmailError> {
        let url = format!("{}/users/me/calendarList", self.calendar_base_url);
        let body = json!({ "id": id, "selected": true });
        let entry: Value = self.call_at(&url, |url| self.http().post(url).json(&body)).await?;
        Ok(google_calendar(&entry))
    }

    /// Google's `calendarList.patch`, with colours given as `#rrggbb`.
    async fn patch_list_entry(&self, id: &str, body: Value) -> Result<calendar::Calendar, GmailError> {
        let url = format!("{}/users/me/calendarList/{}", self.calendar_base_url, encode(id));
        let entry: Value = self
            .call_at(&url, |url| self.http().patch(url).query(&[("colorRgbFormat", "true")]).json(&body))
            .await?;
        Ok(google_calendar(&entry))
    }
}

/// A list entry's colours: `color` behind, and black or white text,
/// whichever reads better on it. Google wants both when it is given one.
fn colored(color: &str) -> Value {
    json!({ "backgroundColor": color, "foregroundColor": text_on(color) })
}

/// Black on a light colour and white on a dark one, by the colour's
/// relative luminance (WCAG's weights, without the gamma curve, which
/// does not move the answer for Google's palette).
fn text_on(color: &str) -> &'static str {
    let hex = color.trim_start_matches('#');
    let channel = |at: usize| hex.get(at..at + 2).and_then(|c| u8::from_str_radix(c, 16).ok()).map_or(0.0, f64::from);
    let luminance = 0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4);
    if luminance > 140.0 { "#000000" } else { "#ffffff" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_colours_take_black_text_and_dark_ones_white() {
        assert_eq!(text_on("#fad165"), "#000000");
        assert_eq!(text_on("#16a766"), "#ffffff");
        assert_eq!(text_on("#3f51b5"), "#ffffff");
        assert_eq!(text_on("#000000"), "#ffffff");
    }
}
