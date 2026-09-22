//! Penguin Mail with no window: a store, sync's in-memory Gmail behind the
//! modules, and fake adapters in front of both ports. Building one costs a
//! tempdir and a tokio runtime, so the whole tool loop runs under
//! `cargo test`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use mailrs_domain::{
    Account, AccountId, AccountState, Address, Category, ChangeEvent, EpochMillis, Label,
    LabelKind, MessageMeta, ThreadSummary,
};
use mailrs_gmail::RemoteLabel;
use mailrs_store::{Db, accounts, messages};
use mailrs_sync::fake::{FakeGmail, fill_store};
use mailrs_sync::{
    AccountSettings, AccountSync, Accounts, Calendar, Invitations, MailAction, MailActions,
    Mailboxes, Outcome, Permitted, View,
};
use serde_json::Value;

use super::{Answer, Background, Desk, Effects, Modules, OnScreen, Permission, Tools};
use crate::compose::Draft;
use crate::hide_my_email::HiddenAddress;
use crate::settings::{Change, Settings};
use crate::unsubscribe::Unsubscribe;

/// The address every fixture account belongs to.
pub const ME: &str = "dana@example.com";

/// The clock the fixture mailbox searches by, 2026-01-02 at noon UTC. The
/// fixture mail sits a few days before it, inside the window a first sync
/// stores.
pub const NOW: EpochMillis = 1_767_355_200_000;

/// The accounts a test connects, by id.
pub struct Connected(HashMap<AccountId, Arc<AccountSync<FakeGmail>>>);

impl Accounts for Connected {
    type Api = FakeGmail;

    fn account(&self, account_id: AccountId) -> Option<Arc<AccountSync<FakeGmail>>> {
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
    /// Accounts offered a permission, and which.
    pub permission_asked: Vec<(AccountId, Permission)>,
    /// The switched-off APIs the window was asked to explain.
    pub api_off: Vec<(String, String)>,
    /// Messages handed to Send Later, with their times.
    pub scheduled: Vec<(Draft, EpochMillis)>,
    pub unsubscribed: Vec<(AccountId, Unsubscribe)>,
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

    fn ask_permission(&self, account_id: AccountId, permission: Permission) {
        self.0
            .borrow_mut()
            .permission_asked
            .push((account_id, permission));
    }

    fn explain_api_off(&self, service: &str, enable_url: &str) {
        self.0
            .borrow_mut()
            .api_off
            .push((service.to_string(), enable_url.to_string()));
    }

    fn send_later(&self, draft: Draft, at: EpochMillis) -> Result<(), String> {
        self.0.borrow_mut().scheduled.push((draft, at));
        Ok(())
    }

    fn unsubscribe(
        &self,
        account_id: AccountId,
        how: Unsubscribe,
    ) -> Answer<'_, Result<(), String>> {
        self.0.borrow_mut().unsubscribed.push((account_id, how));
        Box::pin(async move { Ok(()) })
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
    pub gmail: Arc<FakeGmail>,
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
    /// A Gmail holding `mail`, and a store filled from it by a first sync,
    /// with one account connected.
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
        let gmail = Arc::new(FakeGmail::new());
        gmail.with(|s| {
            s.email = ME.into();
            s.display_name = Some("Dana".into());
            s.clock = Some(NOW);
            // One page holds any search, as the assistant sees it.
            s.page_size = 1000;
            s.labels = vec![RemoteLabel {
                id: "Label_kites".into(),
                name: "Kites".into(),
                kind: Some("user".into()),
                color: None,
            }];
        });
        for message in mail {
            gmail.seed(message);
        }
        let (events, heard) = async_channel::unbounded();
        let sync = Arc::new(AccountSync::new(
            account_id,
            Arc::clone(&gmail),
            db.clone(),
            events,
        ));
        fill_store(&sync).await.expect("the first sync runs");
        let connected = Arc::new(Connected(HashMap::from([(account_id, sync)])));
        let modules = Modules {
            mail: Arc::new(MailActions::new(Arc::clone(&connected), db.clone())),
            lists: Arc::new(Mailboxes::new(Arc::clone(&connected), db.clone())),
            gmail: Arc::new(AccountSettings::new(Arc::clone(&connected), db.clone())),
            calendar: Arc::new(Calendar::new(Arc::clone(&connected))),
            invitations: Arc::new(Invitations::new(Arc::clone(&connected), db.clone())),
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
