//! Penguin Mail with no window: a store, sync's in-memory Gmail behind the
//! modules, and fake adapters in front of both ports. Building one costs a
//! tempdir and a tokio runtime, so the whole tool loop runs under
//! `cargo test`. Where the window hands an effect to a sync module, as with
//! Categorize Sender, unsubscribing and Hide My Email, the fake calls the
//! same module, so the tests see what Gmail and the store end up holding.

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
    AccountSettings, AccountSync, Accounts, Calendar, Categorized, ContactBook, Invitations, Leave,
    MailAction, MailActions, Mailboxes, Outcome, Permitted, View, hidden,
};
use serde_json::Value;
use tokio::task::JoinHandle;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use super::{Answer, Background, Desk, Effects, Modules, OnScreen, Permission, Tools};
use crate::compose::{self, Draft};
use crate::hide_my_email::HiddenAddress;
use crate::protection::{self, Held, Standard};
use crate::settings::{Change, Settings};
use crate::ui::unsubscribe::{ListLine, Way, line_text};
use crate::unsubscribe::Unsubscribe;
use crate::unsubscribe_page::fake::FakeBrowser;
use crate::unsubscribe_page::{Adviser, Browser, PageForm, Plan};

/// The address every fixture account belongs to.
pub const ME: &str = "dana@example.com";

/// The address of the second account `Harness::with_second` connects.
pub const YOU: &str = "sam@example.com";

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
    /// The folder `export_mail` writes to when the user names none, a
    /// fresh one inside the test's temp dir.
    pub downloads: std::path::PathBuf,
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

    fn downloads(&self) -> std::path::PathBuf {
        self.0.borrow().downloads.clone()
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
    /// What leaving a list left for the window: a request to send from the
    /// account, or a page to open in the browser.
    pub left: Vec<(AccountId, Leave)>,
    /// What each unsubscribe dialog was asked about: one string per
    /// line, the list's name and the words under it once its page had
    /// settled.
    pub lists_asked: Vec<Vec<String>>,
    /// Whether the person ticks every line and presses Unsubscribe. The
    /// dialog is that tool's only question, so this stands apart from
    /// `approves`, which answers the pane's card.
    pub approves_lists: bool,
    pub mail_changed: Vec<(MailAction, Outcome)>,
    pub relisted: usize,
    /// Messages whose only copy was opened in a composer, marked unsaved.
    pub reopened: Vec<Draft>,
    /// How often the queue's lists were asked to read again.
    pub queue_changed: usize,
    /// What each undo put back.
    pub undone: Vec<Outcome>,
    /// How often a tool told the window to read the image senders again.
    pub image_senders_changed: usize,
    /// The addresses gpg holds a key for, or `None` for a computer with no
    /// gpg. The fake has no gpgsm.
    pub keys: Option<Vec<String>>,
    /// Drafts saved back into Gmail, as the window's Save Draft saves them.
    pub saved_drafts: Vec<Draft>,
    /// Categorize Sender runs in the background, as the window runs it.
    /// `Harness::categorized` waits for these.
    sorting: Vec<JoinHandle<Categorized>>,
}

/// The window's effects. The ones the window hands to sync go to the same
/// modules the tools use, and settings changes land on the fake desk, as
/// the app's settings reach the window.
pub struct FakeEffects {
    pub asked: RefCell<Asked>,
    /// The pages the hidden view serves, and the page a submission lands
    /// on. A test fills these in before the call; the run takes one
    /// browser built from them, which stays here to be read afterwards.
    pub pages: RefCell<HashMap<String, PageForm>>,
    pub after: RefCell<PageForm>,
    pub browser: RefCell<Option<Rc<FakeBrowser>>>,
    desk: Rc<FakeDesk>,
    mail: Arc<MailActions<Connected>>,
    gmail: Arc<AccountSettings<Connected>>,
    connected: Arc<Connected>,
}

impl FakeEffects {
    /// The connected account with this address, as the app finds the
    /// owner of a Hide My Email address.
    fn account_of(&self, email: &str) -> Result<AccountId, String> {
        self.desk
            .accounts()
            .iter()
            .find(|a| a.email.eq_ignore_ascii_case(email))
            .map(|a| a.id)
            .ok_or_else(|| format!("{email} is not connected"))
    }
}

impl Effects for FakeEffects {
    fn confirm(&self, question: String) -> Answer<'_, bool> {
        let mut asked = self.asked.borrow_mut();
        asked.questions.push(question);
        let answer = asked.approves;
        Box::pin(async move { answer })
    }

    fn ask_permission(&self, account_id: AccountId, permission: Permission) {
        self.asked
            .borrow_mut()
            .permission_asked
            .push((account_id, permission));
    }

    fn explain_api_off(&self, service: &str, enable_url: &str) {
        self.asked
            .borrow_mut()
            .api_off
            .push((service.to_string(), enable_url.to_string()));
    }

    fn send_later(&self, draft: Draft, at: EpochMillis) -> Result<(), String> {
        self.asked.borrow_mut().scheduled.push((draft, at));
        Ok(())
    }

    fn unsubscribe(
        &self,
        account_id: AccountId,
        how: Unsubscribe,
    ) -> Answer<'_, Result<(), String>> {
        Box::pin(async move {
            let leave = self
                .mail
                .unsubscribe(account_id, how)
                .await
                .map_err(|e| e.to_string())?;
            if leave != Leave::Done {
                self.asked.borrow_mut().left.push((account_id, leave));
            }
            Ok(())
        })
    }

    fn page_adviser(&self) -> Option<Box<dyn Adviser>> {
        None
    }

    fn page_browser(&self) -> Rc<dyn Browser> {
        let browser = Rc::new(FakeBrowser {
            pages: self.pages.borrow().clone(),
            after: self.after.borrow().clone(),
            submitted: RefCell::new(Vec::new()),
            typed: RefCell::new(Vec::new()),
            fail: None,
            standing: RefCell::new(String::new()),
        });
        *self.browser.borrow_mut() = Some(Rc::clone(&browser));
        browser
    }

    fn confirm_unsubscribe(
        &self,
        lines: Vec<ListLine>,
        updates: async_channel::Receiver<(usize, Way)>,
    ) -> Answer<'_, Option<Vec<(usize, Way)>>> {
        Box::pin(async move {
            let mut names: Vec<String> = Vec::with_capacity(lines.len());
            let mut ways: Vec<Way> = Vec::with_capacity(lines.len());
            for line in lines {
                names.push(line.name);
                ways.push(line.way);
            }
            // The dialog cannot be answered while a line is still being
            // read, so this waits for the same thing. A run that
            // submitted before a page settled fails a test here rather
            // than passing quietly.
            while let Ok((at, way)) = updates.recv().await {
                if let Some(held) = ways.get_mut(at) {
                    *held = way;
                }
            }
            let mut asked = self.asked.borrow_mut();
            asked.lists_asked.push(
                names
                    .iter()
                    .zip(&ways)
                    .map(|(name, way)| format!("{name}: {}", line_text(way)))
                    .collect(),
            );
            asked
                .approves_lists
                .then(|| ways.into_iter().enumerate().collect())
        })
    }

    fn change_settings(&self, change: Change) -> Result<(), String> {
        self.asked.borrow_mut().changes.push(change.clone());
        change.apply_to(&mut self.desk.0.borrow_mut().settings);
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
        self.asked.borrow_mut().composed.push(draft);
        Ok(())
    }

    fn send(&self, draft: Draft) -> Result<(), String> {
        self.asked.borrow_mut().sent.push(draft);
        Ok(())
    }

    fn show_thread(&self, summary: ThreadSummary) {
        self.asked.borrow_mut().opened.push(summary);
    }

    fn copy(&self, text: &str) {
        self.asked.borrow_mut().copied.push(text.to_string());
    }

    fn mail_changed(&self, action: &MailAction, outcome: &Outcome) {
        self.asked
            .borrow_mut()
            .mail_changed
            .push((action.clone(), outcome.clone()));
    }

    fn relist(&self) {
        self.asked.borrow_mut().relisted += 1;
    }

    fn categorize_sender(
        &self,
        account_id: AccountId,
        email: String,
        _who: String,
        category: Category,
    ) {
        let mail = Arc::clone(&self.mail);
        let sorting = tokio::spawn(async move {
            mail.categorize_sender(account_id, &email, None, category)
                .await
        });
        self.asked.borrow_mut().sorting.push(sorting);
    }

    fn hide_address(
        &self,
        account_id: AccountId,
        note: String,
    ) -> Answer<'_, Result<Permitted<HiddenAddress>, String>> {
        Box::pin(async move {
            let account = self
                .desk
                .accounts()
                .into_iter()
                .find(|a| a.id == account_id)
                .ok_or("that account is not connected")?;
            let taken = self.desk.settings().hidden_addresses;
            let made = self
                .gmail
                .create_hidden_address(account_id, &account.email, &note, &taken)
                .await
                .map_err(|e| e.to_string())?;
            if let Permitted::Done(hidden) = &made {
                self.change_settings(Change::SaveHiddenAddress(hidden.clone()))?;
            }
            Ok(made)
        })
    }

    fn set_address_active(
        &self,
        address: String,
        active: bool,
    ) -> Answer<'_, Result<Permitted<()>, String>> {
        Box::pin(async move {
            let kept = self.desk.settings().hidden_addresses;
            let hidden = hidden::find(&kept, &address)
                .cloned()
                .ok_or_else(|| format!("{address} is not a Hide My Email address"))?;
            let account_id = self.account_of(&hidden.account)?;
            let changed = self
                .gmail
                .set_hidden_address_active(account_id, &hidden, active)
                .await
                .map_err(|e| e.to_string())?;
            let Permitted::Done(changed) = changed else {
                return Ok(Permitted::NeedsPermission);
            };
            self.change_settings(Change::SaveHiddenAddress(changed))?;
            Ok(Permitted::Done(()))
        })
    }

    fn reopen_unsent(&self, draft: Draft) -> Result<(), String> {
        self.asked.borrow_mut().reopened.push(draft);
        Ok(())
    }

    fn queue_changed(&self) {
        self.asked.borrow_mut().queue_changed += 1;
    }

    fn undone(&self, outcome: &Outcome) {
        self.asked.borrow_mut().undone.push(outcome.clone());
    }

    fn image_senders_changed(&self) {
        self.asked.borrow_mut().image_senders_changed += 1;
    }

    // The engines and Gmail's Drafts. The fake gpg holds the keys a test
    // lists, and "encrypts" a draft by wrapping its body in base64 under
    // the header an encrypted draft carries, so a draft saved encrypted
    // reopens through the same `protection::draft` code the window's does.

    fn keys(&self, addresses: Vec<String>) -> Answer<'_, Held> {
        let held = self.asked.borrow().keys.clone().map(|keys| {
            addresses
                .iter()
                .map(|address| mailrs_pgp::Recipient {
                    address: address.clone(),
                    key: keys.contains(address).then(|| mailrs_pgp::Key {
                        fingerprint: "F".repeat(40),
                        user_id: format!("<{address}>"),
                        trust: mailrs_pgp::Trust::Unknown,
                    }),
                })
                .collect()
        });
        Box::pin(async move {
            Held {
                pgp: held,
                smime: None,
            }
        })
    }

    fn signing_standard(&self, _from: String) -> Answer<'_, Standard> {
        Box::pin(async { Standard::Pgp })
    }

    fn reopen_draft(&self, raw: Vec<u8>, draft: Draft) -> Answer<'_, Result<Draft, String>> {
        Box::pin(async move {
            let mut draft = draft;
            match protection::draft::standard_of(&raw) {
                None => protection::draft::reopen_plain(&raw, &mut draft),
                Some(standard) => {
                    let blank = protection::find(&raw, b"\r\n\r\n").ok_or("no body")? + 4;
                    let wrapped: String = String::from_utf8_lossy(&raw[blank..])
                        .split_whitespace()
                        .collect();
                    let part = STANDARD.decode(wrapped).map_err(|e| e.to_string())?;
                    let (body, files) = protection::opened_body(&part);
                    let read = protection::Read {
                        mark: protection::Mark {
                            title: "Encrypted".into(),
                            detail: None,
                            tone: protection::Tone::Good,
                        },
                        body: Some(body),
                        files,
                        sealed: true,
                    };
                    protection::draft::reopen(&raw, standard, read, &mut draft)?;
                }
            }
            Ok(draft)
        })
    }

    fn save_draft(&self, draft: Draft) -> Answer<'_, Result<(), String>> {
        Box::pin(async move {
            let id = compose::new_message_id(&draft.from.email);
            let raw = match draft.encrypt {
                false => compose::build_mime(&draft, NOW / 1000, &id)?,
                true => {
                    let part = compose::build_body_part(&draft)?;
                    let entity = format!(
                        "Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"fake\"\r\n\r\n{}\r\n",
                        STANDARD.encode(part)
                    );
                    protection::draft::build(&draft, NOW / 1000, &id, entity.into_bytes())?
                }
            };
            let account = self
                .connected
                .account(draft.account_id)
                .ok_or("that account is not connected")?;
            account
                .save_draft(raw, draft.thread_id.clone(), draft.draft_id.clone())
                .await
                .map_err(|e| e.to_string())?;
            self.asked.borrow_mut().saved_drafts.push(draft);
            Ok(())
        })
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
    /// The second account and its Gmail, when the test connected one.
    pub second: Option<(AccountId, Arc<FakeGmail>)>,
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
        list_unsubscribe: None,
        one_click: false,
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
        Harness::connect(mail, None).await
    }

    /// As [`Harness::with`], plus a second account, [`YOU`], whose Gmail
    /// holds `second` and no labels of its own.
    pub async fn with_second(mail: Vec<MessageMeta>, second: Vec<MessageMeta>) -> Harness {
        Harness::connect(mail, Some(second)).await
    }

    async fn connect(mail: Vec<MessageMeta>, second: Option<Vec<MessageMeta>>) -> Harness {
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
            events.clone(),
        ));
        fill_store(&sync).await.expect("the first sync runs");
        let mut syncing = HashMap::from([(account_id, sync)]);
        let mut listed = vec![Account {
            id: account_id,
            email: ME.into(),
            state: AccountState::Ok,
        }];
        let mut labels = HashMap::from([(account_id, known)]);
        let mut other = None;
        if let Some(second) = second {
            let id = db
                .write(|c| accounts::insert_account(c, YOU, 0))
                .await
                .expect("the second account goes in");
            let gmail = Arc::new(FakeGmail::new());
            gmail.with(|s| {
                s.email = YOU.into();
                s.clock = Some(NOW);
                s.page_size = 1000;
            });
            for message in second {
                gmail.seed(MessageMeta {
                    account_id: id,
                    ..message
                });
            }
            let sync = Arc::new(AccountSync::new(
                id,
                Arc::clone(&gmail),
                db.clone(),
                events.clone(),
            ));
            fill_store(&sync).await.expect("the second sync runs");
            syncing.insert(id, sync);
            listed.push(Account {
                id,
                email: YOU.into(),
                state: AccountState::Ok,
            });
            labels.insert(id, vec![]);
            other = Some((id, gmail));
        }
        let connected = Arc::new(Connected(syncing));
        let mail = Arc::new(MailActions::new(Arc::clone(&connected), db.clone()));
        let settings = Arc::new(AccountSettings::new(Arc::clone(&connected), db.clone()));
        let modules = Modules {
            mail: Arc::clone(&mail),
            lists: Arc::new(Mailboxes::new(Arc::clone(&connected), db.clone())),
            gmail: Arc::clone(&settings),
            calendar: Arc::new(Calendar::new(Arc::clone(&connected))),
            invitations: Arc::new(Invitations::new(Arc::clone(&connected), db.clone())),
            contacts: Arc::new(ContactBook::new(
                Arc::clone(&connected),
                db.clone(),
                dir.path().join("photos"),
            )),
            accounts: connected,
            db: db.clone(),
        };
        let desk = Rc::new(FakeDesk(RefCell::new(Screen {
            settings: Settings::default(),
            accounts: listed,
            labels,
            view: View::default(),
            on_screen: OnScreen::default(),
            default_account: Some(account_id),
            downloads: {
                let downloads = dir.path().join("Downloads");
                std::fs::create_dir(&downloads).expect("a downloads folder");
                downloads
            },
        })));
        let effects = Rc::new(FakeEffects {
            asked: RefCell::new(Asked {
                approves: true,
                approves_lists: true,
                ..Asked::default()
            }),
            pages: RefCell::new(HashMap::new()),
            after: RefCell::new(PageForm {
                text: "Thanks!".to_string(),
                ..PageForm::default()
            }),
            browser: RefCell::new(None),
            desk: Rc::clone(&desk),
            mail,
            gmail: settings,
            connected: Arc::clone(&modules.accounts),
        });
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
            second: other,
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
        self.effects.asked.borrow()
    }

    /// Waits for every Categorize Sender the tools started, and gives back
    /// what each did.
    pub async fn categorized(&self) -> Vec<Categorized> {
        let sorting = std::mem::take(&mut self.effects.asked.borrow_mut().sorting);
        let mut done = Vec::new();
        for task in sorting {
            done.push(task.await.expect("Categorize Sender finishes"));
        }
        done
    }

    // ---- The hidden view the unsubscribe tool loads pages in -----------

    /// Serves `page` at `url`, the way a sender's unsubscribe page
    /// answers.
    pub fn serve(&self, url: &str, page: PageForm) {
        self.effects
            .pages
            .borrow_mut()
            .insert(url.to_string(), page);
    }

    /// What the page a submission lands on says.
    pub fn after_submitting(&self, text: &str) {
        self.effects.after.borrow_mut().text = text.to_string();
    }

    /// Every plan the run submitted, oldest first, and the addresses it
    /// typed. Empty until a tool has asked for a browser.
    pub fn submissions(&self) -> Vec<Plan> {
        match &*self.effects.browser.borrow() {
            Some(browser) => browser.submissions(),
            None => Vec::new(),
        }
    }

    pub fn typed(&self) -> Vec<String> {
        match &*self.effects.browser.borrow() {
            Some(browser) => browser.typed.borrow().clone(),
            None => Vec::new(),
        }
    }

    /// The labels on a stored message.
    pub async fn labels_of(&self, id: &str) -> Vec<String> {
        self.labels_in(self.account_id, id).await
    }

    /// The labels on a stored message of `account_id`.
    pub async fn labels_in(&self, account_id: AccountId, id: &str) -> Vec<String> {
        let id = id.to_string();
        self.db
            .read(move |c| messages::labels_of(c, account_id, &id))
            .await
            .expect("the message is stored")
    }
}
