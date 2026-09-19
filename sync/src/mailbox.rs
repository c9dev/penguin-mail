//! What a mailbox shows: its rows, its unread count, and what an empty list
//! says. The window and the assistant both read mail through this module, so
//! only one place knows which mailboxes come from the local store and which
//! from a Gmail search, that categories narrow an inbox, and how Follow Up,
//! Remind Me, and Send Later build their rows.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Local, TimeZone};
use mailrs_domain::{
    Account, AccountId, Category, EpochMillis, FlagColor, Folder, MessageMeta, SmartMailbox,
    ThreadSummary, system_label,
};
use mailrs_store::threads::ThreadFilter;
use mailrs_store::{Db, flags, follow_ups, reminders, scheduled, threads};

use crate::{Accounts, SyncError};

/// Rows in one page of a stored mailbox.
pub const PAGE: usize = 500;

/// Rows a Gmail search brings back for a folder or a smart mailbox.
const REMOTE_LIMIT: usize = 100;

/// Rows a Gmail search brings back for what the user typed.
const SEARCH_LIMIT: usize = 50;

const DAY: EpochMillis = 24 * 60 * 60 * 1000;

/// What the thread list shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Mailbox {
    /// One system label across every account.
    Unified(&'static str),
    Label {
        account_id: AccountId,
        label_id: String,
        name: String,
    },
    Search {
        query: String,
        account_id: Option<AccountId>,
    },
    /// Messages waiting to go out at a set time, across all accounts.
    Scheduled,
    /// Conversations set aside with Remind Me, across all accounts.
    Reminders,
    /// Sent mail nobody has answered for a few days, across all accounts.
    FollowUp,
    /// Flagged mail of one colour, across all accounts.
    Flag(FlagColor),
    /// Mail from VIPs: all of them under "VIPs", or one person.
    Vips { emails: Vec<String>, name: String },
    /// A saved search from Preferences' smart mailboxes.
    Smart(SmartMailbox),
    /// Mail Gmail keeps out of the regular listing, fetched on demand.
    Folder {
        account_id: Option<AccountId>,
        folder: Folder,
    },
}

impl Mailbox {
    pub fn title(&self) -> String {
        match self {
            Mailbox::Unified(label) => unified_name(label).into(),
            Mailbox::Label { name, .. } => name.clone(),
            Mailbox::Search { .. } => "Search".into(),
            Mailbox::Folder { folder, .. } => folder_name(*folder).into(),
            Mailbox::Scheduled => "Send Later".into(),
            Mailbox::Reminders => "Remind Me".into(),
            Mailbox::FollowUp => "Follow Up".into(),
            Mailbox::Flag(color) => format!("{} Flag", color.name()),
            Mailbox::Vips { name, .. } => name.clone(),
            Mailbox::Smart(smart) => smart.name.clone(),
        }
    }

    /// The one account this mailbox belongs to, or `None` for a unified one.
    pub fn account(&self) -> Option<AccountId> {
        match self {
            Mailbox::Unified(_)
            | Mailbox::Scheduled
            | Mailbox::Reminders
            | Mailbox::FollowUp
            | Mailbox::Flag(_)
            | Mailbox::Vips { .. }
            | Mailbox::Smart(_) => None,
            Mailbox::Label { account_id, .. } => Some(*account_id),
            Mailbox::Search { account_id, .. } | Mailbox::Folder { account_id, .. } => *account_id,
        }
    }

    pub fn folder(&self) -> Option<Folder> {
        match self {
            Mailbox::Folder { folder, .. } => Some(*folder),
            _ => None,
        }
    }

    /// Unread counts matter for inboxes; drafts show how many there are.
    pub fn counts_unread(&self) -> bool {
        matches!(
            self,
            Mailbox::Unified(system_label::INBOX) | Mailbox::Vips { .. }
        ) || matches!(self, Mailbox::Label { label_id, .. } if label_id == system_label::INBOX)
    }

    /// Whether this mailbox splits into inbox categories.
    pub fn takes_categories(&self) -> bool {
        match self {
            Mailbox::Unified(label) => *label == system_label::INBOX,
            Mailbox::Label { label_id, .. } => label_id == system_label::INBOX,
            _ => false,
        }
    }

    /// Whether the rows come from a Gmail search rather than the store.
    pub fn is_remote(&self) -> bool {
        matches!(
            self,
            Mailbox::Search { .. } | Mailbox::Folder { .. } | Mailbox::Smart(_)
        )
    }

    /// What an empty list says: a title and an icon.
    pub fn empty(&self) -> Empty {
        let empty = |title, icon| Empty { title, icon };
        let label = match self {
            Mailbox::Unified(label) => *label,
            Mailbox::Label { label_id, .. } => label_id.as_str(),
            Mailbox::Search { .. } => return empty("No Results", "system-search-symbolic"),
            Mailbox::Scheduled => return empty("Nothing Scheduled", "mail-send-symbolic"),
            Mailbox::Reminders => return empty("No Reminders", "alarm-symbolic"),
            Mailbox::FollowUp => return empty("No Follow-Ups", "mail-reply-sender-symbolic"),
            Mailbox::Flag(_) => return empty("No Flagged Mail", "penguin-mail-flag-symbolic"),
            Mailbox::Vips { .. } => return empty("No Mail from VIPs", "starred-symbolic"),
            Mailbox::Smart(_) => return empty("No Matching Mail", "folder-saved-search-symbolic"),
            Mailbox::Folder { folder, .. } => {
                let icon = folder_icon(*folder);
                return match folder {
                    Folder::Junk => empty("No Junk", icon),
                    Folder::Trash => empty("Trash Is Empty", icon),
                    Folder::AllMail => empty("No Mail", icon),
                };
            }
        };
        match label {
            system_label::INBOX => empty("Inbox Zero", "penguin-mail-inbox-symbolic"),
            system_label::STARRED => empty("No Starred Mail", "starred-symbolic"),
            system_label::SENT => empty("No Sent Mail", "mail-send-symbolic"),
            system_label::DRAFT => empty("No Drafts", "document-edit-symbolic"),
            _ => empty("No Mail", "penguin-mail-tag-symbolic"),
        }
    }

    /// The store query behind this mailbox, before categories narrow it.
    /// `None` for the mailboxes the store cannot list.
    fn filter(&self) -> Option<ThreadFilter> {
        match self {
            Mailbox::Unified(label) => Some(ThreadFilter::unified(*label)),
            Mailbox::Label {
                account_id,
                label_id,
                ..
            } => Some(ThreadFilter::account(*account_id, label_id.clone())),
            Mailbox::Flag(color) => Some(ThreadFilter::unified("").with_flag(*color)),
            Mailbox::Vips { emails, .. } => {
                Some(ThreadFilter::unified("").from_senders(emails.clone()))
            }
            Mailbox::Search { .. }
            | Mailbox::Folder { .. }
            | Mailbox::Scheduled
            | Mailbox::Reminders
            | Mailbox::FollowUp
            | Mailbox::Smart(_) => None,
        }
    }
}

/// Names the sidebar and the list header use for a unified mailbox.
pub fn unified_name(label: &str) -> &'static str {
    match label {
        system_label::INBOX => "All Inboxes",
        system_label::STARRED => "Flagged",
        system_label::SENT => "Sent",
        system_label::DRAFT => "Drafts",
        _ => "Mail",
    }
}

pub fn folder_name(folder: Folder) -> &'static str {
    match folder {
        Folder::Junk => "Junk",
        Folder::Trash => "Trash",
        Folder::AllMail => "All Mail",
    }
}

pub fn folder_icon(folder: Folder) -> &'static str {
    match folder {
        Folder::Junk => "mail-mark-junk-symbolic",
        Folder::Trash => "user-trash-symbolic",
        Folder::AllMail => "penguin-mail-archive-symbolic",
    }
}

/// What an empty list shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Empty {
    pub title: &'static str,
    pub icon: &'static str,
}

impl Default for Empty {
    fn default() -> Self {
        Empty {
            title: "No Mail",
            icon: "penguin-mail-inbox-symbolic",
        }
    }
}

/// The accounts a listing may read, in the order the sidebar shows them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope(Vec<Account>);

impl Scope {
    pub fn over(accounts: impl IntoIterator<Item = Account>) -> Scope {
        Scope(accounts.into_iter().collect())
    }

    /// The account with this address, ignoring case.
    pub fn id_of(&self, email: &str) -> Option<AccountId> {
        self.0
            .iter()
            .find(|a| a.email.eq_ignore_ascii_case(email))
            .map(|a| a.id)
    }

    /// The accounts to search: `only` alone, or every one in scope.
    fn searched(&self, only: Option<AccountId>) -> Vec<&Account> {
        self.0
            .iter()
            .filter(|a| only.is_none_or(|id| a.id == id))
            .collect()
    }
}

/// The settings that change what a mailbox lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// Group a thread's messages into one row.
    pub threading: bool,
    /// The slice of an inbox to show, or `None` when categories are off.
    /// Mailboxes that are not inboxes ignore it.
    pub category: Option<Category>,
    /// Count and list sent mail nobody has answered.
    pub follow_ups: bool,
    /// The clock the rows read against, in milliseconds since the epoch.
    pub now: EpochMillis,
    /// Most rows to return. `None` asks for one page of a stored mailbox,
    /// or as much as a Gmail search brings back.
    pub limit: Option<usize>,
}

impl Default for View {
    fn default() -> Self {
        View {
            threading: true,
            category: None,
            follow_ups: true,
            now: 0,
            limit: None,
        }
    }
}

impl View {
    fn local_now(&self) -> DateTime<Local> {
        Local
            .timestamp_millis_opt(self.now)
            .single()
            .unwrap_or_else(Local::now)
    }
}

/// One page of a mailbox, with everything the list header needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    pub rows: Vec<ThreadSummary>,
    /// Unread rows in the whole mailbox, not only in this page.
    pub unread: i64,
    pub title: String,
    pub subtitle: String,
    pub empty: Empty,
    /// More rows follow this page.
    pub more: bool,
    /// Lines to tell the user: an account whose search failed, or a smart
    /// mailbox nobody has given conditions yet.
    pub notices: Vec<String>,
}

/// What the sidebar and the category switcher show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    pub mailboxes: HashMap<Mailbox, i64>,
    /// Unread mail in each category of the inbox on screen.
    pub categories: HashMap<Category, i64>,
}

/// Reads mailboxes. One instance serves the window and the assistant.
pub struct Mailboxes<A: Accounts> {
    accounts: Arc<A>,
    db: Db,
}

impl<A: Accounts> Mailboxes<A> {
    pub fn new(accounts: Arc<A>, db: Db) -> Self {
        Mailboxes { accounts, db }
    }

    /// One page of `mailbox`, skipping the `from` rows the caller holds.
    /// Mailboxes that come from a Gmail search answer the first page only.
    pub async fn list(
        &self,
        mailbox: &Mailbox,
        scope: &Scope,
        view: &View,
        from: usize,
    ) -> Result<Listing, SyncError> {
        let base = Listing {
            title: mailbox.title(),
            empty: mailbox.empty(),
            ..Listing::default()
        };
        match mailbox {
            Mailbox::Folder { account_id, folder } => {
                self.remote(folder.query(), *account_id, scope, view, base)
                    .await
            }
            Mailbox::Search { query, account_id } if from == 0 => {
                let limit = view.limit.unwrap_or(SEARCH_LIMIT);
                let found = self
                    .remote(
                        query,
                        *account_id,
                        scope,
                        &View {
                            limit: Some(limit),
                            ..view.clone()
                        },
                        base,
                    )
                    .await?;
                Ok(Listing {
                    subtitle: query.clone(),
                    ..found
                })
            }
            Mailbox::Smart(smart) if from == 0 => {
                let Some(query) = smart.query() else {
                    return Ok(Listing {
                        notices: vec!["This smart mailbox has no conditions".into()],
                        ..base
                    });
                };
                let only = smart.account.as_deref().and_then(|e| scope.id_of(e));
                self.remote(&query, only, scope, view, base).await
            }
            _ if from > 0 && mailbox.is_remote() => Ok(base),
            Mailbox::Scheduled => self.scheduled(view, base, from).await,
            Mailbox::Reminders => self.reminders(view, base, from).await,
            Mailbox::FollowUp => self.follow_ups(view, base, from).await,
            _ => self.stored(mailbox, view, base, from).await,
        }
    }

    /// Counts for every sidebar mailbox, plus the categories of the inbox
    /// on screen. Two grouped queries cover the labels and the flags, so
    /// this costs a handful of queries rather than one per mailbox.
    pub async fn counts(
        &self,
        sidebar: &[Mailbox],
        shown: &Mailbox,
        view: &View,
    ) -> Result<Counts, SyncError> {
        let sidebar = sidebar.to_vec();
        let category_base = view
            .category
            .is_some()
            .then(|| shown.filter())
            .flatten()
            .filter(|_| shown.takes_categories());
        let (follow_ups, now, threading) = (view.follow_ups, view.now, view.threading);
        Ok(self
            .db
            .read(move |c| {
                let labels = threads::label_counts(c)?;
                let flagged = flags::mailbox_counts(c)?;
                let waiting = if follow_ups {
                    follow_ups::waiting(c, now)?.len() as i64
                } else {
                    0
                };
                let scheduled = scheduled::list(c)?.len() as i64;
                let reminders = reminders::list(c)?.len() as i64;
                let mut mailboxes = HashMap::new();
                mailboxes.insert(Mailbox::FollowUp, waiting);
                mailboxes.insert(Mailbox::Scheduled, scheduled);
                mailboxes.insert(Mailbox::Reminders, reminders);
                for mailbox in sidebar {
                    let count = match &mailbox {
                        Mailbox::Unified(label) => {
                            let count = labels.unified(label);
                            if mailbox.counts_unread() {
                                count.unread
                            } else {
                                count.threads
                            }
                        }
                        Mailbox::Label {
                            account_id,
                            label_id,
                            ..
                        } => {
                            let count = labels.account(*account_id, label_id);
                            if mailbox.counts_unread() {
                                count.unread
                            } else {
                                count.threads
                            }
                        }
                        Mailbox::Flag(color) => flagged.get(color).copied().unwrap_or(0),
                        // VIP mail is found by sender, which no grouped
                        // query covers; there are only a few of these.
                        Mailbox::Vips { .. } => match mailbox.filter() {
                            Some(filter) => threads::unread_threads(c, &filter)?,
                            None => continue,
                        },
                        _ => continue,
                    };
                    mailboxes.insert(mailbox, count);
                }
                let categories = match category_base {
                    Some(filter) if threading => threads::category_unread_threads(c, &filter)?,
                    Some(filter) => threads::category_unread_messages(c, &filter)?,
                    None => HashMap::new(),
                };
                Ok(Counts {
                    mailboxes,
                    categories,
                })
            })
            .await?)
    }

    /// The rows for the threads a change event named, as they are now.
    /// A thread that left the mailbox is simply missing from the answer, so
    /// a caller drops every row of a named thread before splicing these in.
    /// `None` when the mailbox has to be listed again from scratch.
    pub async fn changed(
        &self,
        mailbox: &Mailbox,
        changed: &[(AccountId, String)],
        view: &View,
    ) -> Result<Option<Vec<ThreadSummary>>, SyncError> {
        let Some(filter) = self.filter_of(mailbox, view) else {
            return Ok(None);
        };
        if changed.is_empty() {
            return Ok(None);
        }
        let mut per_account: HashMap<AccountId, Vec<String>> = HashMap::new();
        for (account_id, thread_id) in changed {
            per_account
                .entry(*account_id)
                .or_default()
                .push(thread_id.clone());
        }
        let threading = view.threading;
        Ok(Some(
            self.db
                .read(move |c| {
                    let mut rows = Vec::new();
                    for (account_id, thread_ids) in per_account {
                        let limit = thread_ids.len() as i64;
                        let narrowed = ThreadFilter {
                            account_id: Some(account_id),
                            ..filter.clone()
                        }
                        .with_threads(thread_ids);
                        rows.extend(if threading {
                            threads::list_threads(c, &narrowed, 0, limit)?
                        } else {
                            // A thread can hold many messages, so ask for
                            // more rows than threads.
                            threads::list_messages(c, &narrowed, 0, limit * 100)?
                        });
                    }
                    Ok(rows)
                })
                .await?,
        ))
    }

    /// The store query for `mailbox`, narrowed to the chosen category when
    /// the mailbox is an inbox and categories are on.
    fn filter_of(&self, mailbox: &Mailbox, view: &View) -> Option<ThreadFilter> {
        let filter = mailbox.filter()?;
        match view.category.filter(|_| mailbox.takes_categories()) {
            Some(category) => {
                let (any, none) = category.labels();
                Some(filter.with_labels(any, none))
            }
            None => Some(filter),
        }
    }

    async fn stored(
        &self,
        mailbox: &Mailbox,
        view: &View,
        base: Listing,
        from: usize,
    ) -> Result<Listing, SyncError> {
        let Some(filter) = self.filter_of(mailbox, view) else {
            return Ok(base);
        };
        let limit = view.limit.unwrap_or(PAGE);
        let (threading, offset) = (view.threading, from as i64);
        // One row past the page says whether another page follows.
        let asked = limit as i64 + 1;
        let (mut rows, unread) = self
            .db
            .read(move |c| {
                let rows = if threading {
                    threads::list_threads(c, &filter, offset, asked)?
                } else {
                    threads::list_messages(c, &filter, offset, asked)?
                };
                let unread = match (offset, threading) {
                    (0, true) => threads::unread_threads(c, &filter)?,
                    (0, false) => threads::unread_messages(c, &filter)?,
                    _ => 0,
                };
                Ok((rows, unread))
            })
            .await?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        Ok(Listing {
            rows,
            unread,
            subtitle: if from == 0 && unread > 0 {
                format!("{unread} unread")
            } else {
                String::new()
            },
            more,
            ..base
        })
    }

    /// Lists a Gmail search across the accounts in scope, one row per
    /// conversation when threading is on.
    async fn remote(
        &self,
        query: &str,
        only: Option<AccountId>,
        scope: &Scope,
        view: &View,
        base: Listing,
    ) -> Result<Listing, SyncError> {
        let limit = view.limit.unwrap_or(REMOTE_LIMIT);
        let targets = scope.searched(only);
        let searches = targets.iter().map(|account| {
            let sync = self.accounts.account(account.id);
            let query = query.to_string();
            async move {
                match sync {
                    Some(sync) => sync.search(&query, limit).await,
                    None => Ok(Vec::new()),
                }
            }
        });
        let results = futures::future::join_all(searches).await;
        let (mut hits, mut notices) = (Vec::new(), Vec::new());
        for (account, result) in targets.iter().zip(results) {
            match result {
                Ok(found) => hits.extend(found),
                Err(err) => {
                    notices.push(format!("Could not load mail for {}: {err}", account.email))
                }
            }
        }
        let mut rows = summarize_search(hits, view.threading);
        rows.truncate(limit);
        Ok(Listing {
            rows,
            notices,
            ..base
        })
    }

    async fn scheduled(
        &self,
        view: &View,
        base: Listing,
        from: usize,
    ) -> Result<Listing, SyncError> {
        if from > 0 {
            return Ok(base);
        }
        let items = self.db.read(scheduled::list).await?;
        let now = view.local_now();
        let rows: Vec<ThreadSummary> = items
            .iter()
            .map(|item| ThreadSummary {
                account_id: item.account_id,
                id: item.thread_id.clone(),
                message_id: Some(item.message_id.clone()),
                last_message_at: item.send_at,
                subject: item.subject.clone(),
                snippet: format!("Sends {}", future_date(item.send_at, now)),
                from: if item.recipients.is_empty() {
                    "No recipients".into()
                } else {
                    format!("To {}", item.recipients)
                },
                message_count: 1,
                ..ThreadSummary::default()
            })
            .collect();
        let subtitle = match rows.len() {
            0 => String::new(),
            1 => "1 message".into(),
            n => format!("{n} messages"),
        };
        Ok(Listing {
            rows,
            subtitle,
            ..base
        })
    }

    async fn reminders(
        &self,
        view: &View,
        base: Listing,
        from: usize,
    ) -> Result<Listing, SyncError> {
        if from > 0 {
            return Ok(base);
        }
        let waiting = self
            .db
            .read(|c| {
                let mut rows = Vec::new();
                for item in reminders::list(c)? {
                    let stored = threads::get_thread(c, item.account_id, &item.thread_id)?;
                    rows.push((item, stored));
                }
                Ok(rows)
            })
            .await?;
        let now = view.local_now();
        let rows = waiting
            .into_iter()
            .map(|(item, stored)| {
                let mut row = stored.unwrap_or_else(|| ThreadSummary {
                    account_id: item.account_id,
                    id: item.thread_id.clone(),
                    subject: item.subject.clone(),
                    message_count: 1,
                    ..ThreadSummary::default()
                });
                row.snippet = format!("Returns {}", future_date(item.remind_at, now));
                row.last_message_at = item.remind_at;
                row
            })
            .collect();
        Ok(Listing { rows, ..base })
    }

    async fn follow_ups(
        &self,
        view: &View,
        base: Listing,
        from: usize,
    ) -> Result<Listing, SyncError> {
        if from > 0 || !view.follow_ups {
            return Ok(base);
        }
        let now = view.now;
        let waiting = self
            .db
            .read(move |c| {
                let mut rows = Vec::new();
                for item in follow_ups::waiting(c, now)? {
                    let stored = threads::get_thread(c, item.account_id, &item.thread_id)?;
                    rows.push((item, stored));
                }
                Ok(rows)
            })
            .await?;
        let rows: Vec<ThreadSummary> = waiting
            .into_iter()
            .map(|(item, stored)| {
                let mut row = stored.unwrap_or_else(|| ThreadSummary {
                    account_id: item.account_id,
                    id: item.thread_id.clone(),
                    subject: item.subject.clone(),
                    message_count: 1,
                    ..ThreadSummary::default()
                });
                let names: Vec<&str> = item.to.iter().map(|a| a.display()).collect();
                row.from = if names.is_empty() {
                    "No recipients".into()
                } else {
                    format!("To {}", names.join(", "))
                };
                row.snippet = waited(now - item.sent_at);
                row.last_message_at = item.sent_at;
                row
            })
            .collect();
        let subtitle = match rows.len() {
            0 => String::new(),
            1 => "1 conversation".into(),
            n => format!("{n} conversations"),
        };
        Ok(Listing {
            rows,
            subtitle,
            ..base
        })
    }
}

/// "Sent 5 days ago, no reply yet".
fn waited(elapsed: EpochMillis) -> String {
    match elapsed / DAY {
        1 => "Sent yesterday, no reply yet".into(),
        days => format!("Sent {days} days ago, no reply yet"),
    }
}

/// When a scheduled message goes out or a reminder returns: "today at
/// 21:00", "tomorrow at 08:00", "Monday at 08:00" within the week, then
/// "Tue 3 Nov at 08:00".
pub fn future_date(ts: EpochMillis, now: DateTime<Local>) -> String {
    use chrono::Datelike;
    let Some(when) = Local.timestamp_millis_opt(ts).single() else {
        return String::new();
    };
    match (when.date_naive() - now.date_naive()).num_days() {
        ..=0 => when.format("today at %H:%M").to_string(),
        1 => when.format("tomorrow at %H:%M").to_string(),
        2..=6 => when.format("%A at %H:%M").to_string(),
        _ if when.year() == now.year() => when.format("%a %-d %b at %H:%M").to_string(),
        _ => when.format("%-d %b %Y at %H:%M").to_string(),
    }
}

/// Turns search hits into list rows, newest first: one per thread when
/// `grouped`, one per message otherwise.
pub fn summarize_search(mut hits: Vec<MessageMeta>, grouped: bool) -> Vec<ThreadSummary> {
    hits.sort_by_key(|m| std::cmp::Reverse(m.date));
    if !grouped {
        return hits.into_iter().map(|hit| row_of(&hit, true)).collect();
    }
    let mut rows: Vec<(ThreadSummary, EpochMillis)> = Vec::new();
    for hit in hits {
        if let Some((row, oldest)) = rows
            .iter_mut()
            .find(|(r, _)| r.account_id == hit.account_id && r.id == hit.thread_id)
        {
            row.message_count += 1;
            row.unread |= hit.is_unread();
            row.starred |= hit.has_label(system_label::STARRED);
            row.has_attachments |= hit.has_attachments;
            if hit.date <= *oldest {
                *oldest = hit.date;
                row.subject = hit.subject.clone();
            }
            continue;
        }
        let date = hit.date;
        rows.push((row_of(&hit, false), date));
    }
    rows.into_iter().map(|(row, _)| row).collect()
}

/// One search hit as a list row. With `alone` the row stands for the one
/// message rather than for its whole thread.
fn row_of(hit: &MessageMeta, alone: bool) -> ThreadSummary {
    ThreadSummary {
        account_id: hit.account_id,
        id: hit.thread_id.clone(),
        message_id: alone.then(|| hit.id.clone()),
        last_message_at: hit.date,
        subject: hit.subject.clone(),
        snippet: hit.snippet.clone(),
        from: hit
            .from
            .as_ref()
            .map(|a| a.display().to_string())
            .unwrap_or_default(),
        message_count: 1,
        unread: hit.is_unread(),
        starred: hit.has_label(system_label::STARRED),
        has_attachments: hit.has_attachments,
        flag_color: None,
        from_email: hit
            .from
            .as_ref()
            .map(|a| a.email.clone())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use mailrs_domain::{Address, MessageMeta};

    use super::{summarize_search, waited};

    const DAY: i64 = 24 * 60 * 60 * 1000;

    fn hit(id: &str, thread: &str, date: i64, subject: &str, labels: &[&str]) -> MessageMeta {
        MessageMeta {
            account_id: 1,
            id: id.into(),
            thread_id: thread.into(),
            rfc822_msgid: None,
            from: Some(Address {
                name: Some(format!("Sender {id}")),
                email: format!("{id}@example.com"),
            }),
            to: vec![],
            cc: vec![],
            subject: subject.into(),
            date,
            snippet: format!("snippet {id}"),
            size: 0,
            has_attachments: false,
            label_ids: labels.iter().map(|l| l.to_string()).collect(),
        }
    }

    #[test]
    fn hits_group_by_thread_newest_first() {
        let rows = summarize_search(
            vec![
                hit("a1", "ta", 100, "Plans", &[]),
                hit("b1", "tb", 300, "Other", &["STARRED"]),
                hit("a2", "ta", 200, "Re: Plans", &["UNREAD"]),
            ],
            true,
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "tb");
        assert!(rows[0].starred);
        let plans = &rows[1];
        assert_eq!(
            (
                plans.message_count,
                plans.subject.as_str(),
                plans.from.as_str()
            ),
            (2, "Plans", "Sender a2")
        );
        assert!(plans.unread);
        assert_eq!(plans.last_message_at, 200);
    }

    #[test]
    fn ungrouped_hits_stay_separate() {
        let rows = summarize_search(
            vec![
                hit("a1", "ta", 100, "Plans", &[]),
                hit("a2", "ta", 200, "Re: Plans", &[]),
            ],
            false,
        );
        let ids: Vec<Option<&str>> = rows.iter().map(|r| r.message_id.as_deref()).collect();
        assert_eq!(ids, [Some("a2"), Some("a1")]);
        assert_eq!(rows[0].subject, "Re: Plans");
    }

    #[test]
    fn the_wait_reads_in_whole_days() {
        assert_eq!(waited(5 * DAY + 3), "Sent 5 days ago, no reply yet");
        assert_eq!(waited(DAY), "Sent yesterday, no reply yet");
    }
}
