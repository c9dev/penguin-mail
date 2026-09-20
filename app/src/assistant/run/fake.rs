//! Penguin Mail with no window: an in-memory store, a fake Gmail behind the
//! modules, and fake adapters in front of both ports. Building one costs a
//! tempdir and a tokio runtime, so the whole tool loop runs under
//! `cargo test`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use mailrs_domain::{
    Account, AccountId, AccountState, Address, Category, ChangeEvent, EpochMillis, Filter, Label,
    LabelKind, MessageBody, MessageMeta, ThreadSummary, Vacation,
};
use mailrs_gmail::{
    GmailError, HistoryPage, LabelColor, MessagePage, MessageRef, Profile, RemoteLabel,
};
use mailrs_store::{Db, accounts, labels, messages};
use mailrs_sync::{
    AccountSettings, AccountSync, Accounts, GmailApi, MailAction, MailActions, Mailboxes, Outcome,
    Permitted, SavedDraft, View,
};
use serde_json::Value;

use super::{Answer, Background, Desk, Effects, Modules, OnScreen, Tools};
use crate::compose::Draft;
use crate::hide_my_email::HiddenAddress;
use crate::settings::{Change, Settings};

/// The address every fixture account belongs to.
pub const ME: &str = "dana@example.com";

// ---- Gmail ---------------------------------------------------------------

/// One account's Gmail, in memory.
pub struct Gmail(Mutex<Inbox>);

#[derive(Default)]
pub struct Inbox {
    pub messages: Vec<MessageMeta>,
    pub bodies: HashMap<String, MessageBody>,
    pub labels: Vec<RemoteLabel>,
    pub filters: Vec<Filter>,
    pub vacation: Vacation,
    /// What Gmail was asked to change, oldest first.
    pub writes: Vec<String>,
    /// False until the account grants the settings permission, as Gmail
    /// behaves before the user says yes.
    pub settings_allowed: bool,
    next_id: u32,
}

impl Gmail {
    pub fn new() -> Gmail {
        Gmail(Mutex::new(Inbox {
            settings_allowed: true,
            ..Inbox::default()
        }))
    }

    pub fn with<R>(&self, change: impl FnOnce(&mut Inbox) -> R) -> R {
        change(
            &mut self
                .0
                .lock()
                .expect("the fake inbox lock is never poisoned"),
        )
    }

    /// Gmail's answer before the account grants the settings permission.
    fn allowed(&self) -> Result<(), GmailError> {
        match self.with(|i| i.settings_allowed) {
            true => Ok(()),
            false => Err(GmailError::MissingScope),
        }
    }

    fn mint(&self, prefix: &str) -> String {
        self.with(|i| {
            i.next_id += 1;
            format!("{prefix}{}", i.next_id)
        })
    }
}

/// Matches Gmail's query language far enough for the tests: `in:x` and
/// `label:x` match a label, anything else matches the subject or the sender.
fn matches(message: &MessageMeta, query: &str) -> bool {
    query.split_whitespace().all(|word| {
        let word = word.to_lowercase();
        match word.split_once(':') {
            Some(("in" | "label", name)) => message
                .label_ids
                .iter()
                .any(|l| l.eq_ignore_ascii_case(name)),
            _ => {
                message.subject.to_lowercase().contains(&word)
                    || message
                        .from
                        .as_ref()
                        .is_some_and(|a| a.email.to_lowercase().contains(&word))
            }
        }
    })
}

impl GmailApi for Gmail {
    async fn profile(&self) -> Result<Profile, GmailError> {
        Ok(Profile {
            email_address: ME.into(),
            history_id: 1,
        })
    }

    async fn labels(&self) -> Result<Vec<RemoteLabel>, GmailError> {
        Ok(self.with(|i| i.labels.clone()))
    }

    async fn list_messages(
        &self,
        query: &str,
        _page_token: Option<&str>,
    ) -> Result<MessagePage, GmailError> {
        let messages = self.with(|i| {
            i.messages
                .iter()
                .filter(|m| matches(m, query))
                .map(|m| MessageRef {
                    id: m.id.clone(),
                    thread_id: m.thread_id.clone(),
                })
                .collect()
        });
        Ok(MessagePage {
            messages,
            next_page_token: None,
        })
    }

    async fn message_metadata(&self, id: &str) -> Result<MessageMeta, GmailError> {
        self.with(|i| i.messages.iter().find(|m| m.id == id).cloned())
            .ok_or(GmailError::NotFound)
    }

    async fn thread_metadata(&self, thread_id: &str) -> Result<Vec<MessageMeta>, GmailError> {
        Ok(self.with(|i| {
            i.messages
                .iter()
                .filter(|m| m.thread_id == thread_id)
                .cloned()
                .collect()
        }))
    }

    async fn message_body(&self, id: &str) -> Result<MessageBody, GmailError> {
        self.with(|i| i.bodies.get(id).cloned())
            .ok_or(GmailError::NotFound)
    }

    async fn history(
        &self,
        _start: u64,
        _page_token: Option<&str>,
    ) -> Result<HistoryPage, GmailError> {
        Ok(HistoryPage {
            changes: vec![],
            next_page_token: None,
            history_id: 1,
        })
    }

    async fn modify_labels(
        &self,
        id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), GmailError> {
        self.with(|i| {
            i.writes.push(format!(
                "modify {id} +{} -{}",
                add.join(","),
                remove.join(",")
            ));
            if let Some(message) = i.messages.iter_mut().find(|m| m.id == id) {
                message.label_ids.retain(|l| !remove.contains(l));
                for label in add {
                    if !message.label_ids.contains(label) {
                        message.label_ids.push(label.clone());
                    }
                }
            }
        });
        Ok(())
    }

    async fn batch_modify(
        &self,
        ids: &[String],
        add: &[String],
        remove: &[String],
    ) -> Result<(), GmailError> {
        for id in ids {
            self.modify_labels(id, add, remove).await?;
        }
        Ok(())
    }

    async fn trash(&self, id: &str) -> Result<(), GmailError> {
        self.with(|i| i.writes.push(format!("trash {id}")));
        Ok(())
    }

    async fn untrash(&self, id: &str) -> Result<(), GmailError> {
        self.with(|i| i.writes.push(format!("untrash {id}")));
        Ok(())
    }

    /// The assistant has no way to erase mail, so a call here is a bug.
    async fn delete_messages(&self, ids: &[String]) -> Result<(), GmailError> {
        panic!("the assistant asked Gmail to erase {ids:?}");
    }

    async fn send(&self, _raw: &[u8], _thread_id: Option<&str>) -> Result<String, GmailError> {
        Ok(self.mint("sent"))
    }

    async fn save_draft(
        &self,
        _draft_id: Option<&str>,
        _raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SavedDraft, GmailError> {
        Ok(SavedDraft {
            draft_id: self.mint("draft"),
            message_id: self.mint("message"),
            thread_id: thread_id.unwrap_or("thread").to_string(),
        })
    }

    async fn send_draft(&self, _draft_id: &str) -> Result<String, GmailError> {
        Ok(self.mint("sent"))
    }

    async fn delete_draft(&self, _draft_id: &str) -> Result<(), GmailError> {
        Ok(())
    }

    async fn draft_for_message(&self, _message_id: &str) -> Result<Option<String>, GmailError> {
        Ok(None)
    }

    async fn display_name(&self) -> Result<Option<String>, GmailError> {
        Ok(Some("Dana".into()))
    }

    async fn attachment(&self, _message: &str, _id: &str) -> Result<Vec<u8>, GmailError> {
        Ok(Vec::new())
    }

    async fn raw_message(&self, _id: &str) -> Result<Vec<u8>, GmailError> {
        Ok(Vec::new())
    }

    async fn filters(&self) -> Result<Vec<Filter>, GmailError> {
        self.allowed()?;
        Ok(self.with(|i| i.filters.clone()))
    }

    async fn create_filter(&self, filter: &Filter) -> Result<Filter, GmailError> {
        self.allowed()?;
        let created = Filter {
            id: Some(self.mint("filter")),
            ..filter.clone()
        };
        self.with(|i| i.filters.push(created.clone()));
        Ok(created)
    }

    async fn delete_filter(&self, id: &str) -> Result<(), GmailError> {
        self.allowed()?;
        self.with(|i| i.filters.retain(|f| f.id.as_deref() != Some(id)));
        Ok(())
    }

    async fn create_label(&self, name: &str) -> Result<RemoteLabel, GmailError> {
        self.allowed()?;
        let label = RemoteLabel {
            id: self.mint("Label_"),
            name: name.to_string(),
            kind: Some("user".into()),
            color: None,
        };
        self.with(|i| i.labels.push(label.clone()));
        Ok(label)
    }

    async fn rename_label(&self, id: &str, name: &str) -> Result<RemoteLabel, GmailError> {
        self.allowed()?;
        Ok(RemoteLabel {
            id: id.to_string(),
            name: name.to_string(),
            kind: Some("user".into()),
            color: None,
        })
    }

    async fn delete_label(&self, id: &str) -> Result<(), GmailError> {
        self.allowed()?;
        self.with(|i| i.labels.retain(|l| l.id != id));
        Ok(())
    }

    async fn set_label_color(
        &self,
        id: &str,
        color: &LabelColor,
    ) -> Result<RemoteLabel, GmailError> {
        self.allowed()?;
        Ok(RemoteLabel {
            id: id.to_string(),
            name: "Label".into(),
            kind: Some("user".into()),
            color: Some(color.clone()),
        })
    }

    async fn signature(&self) -> Result<Option<String>, GmailError> {
        Ok(None)
    }

    async fn vacation(&self) -> Result<Vacation, GmailError> {
        self.allowed()?;
        Ok(self.with(|i| i.vacation.clone()))
    }

    async fn set_vacation(&self, vacation: &Vacation) -> Result<(), GmailError> {
        self.allowed()?;
        self.with(|i| i.vacation = vacation.clone());
        Ok(())
    }
}

/// The accounts a test connects, by id.
pub struct Connected(HashMap<AccountId, Arc<AccountSync<Gmail>>>);

impl Accounts for Connected {
    type Api = Gmail;

    fn account(&self, account_id: AccountId) -> Option<Arc<AccountSync<Gmail>>> {
        self.0.get(&account_id).cloned()
    }
}

// ---- The ports -----------------------------------------------------------

/// What the fake window has on screen. A test writes to it directly.
pub struct Screen {
    pub settings: Settings,
    pub accounts: Vec<Account>,
    pub labels: HashMap<AccountId, Vec<Label>>,
    pub view: View,
    pub on_screen: OnScreen,
    pub default_account: Option<AccountId>,
}

pub struct FakeDesk(pub RefCell<Screen>);

impl Desk for FakeDesk {
    fn settings(&self) -> Settings {
        self.0.borrow().settings.clone()
    }

    fn accounts(&self) -> Vec<Account> {
        self.0.borrow().accounts.clone()
    }

    fn labels(&self) -> HashMap<AccountId, Vec<Label>> {
        self.0.borrow().labels.clone()
    }

    fn view(&self) -> View {
        self.0.borrow().view.clone()
    }

    fn on_screen(&self) -> OnScreen {
        self.0.borrow().on_screen.clone()
    }

    fn default_account(&self) -> Option<AccountId> {
        self.0.borrow().default_account
    }
}

/// Every effect the tools asked for, and the answers waiting for them.
#[derive(Default)]
pub struct Asked {
    /// The approval questions, oldest first.
    pub questions: Vec<String>,
    /// What the user answers next.
    pub approves: bool,
    pub changes: Vec<Change>,
    pub composed: Vec<Draft>,
    pub sent: Vec<Draft>,
    pub opened: Vec<ThreadSummary>,
    pub copied: Vec<String>,
    /// Accounts offered the Gmail settings permission.
    pub permission_asked: Vec<AccountId>,
    pub mail_changed: Vec<(MailAction, Outcome)>,
    pub relisted: usize,
    pub categorized: Vec<(AccountId, String, String, Category)>,
    /// Hide My Email: what the next call gives back, then what was asked.
    pub hidden: Option<Permitted<HiddenAddress>>,
    pub hide_asked: Vec<(AccountId, String)>,
    pub activated: Vec<(String, bool)>,
}

pub struct FakeEffects(pub RefCell<Asked>);

impl Effects for FakeEffects {
    fn confirm(&self, question: String) -> Answer<'_, bool> {
        let mut asked = self.0.borrow_mut();
        asked.questions.push(question);
        let answer = asked.approves;
        Box::pin(async move { answer })
    }

    fn ask_permission(&self, account_id: AccountId) {
        self.0.borrow_mut().permission_asked.push(account_id);
    }

    fn change_settings(&self, change: Change) -> Result<(), String> {
        self.0.borrow_mut().changes.push(change);
        Ok(())
    }

    fn new_draft(&self, account_id: AccountId) -> Result<Draft, String> {
        Ok(Draft::new(
            account_id,
            Address {
                name: Some("Dana".into()),
                email: ME.into(),
            },
        ))
    }

    fn compose(&self, draft: Draft) -> Result<(), String> {
        self.0.borrow_mut().composed.push(draft);
        Ok(())
    }

    fn send(&self, draft: Draft) -> Result<(), String> {
        self.0.borrow_mut().sent.push(draft);
        Ok(())
    }

    fn show_thread(&self, summary: ThreadSummary) {
        self.0.borrow_mut().opened.push(summary);
    }

    fn copy(&self, text: &str) {
        self.0.borrow_mut().copied.push(text.to_string());
    }

    fn mail_changed(&self, action: &MailAction, outcome: &Outcome) {
        self.0
            .borrow_mut()
            .mail_changed
            .push((action.clone(), outcome.clone()));
    }

    fn relist(&self) {
        self.0.borrow_mut().relisted += 1;
    }

    fn categorize_sender(
        &self,
        account_id: AccountId,
        email: String,
        who: String,
        category: Category,
    ) {
        self.0
            .borrow_mut()
            .categorized
            .push((account_id, email, who, category));
    }

    fn hide_address(
        &self,
        account_id: AccountId,
        note: String,
    ) -> Answer<'_, Result<Permitted<HiddenAddress>, String>> {
        let made = {
            let mut asked = self.0.borrow_mut();
            asked.hide_asked.push((account_id, note));
            asked.hidden.clone()
        };
        Box::pin(async move { made.ok_or_else(|| "no alias to hand out".to_string()) })
    }

    fn set_address_active(
        &self,
        address: String,
        active: bool,
    ) -> Answer<'_, Result<Permitted<()>, String>> {
        self.0.borrow_mut().activated.push((address, active));
        Box::pin(async move { Ok(Permitted::Done(())) })
    }
}

/// Runs the modules' futures on the test's own tokio runtime.
struct Runtime;

impl Background for Runtime {
    fn start(&self, task: Pin<Box<dyn Future<Output = ()> + Send>>) {
        tokio::spawn(task);
    }
}

// ---- The harness ---------------------------------------------------------

pub struct Harness {
    pub tools: Tools<Connected>,
    pub gmail: Arc<Gmail>,
    pub db: Db,
    pub desk: Rc<FakeDesk>,
    pub effects: Rc<FakeEffects>,
    pub account_id: AccountId,
    /// Held so the engine's change events have somewhere to go.
    _heard: async_channel::Receiver<ChangeEvent>,
    _dir: tempfile::TempDir,
}

/// A message for the one fixture account.
pub fn meta(id: &str, thread: &str, from: &str, subject: &str, at: EpochMillis) -> MessageMeta {
    MessageMeta {
        account_id: 1,
        id: id.into(),
        thread_id: thread.into(),
        rfc822_msgid: Some(format!("<{id}@example.com>")),
        from: Some(Address {
            name: Some(from.split('@').next().unwrap_or(from).to_string()),
            email: from.into(),
        }),
        to: vec![Address {
            name: None,
            email: ME.into(),
        }],
        cc: vec![],
        subject: subject.into(),
        date: at,
        snippet: format!("about {subject}"),
        size: 100,
        has_attachments: false,
        label_ids: vec![],
    }
}

/// The message with those labels on it.
pub fn labelled(message: MessageMeta, labels: &[&str]) -> MessageMeta {
    MessageMeta {
        label_ids: labels.iter().map(|l| l.to_string()).collect(),
        ..message
    }
}

impl Harness {
    /// A store and a Gmail holding `mail`, with one account connected.
    pub async fn with(mail: Vec<MessageMeta>) -> Harness {
        let dir = tempfile::tempdir().expect("a temp dir");
        let db = Db::open(&dir.path().join("mail.db")).expect("an empty store");
        let account_id = db
            .write(|c| accounts::insert_account(c, ME, 0))
            .await
            .expect("the account goes in");
        assert_eq!(account_id, 1, "fixture mail belongs to account 1");
        let known = vec![Label {
            account_id,
            id: "Label_kites".into(),
            name: "Kites".into(),
            kind: LabelKind::User,
            color: None,
        }];
        {
            let (mail, known) = (mail.clone(), known.clone());
            db.write(move |c| {
                labels::replace_labels(c, account_id, &known)?;
                for message in &mail {
                    messages::upsert_message(c, message, 1)?;
                }
                for message in &mail {
                    messages::refresh_thread(c, account_id, &message.thread_id)?;
                }
                Ok(())
            })
            .await
            .expect("the fixture mail goes in");
        }
        let gmail = Arc::new(Gmail::new());
        gmail.with(|i| {
            i.messages = mail;
            i.labels = vec![RemoteLabel {
                id: "Label_kites".into(),
                name: "Kites".into(),
                kind: Some("user".into()),
                color: None,
            }];
        });
        let (events, heard) = async_channel::unbounded();
        let sync = Arc::new(AccountSync::new(
            account_id,
            Arc::clone(&gmail),
            db.clone(),
            events,
        ));
        let connected = Arc::new(Connected(HashMap::from([(account_id, sync)])));
        let modules = Modules {
            mail: Arc::new(MailActions::new(Arc::clone(&connected), db.clone())),
            lists: Arc::new(Mailboxes::new(Arc::clone(&connected), db.clone())),
            gmail: Arc::new(AccountSettings::new(Arc::clone(&connected), db.clone())),
            accounts: connected,
            db: db.clone(),
        };
        let desk = Rc::new(FakeDesk(RefCell::new(Screen {
            settings: Settings::default(),
            accounts: vec![Account {
                id: account_id,
                email: ME.into(),
                state: AccountState::Ok,
            }],
            labels: HashMap::from([(account_id, known)]),
            view: View::default(),
            on_screen: OnScreen::default(),
            default_account: Some(account_id),
        })));
        let effects = Rc::new(FakeEffects(RefCell::new(Asked {
            approves: true,
            ..Asked::default()
        })));
        let tools = Tools::new(
            modules,
            Rc::new(Runtime),
            Rc::clone(&desk) as Rc<dyn Desk>,
            Rc::clone(&effects) as Rc<dyn Effects>,
        );
        Harness {
            tools,
            gmail,
            db,
            desk,
            effects,
            account_id,
            _heard: heard,
            _dir: dir,
        }
    }

    /// Runs one tool call and gives back its JSON, or its error message.
    pub async fn run(&self, name: &str, input: Value) -> Result<Value, String> {
        match self.tools.run(name, input).await {
            mailrs_ai::ToolOutcome::Ok(value) => Ok(value),
            mailrs_ai::ToolOutcome::Err(message) => Err(message),
        }
    }

    /// Runs a call that is meant to work.
    pub async fn ok(&self, name: &str, input: Value) -> Value {
        self.run(name, input)
            .await
            .unwrap_or_else(|err| panic!("{name} failed: {err}"))
    }

    pub fn asked(&self) -> std::cell::Ref<'_, Asked> {
        self.effects.0.borrow()
    }

    /// The labels on a stored message.
    pub async fn labels_of(&self, id: &str) -> Vec<String> {
        let (account_id, id) = (self.account_id, id.to_string());
        self.db
            .read(move |c| messages::labels_of(c, account_id, &id))
            .await
            .expect("the message is stored")
    }
}
