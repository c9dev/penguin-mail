//! The Microsoft adapter over `FakeGraph`, through the same engine a real
//! account runs.

mod calendar;
mod contacts;
mod mail;
mod outgoing;
mod rules;
mod writes;

use std::sync::Arc;

use mailrs_domain::{ChangeEvent, Memberships};
use mailrs_store::{Db, accounts, messages};

use crate::fake::FakeGraph;
use crate::{AccountServices, AccountSync};

pub(crate) struct Outlook {
    pub fake: Arc<FakeGraph>,
    pub sync: Arc<AccountSync>,
    pub db: Db,
    pub events: async_channel::Receiver<ChangeEvent>,
    pub account_id: i64,
    _dir: tempfile::TempDir,
}

pub(crate) async fn outlook() -> Outlook {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mail.db")).unwrap();
    let account_id = db.write(|c| accounts::insert_account(c, "me@outlook.com", 0)).await.unwrap();
    let fake = Arc::new(FakeGraph::new());
    let (sender, events) = async_channel::unbounded();
    let sync = Arc::new(AccountSync::new(
        account_id,
        AccountServices::fake_microsoft(Arc::clone(&fake)),
        db.clone(),
        sender,
    ));
    Outlook { fake, sync, db, events, account_id, _dir: dir }
}

impl Outlook {
    /// The first sync: the folders, the feed's start and the window.
    pub async fn bootstrap_all(&self) {
        self.sync.bootstrap().await.unwrap();
        self.drain();
    }

    /// One look at the feed, as the engine's loop makes.
    pub async fn look(&self) {
        self.sync.incremental().await.unwrap();
    }

    pub async fn stored(&self, id: &str) -> bool {
        let (account, wanted) = (self.account_id, vec![id.to_string()]);
        self.db
            .read(move |c| messages::existing_ids(c, account, &wanted))
            .await
            .unwrap()
            .contains(id)
    }

    pub async fn held(&self, id: &str) -> Memberships {
        let (account, wanted) = (self.account_id, vec![id.to_string()]);
        self.db
            .read(move |c| messages::memberships_of(c, account, &wanted))
            .await
            .unwrap()
            .remove(id)
            .unwrap_or_default()
    }

    pub fn drain(&self) -> Vec<ChangeEvent> {
        std::iter::from_fn(|| self.events.try_recv().ok()).collect()
    }
}
