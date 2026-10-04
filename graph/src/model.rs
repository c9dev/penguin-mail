//! Shapes several Graph resources share.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct EmailAddress {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Recipient {
    pub email_address: EmailAddress,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct ItemBody {
    /// `text` or `html`.
    pub content_type: String,
    pub content: String,
}

/// A local time and the zone it is in, as Graph writes one. The adapter
/// asks for events in UTC (`Prefer: outlook.timezone="UTC"`).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct DateTimeZone {
    pub date_time: String,
    pub time_zone: String,
}

/// A delta entry's mark that the item left the set the round covers.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Removed {
    pub reason: Option<String>,
}
