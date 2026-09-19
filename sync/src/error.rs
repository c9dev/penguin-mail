use mailrs_domain::AccountId;
use mailrs_gmail::GmailError;
use mailrs_store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Gmail(#[from] GmailError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("no sync is running for account {0}")]
    UnknownAccount(AccountId),
}
