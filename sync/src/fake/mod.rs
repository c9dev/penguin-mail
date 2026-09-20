//! An in-memory Gmail. Sync's tests and `penguin-mail --demo` both run on
//! it: seed its mailbox, hand it to an `AccountSync`, and every read and
//! write the app makes goes through the same code the real client does.
//!
//! Callers change the mailbox directly through [`FakeGmail::with`]. The
//! changes Gmail would record in history are recorded here too, so a sync
//! replays them.

mod query;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use mailrs_domain::invitation::Answer;
use mailrs_domain::{
    Address, EpochMillis, Filter, MessageBody, MessageMeta, Vacation, system_label,
};
use mailrs_gmail::{
    AccountQuota, Answered, BATCH_LIMIT, Busy, ConnectionsPage, GmailError, HistoryChange,
    HistoryPage, LabelColor, MessagePage, MessageRef, Person, Priority, Profile, QuotaLimiter,
    RemoteLabel, SendAs, cost, limiter,
};

use crate::api::{GmailApi, SavedDraft};
use query::Query;

pub struct FakeGmail {
    state: Mutex<FakeState>,
    /// The budget the app paces itself against, as the real client does.
    /// Without one the fake answers as fast as it is asked.
    quota: Option<std::sync::Arc<AccountQuota>>,
    /// Gmail's own per-user rate. With one, a call that arrives on an empty
    /// bucket comes back rate limited, the way Gmail answers 429.
    limit: Option<QuotaLimiter>,
}

pub struct FakeState {
    pub email: String,
    pub history_id: u64,
    /// `history` answers 404 for starts below this, as Gmail does once history ages out.
    pub history_floor: u64,
    pub labels: Vec<RemoteLabel>,
    pub messages: HashMap<String, MessageMeta>,
    pub history: Vec<(u64, HistoryChange)>,
    pub bodies: HashMap<String, MessageBody>,
    /// Page size for both listings and history.
    pub page_size: usize,
    /// Errors returned by the next calls, one per call.
    pub failures: VecDeque<GmailError>,
    /// What the calls so far would have cost against the real API.
    pub usage: Usage,
    pub body_fetches: usize,
    pub remote_writes: Vec<String>,
    /// Raw messages sent, with their thread ids.
    pub sent: Vec<(Vec<u8>, Option<String>)>,
    /// Draft id to raw content.
    pub drafts: HashMap<String, Vec<u8>>,
    /// Message id backing each draft.
    pub draft_messages: HashMap<String, String>,
    pub attachments: HashMap<(String, String), Vec<u8>>,
    /// RFC 822 bytes to hand back for a message instead of the ones the
    /// fake builds from its metadata. An export test seeds one to put a
    /// line the mbox writer must quote inside a real message.
    pub raws: HashMap<String, Vec<u8>>,
    pub display_name: Option<String>,
    pub signature: Option<String>,
    /// Extra verified send-as addresses, beyond the account's own.
    pub send_as: Vec<SendAs>,
    pub vacation: Vacation,
    pub filters: Vec<Filter>,
    /// The account's contacts, in the order the People API would list
    /// them. A fake with none answers an empty address book.
    pub contacts: Vec<Person>,
    /// Photo bytes by URL. A URL nobody seeded answers `NotFound`.
    pub photos: HashMap<String, Vec<u8>>,
    /// The events on this account's calendar, by their iCalendar UID, with
    /// the answer this account gave each one. A UID that is not here is on
    /// nobody's calendar and cannot be answered.
    pub calendar: HashMap<String, Option<Answer>>,
    /// What the account already has on, as a start, an end and a title.
    /// An invitation for a time one of these covers clashes with it.
    pub busy: Vec<(EpochMillis, EpochMillis, String)>,
}

/// Calls made and quota units spent, priced from Gmail's usage-limits
/// table. Gmail charges for a call whether or not it succeeds, so failures
/// count here too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub calls: u32,
    pub units: u32,
    /// Calls Gmail turned down for quota, with a 429.
    pub refused: u32,
    /// How long the user's own calls waited for budget. Backfill waiting
    /// costs nobody anything; this is the wait a person sits through.
    pub foreground_wait: Duration,
    /// Calls per Gmail method, such as `messages.batchModify`.
    pub by_method: BTreeMap<&'static str, u32>,
}

impl Usage {
    pub fn calls_to(&self, method: &str) -> u32 {
        self.by_method.get(method).copied().unwrap_or(0)
    }
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

impl Default for FakeGmail {
    fn default() -> Self {
        FakeGmail::new()
    }
}

impl FakeGmail {
    /// An empty mailbox for one account, with Gmail's own labels.
    pub fn new() -> Self {
        let label = |id: &str, kind: &str| RemoteLabel {
            id: id.into(),
            name: id.into(),
            kind: Some(kind.into()),
            color: None,
        };
        FakeGmail {
            quota: None,
            limit: None,
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
                history: Vec::new(),
                bodies: HashMap::new(),
                page_size: 2,
                failures: VecDeque::new(),
                usage: Usage::default(),
                body_fetches: 0,
                remote_writes: Vec::new(),
                sent: Vec::new(),
                drafts: HashMap::new(),
                draft_messages: HashMap::new(),
                attachments: HashMap::new(),
                raws: HashMap::new(),
                display_name: Some("Me".into()),
                signature: None,
                send_as: Vec::new(),
                vacation: Vacation::default(),
                filters: Vec::new(),
                contacts: Vec::new(),
                photos: HashMap::new(),
                calendar: HashMap::new(),
                busy: Vec::new(),
            }),
        }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut FakeState) -> R) -> R {
        f(&mut self.state.lock().expect("fake state poisoned"))
    }

    /// A message that already exists. No history. Whether a listing returns
    /// it follows from its date and labels, as it does in Gmail.
    pub fn seed(&self, meta: MessageMeta) {
        self.with(|s| {
            s.messages.insert(meta.id.clone(), meta);
        });
    }

    /// A message arriving now, recorded in history. Gmail's own filters
    /// archive whatever lands on a muted thread and carry the mute label
    /// over to it, so a message delivered into one arrives that way here.
    pub fn deliver(&self, mut meta: MessageMeta) {
        self.with(|s| {
            if s.thread_is_muted(&meta.thread_id) {
                meta.label_ids.retain(|l| l != system_label::INBOX);
                meta.label_ids.push(system_label::MUTE.into());
            }
            let change = HistoryChange::MessageAdded {
                id: meta.id.clone(),
                thread_id: meta.thread_id.clone(),
            };
            s.messages.insert(meta.id.clone(), meta);
            s.record(change);
        });
    }

    pub fn remote_delete(&self, id: &str) {
        self.with(|s| {
            if let Some(meta) = s.messages.remove(id) {
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

    /// Paces calls through `quota`, the way the real client does, and
    /// answers 429 once Gmail's own budget for the account is spent. A test
    /// that wants to watch the app share one account's budget asks for
    /// this; the rest leave it off and the fake answers at once.
    pub fn under_quota(mut self, quota: std::sync::Arc<AccountQuota>) -> Self {
        self.quota = Some(quota);
        self.limit = Some(QuotaLimiter::gmail_server());
        self
    }

    pub fn fail_next(&self, err: GmailError) {
        self.with(|s| s.failures.push_back(err));
    }

    /// Waits for budget, charges one call to the meter, and hands back the
    /// failure a test queued for it, if any. Every API method starts here,
    /// so the usage counts what the real client would have spent.
    async fn call(&self, method: &'static str, units: u32) -> Result<(), GmailError> {
        let mut waited = Duration::ZERO;
        if let Some(quota) = &self.quota {
            let priority = limiter::priority();
            let started = tokio::time::Instant::now();
            quota.acquire(units, priority).await;
            if priority == Priority::Foreground {
                waited = started.elapsed();
            }
        }
        let refused = self
            .limit
            .as_ref()
            .is_some_and(|limit| !limit.try_acquire(units));
        self.with(|s| {
            s.usage.calls += 1;
            s.usage.units += units;
            s.usage.foreground_wait += waited;
            *s.usage.by_method.entry(method).or_default() += 1;
            if refused {
                s.usage.refused += 1;
                // Gmail hands out a Retry-After of a second on a per-user
                // rate limit, and charges for the call all the same.
                return Err(GmailError::RateLimited {
                    retry_after: Some(std::time::Duration::from_secs(1)),
                });
            }
            s.failures.pop_front().map_or(Ok(()), Err)
        })
    }

    /// Calls and units since the last reset.
    pub fn usage(&self) -> Usage {
        self.with(|s| s.usage.clone())
    }

    /// Starts counting again from zero.
    pub fn reset_usage(&self) {
        self.with(|s| s.usage = Usage::default());
    }
}

impl FakeState {
    fn record(&mut self, change: HistoryChange) {
        self.history_id += 1;
        self.history.push((self.history_id, change));
    }

    /// Whether any message of the thread carries Gmail's mute label.
    fn thread_is_muted(&self, thread_id: &str) -> bool {
        self.messages
            .values()
            .any(|m| m.thread_id == thread_id && m.has_label(system_label::MUTE))
    }

    /// The ids a search returns, newest first.
    fn search(&self, query: &str) -> Vec<String> {
        let query = Query::parse(query);
        let now = crate::now_millis();
        let mut hits: Vec<&MessageMeta> = self
            .messages
            .values()
            .filter(|m| query.matches(m, &self.labels, now))
            .collect();
        hits.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.id.cmp(&b.id)));
        hits.into_iter().map(|m| m.id.clone()).collect()
    }
}

impl GmailApi for FakeGmail {
    fn quota(&self) -> Option<&AccountQuota> {
        self.quota.as_deref()
    }

    async fn profile(&self) -> Result<Profile, GmailError> {
        self.call("users.getProfile", cost::PROFILE).await?;
        Ok(self.with(|s| Profile {
            email_address: s.email.clone(),
            history_id: s.history_id,
        }))
    }

    async fn labels(&self) -> Result<Vec<RemoteLabel>, GmailError> {
        self.call("users.labels.list", cost::LABELS).await?;
        Ok(self.with(|s| s.labels.clone()))
    }

    async fn list_messages(
        &self,
        query: &str,
        page_token: Option<&str>,
    ) -> Result<MessagePage, GmailError> {
        self.call("users.messages.list", cost::LIST).await?;
        let start = match page_token {
            None => 0,
            Some(token) => token.parse::<usize>().map_err(|_| GmailError::Http {
                status: 400,
                body: "Invalid pageToken".into(),
            })?,
        };
        Ok(self.with(|s| {
            let found = s.search(query);
            let end = (start + s.page_size).min(found.len());
            let messages = found[start.min(end)..end]
                .iter()
                .map(|id| MessageRef {
                    id: id.clone(),
                    thread_id: s.messages[id].thread_id.clone(),
                })
                .collect();
            MessagePage {
                messages,
                next_page_token: (end < found.len()).then(|| end.to_string()),
            }
        }))
    }

    async fn message_metadata(&self, id: &str) -> Result<MessageMeta, GmailError> {
        self.call("users.messages.get", cost::GET).await?;
        self.with(|s| s.messages.get(id).cloned().ok_or(GmailError::NotFound))
    }

    async fn thread_metadata(&self, thread_id: &str) -> Result<Vec<MessageMeta>, GmailError> {
        self.call("users.threads.get", cost::THREAD).await?;
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
        self.call("users.messages.get", cost::GET).await?;
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
        self.call("users.history.list", cost::HISTORY).await?;
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
        self.call("users.messages.modify", cost::MODIFY).await?;
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

    async fn batch_modify(
        &self,
        ids: &[String],
        add: &[String],
        remove: &[String],
    ) -> Result<(), GmailError> {
        assert!(
            ids.len() <= BATCH_LIMIT,
            "batch of {} exceeds Gmail's limit of {BATCH_LIMIT}",
            ids.len()
        );
        self.call("users.messages.batchModify", cost::BATCH_MODIFY)
            .await?;
        self.with(|s| {
            s.remote_writes.push(format!(
                "batchModify {} +{} -{}",
                ids.join(","),
                add.join(","),
                remove.join(",")
            ))
        });
        let add: Vec<&str> = add.iter().map(String::as_str).collect();
        let remove: Vec<&str> = remove.iter().map(String::as_str).collect();
        for id in ids {
            self.remote_relabel(id, &add, &remove);
        }
        Ok(())
    }

    async fn trash(&self, id: &str) -> Result<(), GmailError> {
        self.call("users.messages.trash", cost::TRASH).await?;
        self.with(|s| s.remote_writes.push(format!("trash {id}")));
        self.remote_relabel(id, &["TRASH"], &["INBOX"]);
        Ok(())
    }

    async fn untrash(&self, id: &str) -> Result<(), GmailError> {
        self.call("users.messages.untrash", cost::TRASH).await?;
        self.with(|s| s.remote_writes.push(format!("untrash {id}")));
        self.remote_relabel(id, &["INBOX"], &["TRASH"]);
        Ok(())
    }

    async fn delete_messages(&self, ids: &[String]) -> Result<(), GmailError> {
        self.call("users.messages.batchDelete", cost::BATCH_DELETE)
            .await?;
        self.with(|s| s.remote_writes.push(format!("delete {}", ids.join(","))));
        for id in ids {
            self.remote_delete(id);
        }
        Ok(())
    }

    async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<String, GmailError> {
        self.call("users.messages.send", cost::SEND).await?;
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
        match draft_id {
            Some(_) => self.call("users.drafts.update", cost::DRAFT_UPDATE).await?,
            None => self.call("users.drafts.create", cost::DRAFT_CREATE).await?,
        }
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
        self.call("users.drafts.send", cost::SEND).await?;
        self.with(|s| {
            let raw = s.drafts.remove(draft_id).ok_or(GmailError::NotFound)?;
            s.draft_messages.remove(draft_id);
            s.sent.push((raw, None));
            Ok(format!("sent{}", s.sent.len()))
        })
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), GmailError> {
        self.call("users.drafts.delete", cost::DRAFT_DELETE).await?;
        self.with(|s| {
            s.draft_messages.remove(draft_id);
            s.drafts
                .remove(draft_id)
                .map(|_| ())
                .ok_or(GmailError::NotFound)
        })
    }

    async fn draft_for_message(&self, message_id: &str) -> Result<Option<String>, GmailError> {
        self.call("users.drafts.list", cost::DRAFT_LIST).await?;
        Ok(self.with(|s| {
            s.draft_messages
                .iter()
                .find(|(_, m)| *m == message_id)
                .map(|(d, _)| d.clone())
        }))
    }

    async fn send_as(&self) -> Result<Vec<SendAs>, GmailError> {
        self.call("users.settings.sendAs.list", cost::SEND_AS)
            .await?;
        Ok(self.with(|s| {
            let mut all = vec![SendAs {
                send_as_email: s.email.clone(),
                display_name: s.display_name.clone().unwrap_or_default(),
                is_default: true,
                is_primary: true,
                signature: s.signature.clone().unwrap_or_default(),
                verification_status: None,
            }];
            all.extend(s.send_as.iter().cloned());
            all
        }))
    }

    async fn display_name(&self) -> Result<Option<String>, GmailError> {
        self.call("users.settings.sendAs.list", cost::SEND_AS)
            .await?;
        Ok(self.with(|s| s.display_name.clone()))
    }

    async fn attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, GmailError> {
        self.call("users.messages.attachments.get", cost::ATTACHMENT)
            .await?;
        self.with(|s| {
            s.attachments
                .get(&(message_id.to_string(), attachment_id.to_string()))
                .cloned()
                .ok_or(GmailError::NotFound)
        })
    }

    async fn signature(&self) -> Result<Option<String>, GmailError> {
        self.call("users.settings.sendAs.list", cost::SEND_AS)
            .await?;
        Ok(self.with(|s| s.signature.clone()))
    }

    async fn vacation(&self) -> Result<Vacation, GmailError> {
        self.call("users.settings.getVacation", cost::SETTINGS)
            .await?;
        Ok(self.with(|s| s.vacation.clone()))
    }

    async fn set_vacation(&self, vacation: &Vacation) -> Result<(), GmailError> {
        self.call("users.settings.updateVacation", cost::SETTINGS)
            .await?;
        self.with(|s| s.vacation = vacation.clone());
        Ok(())
    }

    async fn answer_invitation(
        &self,
        ical_uid: &str,
        _me: &str,
        answer: Answer,
    ) -> Result<Answered, GmailError> {
        // The Calendar API spends none of the Gmail budget, so this call
        // is priced at nothing and only the failure queue applies.
        self.call("calendar.events.patch", 0).await?;
        Ok(self.with(|s| match s.calendar.get_mut(ical_uid) {
            Some(held) => {
                *held = Some(answer);
                Answered::Done
            }
            None => Answered::NotOnCalendar,
        }))
    }

    async fn busy_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Result<Vec<Busy>, GmailError> {
        self.call("calendar.events.list", 0).await?;
        Ok(self.with(|s| {
            s.busy
                .iter()
                .filter(|(starts, ends, _)| *starts < to && *ends > from)
                .map(|(_, _, summary)| Busy {
                    uid: format!("busy-{summary}"),
                    summary: summary.clone(),
                })
                .collect()
        }))
    }

    async fn create_label(&self, name: &str) -> Result<RemoteLabel, GmailError> {
        self.call("users.labels.create", cost::LABELS).await?;
        Ok(self.with(|s| {
            let label = RemoteLabel {
                id: format!("Label_{}", s.labels.len() + 1),
                name: name.to_string(),
                kind: Some("user".into()),
                color: None,
            };
            s.labels.push(label.clone());
            label
        }))
    }

    async fn rename_label(&self, id: &str, name: &str) -> Result<RemoteLabel, GmailError> {
        self.call("users.labels.patch", cost::LABELS).await?;
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
        self.call("users.labels.delete", cost::LABELS).await?;
        self.with(|s| {
            s.labels.retain(|l| l.id != id);
            for message in s.messages.values_mut() {
                message.label_ids.retain(|l| l != id);
            }
        });
        Ok(())
    }

    async fn filters(&self) -> Result<Vec<Filter>, GmailError> {
        self.call("users.settings.filters.list", cost::SETTINGS)
            .await?;
        Ok(self.with(|s| s.filters.clone()))
    }

    async fn create_filter(&self, filter: &Filter) -> Result<Filter, GmailError> {
        self.call("users.settings.filters.create", cost::SETTINGS)
            .await?;
        Ok(self.with(|s| {
            let created = Filter {
                id: Some(format!("filter{}", s.filters.len() + 1)),
                ..filter.clone()
            };
            s.filters.push(created.clone());
            created
        }))
    }

    async fn delete_filter(&self, id: &str) -> Result<(), GmailError> {
        self.call("users.settings.filters.delete", cost::SETTINGS)
            .await?;
        self.with(|s| {
            let before = s.filters.len();
            s.filters.retain(|f| f.id.as_deref() != Some(id));
            if s.filters.len() == before {
                Err(GmailError::NotFound)
            } else {
                Ok(())
            }
        })
    }

    /// The message as it arrived: the bytes a caller seeded in `raws`, or
    /// ones built from the stored metadata and body, which is enough for
    /// View Source, for a reply to quote, and for an export.
    async fn raw_message(&self, id: &str) -> Result<Vec<u8>, GmailError> {
        self.call("users.messages.get", cost::GET).await?;
        self.with(|s| {
            let meta = s.messages.get(id).ok_or(GmailError::NotFound)?;
            if let Some(raw) = s.raws.get(id) {
                return Ok(raw.clone());
            }
            let text = s
                .bodies
                .get(id)
                .and_then(|b| b.text.clone())
                .unwrap_or_else(|| meta.snippet.clone());
            let from = meta.from.as_ref().map(|a| a.email.as_str()).unwrap_or("");
            let date = chrono::DateTime::from_timestamp_millis(meta.date)
                .unwrap_or_default()
                .to_rfc2822();
            Ok(format!(
                "From: {from}\r\nDate: {date}\r\nSubject: {}\r\nMessage-ID: {}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n",
                meta.subject,
                meta.rfc822_msgid.clone().unwrap_or_default(),
                text.replace('\n', "\r\n")
            )
            .into_bytes())
        })
    }

    async fn set_label_color(
        &self,
        id: &str,
        color: &LabelColor,
    ) -> Result<RemoteLabel, GmailError> {
        self.call("users.labels.patch", cost::LABELS).await?;
        self.with(|s| {
            let label = s
                .labels
                .iter_mut()
                .find(|l| l.id == id)
                .ok_or(GmailError::NotFound)?;
            label.color = Some(color.clone());
            Ok(label.clone())
        })
    }

    /// Contacts a page at a time, with the page token counting from zero.
    /// A sync token means nothing changed, so the reply is empty and
    /// carries the same token back.
    async fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<ConnectionsPage, GmailError> {
        self.call("people.connections.list", cost::CONNECTIONS)
            .await?;
        if let Some(token) = sync_token {
            return Ok(ConnectionsPage {
                next_sync_token: Some(token.to_string()),
                ..ConnectionsPage::default()
            });
        }
        let from: usize = page_token.and_then(|t| t.parse().ok()).unwrap_or(0);
        self.with(|s| {
            let page: Vec<Person> = s
                .contacts
                .iter()
                .skip(from)
                .take(s.page_size)
                .cloned()
                .collect();
            let next = from + page.len();
            let more = next < s.contacts.len();
            Ok(ConnectionsPage {
                people: page,
                deleted: Vec::new(),
                next_page_token: more.then(|| next.to_string()),
                next_sync_token: (!more).then(|| format!("sync-{}", s.contacts.len())),
            })
        })
    }

    async fn contact_photo(&self, url: &str) -> Result<Vec<u8>, GmailError> {
        self.with(|s| s.photos.get(url).cloned())
            .ok_or(GmailError::NotFound)
    }
}
