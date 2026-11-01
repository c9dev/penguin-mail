//! Shapes several Graph resources share.

use serde::{Deserialize, Serialize};

/// Graph's nullable fields can be absent or explicitly null. Serde's
/// `default` handles only the absent case; both mean an unset value here.
pub(crate) fn default_if_null<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

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
    #[serde(deserialize_with = "default_if_null")]
    pub content_type: String,
    #[serde(deserialize_with = "default_if_null")]
    pub content: String,
}

/// A local time and the zone it is in, as Graph writes one. The adapter
/// asks for events in UTC (`Prefer: outlook.timezone="UTC"`).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct DateTimeZone {
    pub date_time: String,
    #[serde(deserialize_with = "default_if_null")]
    pub time_zone: String,
}

/// A delta entry's mark that the item left the set the round covers.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Removed {
    pub reason: Option<String>,
}
