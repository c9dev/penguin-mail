//! Creating, renaming, and deleting Gmail labels.

use std::collections::BTreeSet;

use mailrs_domain::{ChangeEvent, Label, LabelKind};
use mailrs_gmail::RemoteLabel;
use mailrs_store::labels;

use super::AccountSync;
use crate::{GmailApi, SyncError};

impl<G: GmailApi> AccountSync<G> {
    /// Creates a label in Gmail and stores it. Slashes nest it under
    /// another label, as in Gmail: "Work/Clients".
    pub async fn create_label(&self, name: &str) -> Result<Label, SyncError> {
        let remote = self.api.create_label(name.trim()).await?;
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
        let prefix = format!("{old}/");
        let mut renamed = vec![self.api.rename_label(id, &name).await?];
        for child in all.iter().filter(|l| l.name.starts_with(&prefix)) {
            let child_name = format!("{name}/{}", &child.name[prefix.len()..]);
            renamed.push(self.api.rename_label(&child.id, &child_name).await?);
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

    /// Deletes a label. Its mail stays, without the label.
    pub async fn delete_label(&self, id: &str) -> Result<(), SyncError> {
        self.api.delete_label(id).await?;
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
        }
    }
}
