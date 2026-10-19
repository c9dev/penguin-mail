//! How full an IMAP account is, from the quota root its INBOX falls under
//! (RFC 9208). A server that does not list QUOTA is never asked.

use super::{Imap, ImapApi, Submit};
use crate::BackendError;
use crate::services::Storage;

/// RFC 9208 counts STORAGE in units of 1024 octets.
const KIB: u64 = 1024;

impl<I: ImapApi, S: Submit> Imap<I, S> {
    pub(super) async fn space(&self) -> Result<Option<Storage>, BackendError> {
        if !self.capabilities_now().await?.quota {
            return Ok(None);
        }
        let uses = self.api.quota_root("INBOX").await?;
        Ok(from_quotas(&uses))
    }
}

/// The tightest byte quota on INBOX, converting IMAP's KiB units to bytes.
pub(crate) fn from_quotas(uses: &[mailrs_imap::QuotaUse]) -> Option<Storage> {
    uses.iter()
        .filter(|u| u.resource == "STORAGE" && u.limit > 0)
        .min_by_key(|u| u.limit.saturating_sub(u.usage))
        .and_then(|u| {
            Storage::new(
                u.usage.saturating_mul(KIB),
                u.limit.saturating_mul(KIB),
                None,
            )
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mailrs_imap::QuotaUse;

    use super::super::Imap;
    use crate::MailBackend;
    use crate::fake::{FakeImap, FakeSmtp};
    use crate::services::{ImapSettings, Storage};

    /// Wraps `imap` and keeps it, so a test can look at its calls.
    fn services(imap: &Arc<FakeImap>) -> Imap<FakeImap, FakeSmtp> {
        let settings = ImapSettings {
            address: "me@example.com".into(),
            provider_name: "Dovecot".into(),
            files_sent_mail: false,
            window_days: 30,
        };
        Imap::new(Arc::clone(imap), Arc::new(FakeSmtp::default()), settings)
    }

    fn quota(resource: &str, usage: u64, limit: u64) -> QuotaUse {
        QuotaUse {
            root: "".into(),
            resource: resource.into(),
            usage,
            limit,
        }
    }

    #[tokio::test]
    async fn storage_is_the_inbox_roots_storage_in_bytes() {
        let imap = Arc::new(FakeImap::new());
        imap.with(|s| s.quota = vec![quota("MESSAGE", 3, 100), quota("STORAGE", 512, 1024)]);
        assert_eq!(
            services(&imap).storage().await.unwrap(),
            Storage::new(512 * 1024, 1024 * 1024, None)
        );
    }

    #[tokio::test]
    async fn a_server_without_quota_is_not_asked() {
        let imap = Arc::new(FakeImap::new());
        imap.with(|s| s.capabilities.quota = false);
        let services = services(&imap);
        assert_eq!(services.storage().await.unwrap(), None);
        assert_eq!(imap.calls_to("getquotaroot"), 0);
    }

    #[tokio::test]
    async fn a_zero_limit_reads_as_no_storage() {
        let imap = Arc::new(FakeImap::new());
        imap.with(|s| s.quota = vec![quota("STORAGE", 5, 0)]);
        assert_eq!(services(&imap).storage().await.unwrap(), None);
    }

    #[tokio::test]
    async fn usage_past_the_limit_reads_as_full() {
        let imap = Arc::new(FakeImap::new());
        imap.with(|s| s.quota = vec![quota("STORAGE", 2000, 1000)]);
        assert!(services(&imap).storage().await.unwrap().unwrap().full());
    }

    #[tokio::test]
    async fn huge_numbers_saturate_instead_of_overflowing() {
        let imap = Arc::new(FakeImap::new());
        imap.with(|s| s.quota = vec![quota("STORAGE", u64::MAX, u64::MAX)]);
        let storage = services(&imap).storage().await.unwrap().unwrap();
        assert_eq!((storage.used, storage.limit), (u64::MAX, u64::MAX));
    }

    #[tokio::test]
    async fn the_nearest_full_root_wins() {
        let imap = Arc::new(FakeImap::new());
        imap.with(|s| s.quota = vec![quota("STORAGE", 10, 1000), quota("STORAGE", 90, 100)]);
        assert_eq!(
            services(&imap).storage().await.unwrap(),
            Storage::new(90 * 1024, 100 * 1024, None)
        );
    }
}
