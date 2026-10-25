//! Emptying one account's Trash, or every account named by the unified Trash.

use super::*;

impl MainWindow {
    pub(super) fn install_empty_trash(self: &Rc<Self>) {
        let action = gio::SimpleAction::new(
            "empty-trash",
            Some(&glib::VariantType::new("mx").expect("valid type")),
        );
        let weak = Rc::downgrade(self);
        action.connect_activate(move |_, parameter| {
            if let (Some(win), Some(account)) = (
                weak.upgrade(),
                parameter.and_then(|p| p.get::<Option<AccountId>>()),
            ) {
                win.empty_trash(account);
            }
        });
        self.actions.add_action(&action);
    }

    fn empty_trash(self: &Rc<Self>, account_id: Option<AccountId>) {
        if self.emptying_trash.replace(true) {
            return;
        }
        // Capture the accounts before asking; a later selection changes no scope.
        let accounts: Vec<_> = self
            .mail_accounts()
            .into_iter()
            .filter(|account| account_id.is_none_or(|id| id == account.id))
            .collect();
        if accounts.is_empty() {
            self.emptying_trash.set(false);
            return;
        }
        let names = accounts
            .iter()
            .map(|account| account.email.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let question = confirm(
            &gettext("Empty Trash?"),
            &fill(
                &gettext(
                    "Permanently delete every message in Trash for these accounts, including older mail not stored on this computer? This cannot be undone.\n\n{accounts}",
                ),
                &[("accounts", &names)],
            ),
            &gettext("Empty Trash"),
            Tone::Destructive,
        );
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if question.ask(&this.window).await {
                this.toast(&gettext("Emptying Trash…"));
                let actions = this.core.actions();
                let ids: Vec<_> = accounts.iter().map(|a| a.id).collect();
                let results = this
                    .core
                    .call(async move {
                        Ok::<_, mailrs_sync::SyncError>(actions.empty_trashes(&ids).await)
                    })
                    .await;
                this.core.forget_remote();
                this.listing.reload_remote();
                this.listing.everything();
                this.refresh_counts();
                this.refresh_open_threads(|id, _| accounts.iter().any(|a| a.id == id));
                let results = match results {
                    Ok(results) => results,
                    Err(error) => {
                        this.toast(&fill(
                            &gettext("Could not empty Trash for {account}: {reason}"),
                            &[("account", &names), ("reason", &error.to_string())],
                        ));
                        this.emptying_trash.set(false);
                        return;
                    }
                };
                let mut permission_needed = Vec::new();
                for (account, result) in accounts.into_iter().zip(results) {
                    match result {
                        Ok(Permitted::Done(outcome)) => {
                            if outcome.needs_permission {
                                permission_needed.push(account.clone());
                            }
                            match outcome.error.as_deref() {
                                Some(error) => this.toast(&fill(
                                    &gettext("Could not empty Trash for {account}: {reason}"),
                                    &[("account", &account.email), ("reason", error)],
                                )),
                                None => this.toast(&fill_plural(
                                    "Deleted {count} message from Trash for {account}",
                                    "Deleted {count} messages from Trash for {account}",
                                    outcome.deleted,
                                    &[
                                        ("account", &account.email),
                                        ("count", &outcome.deleted.to_string()),
                                    ],
                                )),
                            }
                        }
                        Ok(Permitted::NeedsPermission) => {
                            this.toast(&fill(
                                &gettext("Could not empty Trash for {account}: {reason}"),
                                &[
                                    ("account", &account.email),
                                    (
                                        "reason",
                                        &mailrs_sync::BackendError::NeedsPermission.to_string(),
                                    ),
                                ],
                            ));
                            permission_needed.push(account);
                        }
                        Err(error) => this.toast(&fill(
                            &gettext("Could not empty Trash for {account}: {reason}"),
                            &[("account", &account.email), ("reason", &error.to_string())],
                        )),
                    }
                }
                for account in permission_needed {
                    if permission::ask(
                        &this.window,
                        account.id,
                        &account.email,
                        account.provider,
                        Permission::Delete,
                        Occasion::Needed,
                    )
                    .await
                    {
                        this.grant(account.email);
                    }
                }
            }
            this.emptying_trash.set(false);
        });
    }
}
