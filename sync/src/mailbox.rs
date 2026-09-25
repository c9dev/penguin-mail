//! What a mailbox shows: its rows, its unread count, and what an empty list
//! says. The window and the assistant both read mail through this module, so
//! only one place knows which mailboxes come from the local store and which
//! from a Gmail search, that categories narrow an inbox, and how Follow Up,
//! Remind Me, Send Later, and the Outbox build their rows.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, TimeZone};
use mailrs_domain::translate::{date_locale, fill, fill_plural, gettext, pgettext};
use mailrs_domain::{
    Account, AccountId, Category, EpochMillis, FlagColor, Folder, MailSet, MessageMeta, Role,
    SmartMailbox, ThreadSummary,
};
use mailrs_store::threads::ThreadFilter;
use mailrs_store::{Db, flags, follow_ups, outbox, reminders, threads};

use crate::{Accounts, SearchQuery, SyncError};

/// Rows in one page of a stored mailbox.
pub const PAGE: usize = 500;

/// Rows a Gmail search brings back for a folder or a smart mailbox.
const REMOTE_LIMIT: usize = 100;

/// Rows a Gmail search brings back for what the user typed.
const SEARCH_LIMIT: usize = 50;

/// Rows of a Gmail search whose metadata is fetched at once. Gmail charges
/// 5 units a message for it, so a folder pays for the screenful it shows
/// and leaves the rest until the reader scrolls.
const REMOTE_PAGE: usize = 25;

/// How long a Gmail search answers again without asking Gmail. The engine
/// polls every 30 seconds, so a folder is at most one poll behind.
const REMOTE_FRESH: Duration = Duration::from_secs(60);

const DAY: EpochMillis = 24 * 60 * 60 * 1000;

/// Inbox, Flagged, Sent, Drafts or Muted: the five mailboxes the sidebar
/// shows for all accounts together and for each one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Standard {
    Inbox,
    Flagged,
    Sent,
    Drafts,
    Muted,
}

impl Standard {
    /// In the order the sidebar lists them.
    pub const ALL: [Standard; 5] = [
        Standard::Inbox,
        Standard::Flagged,
        Standard::Sent,
        Standard::Drafts,
        Standard::Muted,
    ];

    /// The stored mail this mailbox lists.
    pub fn set(self) -> MailSet {
        match self {
            Standard::Inbox => MailSet::Role(Role::Inbox),
            Standard::Flagged => MailSet::flagged(),
            Standard::Sent => MailSet::Role(Role::Sent),
            Standard::Drafts => MailSet::Role(Role::Drafts),
            Standard::Muted => MailSet::muted(),
        }
    }

    /// The name under one account.
    pub fn name(self) -> String {
        match self {
            Standard::Inbox => gettext("Inbox"),
            Standard::Flagged => gettext("Flagged"),
            Standard::Sent => gettext("Sent"),
            Standard::Drafts => gettext("Drafts"),
            Standard::Muted => gettext("Muted"),
        }
    }

    /// The name for all accounts together.
    pub fn unified_name(self) -> String {
        match self {
            Standard::Inbox => gettext("All Inboxes"),
            other => other.name(),
        }
    }

    /// The icon its sidebar row shows.
    pub fn icon(self) -> &'static str {
        match self {
            Standard::Inbox => "penguin-mail-inbox-symbolic",
            Standard::Flagged => "penguin-mail-flag-symbolic",
            Standard::Sent => "mail-send-symbolic",
            Standard::Drafts => "document-edit-symbolic",
            Standard::Muted => "audio-volume-muted-symbolic",
        }
    }

    /// The word the CLI takes for it.
    pub fn key(self) -> &'static str {
        match self {
            Standard::Inbox => "inbox",
            Standard::Flagged => "flagged",
            Standard::Sent => "sent",
            Standard::Drafts => "drafts",
            Standard::Muted => "muted",
        }
    }

    /// The mailbox `key` names, ignoring case, with the words Gmail used
    /// for the same mailboxes, so `INBOX` and `starred` still work.
    pub fn from_key(key: &str) -> Option<Standard> {
        match key.to_ascii_lowercase().as_str() {
            "inbox" => Some(Standard::Inbox),
            "flagged" | "starred" => Some(Standard::Flagged),
            "sent" => Some(Standard::Sent),
            "drafts" | "draft" => Some(Standard::Drafts),
            "muted" | "mute" => Some(Standard::Muted),
            _ => None,
        }
    }
}

/// What the thread list shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Mailbox {
    /// One standard mailbox across every account.
    Unified(Standard),
    /// One standard mailbox of one account.
    Standard {
        account_id: AccountId,
        which: Standard,
    },
    /// A person's own label or folder in one account.
    Label {
        account_id: AccountId,
        label_id: String,
        name: String,
    },
    /// Mail in one set of one account that has no sidebar row of its own,
    /// such as unread mail or one inbox category. The assistant reaches
    /// these by the name the server gives them.
    Set {
        account_id: AccountId,
        set: MailSet,
        name: String,
    },
    Search {
        query: String,
        account_id: Option<AccountId>,
    },
    /// Messages waiting to go out at a set time, across all accounts.
    Scheduled,
    /// Messages that could not go out and are waiting here to be tried
    /// again, across all accounts.
    Outbox,
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
            Mailbox::Unified(which) => which.unified_name(),
            Mailbox::Standard { which, .. } => which.name(),
            Mailbox::Label { name, .. } | Mailbox::Set { name, .. } => name.clone(),
            Mailbox::Search { .. } => gettext("Search"),
            Mailbox::Folder { folder, .. } => folder_name(*folder),
            Mailbox::Scheduled => gettext("Send Later"),
            Mailbox::Outbox => gettext("Outbox"),
            Mailbox::Reminders => gettext("Remind Me"),
            Mailbox::FollowUp => gettext("Follow Up"),
            Mailbox::Flag(color) => fill(&gettext("{color} Flag"), &[("color", &color.name())]),
            Mailbox::Vips { name, .. } => name.clone(),
            Mailbox::Smart(smart) => smart.name.clone(),
        }
    }

    /// The one account this mailbox belongs to, or `None` for a unified one.
    pub fn account(&self) -> Option<AccountId> {
        match self {
            Mailbox::Unified(_)
            | Mailbox::Scheduled
            | Mailbox::Outbox
            | Mailbox::Reminders
            | Mailbox::FollowUp
            | Mailbox::Flag(_)
            | Mailbox::Vips { .. }
            | Mailbox::Smart(_) => None,
            Mailbox::Standard { account_id, .. }
            | Mailbox::Label { account_id, .. }
            | Mailbox::Set { account_id, .. } => Some(*account_id),
            Mailbox::Search { account_id, .. } | Mailbox::Folder { account_id, .. } => *account_id,
        }
    }

    pub fn folder(&self) -> Option<Folder> {
        match self {
            Mailbox::Folder { folder, .. } => Some(*folder),
            _ => None,
        }
    }

    /// The standard mailbox this is, for all accounts or for one.
    pub fn standard(&self) -> Option<Standard> {
        match self {
            Mailbox::Unified(which) | Mailbox::Standard { which, .. } => Some(*which),
            _ => None,
        }
    }

    /// Unread counts matter for inboxes; drafts show how many there are.
    pub fn counts_unread(&self) -> bool {
        self.standard() == Some(Standard::Inbox) || matches!(self, Mailbox::Vips { .. })
    }

    /// Whether this mailbox splits into inbox categories.
    pub fn takes_categories(&self) -> bool {
        self.standard() == Some(Standard::Inbox)
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
        let empty = |title: String, icon| Empty { title, icon };
        match self {
            Mailbox::Unified(_)
            | Mailbox::Standard { .. }
            | Mailbox::Label { .. }
            | Mailbox::Set { .. } => {}
            Mailbox::Search { .. } => {
                return empty(gettext("No Results"), "system-search-symbolic");
            }
            Mailbox::Scheduled => {
                return empty(gettext("Nothing Scheduled"), "mail-send-symbolic");
            }
            Mailbox::Outbox => {
                return empty(gettext("Outbox Is Empty"), "penguin-mail-outbox-symbolic");
            }
            Mailbox::Reminders => return empty(gettext("No Reminders"), "alarm-symbolic"),
            Mailbox::FollowUp => {
                return empty(gettext("No Follow-Ups"), "mail-reply-sender-symbolic");
            }
            Mailbox::Flag(_) => {
                return empty(gettext("No Flagged Mail"), "penguin-mail-flag-symbolic");
            }
            Mailbox::Vips { .. } => return empty(gettext("No Mail from VIPs"), "starred-symbolic"),
            Mailbox::Smart(_) => {
                return empty(gettext("No Matching Mail"), "folder-saved-search-symbolic");
            }
            Mailbox::Folder { folder, .. } => {
                let icon = folder_icon(*folder);
                return match folder {
                    Folder::Archive => empty(gettext("No Archived Mail"), icon),
                    Folder::Junk => empty(gettext("No Junk"), icon),
                    Folder::Trash => empty(gettext("Trash Is Empty"), icon),
                    Folder::AllMail => empty(gettext("No Mail"), icon),
                };
            }
        }
        match self.standard() {
            Some(Standard::Inbox) => empty(gettext("Inbox Zero"), "penguin-mail-inbox-symbolic"),
            Some(Standard::Flagged) => empty(gettext("No Starred Mail"), "starred-symbolic"),
            Some(Standard::Sent) => empty(gettext("No Sent Mail"), "mail-send-symbolic"),
            Some(Standard::Drafts) => empty(gettext("No Drafts"), "document-edit-symbolic"),
            Some(Standard::Muted) => empty(gettext("No Muted Mail"), "audio-volume-muted-symbolic"),
            None => empty(gettext("No Mail"), "penguin-mail-tag-symbolic"),
        }
    }

    /// The store query behind this mailbox, before categories narrow it.
    /// `None` for the mailboxes the store cannot list.
    fn filter(&self) -> Option<ThreadFilter> {
        match self {
            Mailbox::Unified(which) => Some(ThreadFilter::unified(which.set())),
            Mailbox::Standard { account_id, which } => {
                Some(ThreadFilter::account(*account_id, which.set()))
            }
            Mailbox::Label {
                account_id,
                label_id,
                ..
            } => Some(ThreadFilter::account(*account_id, MailSet::Mailbox(label_id.clone()))),
            Mailbox::Set {
                account_id, set, ..
            } => Some(ThreadFilter::account(*account_id, set.clone())),
            Mailbox::Flag(color) => Some(ThreadFilter::everything().with_flag(*color)),
            Mailbox::Vips { emails, .. } => {
                Some(ThreadFilter::everything().from_senders(emails.clone()))
            }
            Mailbox::Search { .. }
            | Mailbox::Folder { .. }
            | Mailbox::Scheduled
            | Mailbox::Outbox
            | Mailbox::Reminders
            | Mailbox::FollowUp
            | Mailbox::Smart(_) => None,
        }
    }
}

pub fn folder_name(folder: Folder) -> String {
    match folder {
        Folder::Archive => pgettext("mailbox", "Archive"),
        Folder::Junk => gettext("Junk"),
        Folder::Trash => gettext("Trash"),
        Folder::AllMail => gettext("All Mail"),
    }
}

pub fn folder_icon(folder: Folder) -> &'static str {
    match folder {
        Folder::Archive => "penguin-mail-archive-symbolic",
        Folder::Junk => "mail-mark-junk-symbolic",
        Folder::Trash => "user-trash-symbolic",
        Folder::AllMail => "penguin-mail-all-mail-symbolic",
    }
}

/// What an empty list shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Empty {
    pub title: String,
    pub icon: &'static str,
}

impl Default for Empty {
    fn default() -> Self {
        Empty {
            title: gettext("No Mail"),
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

/// The rows of a mailbox a caller already holds, so that a listing answers
/// with the page after them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Loaded {
    /// How many rows. A Gmail search and the short lists of Send Later,
    /// the Outbox, Remind Me and Follow Up skip this many.
    pub count: usize,
    /// The last row. A stored mailbox starts its page after this row
    /// rather than after `count` rows, so mail that arrives or leaves
    /// while the reader scrolls neither shows a row twice nor skips one.
    pub last: Option<ThreadSummary>,
}

impl Loaded {
    /// No rows yet: the listing answers with the first page.
    pub fn nothing() -> Loaded {
        Loaded::default()
    }

    /// The rows a list shows, in the order the mailbox gave them.
    pub fn rows<R: Borrow<ThreadSummary>>(rows: &[R]) -> Loaded {
        Loaded {
            count: rows.len(),
            last: rows.last().map(|row| row.borrow().clone()),
        }
    }
}

/// The rows a change event touched, ready to splice into a list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changed {
    /// The named threads' rows, for the ones that still belong here.
    pub rows: Vec<ThreadSummary>,
    /// Unread rows in the whole mailbox.
    pub unread: i64,
    /// The header's subtitle, now that the count moved.
    pub subtitle: String,
}

/// What the sidebar and the category switcher show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    pub mailboxes: HashMap<Mailbox, i64>,
    /// Unread mail in each category of the inbox on screen.
    pub categories: HashMap<Category, i64>,
}

/// What one Gmail search returned, so the folder it fills does not ask
/// again on every refresh. Gmail hands back every id in one 5-unit call
/// but charges 5 units for each message's metadata, so the ids are kept
/// and the metadata is fetched a screen at a time.
struct RemoteListing {
    query: SearchQuery,
    accounts: Vec<AccountId>,
    at: Instant,
    /// Per account: the ids the search returned, newest first, and the
    /// metadata fetched so far.
    pages: Vec<RemotePage>,
    notices: Vec<String>,
}

#[derive(Default)]
struct RemotePage {
    /// Whether the search has run for this account yet.
    listed: bool,
    /// The messages the search returned, with their threads.
    ids: Vec<crate::RemoteRef>,
    metas: Vec<MessageMeta>,
    /// Ids whose metadata has been asked for, from the front of `ids`.
    fetched: usize,
}

impl RemoteListing {
    /// Whether this is still the search the caller wants, and recent
    /// enough to answer from.
    fn answers(&self, query: &SearchQuery, accounts: &[AccountId]) -> bool {
        self.query == *query && self.accounts == accounts && self.at.elapsed() < REMOTE_FRESH
    }

    /// Whether every account has metadata for its first `wanted` ids, or
    /// has no more ids to fetch.
    fn complete(&self, wanted: usize) -> bool {
        !self.pages.is_empty() && self.pages.iter().all(|p| p.done() || p.fetched >= wanted)
    }
}

impl RemotePage {
    /// Whether Gmail has no more ids to fetch metadata for.
    fn done(&self) -> bool {
        self.listed && self.fetched >= self.ids.len()
    }
}

/// Reads mailboxes. One instance serves the window and the assistant.
pub struct Mailboxes<A: Accounts> {
    accounts: Arc<A>,
    db: Db,
    /// The last Gmail search, kept so a folder that reloads a second later
    /// costs nothing.
    remote: Mutex<Option<RemoteListing>>,
}

impl<A: Accounts> Mailboxes<A> {
    pub fn new(accounts: Arc<A>, db: Db) -> Self {
        Mailboxes {
            accounts,
            db,
            remote: Mutex::new(None),
        }
    }

    /// Throws away the kept Gmail search, so the next listing asks Gmail
    /// again. The window calls this when the reader asks for a refresh.
    pub fn forget_remote(&self) {
        *self.remote.lock().expect("remote listing poisoned") = None;
    }

    /// The page of `mailbox` that follows the rows the caller holds.
    pub async fn list(
        &self,
        mailbox: &Mailbox,
        scope: &Scope,
        view: &View,
        held: Loaded,
    ) -> Result<Listing, SyncError> {
        let from = held.count;
        let base = Listing {
            title: mailbox.title(),
            empty: mailbox.empty(),
            ..Listing::default()
        };
        match mailbox {
            Mailbox::Folder { account_id, folder } => {
                let query = SearchQuery::Tree(folder.query());
                self.remote(&query, *account_id, scope, view, base, from)
                    .await
            }
            Mailbox::Search { query, account_id } => {
                let limit = view.limit.unwrap_or(SEARCH_LIMIT);
                let found = self
                    .remote(
                        &SearchQuery::Native(query.clone()),
                        *account_id,
                        scope,
                        &View {
                            limit: Some(limit),
                            ..view.clone()
                        },
                        base,
                        from,
                    )
                    .await?;
                Ok(Listing {
                    subtitle: query.clone(),
                    ..found
                })
            }
            Mailbox::Smart(smart) => {
                let Some(query) = smart.query() else {
                    return Ok(Listing {
                        notices: vec![gettext("This smart mailbox has no conditions")],
                        ..base
                    });
                };
                let only = smart.account.as_deref().and_then(|e| scope.id_of(e));
                let query = SearchQuery::Tree(query);
                self.remote(&query, only, scope, view, base, from).await
            }
            Mailbox::Scheduled => self.scheduled(view, base, from).await,
            Mailbox::Outbox => self.outbox(view, base, from).await,
            Mailbox::Reminders => self.reminders(view, base, from).await,
            Mailbox::FollowUp => self.follow_ups(view, base, from).await,
            _ => self.stored(mailbox, view, base, held.last).await,
        }
    }

    /// Counts for every sidebar mailbox, plus the categories of the inbox
    /// on screen. Grouped queries cover the labels, the flags and the VIPs,
    /// so this costs a handful of queries rather than one per mailbox.
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
                let labels = threads::mail_counts(c)?;
                let flagged = flags::mailbox_counts(c)?;
                let waiting = if follow_ups {
                    follow_ups::waiting_count(c, now)?
                } else {
                    0
                };
                let (scheduled, stuck) = outbox::counts(c)?;
                let reminders = reminders::count(c)?;
                let vips: Vec<String> = sidebar
                    .iter()
                    .filter_map(|m| match m {
                        Mailbox::Vips { emails, .. } => Some(emails.iter().cloned()),
                        _ => None,
                    })
                    .flatten()
                    .collect();
                let from_vips = threads::sender_counts(c, &vips)?;
                let mut mailboxes = HashMap::new();
                mailboxes.insert(Mailbox::FollowUp, waiting);
                mailboxes.insert(Mailbox::Scheduled, scheduled);
                mailboxes.insert(Mailbox::Outbox, stuck);
                mailboxes.insert(Mailbox::Reminders, reminders);
                for mailbox in sidebar {
                    let count = match &mailbox {
                        Mailbox::Unified(which) => {
                            let count = labels.unified(&which.set());
                            if mailbox.counts_unread() {
                                count.unread
                            } else {
                                count.threads
                            }
                        }
                        Mailbox::Standard { account_id, which } => {
                            let count = labels.account(*account_id, &which.set());
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
                            let count = labels
                                .account(*account_id, &MailSet::Mailbox(label_id.clone()));
                            if mailbox.counts_unread() {
                                count.unread
                            } else {
                                count.threads
                            }
                        }
                        Mailbox::Flag(color) => flagged.get(color).copied().unwrap_or(0),
                        Mailbox::Vips { emails, .. } => from_vips.unread(emails),
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

    /// The rows for the threads a change event named, as they are now, with
    /// the mailbox's unread count so the header keeps up. A thread that left
    /// the mailbox is simply missing from `rows`, so a caller drops every row
    /// of a named thread before splicing these in. `None` when the mailbox
    /// has to be listed again from scratch.
    pub async fn changed(
        &self,
        mailbox: &Mailbox,
        changed: &[(AccountId, String)],
        view: &View,
    ) -> Result<Option<Changed>, SyncError> {
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
                        let narrowed = filter
                            .clone()
                            .in_account(account_id)
                            .with_threads(thread_ids);
                        rows.extend(if threading {
                            threads::list_threads(c, &narrowed, 0, limit)?
                        } else {
                            // A thread can hold many messages, so ask for
                            // more rows than threads.
                            threads::list_messages(c, &narrowed, 0, limit * 100)?
                        });
                    }
                    let unread = if threading {
                        threads::unread_threads(c, &filter)?
                    } else {
                        threads::unread_messages(c, &filter)?
                    };
                    Ok(Changed {
                        rows,
                        unread,
                        subtitle: unread_subtitle(unread),
                    })
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
                let (any, none) = category.categories();
                Some(filter.with_categories(any, none))
            }
            None => Some(filter),
        }
    }

    /// A mailbox the store holds, from the row after `last`, or from the
    /// top when there is none. The page is read from where `last` sits in
    /// the order, so a deep page costs what the first one does.
    async fn stored(
        &self,
        mailbox: &Mailbox,
        view: &View,
        base: Listing,
        last: Option<ThreadSummary>,
    ) -> Result<Listing, SyncError> {
        let Some(filter) = self.filter_of(mailbox, view) else {
            return Ok(base);
        };
        let limit = view.limit.unwrap_or(PAGE);
        let (threading, first) = (view.threading, last.is_none());
        // One row past the page says whether another page follows.
        let asked = limit as i64 + 1;
        let (mut rows, unread) = self
            .db
            .read(move |c| {
                let rows = if threading {
                    threads::list_threads_after(c, &filter, last.as_ref(), asked)?
                } else {
                    threads::list_messages_after(c, &filter, last.as_ref(), asked)?
                };
                let unread = match (first, threading) {
                    (true, true) => threads::unread_threads(c, &filter)?,
                    (true, false) => threads::unread_messages(c, &filter)?,
                    (false, _) => 0,
                };
                Ok((rows, unread))
            })
            .await?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        Ok(Listing {
            rows,
            unread,
            subtitle: if first {
                unread_subtitle(unread)
            } else {
                String::new()
            },
            more,
            ..base
        })
    }

    /// Lists a Gmail search across the accounts in scope, one row per
    /// conversation when threading is on, skipping the `from` rows the
    /// caller already has.
    ///
    /// The ids come from one 5-unit call per account and are kept for
    /// [`REMOTE_FRESH`]; metadata costs 5 units a message, so it is fetched
    /// [`REMOTE_PAGE`] rows at a time as the reader scrolls. Opening Junk
    /// with 100 messages in it costs 130 units rather than 505, and opening
    /// it again a moment later costs nothing.
    async fn remote(
        &self,
        query: &SearchQuery,
        only: Option<AccountId>,
        scope: &Scope,
        view: &View,
        base: Listing,
        from: usize,
    ) -> Result<Listing, SyncError> {
        let limit = view.limit.unwrap_or(REMOTE_LIMIT);
        let targets = scope.searched(only);
        let ids: Vec<AccountId> = targets.iter().map(|a| a.id).collect();

        let kept = {
            let mut held = self.remote.lock().expect("remote listing poisoned");
            match held.take() {
                Some(listing) if listing.answers(query, &ids) => Some(listing),
                _ => None,
            }
        };
        let mut listing = kept.unwrap_or_else(|| RemoteListing {
            query: query.clone(),
            accounts: ids,
            at: Instant::now(),
            pages: Vec::new(),
            notices: Vec::new(),
        });

        // Grouping messages into conversations can leave a page with fewer
        // rows than it fetched messages, so keep asking until the page the
        // reader wants has rows in it or Gmail has no more ids.
        let mut wanted = (from + REMOTE_PAGE).min(limit);
        let mut rows = Self::rows(&listing, view, limit);
        while rows.len() <= from && !listing.complete(wanted) {
            self.fetch_page(&mut listing, &targets, limit, wanted).await;
            rows = Self::rows(&listing, view, limit);
            wanted = (wanted + REMOTE_PAGE).min(limit);
            if wanted >= limit && listing.complete(wanted) {
                break;
            }
        }

        let more = rows.len() < limit && !listing.complete(limit);
        let notices = listing.notices.clone();
        *self.remote.lock().expect("remote listing poisoned") = Some(listing);
        Ok(Listing {
            rows: rows.split_off(from.min(rows.len())),
            notices,
            more,
            ..base
        })
    }

    /// Fetches metadata up to the `wanted`th id of every account, listing
    /// the ids first for an account that has none yet. An account whose
    /// server did not run the search says so once, when it is listed.
    async fn fetch_page(
        &self,
        listing: &mut RemoteListing,
        targets: &[&Account],
        limit: usize,
        wanted: usize,
    ) {
        if listing.pages.is_empty() {
            listing.pages = targets.iter().map(|_| RemotePage::default()).collect();
        }
        let loads = targets
            .iter()
            .zip(listing.pages.drain(..))
            .map(|(account, mut page)| {
                let sync = self.accounts.account(account.id);
                let query = listing.query.clone();
                async move {
                    let Some(sync) = sync else {
                        return Ok((page, false));
                    };
                    let mut store_only = false;
                    if !page.listed {
                        let found = sync.search_listing(&query, limit).await?;
                        page.ids = found.refs;
                        store_only = found.store_only;
                        page.listed = true;
                    }
                    let take = wanted.min(page.ids.len());
                    if take > page.fetched {
                        let next = sync.metadata_of(&page.ids[page.fetched..take]).await?;
                        page.metas.extend(next);
                        page.fetched = take;
                    }
                    Ok((page, store_only))
                }
            });
        let results: Vec<Result<(RemotePage, bool), SyncError>> =
            futures::future::join_all(loads).await;
        for (account, result) in targets.iter().zip(results) {
            match result {
                Ok((page, store_only)) => {
                    if store_only {
                        listing.notices.push(fill(
                            &gettext(
                                "Results for {account} come from the mail on this computer alone",
                            ),
                            &[("account", &account.email)],
                        ));
                    }
                    listing.pages.push(page);
                }
                Err(err) => {
                    listing.notices.push(fill(
                        &gettext("Could not load mail for {account}: {reason}"),
                        &[("account", &account.email), ("reason", &err.to_string())],
                    ));
                    listing.pages.push(RemotePage::default());
                }
            }
        }
    }

    /// The rows of a kept search, merged across its accounts and newest
    /// first.
    fn rows(listing: &RemoteListing, view: &View, limit: usize) -> Vec<ThreadSummary> {
        let hits: Vec<MessageMeta> = listing
            .pages
            .iter()
            .flat_map(|p| p.metas.iter().cloned())
            .collect();
        let mut rows = summarize_search(hits, view.threading);
        rows.truncate(limit);
        rows
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
        let items = self.db.read(outbox::scheduled).await?;
        let now = view.local_now();
        let rows: Vec<ThreadSummary> = items
            .iter()
            .map(|item| ThreadSummary {
                account_id: item.account_id,
                id: item
                    .thread_id
                    .clone()
                    .unwrap_or_else(|| outbox_row(item.id)),
                message_id: item.message_id.clone(),
                last_message_at: item.send_at,
                subject: item.subject.clone(),
                snippet: waiting_line(item, now),
                from: recipients_of(item),
                message_count: 1,
                ..ThreadSummary::default()
            })
            .collect();
        Ok(Listing {
            subtitle: counted(rows.len()),
            rows,
            ..base
        })
    }

    /// The Outbox: what could not go out, why, and when the next try is.
    /// A row is named after its own place in the table rather than after a
    /// Gmail thread, because a message that never reached Gmail has none.
    async fn outbox(&self, view: &View, base: Listing, from: usize) -> Result<Listing, SyncError> {
        if from > 0 {
            return Ok(base);
        }
        let items = self.db.read(outbox::stuck).await?;
        let now = view.local_now();
        let rows: Vec<ThreadSummary> = items
            .iter()
            .map(|item| ThreadSummary {
                account_id: item.account_id,
                id: outbox_row(item.id),
                last_message_at: item.send_at,
                subject: item.subject.clone(),
                snippet: waiting_line(item, now),
                from: recipients_of(item),
                message_count: 1,
                ..ThreadSummary::default()
            })
            .collect();
        Ok(Listing {
            subtitle: counted(rows.len()),
            rows,
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
                row.snippet = fill(
                    &gettext("Returns {when}"),
                    &[("when", &future_date(item.remind_at, now))],
                );
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
                    gettext("No recipients")
                } else {
                    fill(
                        &gettext("To {recipients}"),
                        &[("recipients", &names.join(", "))],
                    )
                };
                row.snippet = waited(now - item.sent_at);
                row.last_message_at = item.sent_at;
                row
            })
            .collect();
        let subtitle = match rows.len() {
            0 => String::new(),
            count => fill_plural(
                "{count} conversation",
                "{count} conversations",
                count,
                &[("count", &count.to_string())],
            ),
        };
        Ok(Listing {
            rows,
            subtitle,
            ..base
        })
    }
}

/// "3 unread", or nothing when everything has been read.
fn unread_subtitle(unread: i64) -> String {
    if unread <= 0 {
        return String::new();
    }
    let count = unread.max(0) as usize;
    fill_plural(
        "{count} unread",
        "{count} unread",
        count,
        &[("count", &count.to_string())],
    )
}

/// "Sent 5 days ago, no reply yet".
fn waited(elapsed: EpochMillis) -> String {
    match elapsed / DAY {
        1 => gettext("Sent yesterday, no reply yet"),
        days => fill_plural(
            "Sent {days} day ago, no reply yet",
            "Sent {days} days ago, no reply yet",
            days.max(0) as usize,
            &[("days", &days.to_string())],
        ),
    }
}

/// What an Outbox row is named, since a message that never reached Gmail
/// has no thread to be named after. The window reads its place in the
/// table back out with [`outbox_id`].
pub fn outbox_row(id: i64) -> String {
    format!("outbox:{id}")
}

/// The message an Outbox row stands for, or `None` for an ordinary row.
pub fn outbox_id(row_id: &str) -> Option<i64> {
    row_id.strip_prefix("outbox:")?.parse().ok()
}

/// Who a waiting message goes to, as its row shows it.
fn recipients_of(message: &outbox::Queued) -> String {
    if message.recipients.is_empty() {
        return gettext("No recipients");
    }
    fill(
        &gettext("To {recipients}"),
        &[("recipients", &message.recipients)],
    )
}

/// What a queued message's row says under the subject, and what the
/// conversation pane says above the message. A Send Later message says
/// when it goes; one in the Outbox says why it has not gone and when the
/// next try is.
pub fn waiting_line(message: &outbox::Queued, now: DateTime<Local>) -> String {
    let Some(problem) = message.problem.as_deref() else {
        return fill(
            &gettext("Sends {when}"),
            &[("when", &future_date(message.send_at, now))],
        );
    };
    let unsent = gettext("Not sent");
    let problem = match problem.trim_end_matches(['.', ' ']) {
        "" => unsent.as_str(),
        said => said,
    };
    match crate::backoff::retry_delay(message.attempts) {
        Some(_) => fill(
            &gettext("{problem}. Trying again {when}"),
            &[
                ("problem", problem),
                ("when", &future_date(message.send_at, now)),
            ],
        ),
        None => fill(
            &gettext("{problem}. Penguin Mail stopped trying"),
            &[("problem", problem)],
        ),
    }
}

/// How many messages a waiting list holds, for its header.
fn counted(rows: usize) -> String {
    match rows {
        0 => String::new(),
        count => fill_plural(
            "{count} message",
            "{count} messages",
            count,
            &[("count", &count.to_string())],
        ),
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
    // The patterns are translated whole, so a language that puts the
    // time first can, and the names come in the interface's language.
    let pattern = match (when.date_naive() - now.date_naive()).num_days() {
        ..=0 => gettext("today at %H:%M"),
        1 => gettext("tomorrow at %H:%M"),
        2..=6 => gettext("%A at %H:%M"),
        _ if when.year() == now.year() => gettext("%a %-d %b at %H:%M"),
        _ => gettext("%-d %b %Y at %H:%M"),
    };
    when.format_localized(&pattern, date_locale()).to_string()
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
            row.starred |= hit.is_flagged();
            row.has_attachments |= hit.has_attachments;
            row.muted |= hit.is_muted();
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
        starred: hit.is_flagged(),
        has_attachments: hit.has_attachments,
        muted: hit.is_muted(),
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
        let mut meta = MessageMeta {
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
            held: Default::default(),
            roles: vec![],
            list_unsubscribe: None,
            one_click: false,
        };
        let labels: Vec<String> = labels.iter().map(|l| l.to_string()).collect();
        mailrs_gmail::labels::set_label_ids(&mut meta, &labels);
        meta
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
