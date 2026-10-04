//! A ManageSieve server in memory, behind [`ManageSieveApi`]: scripts by
//! name, the active one, the Sieve extensions it offers, a refusal to
//! hand the next PUTSCRIPT, and a switch that takes it off the network.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use crate::client::{Capabilities, Listed, ManageSieveApi, SieveError};
use crate::script::Extensions;

#[derive(Default)]
pub struct SieveState {
    pub scripts: BTreeMap<String, String>,
    pub active: Option<String>,
    pub extensions: Extensions,
    pub refuse_put: Option<String>,
    pub down: bool,
    pub puts: usize,
}

#[derive(Default)]
pub struct FakeSieve {
    state: Mutex<SieveState>,
}

impl FakeSieve {
    pub fn new(extensions: &str) -> FakeSieve {
        let fake = FakeSieve::default();
        fake.with(|s| s.extensions = Extensions::parse(extensions));
        fake
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut SieveState) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub fn script(&self, name: &str) -> Option<String> {
        self.with(|s| s.scripts.get(name).cloned())
    }

    pub fn active(&self) -> Option<String> {
        self.with(|s| s.active.clone())
    }

    /// A script the person made in another client.
    pub fn put_elsewhere(&self, name: &str, text: &str, active: bool) {
        self.with(|s| {
            s.scripts.insert(name.into(), text.into());
            if active {
                s.active = Some(name.into());
            }
        });
    }

    pub fn refuse_next_put(&self, words: &str) {
        self.with(|s| s.refuse_put = Some(words.into()));
    }

    pub fn set_down(&self, down: bool) {
        self.with(|s| s.down = down);
    }

    pub fn puts(&self) -> usize {
        self.with(|s| s.puts)
    }

    fn up(&self) -> Result<(), SieveError> {
        match self.with(|s| s.down) {
            true => Err(SieveError::Network("the fake server is down".into())),
            false => Ok(()),
        }
    }
}

impl ManageSieveApi for FakeSieve {
    async fn capabilities(&self) -> Result<Capabilities, SieveError> {
        self.up()?;
        Ok(self.with(|s| Capabilities {
            implementation: Some("FakeSieve".into()),
            sieve: s.extensions.clone(),
            sasl: vec!["PLAIN".into()],
            starttls: true,
        }))
    }

    async fn scripts(&self) -> Result<Vec<Listed>, SieveError> {
        self.up()?;
        Ok(self.with(|s| {
            s.scripts
                .keys()
                .map(|name| Listed {
                    name: name.clone(),
                    active: s.active.as_deref() == Some(name),
                })
                .collect()
        }))
    }

    async fn get(&self, name: &str) -> Result<String, SieveError> {
        self.up()?;
        self.script(name).ok_or(SieveError::NotFound)
    }

    async fn put(&self, name: &str, script: &str) -> Result<(), SieveError> {
        self.up()?;
        self.with(|s| {
            if let Some(words) = s.refuse_put.take() {
                return Err(SieveError::Refused(words));
            }
            s.puts += 1;
            s.scripts.insert(name.into(), script.into());
            Ok(())
        })
    }

    async fn activate(&self, name: &str) -> Result<(), SieveError> {
        self.up()?;
        self.with(|s| {
            if !s.scripts.contains_key(name) {
                return Err(SieveError::NotFound);
            }
            s.active = Some(name.into());
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_put_is_refused_once_when_told_and_a_down_server_is_offline() {
        let sieve = FakeSieve::new("fileinto vacation");
        sieve.refuse_next_put("line 1: error");
        assert!(matches!(
            sieve.put("penguin-mail", "keep;").await,
            Err(SieveError::Refused(_))
        ));
        sieve.put("penguin-mail", "keep;").await.unwrap();
        sieve.activate("penguin-mail").await.unwrap();
        assert_eq!(sieve.active().as_deref(), Some("penguin-mail"));
        sieve.set_down(true);
        assert!(matches!(sieve.scripts().await, Err(SieveError::Network(_))));
    }
}
