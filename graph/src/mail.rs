//! Mail folders, Outlook categories and messages: the calls the adapter
//! reads and changes mail with, each a thin wrapper over one Graph call.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::Me;
use crate::error::GraphError;
use crate::http::{BatchRequest, DeltaPage, Graph, Method, Page};
use crate::model::{EmailAddress, ItemBody, Recipient, Removed};

/// `PidTagMessageSize`, the size Exchange keeps for a message, which
/// v1.0's message resource does not carry as a property of its own.
pub const SIZE_PROPERTY: &str = "Integer 0x0E08";

const DELTA_FIELDS: &str = "id,conversationId,parentFolderId,isRead,flag,categories,inferenceClassification,receivedDateTime";
const IDS_FIELDS: &str = "id,conversationId,parentFolderId,receivedDateTime";
const META_FIELDS: &str = "id,conversationId,internetMessageId,parentFolderId,subject,bodyPreview,\
     receivedDateTime,from,toRecipients,ccRecipients,hasAttachments,isRead,isDraft,flag,categories,\
     inferenceClassification,internetMessageHeaders";
const EXPAND_SIZE: &str = "singleValueExtendedProperties($filter=id eq 'Integer 0x0E08')";
const PAGE_PREFER: &str = "odata.maxpagesize=50";

/// `path` with `pairs` as an encoded query, for a `$batch` entry, whose
/// URL Graph reads as it would a request line.
pub(crate) fn with_query(path: &str, pairs: &[(&str, &str)]) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish();
    format!("{path}?{query}")
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct MailFolder {
    pub id: String,
    pub display_name: String,
    pub parent_folder_id: Option<String>,
    pub child_folder_count: u32,
    pub total_item_count: u64,
    pub unread_item_count: u64,
    pub is_hidden: bool,
}

/// One entry of the account's Outlook categories. `color` is one of
/// Outlook's presets, `preset0` to `preset24`, or `none`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct MasterCategory {
    pub id: String,
    pub display_name: String,
    pub color: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Flag {
    /// `notFlagged`, `flagged` or `complete`.
    pub flag_status: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ExtendedProperty {
    pub id: String,
    pub value: String,
}

/// A message as a delta, a listing or a fetch hands it over. Every field
/// but the id may be missing, since each call selects its own.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Message {
    pub id: String,
    pub conversation_id: Option<String>,
    pub internet_message_id: Option<String>,
    pub parent_folder_id: Option<String>,
    pub subject: Option<String>,
    pub body_preview: Option<String>,
    pub received_date_time: Option<String>,
    pub from: Option<Recipient>,
    pub to_recipients: Vec<Recipient>,
    pub cc_recipients: Vec<Recipient>,
    pub has_attachments: Option<bool>,
    pub is_read: Option<bool>,
    pub is_draft: Option<bool>,
    pub flag: Option<Flag>,
    pub categories: Option<Vec<String>>,
    pub inference_classification: Option<String>,
    pub internet_message_headers: Option<Vec<Header>>,
    pub single_value_extended_properties: Option<Vec<ExtendedProperty>>,
    #[serde(rename = "@removed")]
    pub removed: Option<Removed>,
}

impl Message {
    pub fn received_millis(&self) -> Option<i64> {
        let text = self.received_date_time.as_deref()?;
        chrono::DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|t| t.timestamp_millis())
    }

    /// The size Exchange keeps, when the fetch expanded it. Graph answers
    /// the property's id in its own spelling (`Integer 0xe08`).
    pub fn size(&self) -> Option<i64> {
        self.single_value_extended_properties
            .as_ref()?
            .iter()
            .find(|p| {
                p.id.eq_ignore_ascii_case(SIZE_PROPERTY)
                    || p.id.eq_ignore_ascii_case("Integer 0xe08")
            })
            .and_then(|p| p.value.parse().ok())
    }

    pub fn is_flagged(&self) -> bool {
        self.flag
            .as_ref()
            .is_some_and(|f| f.flag_status == "flagged")
    }

    pub fn is_other(&self) -> bool {
        self.inference_classification.as_deref() == Some("other")
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.internet_message_headers
            .as_ref()?
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str())
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct AttachmentInfo {
    pub id: String,
    pub name: String,
    pub content_type: String,
    pub size: i64,
    pub is_inline: bool,
    pub content_id: Option<String>,
}

/// A message's body and the list of its files, for a message too large
/// to fetch whole.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct MessageBody {
    pub body: ItemBody,
    pub attachments: Vec<AttachmentInfo>,
}

/// Which fields a listing asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Fields {
    /// Ids, thread and folder, for a window or an inbox check.
    #[default]
    Ids,
    /// Everything the store keeps.
    Meta,
}

/// One listing of messages: in one folder or the whole mailbox, received
/// since a moment, in one conversation, with one `Message-ID`, or a
/// `$search`. Graph refuses `$orderby` and `$filter` beside `$search`, so
/// a search leaves both out and answers in Graph's relevance order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    pub folder: Option<String>,
    /// An RFC 3339 instant.
    pub received_since: Option<String>,
    pub conversation: Option<String>,
    /// With its angle brackets, as Graph keeps it.
    pub internet_message_id: Option<String>,
    /// KQL, without the quotes Graph wants around it.
    pub search: Option<String>,
    pub top: u32,
    pub fields: Fields,
}

/// What `MessagePatch` changes; a field left `None` stays.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessagePatch {
    pub is_read: Option<bool>,
    pub flagged: Option<bool>,
    /// The message's whole category list, which Graph replaces.
    pub categories: Option<Vec<String>>,
    /// `true` files it under Other, `false` under Focused.
    pub other: Option<bool>,
}

impl MessagePatch {
    fn json(&self) -> Value {
        let mut body = serde_json::Map::new();
        if let Some(read) = self.is_read {
            body.insert("isRead".into(), read.into());
        }
        if let Some(flagged) = self.flagged {
            let status = if flagged { "flagged" } else { "notFlagged" };
            body.insert("flag".into(), json!({ "flagStatus": status }));
        }
        if let Some(categories) = &self.categories {
            body.insert("categories".into(), json!(categories));
        }
        if let Some(other) = self.other {
            body.insert(
                "inferenceClassification".into(),
                (if other { "other" } else { "focused" }).into(),
            );
        }
        Value::Object(body)
    }
}

/// One change to one message, sent inside a `$batch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Write {
    Move {
        id: String,
        folder: String,
    },
    Patch {
        id: String,
        patch: MessagePatch,
    },
    /// Graph moves it to Deleted Items.
    Delete {
        id: String,
    },
    /// Gone for good, as Delete Forever means.
    PermanentDelete {
        id: String,
    },
}

impl Write {
    fn request(&self) -> BatchRequest {
        match self {
            Write::Move { id, folder } => BatchRequest::new(
                Method::Post,
                format!("me/messages/{id}/move"),
                Some(json!({ "destinationId": folder })),
            ),
            Write::Patch { id, patch } => BatchRequest::new(
                Method::Patch,
                format!("me/messages/{id}"),
                Some(patch.json()),
            ),
            Write::Delete { id } => {
                BatchRequest::new(Method::Delete, format!("me/messages/{id}"), None)
            }
            Write::PermanentDelete { id } => BatchRequest::new(
                Method::Post,
                format!("me/messages/{id}/permanentDelete"),
                None,
            ),
        }
    }
}

/// One sender Focused Inbox always files the same way.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Override {
    pub id: String,
    /// `focused` or `other`.
    pub classify_as: String,
    pub sender_email_address: EmailAddress,
}

impl Graph {
    pub async fn me(&self) -> Result<Me, GraphError> {
        self.get("me", &[("$select", "displayName,mail,userPrincipalName")])
            .await
    }

    /// The folders Graph names by their well-known names (`inbox`,
    /// `sentitems`, `drafts`, `deleteditems`, `junkemail`, `archive`), in
    /// one `$batch`. A folder the mailbox lacks is `None`.
    pub async fn well_known(&self, names: &[&str]) -> Result<Vec<Option<MailFolder>>, GraphError> {
        let requests: Vec<BatchRequest> = names
            .iter()
            .map(|name| {
                BatchRequest::get(with_query(
                    &format!("me/mailFolders/{name}"),
                    &[("$select", "id,displayName,parentFolderId")],
                ))
            })
            .collect();
        self.batch(&requests)
            .await?
            .into_iter()
            .map(|answer| match answer.into_json::<MailFolder>() {
                Ok(folder) => Ok(folder),
                Err(GraphError::NotFound) => Ok(None),
                Err(err) => Err(err),
            })
            .collect()
    }

    /// One page of the top folders, or of `parent`'s children.
    pub async fn folders(
        &self,
        parent: Option<&str>,
        next: Option<&str>,
    ) -> Result<Page<MailFolder>, GraphError> {
        if let Some(link) = next {
            return self.follow(link, &[]).await;
        }
        let path = match parent {
            Some(id) => format!("me/mailFolders/{id}/childFolders"),
            None => "me/mailFolders".to_string(),
        };
        self.get(
            &path,
            &[
                ("$top", "100"),
                ("includeHiddenFolders", "false"),
                ("$select", "id,displayName,parentFolderId,childFolderCount,totalItemCount,unreadItemCount,isHidden"),
            ],
        )
        .await
    }

    pub async fn categories(&self) -> Result<Vec<MasterCategory>, GraphError> {
        let page: Page<MasterCategory> = self.get("me/outlook/masterCategories", &[]).await?;
        Ok(page.value)
    }

    /// One page of `folder`'s delta. `link` is the next or delta link the
    /// last page gave; without one the round starts, over mail received
    /// since `received_since` (RFC 3339).
    pub async fn message_delta(
        &self,
        folder: &str,
        link: Option<&str>,
        received_since: &str,
    ) -> Result<DeltaPage<Message>, GraphError> {
        if let Some(link) = link {
            return self.follow(link, &[PAGE_PREFER]).await;
        }
        let filter = format!("receivedDateTime ge {received_since}");
        self.get_with(
            &format!("me/mailFolders/{folder}/messages/delta"),
            &[("$select", DELTA_FIELDS), ("$filter", &filter)],
            &[PAGE_PREFER],
        )
        .await
    }

    pub async fn list_messages(
        &self,
        listing: &Listing,
        next: Option<&str>,
    ) -> Result<Page<Message>, GraphError> {
        if let Some(link) = next {
            return self.follow(link, &[]).await;
        }
        let path = match &listing.folder {
            Some(folder) => format!("me/mailFolders/{folder}/messages"),
            None => "me/messages".to_string(),
        };
        let top = listing.top.clamp(1, 1000).to_string();
        let fields = match listing.fields {
            Fields::Ids => IDS_FIELDS,
            Fields::Meta => META_FIELDS,
        };
        let mut query: Vec<(&str, String)> = vec![("$top", top), ("$select", fields.to_string())];
        if listing.fields == Fields::Meta {
            query.push(("$expand", EXPAND_SIZE.to_string()));
        }
        match &listing.search {
            // Graph reads the KQL inside one quoted string, so a quote in
            // it is escaped rather than dropped: `subject:"weekly report"`.
            Some(kql) => query.push((
                "$search",
                format!("\"{}\"", kql.replace('\\', "\\\\").replace('"', "\\\"")),
            )),
            None => {
                let mut filters = Vec::new();
                if let Some(since) = &listing.received_since {
                    filters.push(format!("receivedDateTime ge {since}"));
                }
                if let Some(conversation) = &listing.conversation {
                    filters.push(format!(
                        "conversationId eq '{}'",
                        conversation.replace('\'', "''")
                    ));
                }
                if let Some(id) = &listing.internet_message_id {
                    filters.push(format!("internetMessageId eq '{}'", id.replace('\'', "''")));
                }
                if !filters.is_empty() {
                    query.push(("$filter", filters.join(" and ")));
                }
                // Graph orders only by a property the filter names first.
                if listing.received_since.is_some() || filters.is_empty() {
                    query.push(("$orderby", "receivedDateTime desc".to_string()));
                }
            }
        }
        let pairs: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.get(&path, &pairs).await
    }

    /// Each message's metadata, in `$batch` calls of 20, one answer per id
    /// in order; a message Graph no longer has answers `NotFound` in its
    /// place.
    pub async fn messages(
        &self,
        ids: &[String],
    ) -> Result<Vec<Result<Message, GraphError>>, GraphError> {
        let requests: Vec<BatchRequest> = ids
            .iter()
            .map(|id| {
                BatchRequest::get(with_query(
                    &format!("me/messages/{id}"),
                    &[("$select", META_FIELDS), ("$expand", EXPAND_SIZE)],
                ))
            })
            .collect();
        Ok(self
            .batch(&requests)
            .await?
            .into_iter()
            .map(|answer| {
                answer
                    .into_json::<Message>()
                    .and_then(|m| m.ok_or(GraphError::NotFound))
            })
            .collect())
    }

    /// The message as MIME, refused past `limit` bytes.
    pub async fn raw(&self, id: &str, limit: usize) -> Result<Vec<u8>, GraphError> {
        self.get_bytes(&format!("me/messages/{id}/$value"), limit)
            .await
    }

    pub async fn body(&self, id: &str) -> Result<MessageBody, GraphError> {
        self.get(
            &format!("me/messages/{id}"),
            &[
                ("$select", "body,hasAttachments"),
                (
                    "$expand",
                    "attachments($select=id,name,contentType,size,isInline,contentId)",
                ),
            ],
        )
        .await
    }

    pub async fn attachment(
        &self,
        message: &str,
        attachment: &str,
        limit: usize,
    ) -> Result<Vec<u8>, GraphError> {
        self.get_bytes(
            &format!("me/messages/{message}/attachments/{attachment}/$value"),
            limit,
        )
        .await
    }

    /// Each write's outcome, in order.
    pub async fn apply(&self, writes: &[Write]) -> Result<Vec<Result<(), GraphError>>, GraphError> {
        let requests: Vec<BatchRequest> = writes.iter().map(Write::request).collect();
        Ok(self
            .batch(&requests)
            .await?
            .into_iter()
            .map(|answer| answer.into_json::<Value>().map(|_| ()))
            .collect())
    }

    pub async fn create_folder(
        &self,
        parent: Option<&str>,
        name: &str,
    ) -> Result<MailFolder, GraphError> {
        let path = match parent {
            Some(id) => format!("me/mailFolders/{id}/childFolders"),
            None => "me/mailFolders".to_string(),
        };
        self.send(
            Method::Post,
            &path,
            Some(&json!({ "displayName": name })),
            &[],
        )
        .await?
        .ok_or_else(|| GraphError::Decode("no folder in the answer".into()))
    }

    pub async fn rename_folder(&self, id: &str, name: &str) -> Result<MailFolder, GraphError> {
        self.send(
            Method::Patch,
            &format!("me/mailFolders/{id}"),
            Some(&json!({ "displayName": name })),
            &[],
        )
        .await?
        .ok_or_else(|| GraphError::Decode("no folder in the answer".into()))
    }

    pub async fn delete_folder(&self, id: &str) -> Result<(), GraphError> {
        self.send::<Value>(Method::Delete, &format!("me/mailFolders/{id}"), None, &[])
            .await
            .map(|_| ())
    }

    pub async fn set_category_color(
        &self,
        id: &str,
        color: &str,
    ) -> Result<MasterCategory, GraphError> {
        self.send(
            Method::Patch,
            &format!("me/outlook/masterCategories/{id}"),
            Some(&json!({ "color": color })),
            &[],
        )
        .await?
        .ok_or_else(|| GraphError::Decode("no category in the answer".into()))
    }

    /// Sends a MIME message as it is. Graph files the copy in Sent Items.
    pub async fn send_mime(&self, raw: &[u8]) -> Result<(), GraphError> {
        self.post_mime("me/sendMail", raw).await.map(|_| ())
    }

    /// A draft in Drafts made from a MIME message.
    pub async fn create_draft_mime(&self, raw: &[u8]) -> Result<Message, GraphError> {
        let answer = self.post_mime("me/messages", raw).await?;
        serde_json::from_value(answer.unwrap_or_default())
            .map_err(|e| GraphError::Decode(e.to_string()))
    }

    /// A draft from Graph's own message shape, for a message too large to
    /// go as MIME (Task 20).
    pub async fn create_draft(&self, draft: &Value) -> Result<Message, GraphError> {
        self.send(Method::Post, "me/messages", Some(draft), &[])
            .await?
            .ok_or_else(|| GraphError::Decode("no draft in the answer".into()))
    }

    /// Where to upload a file of `size` bytes onto draft `message`.
    pub async fn upload_session(
        &self,
        message: &str,
        name: &str,
        size: u64,
        is_inline: bool,
        content_id: Option<&str>,
    ) -> Result<String, GraphError> {
        let mut item =
            json!({ "attachmentType": "file", "name": name, "size": size, "isInline": is_inline });
        if let Some(id) = content_id {
            item["contentId"] = json!(id);
        }
        let body = json!({ "AttachmentItem": item });
        let answer: Value = self
            .send(
                Method::Post,
                &format!("me/messages/{message}/attachments/createUploadSession"),
                Some(&body),
                &[],
            )
            .await?
            .unwrap_or_default();
        answer["uploadUrl"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| GraphError::Decode("no uploadUrl in the answer".into()))
    }

    /// One piece of an upload, `bytes` at `offset` of `total`. `true` once
    /// the last piece landed.
    pub async fn upload_chunk(
        &self,
        url: &str,
        offset: u64,
        total: u64,
        bytes: &[u8],
    ) -> Result<bool, GraphError> {
        self.upload(url, offset, total, bytes).await
    }

    pub async fn send_draft(&self, id: &str) -> Result<(), GraphError> {
        self.send::<Value>(Method::Post, &format!("me/messages/{id}/send"), None, &[])
            .await
            .map(|_| ())
    }

    pub async fn delete_message(&self, id: &str) -> Result<(), GraphError> {
        self.send::<Value>(Method::Delete, &format!("me/messages/{id}"), None, &[])
            .await
            .map(|_| ())
    }
}
