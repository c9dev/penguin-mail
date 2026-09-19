//! Hide My Email: plus addresses with Gmail filters behind them. The
//! dialog calls these, and so can anything else that needs an alias.

use std::rc::Rc;

use anyhow::{anyhow, bail};
use mailrs_domain::{Account, AccountId};
use mailrs_sync::{HiddenFilters, Permitted};

use super::MainWindow;
use crate::hide_my_email::{self, HiddenAddress};

impl MainWindow {
    /// Makes a new alias of `account_id`, gives it its Gmail filters, and
    /// saves it.
    pub async fn create_hidden_address(
        self: &Rc<Self>,
        account_id: AccountId,
        note: &str,
    ) -> anyhow::Result<Permitted<HiddenAddress>> {
        let account = self
            .account(account_id)
            .ok_or_else(|| anyhow!("that account is not connected"))?;
        if self.core.account(account_id).is_none() {
            bail!("{} is not syncing yet", account.email);
        }
        let taken = self.hidden_addresses();
        let address = loop {
            let address = hide_my_email::generate(&account.email)?;
            if !taken.iter().any(|h| h.address == address) {
                break address;
            }
        };
        let settings = self.core.gmail_settings();
        let made = {
            let address = address.clone();
            self.core
                .call(async move { settings.hide_address(account_id, &address).await })
                .await?
        };
        let Permitted::Done(filters) = made else {
            return Ok(Permitted::NeedsPermission);
        };
        let hidden = HiddenAddress {
            account: account.email,
            address,
            note: note.trim().to_string(),
            created: mailrs_sync::now_millis(),
            active: true,
            label_filter: filters.label,
            trash_filter: filters.trash,
        };
        if let Some(app) = self.app.upgrade() {
            let saved = hidden.clone();
            app.update_settings(|s| s.hidden_addresses.push(saved));
        }
        Ok(Permitted::Done(hidden))
    }

    /// Turns an alias on or off. Off sends its mail to the Trash.
    pub async fn set_hidden_address_active(
        self: &Rc<Self>,
        address: &str,
        active: bool,
    ) -> anyhow::Result<Permitted<()>> {
        let hidden = self.hidden_address(address)?;
        let account_id = self.hidden_account(&hidden)?;
        let settings = self.core.gmail_settings();
        let filters = filters_of(&hidden);
        let changed = {
            let address = hidden.address.clone();
            self.core
                .call(async move {
                    settings
                        .set_address_active(account_id, &address, active, &filters)
                        .await
                })
                .await?
        };
        let Permitted::Done(filters) = changed else {
            return Ok(Permitted::NeedsPermission);
        };
        self.change_hidden_address(&hidden.address, |h| {
            h.active = active;
            h.trash_filter = filters.trash;
        });
        Ok(Permitted::Done(()))
    }

    /// Removes an alias and its Gmail filters. Mail to it then arrives like
    /// any other mail.
    pub async fn delete_hidden_address(
        self: &Rc<Self>,
        address: &str,
    ) -> anyhow::Result<Permitted<()>> {
        let hidden = self.hidden_address(address)?;
        let account_id = self.hidden_account(&hidden)?;
        let settings = self.core.gmail_settings();
        let filters = filters_of(&hidden);
        let dropped = self
            .core
            .call(async move { settings.unhide_address(account_id, &filters).await })
            .await?;
        if dropped == Permitted::NeedsPermission {
            return Ok(Permitted::NeedsPermission);
        }
        if let Some(app) = self.app.upgrade() {
            app.update_settings(|s| s.hidden_addresses.retain(|h| h.address != hidden.address));
        }
        Ok(Permitted::Done(()))
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

    fn hidden_address(&self, address: &str) -> anyhow::Result<HiddenAddress> {
        hide_my_email::is_alias(address)
            .then(|| self.hidden_addresses())
            .into_iter()
            .flatten()
            .find(|h| h.address.eq_ignore_ascii_case(address))
            .ok_or_else(|| anyhow!("{address} is not a Hide My Email address"))
    }

    /// The connected account an alias belongs to.
    fn hidden_account(&self, hidden: &HiddenAddress) -> anyhow::Result<AccountId> {
        self.accounts
            .borrow()
            .iter()
            .find(|a| a.email.eq_ignore_ascii_case(&hidden.account))
            .map(|a| a.id)
            .filter(|id| self.core.account(*id).is_some())
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

/// The Gmail filters an alias was saved with.
fn filters_of(hidden: &HiddenAddress) -> HiddenFilters {
    HiddenFilters {
        label: hidden.label_filter.clone(),
        trash: hidden.trash_filter.clone(),
    }
}
