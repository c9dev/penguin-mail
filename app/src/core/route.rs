//! Signing a Gmail account in through the route it chose: Online Accounts,
//! an app password or the person's own Google project, for a new address
//! or for an account that moves. A move stops the account, sends what is
//! due through the old sign-in and starts it again on the new one, and
//! runs through one setup whatever the route.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use mailrs_domain::Account;
use mailrs_domain::translate::gettext;
use mailrs_sync::goa::GoaAccount;
use mailrs_sync::route_move;
use mailrs_sync::sign_in::app_password::{
    NewGmailImap, check_gmail, gmail_imap_signed_in, read_pasted,
};
use mailrs_sync::sign_in::own_client::OwnClient;
use mailrs_sync::sign_in::{
    Browser, BrowserSignInError, Wanted, goa_link, keep_chosen_project, move_choice,
};
use mailrs_sync::{Clients, now_millis};

use super::{Core, MovePort, Reach, authorize_until_canceled, start_again};

/// The route a Gmail account chose, with what the person gave for it.
pub enum ChosenRoute {
    /// The desktop's Online Accounts account that holds the address.
    OnlineAccounts(GoaAccount),
    /// An app password as the person pasted it.
    AppPassword { pasted: String },
    /// The person's own Google project: `chosen` is the setup's client,
    /// `urls` gets Google's page, and closing the channel behind `cancel`
    /// ends the wait for the browser.
    OwnProject {
        chosen: Option<OwnClient>,
        urls: async_channel::Sender<String>,
        cancel: async_channel::Receiver<()>,
    },
}

/// Who takes the route.
pub enum Taker {
    /// The address the person typed, which no account here holds yet. An
    /// address Online Accounts names that a Gmail account holds moves
    /// instead.
    New { address: String },
    /// An account that moves to the route, keeping its id and its mail.
    Moving(Account),
}

impl Core {
    /// Signs `taker` in through `route`, and starts the account. A move
    /// checks the new sign-in before anything stops, then stops the
    /// account, sends what is due and finishes the writes under way
    /// through the old sign-in, stores the new one and starts the account
    /// again; on any error it starts again as it was. `committed` gets a
    /// message once a move can no longer be cancelled. A second move of
    /// the same account is refused while one runs.
    pub async fn take_route(
        &self,
        route: ChosenRoute,
        taker: Taker,
        committed: async_channel::Sender<()>,
    ) -> Result<Account> {
        match taker {
            Taker::Moving(account) => self.move_through(route, account, committed).await,
            Taker::New { address } => match route {
                ChosenRoute::OnlineAccounts(goa_account) => {
                    self.link_goa(goa_account, committed).await
                }
                ChosenRoute::AppPassword { pasted } => {
                    self.add_with_app_password(address, pasted).await
                }
                ChosenRoute::OwnProject {
                    chosen,
                    urls,
                    cancel,
                } => {
                    let wanted = match address.is_empty() {
                        true => Wanted::Anyone,
                        false => Wanted::Only(address),
                    };
                    self.sign_in_in_browser(Browser::Google, urls, wanted, cancel, chosen)
                        .await
                        .map(|(account, _)| account)
                }
            },
        }
    }

    /// Signs in to a Gmail account with the app password `pasted` holds:
    /// Gmail's servers take the login first, then the account, its servers
    /// and its password are kept and the account starts. Nothing is kept
    /// when the login fails.
    async fn add_with_app_password(&self, address: String, pasted: String) -> Result<Account> {
        if self.demo {
            bail!(gettext("Demo mode cannot add real accounts."));
        }
        let engine = self
            .engine
            .current()
            .ok_or_else(|| anyhow!("sync is not running"))?;
        let (db, secrets) = (self.db.clone(), self.secrets.clone());
        let reach = Reach {
            connector: self.connector(),
            demo: self.demo_mail.clone(),
        };
        self.call(async move {
            let pasted = read_pasted(&pasted);
            let checked = check_gmail(&address, pasted.secret()).await?;
            let new = NewGmailImap {
                address,
                servers: checked.servers,
                password: pasted.secret().to_string(),
            };
            let account = gmail_imap_signed_in(&db, &secrets, new, now_millis()).await?;
            start_again(&reach, &engine, &db, &account, false).await?;
            Ok::<_, anyhow::Error>(account)
        })
        .await
    }

    /// Moves `account` to `route`, as [`Core::take_route`] says.
    pub(super) async fn move_through(
        &self,
        route: ChosenRoute,
        account: Account,
        committed: async_channel::Sender<()>,
    ) -> Result<Account> {
        if self.demo {
            bail!(gettext("The demo cannot switch accounts."));
        }
        // Refused as adding an account through Online Accounts is, before
        // anything stops.
        if let ChosenRoute::OnlineAccounts(goa_account) = &route {
            goa_link::usable(goa_account)?;
        }
        let lease = self.move_leases.lease(account.id).ok_or_else(|| {
            anyhow!(gettext(
                "This account is already moving to its new sign-in."
            ))
        })?;
        let engine = self
            .engine
            .current()
            .ok_or_else(|| anyhow!("sync is not running"))?;
        let (db, secrets, outbox) = (
            self.db.clone(),
            self.secrets.clone(),
            Arc::clone(&self.outbox),
        );
        let reach = Reach {
            connector: self.connector(),
            demo: self.demo_mail.clone(),
        };
        let goa = self.goa.clone();
        // An app password sets nothing up in Google, so it leaves the
        // own-project setup where it was.
        let finishes_setup = !matches!(route, ChosenRoute::AppPassword { .. });
        let moved = self
            .call(async move {
                // Held until the move ends, even when the dialog has gone.
                let _lease = lease;
                let port = MovePort {
                    engine,
                    reach,
                    db: db.clone(),
                };
                let mover = route_move::Mover {
                    engine: &port,
                    db: &db,
                    secrets: &secrets,
                    outbox: &outbox,
                };
                let committed = move || {
                    let _ = committed.try_send(());
                };
                let moved = match route {
                    ChosenRoute::OnlineAccounts(goa_account) => {
                        goa_link::check_token(&goa, &goa_account).await?;
                        mover.to_goa(&account, &goa_account, committed).await?
                    }
                    ChosenRoute::AppPassword { pasted } => {
                        let pasted = read_pasted(&pasted);
                        let checked = check_gmail(&account.email, pasted.secret()).await?;
                        let new = NewGmailImap {
                            address: account.email.clone(),
                            servers: checked.servers,
                            password: pasted.secret().to_string(),
                        };
                        mover.to_imap(&account, new, committed).await?
                    }
                    ChosenRoute::OwnProject {
                        chosen,
                        urls,
                        cancel,
                    } => {
                        let choice = move_choice(&Clients::current().with_chosen(chosen))?;
                        let own = choice
                            .own
                            .clone()
                            .ok_or(BrowserSignInError::NoClient(Browser::Google))?;
                        let open = move |url: &str| {
                            let _ = urls.try_send(url.to_string());
                        };
                        let authorized =
                            authorize_until_canceled(&choice.oauth, open, cancel).await?;
                        let moved = mover
                            .to_own_client(&account, &own, authorized, committed)
                            .await?;
                        keep_chosen_project(&secrets, &choice).await;
                        moved
                    }
                };
                Ok::<_, anyhow::Error>(moved)
            })
            .await?;
        if finishes_setup {
            self.finish_google_setup();
        }
        Ok(moved)
    }
}
