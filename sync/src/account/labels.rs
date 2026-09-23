//! Creating, renaming, and deleting Gmail labels.

use std::collections::BTreeSet;

use mailrs_domain::{AccountId, ChangeEvent, Label, LabelKind};
use mailrs_gmail::{LabelColor, RemoteLabel};
use mailrs_store::labels;

use super::AccountSync;
use mailrs_gmail::is_reserved_label_name;

use crate::{MailBackend, SyncError};

impl AccountSync {
    /// Lists Gmail's labels and brings the stored ones in line: a label
    /// made, renamed or recoloured elsewhere is stored, and one deleted
    /// elsewhere leaves the store and the mail that carried it. One call,
    /// 1 quota unit. Says whether anything changed, and only then tells
    /// the window, since the sidebar redraws on `LabelsChanged`.
    pub async fn refresh_labels(&self) -> Result<bool, SyncError> {
        let account_id = self.account_id;
        let remote = domain_labels(account_id, &self.services.mail.labels().await?);
        let (changed, threads) = self
            .db
            .write(move |c| {
                let stored = labels::list_labels(c, account_id)?;
                let mut threads = Vec::new();
                let mut changed = false;
                for gone in stored
                    .iter()
                    .filter(|s| remote.iter().all(|r| r.id != s.id))
                {
                    threads.extend(labels::delete_label(c, account_id, &gone.id)?);
                    changed = true;
                }
                for label in remote.iter().filter(|r| !stored.contains(r)) {
                    labels::upsert_label(c, label)?;
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
        let remote = self.services.mail.create_label(name).await?;
        let label = self.user_label(&remote);
        let stored = label.clone();
        self.db
            .write(move |c| labels::upsert_label(c, &stored))
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
        let mut renamed = vec![self.services.mail.rename_label(id, &name).await?];
        for child in all.iter().filter(|l| l.name.starts_with(&prefix)) {
            let child_name = format!("{name}/{}", &child.name[prefix.len()..]);
            renamed.push(self.services.mail.rename_label(&child.id, &child_name).await?);
        }
        let stored: Vec<Label> = renamed.iter().map(|r| self.user_label(r)).collect();
        self.db
            .write(move |c| {
                for label in &stored {
                    labels::upsert_label(c, label)?;
                }
                Ok(())
            })
            .await?;
        self.emit(ChangeEvent::LabelsChanged { account_id });
        Ok(())
    }

    /// Gives a label one of Gmail's colours.
    pub async fn set_label_color(&self, id: &str, color: LabelColor) -> Result<(), SyncError> {
        let remote = self.services.mail.set_label_color(id, &color).await?;
        let label = self.user_label(&remote);
        self.db
            .write(move |c| labels::upsert_label(c, &label))
            .await?;
        self.emit(ChangeEvent::LabelsChanged {
            account_id: self.account_id,
        });
        Ok(())
    }

    /// How many conversations in the whole mailbox carry the label.
    pub async fn label_threads(&self, id: &str) -> Result<u64, SyncError> {
        Ok(self.services.mail.label_threads(id).await?)
    }

    /// Deletes a label. Its mail stays, without the label.
    pub async fn delete_label(&self, id: &str) -> Result<(), SyncError> {
        self.services.mail.delete_label(id).await?;
        let (account_id, key) = (self.account_id, id.to_string());
        let threads = self
            .db
            .write(move |c| labels::delete_label(c, account_id, &key))
            .await?;
        self.emit(ChangeEvent::LabelsChanged { account_id });
        self.emit_threads(threads.into_iter().collect::<BTreeSet<_>>());
        Ok(())
    }

    fn user_label(&self, remote: &RemoteLabel) -> Label {
        Label {
            account_id: self.account_id,
            id: remote.id.clone(),
            name: remote.name.clone(),
            kind: LabelKind::User,
            color: remote.color.as_ref().map(|c| c.background_color.clone()),
        }
    }
}

/// Gmail's labels as the store keeps them.
pub(super) fn domain_labels(account_id: AccountId, remote: &[RemoteLabel]) -> Vec<Label> {
    remote
        .iter()
        .map(|l| Label {
            account_id,
            id: l.id.clone(),
            name: l.name.clone(),
            kind: if l.kind.as_deref() == Some("system") {
                LabelKind::System
            } else {
                LabelKind::User
            },
            color: l.color.as_ref().map(|c| c.background_color.clone()),
        })
        .collect()
}

/// Whether `id` names a label a person made. Gmail numbers those
/// `Label_1`, `Label_2` and so on; its own labels have fixed names, some
/// of which `labels.list` never returns, so only a person's label that
/// the store lacks says the list has changed.
pub(super) fn is_user_label(id: &str) -> bool {
    id.starts_with("Label_")
}
