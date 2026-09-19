//! Gmail REST wire types. Google sends int64 fields as JSON strings.

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Deserialize)]
#[serde(untagged)]
enum StrOrNum<N> {
    Str(String),
    Num(N),
}

fn u64_from_string<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    match StrOrNum::<u64>::deserialize(d)? {
        StrOrNum::Str(s) => s.parse().map_err(serde::de::Error::custom),
        StrOrNum::Num(n) => Ok(n),
    }
}

fn opt_i64_from_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    match Option::<StrOrNum<i64>>::deserialize(d)? {
        None => Ok(None),
        Some(StrOrNum::Str(s)) => s.parse().map(Some).map_err(serde::de::Error::custom),
        Some(StrOrNum::Num(n)) => Ok(Some(n)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub email_address: String,
    #[serde(deserialize_with = "u64_from_string")]
    pub history_id: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LabelList {
    #[serde(default)]
    pub labels: Vec<RemoteLabel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RemoteLabel {
    pub id: String,
    pub name: String,
    /// "system" or "user".
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub color: Option<LabelColor>,
}

/// A label's colours. Gmail accepts only colours from its own palette.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelColor {
    pub background_color: String,
    pub text_color: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRef {
    pub id: String,
    pub thread_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagePage {
    #[serde(default)]
    pub messages: Vec<MessageRef>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub thread_id: String,
    #[serde(default)]
    pub label_ids: Vec<String>,
    #[serde(default)]
    pub snippet: String,
    #[serde(default, deserialize_with = "opt_i64_from_string")]
    pub internal_date: Option<i64>,
    #[serde(default)]
    pub size_estimate: i64,
    pub payload: Option<MessagePart>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagePart {
    #[serde(default)]
    pub part_id: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub headers: Vec<Header>,
    #[serde(default)]
    pub body: PartBody,
    #[serde(default)]
    pub parts: Vec<MessagePart>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartBody {
    pub attachment_id: Option<String>,
    #[serde(default)]
    pub size: i64,
    /// Base64url content, already decoded from its transfer encoding by Gmail.
    pub data: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Thread {
    pub id: String,
    #[serde(default)]
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryList {
    #[serde(default)]
    pub history: Vec<History>,
    pub next_page_token: Option<String>,
    #[serde(deserialize_with = "u64_from_string")]
    pub history_id: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct History {
    #[serde(default)]
    pub messages_added: Vec<HistoryMessage>,
    #[serde(default)]
    pub messages_deleted: Vec<HistoryMessage>,
    #[serde(default)]
    pub labels_added: Vec<HistoryMessage>,
    #[serde(default)]
    pub labels_removed: Vec<HistoryMessage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryMessage {
    pub message: Message,
    /// For label changes, the labels that were added or removed.
    #[serde(default)]
    pub label_ids: Vec<String>,
}

/// A draft and the message that currently backs it. Gmail replaces the
/// message on every update; the draft id stays the same.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Draft {
    pub id: String,
    pub message: MessageRef,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftList {
    #[serde(default)]
    pub drafts: Vec<Draft>,
    pub next_page_token: Option<String>,
}

/// An address the account can send from.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendAs {
    pub send_as_email: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub is_primary: bool,
    /// HTML, empty when the identity has none.
    #[serde(default)]
    pub signature: String,
}

/// `users.settings.vacation`. Times are epoch milliseconds sent as strings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VacationSettings {
    #[serde(default)]
    pub enable_auto_reply: bool,
    #[serde(default)]
    pub response_subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body_plain_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body_html: Option<String>,
    #[serde(default)]
    pub restrict_to_contacts: bool,
    #[serde(default)]
    pub restrict_to_domain: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendAsList {
    #[serde(default)]
    pub send_as: Vec<SendAs>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AttachmentBody {
    pub data: Option<String>,
}
