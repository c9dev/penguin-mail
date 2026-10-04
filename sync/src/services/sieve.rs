//! Rules and the automatic reply on a server that runs Sieve, through
//! ManageSieve. Both live in the one script Penguin Mail keeps there,
//! `penguin-mail`, which every change reads, edits and writes whole. A
//! script the person already runs is included where the server can
//! include; otherwise the first write answers `WouldReplace` until the
//! person says yes. Folder names go to the server in UTF-8, decoded from
//! the IMAP adapter's modified UTF-7 ids.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{Filter, MailSet, Vacation};
use mailrs_sieve::SCRIPT_NAME;
use mailrs_sieve::client::{ManageSieveApi, SieveError};
use mailrs_sieve::script::{self, Extensions, Script, Unsayable, WriteError};

use super::{AutoReplyService, MailBackend, RulesService};
use crate::BackendError;

pub struct SieveRules<M, B> {
    api: Arc<M>,
    mail: B,
    address: String,
    provider: String,
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    take_over: bool,
    extensions: Option<Extensions>,
}

impl<M, B: Clone> Clone for SieveRules<M, B> {
    fn clone(&self) -> Self {
        SieveRules {
            api: Arc::clone(&self.api),
            mail: self.mail.clone(),
            address: self.address.clone(),
            provider: self.provider.clone(),
            state: Arc::clone(&self.state),
        }
    }
}

/// Which script the server runs now.
enum Running {
    Ours,
    Theirs(String),
    Nothing,
}

impl<M: ManageSieveApi, B: MailBackend + Clone> SieveRules<M, B> {
    pub fn new(api: Arc<M>, mail: B, address: String, provider: String) -> Self {
        SieveRules {
            api,
            mail,
            address,
            provider,
            state: Arc::default(),
        }
    }

    /// The Sieve extensions the server offered when last asked.
    pub fn extensions(&self) -> Option<Extensions> {
        self.state().extensions.clone()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn err(&self, err: SieveError) -> BackendError {
        match err {
            SieveError::Network(detail) => BackendError::Offline(detail),
            SieveError::Auth(_) => BackendError::NeedsReauth,
            SieveError::NotFound => BackendError::NotFound,
            SieveError::Refused(words) => BackendError::Refused(words),
            SieveError::NoStartTls => BackendError::Refused(fill(
                &gettext(
                    "{provider} offers no encrypted connection for rules, so Penguin Mail does not send your password there.",
                ),
                &[("provider", &self.provider)],
            )),
            other => BackendError::Refused(other.to_string()),
        }
    }

    fn words(&self, err: WriteError) -> BackendError {
        let provider = [("provider", self.provider.as_str())];
        let text = match err {
            WriteError::Needs(extension) => fill(
                &gettext(
                    "{provider} cannot run this rule: its server lacks the Sieve extension “{extension}”.",
                ),
                &[("provider", &self.provider), ("extension", extension)],
            ),
            WriteError::NoFolder(_) => {
                gettext("The folder this rule files into is not on the server yet.")
            }
            WriteError::Unsayable(Unsayable::NeverSpam) => fill(
                &gettext("A rule on {provider} cannot keep mail out of Spam."),
                &provider,
            ),
            WriteError::Unsayable(Unsayable::Importance) => fill(
                &gettext("{provider} does not mark mail as important."),
                &provider,
            ),
            WriteError::Unsayable(Unsayable::TwoFolders) => {
                gettext("A rule can file mail into one folder.")
            }
            WriteError::Unsayable(Unsayable::NothingToMatch) => {
                gettext("Say which mail the rule is for")
            }
            WriteError::Unsayable(Unsayable::NothingToDo) => gettext("Choose what the rule does"),
        };
        BackendError::Refused(text)
    }

    /// The UTF-8 name of the folder a set files into.
    fn folder(&self, set: &MailSet) -> Option<String> {
        let id = match set {
            MailSet::Role(role) => self.mail.mailbox_for(*role)?,
            MailSet::Mailbox(id) => id.clone(),
            _ => return None,
        };
        Some(mailrs_imap::utf7::decode(&id))
    }

    async fn load(&self) -> Result<(Script, Extensions, Running), BackendError> {
        let caps = self.api.capabilities().await.map_err(|e| self.err(e))?;
        self.state().extensions = Some(caps.sieve.clone());
        let listed = self.api.scripts().await.map_err(|e| self.err(e))?;
        let script = if listed.iter().any(|l| l.name == SCRIPT_NAME) {
            let text = self.api.get(SCRIPT_NAME).await.map_err(|e| self.err(e))?;
            script::read(&text)
        } else {
            Script::default()
        };
        let running = match listed.iter().find(|l| l.active) {
            Some(l) if l.name == SCRIPT_NAME => Running::Ours,
            Some(l) => Running::Theirs(l.name.clone()),
            None => Running::Nothing,
        };
        Ok((script, caps.sieve, running))
    }

    async fn store(
        &self,
        mut script: Script,
        ext: &Extensions,
        running: Running,
    ) -> Result<(), BackendError> {
        if let Running::Theirs(name) = &running {
            if ext.has("include") {
                script.include = Some(name.clone());
            } else if !self.state().take_over {
                return Err(BackendError::WouldReplace {
                    script: name.clone(),
                });
            }
        }
        let text = script::write(&script, &self.address, &|set| self.folder(set), ext)
            .map_err(|e| self.words(e))?;
        self.api
            .put(SCRIPT_NAME, &text)
            .await
            .map_err(|e| self.err(e))?;
        if !matches!(running, Running::Ours) {
            self.api
                .activate(SCRIPT_NAME)
                .await
                .map_err(|e| self.err(e))?;
        }
        Ok(())
    }

    /// The script holds only rules of Penguin Mail's own, so it cannot
    /// keep one the app marked read-only.
    fn refuse_read_only(filter: &Filter) -> Result<(), BackendError> {
        if filter.read_only {
            return Err(BackendError::Refused(gettext("The rule is read-only.")));
        }
        Ok(())
    }

    fn fresh(filter: &Filter) -> Filter {
        Filter {
            id: Some(format!("sieve-{:016x}", rand::random::<u64>())),
            ..filter.clone()
        }
    }
}

impl<M: ManageSieveApi, B: MailBackend + Clone> RulesService for SieveRules<M, B> {
    async fn filters(&self) -> Result<Vec<Filter>, BackendError> {
        Ok(self.load().await?.0.rules)
    }

    async fn create_filter(&self, filter: &Filter) -> Result<Filter, BackendError> {
        Self::refuse_read_only(filter)?;
        let (mut script, ext, running) = self.load().await?;
        let made = Self::fresh(filter);
        script.rules.push(made.clone());
        self.store(script, &ext, running).await?;
        Ok(made)
    }

    async fn delete_filter(&self, id: &str) -> Result<(), BackendError> {
        let (mut script, ext, running) = self.load().await?;
        let before = script.rules.len();
        script.rules.retain(|r| r.id.as_deref() != Some(id));
        if script.rules.len() == before {
            return Err(BackendError::NotFound);
        }
        self.store(script, &ext, running).await
    }

    async fn replace_filter(&self, old_id: &str, new: &Filter) -> Result<Filter, BackendError> {
        Self::refuse_read_only(new)?;
        let (mut script, ext, running) = self.load().await?;
        let slot = script
            .rules
            .iter()
            .position(|r| r.id.as_deref() == Some(old_id))
            .ok_or(BackendError::NotFound)?;
        let made = Self::fresh(new);
        script.rules[slot] = made.clone();
        self.store(script, &ext, running).await?;
        Ok(made)
    }

    async fn take_over(&self) {
        self.state().take_over = true;
    }

    fn queues_offline(&self) -> bool {
        true
    }
}

impl<M: ManageSieveApi, B: MailBackend + Clone> AutoReplyService for SieveRules<M, B> {
    async fn vacation(&self) -> Result<Vacation, BackendError> {
        Ok(self.load().await?.0.vacation.unwrap_or_default())
    }

    async fn set_vacation(&self, vacation: &Vacation) -> Result<(), BackendError> {
        let (mut script, ext, running) = self.load().await?;
        script.vacation = Some(vacation.clone());
        self.store(script, &ext, running).await
    }
}
