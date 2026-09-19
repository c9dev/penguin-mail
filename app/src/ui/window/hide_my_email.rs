//! Hide My Email: plus addresses with Gmail filters behind them. The
//! dialog calls these, and so can anything else that needs an alias.

use std::rc::Rc;

use anyhow::{Context, anyhow};
use mailrs_domain::{Account, AccountId, LabelKind};

use super::MainWindow;
use crate::hide_my_email::{self, HiddenAddress};

impl MainWindow {
    /// Makes a new alias of `account_id`, adds a Gmail filter that gives
    /// its mail the Hide My Email label, and saves it.
    pub async fn create_hidden_address(
        self: &Rc<Self>,
        account_id: AccountId,
        note: &str,
    ) -> anyhow::Result<HiddenAddress> {
        let account = self
            .account(account_id)
            .ok_or_else(|| anyhow!("that account is not connected"))?;
        let sync = self
            .core
            .account(account_id)
            .ok_or_else(|| anyhow!("{} is not syncing yet", account.email))?;
        let taken = self.hidden_addresses();
        let address = loop {
            let address = hide_my_email::generate(&account.email)?;
            if !taken.iter().any(|h| h.address == address) {
                break address;
            }
        };
        let label_id = match self.hide_my_email_label(account_id) {
            Some(id) => id,
            None => {
                let label = {
                    let sync = sync.clone();
                    self.core
                        .call(async move { sync.create_label(hide_my_email::LABEL).await })
                        .await
                        .context("could not create the Hide My Email label")?
                };
                let id = label.id.clone();
                // Keep it here so a second address made before the label
                // list reloads does not create the label again.
                self.labels
                    .borrow_mut()
                    .entry(account_id)
                    .or_default()
                    .push(label);
                id
            }
        };
        let filter = hide_my_email::label_filter(&address, &label_id);
        let created = self
            .core
            .call(async move { sync.create_filter(filter).await })
            .await?;
        let hidden = HiddenAddress {
            account: account.email,
            address,
            note: note.trim().to_string(),
            created: mailrs_sync::now_millis(),
            active: true,
            label_filter: created.id,
            trash_filter: None,
        };
        if let Some(app) = self.app.upgrade() {
            let saved = hidden.clone();
            app.update_settings(|s| s.hidden_addresses.push(saved));
        }
        Ok(hidden)
    }

    /// Turns an alias on or off. Off adds a Gmail filter that sends its mail
    /// to the Trash; on removes that filter.
    pub async fn set_hidden_address_active(
        self: &Rc<Self>,
        address: &str,
        active: bool,
    ) -> anyhow::Result<()> {
        let hidden = self.hidden_address(address)?;
        let sync = self.sync_for(&hidden)?;
        let trash_filter = match (active, hidden.trash_filter.clone()) {
            (true, Some(id)) => {
                self.core
                    .call(async move { sync.delete_filter(&id).await })
                    .await?;
                None
            }
            (false, None) => {
                let filter = hide_my_email::trash_filter(&hidden.address);
                self.core
                    .call(async move { sync.create_filter(filter).await })
                    .await?
                    .id
            }
            (_, current) => current,
        };
        self.change_hidden_address(&hidden.address, |h| {
            h.active = active;
            h.trash_filter = trash_filter;
        });
        Ok(())
    }

    /// Removes an alias and its Gmail filters. Mail to it then arrives like
    /// any other mail.
    pub async fn delete_hidden_address(self: &Rc<Self>, address: &str) -> anyhow::Result<()> {
        let hidden = self.hidden_address(address)?;
        let sync = self.sync_for(&hidden)?;
        let ids: Vec<String> = [hidden.label_filter, hidden.trash_filter]
            .into_iter()
            .flatten()
            .collect();
        self.core
            .call(async move {
                for id in ids {
                    sync.delete_filter(&id).await?;
                }
                Ok::<_, mailrs_sync::SyncError>(())
            })
            .await?;
        if let Some(app) = self.app.upgrade() {
            app.update_settings(|s| s.hidden_addresses.retain(|h| h.address != hidden.address));
        }
        Ok(())
    }

    pub fn hidden_addresses(&self) -> Vec<HiddenAddress> {
        self.settings().hidden_addresses
    }

    pub fn accounts(&self) -> Vec<Account> {
        self.accounts.borrow().clone()
    }

    /// Opens the Hide My Email dialog, with `account_id` chosen for new
    /// addresses when given.
    pub(super) fn show_hide_my_email(self: &Rc<Self>, account_id: Option<AccountId>) {
        let weak = Rc::downgrade(self);
        crate::ui::hide_my_email::present(self, account_id, move |email| {
            if let Some(win) = weak.upgrade() {
                win.authorize(Some(email));
            }
        });
    }

    fn hide_my_email_label(&self, account_id: AccountId) -> Option<String> {
        self.labels
            .borrow()
            .get(&account_id)?
            .iter()
            .find(|l| {
                l.kind == LabelKind::User && l.name.eq_ignore_ascii_case(hide_my_email::LABEL)
            })
            .map(|l| l.id.clone())
    }

    fn hidden_address(&self, address: &str) -> anyhow::Result<HiddenAddress> {
        hide_my_email::is_alias(address)
            .then(|| self.hidden_addresses())
            .into_iter()
            .flatten()
            .find(|h| h.address.eq_ignore_ascii_case(address))
            .ok_or_else(|| anyhow!("{address} is not a Hide My Email address"))
    }

    fn sync_for(
        &self,
        hidden: &HiddenAddress,
    ) -> anyhow::Result<std::sync::Arc<crate::core::Sync>> {
        self.accounts
            .borrow()
            .iter()
            .find(|a| a.email.eq_ignore_ascii_case(&hidden.account))
            .and_then(|a| self.core.account(a.id))
            .ok_or_else(|| anyhow!("{} is not connected", hidden.account))
    }

    fn change_hidden_address(&self, address: &str, change: impl FnOnce(&mut HiddenAddress)) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        app.update_settings(|s| {
            if let Some(h) = s.hidden_addresses.iter_mut().find(|h| h.address == address) {
                change(h);
            }
        });
    }
}
