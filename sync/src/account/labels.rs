//! Creating, renaming, and deleting a person's server mailboxes, which
//! Gmail calls labels.

use std::collections::BTreeSet;

use mailrs_domain::{ChangeEvent, Label, LabelKind, RemoteMailbox};
use mailrs_gmail::{LabelColor, is_reserved_label_name};
use mailrs_store::{labels, mailboxes};

use super::AccountSync;
use crate::{MailBackend, SyncError};

impl AccountSync {
    /// Lists the server's mailboxes and brings the stored ones in line: a
    /// mailbox made, renamed or recoloured elsewhere is stored, and one
    /// deleted elsewhere leaves the store and the mail filed under it. One
    /// call, 1 quota unit on Gmail. Says whether anything changed, and only
    /// then tells the window, since the sidebar redraws on `LabelsChanged`.
    pub async fn refresh_labels(&self) -> Result<bool, SyncError> {
        let account_id = self.account_id;
        let remote = self.services.mail.mailboxes().await?;
        let (changed, threads) = self
            .db
            .write(move |c| {
                let stored = mailboxes::listed(c, account_id)?;
                let mut threads = Vec::new();
                let mut changed = false;
                for gone in stored
                    .iter()
                    .filter(|s| remote.iter().all(|r| r.id != s.id))
                {
                    threads.extend(mailboxes::delete(c, account_id, &gone.id)?);
                    changed = true;
                }
                for mailbox in remote.iter().filter(|r| !stored.contains(r)) {
                    mailboxes::upsert(c, account_id, mailbox)?;
                    changed = true;
                }
                Ok((changed, threads))
            })
            .await?;
        if changed {
            self.emit(ChangeEvent::LabelsChanged { account_id });
            self.emit_threads(threads.into_iter().collect());
        }
        Ok(changed)
    }

    /// Creates a label in Gmail and stores it. Slashes nest it under
    /// another label, as in Gmail: "Work/Clients".
    pub async fn create_label(&self, name: &str) -> Result<Label, SyncError> {
        let name = name.trim();
        if is_reserved_label_name(name) {
            return Err(SyncError::ReservedLabel(name.to_string()));
        }
        let remote = self.services.mail.create_mailbox(name).await?;
        let label = self.user_label(&remote);
        let account_id = self.account_id;
        self.db
            .write(move |c| mailboxes::upsert(c, account_id, &remote))
            .await?;
        self.emit(ChangeEvent::LabelsChanged {
            account_id: self.account_id,
        });
        Ok(label)
    }

    /// Renames a label, and the labels nested under it along with it.
    pub async fn rename_label(&self, id: &str, name: &str) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let all = self
            .db
            .read(move |c| labels::list_labels(c, account_id))
            .await?;
        let Some(old) = all.iter().find(|l| l.id == id).map(|l| l.name.clone()) else {
            return Ok(());
        };
        let name = name.trim().to_string();
        if is_reserved_label_name(&name) {
            return Err(SyncError::ReservedLabel(name));
        }
        let prefix = format!("{old}/");
        let mut renamed = vec![self.services.mail.rename_mailbox(id, &name).await?];
        for child in all.iter().filter(|l| l.name.starts_with(&prefix)) {
            let child_name = format!("{name}/{}", &child.name[prefix.len()..]);
            renamed.push(
                self.services
                    .mail
                    .rename_mailbox(&child.id, &child_name)
                    .await?,
            );
        }
        self.db
            .write(move |c| {
                for mailbox in &renamed {
                    mailboxes::upsert(c, account_id, mailbox)?;
                }
                Ok(())
            })
            .await?;
        self.emit(ChangeEvent::LabelsChanged { account_id });
        Ok(())
    }

    /// Gives a label one of Gmail's colours.
    pub async fn set_label_color(&self, id: &str, color: LabelColor) -> Result<(), SyncError> {
        let remote = self.services.mail.set_mailbox_color(id, &color).await?;
        let account_id = self.account_id;
        self.db
            .write(move |c| mailboxes::upsert(c, account_id, &remote))
            .await?;
        self.emit(ChangeEvent::LabelsChanged {
            account_id: self.account_id,
        });
        Ok(())
    }

    /// How many conversations in the whole mailbox carry the label.
    pub async fn label_threads(&self, id: &str) -> Result<u64, SyncError> {
        Ok(self.services.mail.mailbox_threads(id).await?)
    }

    /// Deletes a label. Its mail stays, without the label.
    pub async fn delete_label(&self, id: &str) -> Result<(), SyncError> {
        self.services.mail.delete_mailbox(id).await?;
        let (account_id, key) = (self.account_id, id.to_string());
        let threads = self
            .db
            .write(move |c| mailboxes::delete(c, account_id, &key))
            .await?;
        self.emit(ChangeEvent::LabelsChanged { account_id });
        self.emit_threads(threads.into_iter().collect::<BTreeSet<_>>());
        Ok(())
    }

    /// A person's mailbox as the label the window and the assistant read.
    fn user_label(&self, remote: &RemoteMailbox) -> Label {
        Label {
            account_id: self.account_id,
            id: remote.id.clone(),
            name: remote.name.clone(),
            kind: LabelKind::User,
            color: remote.color.clone(),
        }
    }
}
