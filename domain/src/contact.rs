//! Contact details shared by address book services.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Details {
    /// The revision read from CardDAV, for detecting edits on another device.
    pub revision: String,
    pub uid: String,
    /// Group cards have members instead of representing a person.
    pub members: Option<Vec<String>>,
    pub phones: Vec<String>,
    pub birthday: String,
    pub notes: String,
    pub addresses: Vec<String>,
    pub urls: Vec<String>,
    pub groups: Vec<String>,
    pub dates: Vec<Field>,
    pub custom: Vec<Field>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub label: String,
    pub value: String,
}

/// Only the fields the person changed. None leaves a server's data alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub birthday: Option<String>,
    pub notes: Option<String>,
    pub addresses: Option<Vec<String>>,
    pub urls: Option<Vec<String>>,
    pub groups: Option<Vec<String>>,
    pub dates: Option<Vec<Field>>,
    pub custom: Option<Vec<Field>>,
}
