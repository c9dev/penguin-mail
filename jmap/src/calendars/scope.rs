//! Shared calendars belong to separate JMAP accounts. Their local ids and
//! change tokens carry that account; personal ids keep their old form.

use serde_json::{Map, Value};

use super::{CALENDARS, fits};
use crate::{JmapError, Session};

const PREFIX: &str = "~shared:";

fn split(value: &str) -> Option<(&str, &str)> {
    value.strip_prefix(PREFIX)?.split_once(':')
}

pub(super) struct Scope {
    pub account: String,
    primary: String,
}

impl Scope {
    pub fn of(session: &Session, value: &str) -> Result<Self, JmapError> {
        let primary = session
            .account_for(CALENDARS)
            .ok_or_else(|| JmapError::UnknownCapability(CALENDARS.into()))?;
        let account = split(value).map_or(primary, |(account, _)| account);
        if !session
            .accounts
            .get(account)
            .and_then(|a| a.account_capabilities.get(CALENDARS))
            .is_some_and(fits)
        {
            return Err(JmapError::UnknownCapability(CALENDARS.into()));
        }
        Ok(Self {
            account: account.into(),
            primary: primary.into(),
        })
    }

    pub fn for_account(session: &Session, account: &str) -> Result<Self, JmapError> {
        Self::of(session, &format!("{PREFIX}{account}:"))
    }

    pub fn id(&self, value: &str) -> String {
        if self.account == self.primary {
            value.into()
        } else {
            format!("{PREFIX}{}:{value}", self.account)
        }
    }

    pub fn raw(&self, value: &str) -> Result<String, JmapError> {
        let (account, raw) = split(value).unwrap_or((&self.primary, value));
        if account != self.account {
            return Err(JmapError::Forbidden(
                "calendar data belongs to another account".into(),
            ));
        }
        Ok(raw.into())
    }

    pub fn event(&self, mut event: Value) -> Value {
        if let Some(id) = event.get_mut("id")
            && let Some(raw) = id.as_str()
        {
            *id = Value::String(self.id(raw));
        }
        if let Some(calendars) = event.get_mut("calendarIds")
            && let Some(ids) = calendars.as_object()
        {
            *calendars = Value::Object(
                ids.iter()
                    .map(|(id, held)| (self.id(id), held.clone()))
                    .collect(),
            );
        }
        event
    }

    pub fn fields(&self, fields: &Map<String, Value>) -> Result<Map<String, Value>, JmapError> {
        fields
            .iter()
            .map(|(key, value)| {
                if key == "calendarIds" {
                    let mut value = value.clone();
                    if let Some(ids) = value.as_object() {
                        value = Value::Object(
                            ids.iter()
                                .map(|(id, held)| Ok((self.raw(id)?, held.clone())))
                                .collect::<Result<_, JmapError>>()?,
                        );
                    }
                    Ok((key.clone(), value))
                } else if let Some(id) = key.strip_prefix("calendarIds/") {
                    let id = id.replace("~1", "/").replace("~0", "~");
                    Ok((
                        format!("calendarIds/{}", crate::pointer(&self.raw(&id)?)),
                        value.clone(),
                    ))
                } else {
                    Ok((key.clone(), value.clone()))
                }
            })
            .collect()
    }

    pub fn filter(&self, filter: &Value) -> Result<Value, JmapError> {
        let mut filter = filter.clone();
        if let Some(id) = filter.get_mut("inCalendar")
            && let Some(raw) = id.as_str()
        {
            *id = Value::String(self.raw(raw)?);
        }
        if let Some(conditions) = filter.get_mut("conditions").and_then(Value::as_array_mut) {
            for condition in conditions {
                *condition = self.filter(condition)?;
            }
        }
        Ok(filter)
    }
}

pub(super) fn calendar(filter: &Value) -> Option<&str> {
    filter
        .get("inCalendar")
        .and_then(Value::as_str)
        .or_else(|| {
            filter
                .get("conditions")
                .and_then(Value::as_array)?
                .iter()
                .find_map(calendar)
        })
}
