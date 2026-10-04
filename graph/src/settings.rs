//! Inbox rules, the automatic reply and Focused Inbox's sender overrides.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::GraphError;
use crate::http::{Graph, Method, Page};
use crate::mail::Override;
use crate::model::{DateTimeZone, EmailAddress, Recipient};

/// A rule's conditions or exceptions. What the app has no word for stays
/// in `other`, which is how the adapter knows to leave the rule read-only.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RulePredicates {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub from_addresses: Vec<Recipient>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sent_to_addresses: Vec<Recipient>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subject_contains: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub body_or_subject_contains: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_attachments: Option<bool>,
    #[serde(flatten, skip_serializing)]
    pub other: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RuleActions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_to_folder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mark_as_read: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assign_categories: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub forward_to: Vec<Recipient>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_processing_rules: Option<bool>,
    #[serde(flatten, skip_serializing)]
    pub other: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct MessageRule {
    #[serde(skip_serializing)]
    pub id: String,
    pub display_name: String,
    pub sequence: u32,
    pub is_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conditions: Option<RulePredicates>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<RuleActions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exceptions: Option<RulePredicates>,
    #[serde(skip_serializing)]
    pub is_read_only: bool,
    #[serde(skip_serializing)]
    pub has_error: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct AutomaticReplies {
    /// `disabled`, `alwaysEnabled` or `scheduled`.
    pub status: String,
    /// `none`, `contactsOnly` or `all`.
    pub external_audience: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_start_date_time: Option<DateTimeZone>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_end_date_time: Option<DateTimeZone>,
    pub internal_reply_message: String,
    pub external_reply_message: String,
}

impl Graph {
    pub async fn rules(&self) -> Result<Vec<MessageRule>, GraphError> {
        let page: Page<MessageRule> = self.get("me/mailFolders/inbox/messageRules", &[]).await?;
        Ok(page.value)
    }

    pub async fn create_rule(&self, rule: &MessageRule) -> Result<MessageRule, GraphError> {
        let body = serde_json::to_value(rule).map_err(|e| GraphError::Decode(e.to_string()))?;
        self.send(
            Method::Post,
            "me/mailFolders/inbox/messageRules",
            Some(&body),
            &[],
        )
        .await?
        .ok_or_else(|| GraphError::Decode("no rule in the answer".into()))
    }

    pub async fn delete_rule(&self, id: &str) -> Result<(), GraphError> {
        self.send::<Value>(
            Method::Delete,
            &format!("me/mailFolders/inbox/messageRules/{id}"),
            None,
            &[],
        )
        .await
        .map(|_| ())
    }

    pub async fn automatic_replies(&self) -> Result<AutomaticReplies, GraphError> {
        self.get_with(
            "me/mailboxSettings/automaticRepliesSetting",
            &[],
            &["outlook.timezone=\"UTC\""],
        )
        .await
    }

    pub async fn set_automatic_replies(
        &self,
        replies: &AutomaticReplies,
    ) -> Result<(), GraphError> {
        let body = json!({ "automaticRepliesSetting": replies });
        self.send::<Value>(Method::Patch, "me/mailboxSettings", Some(&body), &[])
            .await
            .map(|_| ())
    }

    pub async fn overrides(&self) -> Result<Vec<Override>, GraphError> {
        let page: Page<Override> = self
            .get("me/inferenceClassification/overrides", &[])
            .await?;
        Ok(page.value)
    }

    /// Always files `address`'s mail under Other (`other`) or Focused.
    pub async fn set_override(&self, address: &str, other: bool) -> Result<Override, GraphError> {
        let body = json!({
            "classifyAs": if other { "other" } else { "focused" },
            "senderEmailAddress": EmailAddress { name: None, address: Some(address.to_string()) },
        });
        self.send(
            Method::Post,
            "me/inferenceClassification/overrides",
            Some(&body),
            &[],
        )
        .await?
        .ok_or_else(|| GraphError::Decode("no override in the answer".into()))
    }

    pub async fn delete_override(&self, id: &str) -> Result<(), GraphError> {
        self.send::<Value>(
            Method::Delete,
            &format!("me/inferenceClassification/overrides/{id}"),
            None,
            &[],
        )
        .await
        .map(|_| ())
    }
}
