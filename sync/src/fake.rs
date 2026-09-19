//! An in-memory Gmail for sync tests. Tests change its mailbox directly; the
//! changes that Gmail would record in history are recorded here too.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use mailrs_domain::{Address, EpochMillis, MessageBody, MessageMeta, Vacation};
use mailrs_gmail::{
    GmailError, HistoryChange, HistoryPage, MessagePage, MessageRef, Profile, RemoteLabel,
};

use crate::api::{GmailApi, SavedDraft};

pub struct FakeGmail {
    state: Mutex<FakeState>,
}

pub struct FakeState {
    pub email: String,
    pub history_id: u64,
    /// `history` answers 404 for starts below this, as Gmail does once history ages out.
    pub history_floor: u64,
    pub labels: Vec<RemoteLabel>,
    pub messages: HashMap<String, MessageMeta>,
    /// Ids the window listing returns, newest first.
    pub listed: Vec<String>,
    pub history: Vec<(u64, HistoryChange)>,
    pub bodies: HashMap<String, MessageBody>,
    /// Page size for both listings and history.
    pub page_size: usize,
    /// Errors returned by the next calls, one per call.
    pub failures: VecDeque<GmailError>,
    pub body_fetches: usize,
    pub remote_writes: Vec<String>,
    /// Raw messages sent, with their thread ids.
    pub sent: Vec<(Vec<u8>, Option<String>)>,
    /// Draft id to raw content.
    pub drafts: HashMap<String, Vec<u8>>,
    /// Message id backing each draft.
    pub draft_messages: HashMap<String, String>,
    pub attachments: HashMap<(String, String), Vec<u8>>,
    pub display_name: Option<String>,
    pub signature: Option<String>,
    pub vacation: Vacation,
}

/// A message for account 1.
pub fn meta(id: &str, thread: &str, date: EpochMillis, labels: &[&str]) -> MessageMeta {
    MessageMeta {
        account_id: 1,
        id: id.into(),
        thread_id: thread.into(),
        rfc822_msgid: Some(format!("<{id}@example.com>")),
        from: Some(Address {
            name: Some("Ann".into()),
            email: "ann@example.com".into(),
        }),
        to: vec![],
        cc: vec![],
        subject: format!("Subject {id}"),
        date,
        snippet: format!("snippet {id}"),
        size: 100,
        has_attachments: false,
        label_ids: labels.iter().map(|l| l.to_string()).collect(),
    }
}

impl FakeGmail {
    pub fn new() -> Self {
        let label = |id: &str, kind: &str| RemoteLabel {
            id: id.into(),
            name: id.into(),
            kind: Some(kind.into()),
        };
        FakeGmail {
            state: Mutex::new(FakeState {
                email: "me@example.com".into(),
                history_id: 100,
                history_floor: 0,
                labels: vec![
                    label("INBOX", "system"),
                    label("UNREAD", "system"),
                    label("STARRED", "system"),
                    label("Label_1", "user"),
                ],
                messages: HashMap::new(),
                listed: Vec::new(),
                history: Vec::new(),
                bodies: HashMap::new(),
                page_size: 2,
                failures: VecDeque::new(),
                body_fetches: 0,
                remote_writes: Vec::new(),
                sent: Vec::new(),
                drafts: HashMap::new(),
                draft_messages: HashMap::new(),
                attachments: HashMap::new(),
                display_name: Some("Me".into()),
                signature: None,
                vacation: Vacation::default(),
            }),
        }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut FakeState) -> R) -> R {
        f(&mut self.state.lock().expect("fake state poisoned"))
    }

    /// A message that already exists and matches the window listing. No history.
    pub fn seed(&self, meta: MessageMeta) {
        self.with(|s| {
            s.listed.push(meta.id.clone());
            s.messages.insert(meta.id.clone(), meta);
            s.sort_listed();
        });
    }

    /// A message that exists but that the window listing does not return.
    pub fn seed_outside_window(&self, meta: MessageMeta) {
        self.with(|s| {
            s.messages.insert(meta.id.clone(), meta);
        });
    }

    /// A message arriving now, recorded in history.
    pub fn deliver(&self, meta: MessageMeta) {
        self.with(|s| {
            let change = HistoryChange::MessageAdded {
                id: meta.id.clone(),
                thread_id: meta.thread_id.clone(),
            };
            s.listed.push(meta.id.clone());
            s.messages.insert(meta.id.clone(), meta);
            s.sort_listed();
            s.record(change);
        });
    }

    pub fn remote_delete(&self, id: &str) {
        self.with(|s| {
            if let Some(meta) = s.messages.remove(id) {
                s.listed.retain(|l| l != id);
                s.record(HistoryChange::MessageDeleted {
                    id: id.into(),
                    thread_id: meta.thread_id,
                });
            }
        });
    }

    /// Deletes a message without recording history, as if it happened during a gap.
    pub fn remote_delete_silently(&self, id: &str) {
        self.with(|s| {
            s.messages.remove(id);
            s.listed.retain(|l| l != id);
        });
    }

    pub fn remote_relabel(&self, id: &str, add: &[&str], remove: &[&str]) {
        self.with(|s| {
            let Some(meta) = s.messages.get_mut(id) else {
                return;
            };
            let thread_id = meta.thread_id.clone();
            for label in add {
                if !meta.has_label(label) {
                    meta.label_ids.push(label.to_string());
                }
            }
            meta.label_ids.retain(|l| !remove.contains(&l.as_str()));
            if !add.is_empty() {
                s.record(HistoryChange::LabelsAdded {
                    id: id.into(),
                    thread_id: thread_id.clone(),
                    label_ids: add.iter().map(|l| l.to_string()).collect(),
                });
            }
            if !remove.is_empty() {
                s.record(HistoryChange::LabelsRemoved {
                    id: id.into(),
                    thread_id,
                    label_ids: remove.iter().map(|l| l.to_string()).collect(),
                });
            }
        });
    }

    /// Makes every history id recorded so far too old to replay.
    pub fn expire_history(&self) {
        self.with(|s| {
            s.history_id += 1;
            s.history_floor = s.history_id;
        });
    }

    pub fn fail_next(&self, err: GmailError) {
        self.with(|s| s.failures.push_back(err));
    }

    fn check_failure(&self) -> Result<(), GmailError> {
        self.with(|s| s.failures.pop_front().map_or(Ok(()), Err))
    }
}

impl FakeState {
    fn record(&mut self, change: HistoryChange) {
        self.history_id += 1;
        self.history.push((self.history_id, change));
    }

    fn sort_listed(&mut self) {
        let messages = &self.messages;
        self.listed
            .sort_by_key(|id| std::cmp::Reverse(messages[id].date));
    }
}

impl GmailApi for FakeGmail {
    async fn profile(&self) -> Result<Profile, GmailError> {
        self.check_failure()?;
        Ok(self.with(|s| Profile {
            email_address: s.email.clone(),
            history_id: s.history_id,
        }))
    }

    async fn labels(&self) -> Result<Vec<RemoteLabel>, GmailError> {
        self.check_failure()?;
        Ok(self.with(|s| s.labels.clone()))
    }

    async fn list_messages(
        &self,
        _query: &str,
        page_token: Option<&str>,
    ) -> Result<MessagePage, GmailError> {
        self.check_failure()?;
        let start = match page_token {
            None => 0,
            Some(token) => token.parse::<usize>().map_err(|_| GmailError::Http {
                status: 400,
                body: "Invalid pageToken".into(),
            })?,
        };
        Ok(self.with(|s| {
            let end = (start + s.page_size).min(s.listed.len());
            let messages = s.listed[start.min(end)..end]
                .iter()
                .map(|id| MessageRef {
                    id: id.clone(),
                    thread_id: s.messages[id].thread_id.clone(),
                })
                .collect();
            MessagePage {
                messages,
                next_page_token: (end < s.listed.len()).then(|| end.to_string()),
            }
        }))
    }

    async fn message_metadata(&self, id: &str) -> Result<MessageMeta, GmailError> {
        self.check_failure()?;
        self.with(|s| s.messages.get(id).cloned().ok_or(GmailError::NotFound))
    }

    async fn thread_metadata(&self, thread_id: &str) -> Result<Vec<MessageMeta>, GmailError> {
        self.check_failure()?;
        self.with(|s| {
            let mut metas: Vec<MessageMeta> = s
                .messages
                .values()
                .filter(|m| m.thread_id == thread_id)
                .cloned()
                .collect();
            if metas.is_empty() {
                return Err(GmailError::NotFound);
            }
            metas.sort_by_key(|m| m.date);
            Ok(metas)
        })
    }

    async fn message_body(&self, id: &str) -> Result<MessageBody, GmailError> {
        self.check_failure()?;
        self.with(|s| {
            s.body_fetches += 1;
            s.bodies.get(id).cloned().ok_or(GmailError::NotFound)
        })
    }

    async fn history(
        &self,
        start: u64,
        page_token: Option<&str>,
    ) -> Result<HistoryPage, GmailError> {
        self.check_failure()?;
        self.with(|s| {
            if start < s.history_floor {
                return Err(GmailError::NotFound);
            }
            let pending: Vec<HistoryChange> = s
                .history
                .iter()
                .filter(|(h, _)| *h > start)
                .map(|(_, c)| c.clone())
                .collect();
            let offset = page_token
                .and_then(|t| t.parse::<usize>().ok())
                .unwrap_or(0);
            let end = (offset + s.page_size).min(pending.len());
            Ok(HistoryPage {
                changes: pending[offset.min(end)..end].to_vec(),
                next_page_token: (end < pending.len()).then(|| end.to_string()),
                history_id: s.history_id,
            })
        })
    }

    async fn modify_labels(
        &self,
        id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), GmailError> {
        self.check_failure()?;
        self.with(|s| {
            s.remote_writes.push(format!(
                "modify {id} +{} -{}",
                add.join(","),
                remove.join(",")
            ))
        });
        let add: Vec<&str> = add.iter().map(String::as_str).collect();
        let remove: Vec<&str> = remove.iter().map(String::as_str).collect();
        self.remote_relabel(id, &add, &remove);
        Ok(())
    }

    async fn trash(&self, id: &str) -> Result<(), GmailError> {
        self.check_failure()?;
        self.with(|s| s.remote_writes.push(format!("trash {id}")));
        self.remote_relabel(id, &["TRASH"], &["INBOX"]);
        Ok(())
    }

    async fn untrash(&self, id: &str) -> Result<(), GmailError> {
        self.check_failure()?;
        self.with(|s| s.remote_writes.push(format!("untrash {id}")));
        self.remote_relabel(id, &["INBOX"], &["TRASH"]);
        Ok(())
    }

    async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<String, GmailError> {
        self.check_failure()?;
        Ok(self.with(|s| {
            s.sent.push((raw.to_vec(), thread_id.map(str::to_string)));
            format!("sent{}", s.sent.len())
        }))
    }

    async fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        _thread_id: Option<&str>,
    ) -> Result<SavedDraft, GmailError> {
        self.check_failure()?;
        self.with(|s| {
            let id = match draft_id {
                Some(id) if !s.drafts.contains_key(id) => return Err(GmailError::NotFound),
                Some(id) => id.to_string(),
                None => format!("draft{}", s.drafts.len() + 1),
            };
            let message_id = format!("{id}-m{}", raw.len());
            s.drafts.insert(id.clone(), raw.to_vec());
            s.draft_messages.insert(id.clone(), message_id.clone());
            Ok(SavedDraft {
                thread_id: format!("{id}-t"),
                draft_id: id,
                message_id,
            })
        })
    }

    async fn send_draft(&self, draft_id: &str) -> Result<String, GmailError> {
        self.check_failure()?;
        self.with(|s| {
            let raw = s.drafts.remove(draft_id).ok_or(GmailError::NotFound)?;
            s.draft_messages.remove(draft_id);
            s.sent.push((raw, None));
            Ok(format!("sent{}", s.sent.len()))
        })
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), GmailError> {
        self.check_failure()?;
        self.with(|s| {
            s.draft_messages.remove(draft_id);
            s.drafts
                .remove(draft_id)
                .map(|_| ())
                .ok_or(GmailError::NotFound)
        })
    }

    async fn draft_for_message(&self, message_id: &str) -> Result<Option<String>, GmailError> {
        self.check_failure()?;
        Ok(self.with(|s| {
            s.draft_messages
                .iter()
                .find(|(_, m)| *m == message_id)
                .map(|(d, _)| d.clone())
        }))
    }

    async fn display_name(&self) -> Result<Option<String>, GmailError> {
        self.check_failure()?;
        Ok(self.with(|s| s.display_name.clone()))
    }

    async fn attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, GmailError> {
        self.check_failure()?;
        self.with(|s| {
            s.attachments
                .get(&(message_id.to_string(), attachment_id.to_string()))
                .cloned()
                .ok_or(GmailError::NotFound)
        })
    }

    async fn signature(&self) -> Result<Option<String>, GmailError> {
        self.check_failure()?;
        Ok(self.with(|s| s.signature.clone()))
    }

    async fn vacation(&self) -> Result<Vacation, GmailError> {
        self.check_failure()?;
        Ok(self.with(|s| s.vacation.clone()))
    }

    async fn set_vacation(&self, vacation: &Vacation) -> Result<(), GmailError> {
        self.check_failure()?;
        self.with(|s| s.vacation = vacation.clone());
        Ok(())
    }

    async fn create_label(&self, name: &str) -> Result<RemoteLabel, GmailError> {
        self.check_failure()?;
        Ok(self.with(|s| {
            let label = RemoteLabel {
                id: format!("Label_{}", s.labels.len() + 1),
                name: name.to_string(),
                kind: Some("user".into()),
            };
            s.labels.push(label.clone());
            label
        }))
    }

    async fn rename_label(&self, id: &str, name: &str) -> Result<RemoteLabel, GmailError> {
        self.check_failure()?;
        self.with(|s| {
            let label = s
                .labels
                .iter_mut()
                .find(|l| l.id == id)
                .ok_or(GmailError::NotFound)?;
            label.name = name.to_string();
            Ok(label.clone())
        })
    }

    async fn delete_label(&self, id: &str) -> Result<(), GmailError> {
        self.check_failure()?;
        self.with(|s| {
            s.labels.retain(|l| l.id != id);
            for message in s.messages.values_mut() {
                message.label_ids.retain(|l| l != id);
            }
        });
        Ok(())
    }
}
