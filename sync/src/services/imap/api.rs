//! The IMAP and SMTP calls the adapter makes, as traits, so tests and the
//! demo hand it `FakeImap` and `FakeSmtp` in place of a server. The
//! clients in `mailrs_imap` serve them one call to one method. Mailbox
//! names are the server's, in modified UTF-7 (`mailrs_imap::utf7`).

use std::time::Duration;

use mailrs_imap::{
    AppendUid, BodyStructure, Capabilities, CopyUid, Fetched, FlagsOf, ImapClient, ImapError,
    Listed, Selected, Since, SmtpClient, UidSet, Woke,
};

/// An account's IMAP server.
pub trait ImapApi: Send + Sync + 'static {
    /// What the server offers after login.
    fn capabilities(&self) -> impl Future<Output = Result<Capabilities, ImapError>> + Send;

    /// Every mailbox, parents that hold no mail among them.
    fn list(&self) -> impl Future<Output = Result<Vec<Listed>, ImapError>> + Send;

    /// Selects `mailbox` (with QRESYNC parameters when given) and reports its state and, under QRESYNC, what changed.
    /// Without QRESYNC, or when `since` names another UIDVALIDITY, `since`
    /// goes unused and `vanished` and `changed` stay empty.
    fn select(
        &self,
        mailbox: &str,
        since: Option<Since>,
    ) -> impl Future<Output = Result<Selected, ImapError>> + Send;

    /// The flags of the messages in `uids`, only those whose MODSEQ passed
    /// `changed_since` when it is given. `changed_since` needs CONDSTORE:
    /// without it the answer is `ImapError::Unsupported("CONDSTORE")`.
    fn flags(
        &self,
        mailbox: &str,
        uids: &UidSet,
        changed_since: Option<u64>,
    ) -> impl Future<Output = Result<Vec<FlagsOf>, ImapError>> + Send;

    /// The UIDs matching `keys`, IMAP SEARCH keys such as
    /// `FROM "ann" SINCE 1-Feb-2026`, lowest first. A quoted string may
    /// hold any text.
    fn search(
        &self,
        mailbox: &str,
        keys: &str,
    ) -> impl Future<Output = Result<Vec<u32>, ImapError>> + Send;

    /// Flags, dates, size and headers of the messages in `uids`. Only UIDs
    /// in the set come back, whatever the server does with `n:*`.
    fn headers(
        &self,
        mailbox: &str,
        uids: &UidSet,
    ) -> impl Future<Output = Result<Vec<Fetched>, ImapError>> + Send;

    /// `BODY.PEEK[<section>]`: `""` for the whole message, `"HEADER"`, or
    /// a part path such as `"1.2"`, whose bytes come still
    /// transfer-encoded for [`BodyStructure::decode`]. `None` when the
    /// mailbox holds no message with that UID.
    fn body(
        &self,
        mailbox: &str,
        uid: u32,
        section: &str,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, ImapError>> + Send;

    /// The message's BODYSTRUCTURE. `None` when there is no such UID.
    fn structure(
        &self,
        mailbox: &str,
        uid: u32,
    ) -> impl Future<Output = Result<Option<BodyStructure>, ImapError>> + Send;

    /// Adds `flags` to `uids`, or takes them away. A keyword the mailbox's
    /// PERMANENTFLAGS does not allow is dropped without an error, as
    /// servers do.
    fn store(
        &self,
        mailbox: &str,
        uids: &UidSet,
        add: bool,
        flags: &[String],
    ) -> impl Future<Output = Result<(), ImapError>> + Send;

    /// `UID MOVE`. `ImapError::Unsupported("MOVE")` without MOVE; the
    /// pairs of old and new UIDs with UIDPLUS, `None` without.
    fn move_to(
        &self,
        mailbox: &str,
        uids: &UidSet,
        to: &str,
    ) -> impl Future<Output = Result<Option<CopyUid>, ImapError>> + Send;

    /// `UID COPY`, with the pairs of UIDs under UIDPLUS.
    fn copy_to(
        &self,
        mailbox: &str,
        uids: &UidSet,
        to: &str,
    ) -> impl Future<Output = Result<Option<CopyUid>, ImapError>> + Send;

    /// Expunges those of `uids` that carry `\Deleted`, and no others.
    /// `ImapError::Unsupported("UIDPLUS")` without UIDPLUS.
    fn expunge(
        &self,
        mailbox: &str,
        uids: &UidSet,
    ) -> impl Future<Output = Result<(), ImapError>> + Send;

    /// Files `raw` in `mailbox` with `flags`; its UID under UIDPLUS.
    fn append(
        &self,
        mailbox: &str,
        flags: &[String],
        raw: &[u8],
    ) -> impl Future<Output = Result<Option<AppendUid>, ImapError>> + Send;

    fn create(&self, mailbox: &str) -> impl Future<Output = Result<(), ImapError>> + Send;

    fn rename(&self, from: &str, to: &str) -> impl Future<Output = Result<(), ImapError>> + Send;

    fn delete(&self, mailbox: &str) -> impl Future<Output = Result<(), ImapError>> + Send;

    /// Waits in IDLE on `mailbox` until the server reports a change or `limit` passes.
    /// `ImapError::Unsupported("IDLE")` without IDLE.
    fn idle(
        &self,
        mailbox: &str,
        limit: Duration,
    ) -> impl Future<Output = Result<Woke, ImapError>> + Send;
}

/// An account's SMTP submission server.
pub trait Submit: Send + Sync + 'static {
    /// Hands `raw` to the server for the addresses in `to`, Bcc included,
    /// with `from` as the envelope sender.
    fn submit(
        &self,
        from: &str,
        to: &[String],
        raw: &[u8],
    ) -> impl Future<Output = Result<(), ImapError>> + Send;
}

impl ImapApi for ImapClient {
    async fn capabilities(&self) -> Result<Capabilities, ImapError> {
        ImapClient::capabilities(self).await
    }

    async fn list(&self) -> Result<Vec<Listed>, ImapError> {
        ImapClient::list(self).await
    }

    async fn select(&self, mailbox: &str, since: Option<Since>) -> Result<Selected, ImapError> {
        ImapClient::select(self, mailbox, since.as_ref()).await
    }

    async fn flags(
        &self,
        mailbox: &str,
        uids: &UidSet,
        changed_since: Option<u64>,
    ) -> Result<Vec<FlagsOf>, ImapError> {
        ImapClient::flags(self, mailbox, uids, changed_since).await
    }

    async fn search(&self, mailbox: &str, keys: &str) -> Result<Vec<u32>, ImapError> {
        ImapClient::search(self, mailbox, keys).await
    }

    async fn headers(&self, mailbox: &str, uids: &UidSet) -> Result<Vec<Fetched>, ImapError> {
        ImapClient::headers(self, mailbox, uids).await
    }

    async fn body(
        &self,
        mailbox: &str,
        uid: u32,
        section: &str,
    ) -> Result<Option<Vec<u8>>, ImapError> {
        ImapClient::body(self, mailbox, uid, section).await
    }

    async fn structure(&self, mailbox: &str, uid: u32) -> Result<Option<BodyStructure>, ImapError> {
        ImapClient::structure(self, mailbox, uid).await
    }

    async fn store(
        &self,
        mailbox: &str,
        uids: &UidSet,
        add: bool,
        flags: &[String],
    ) -> Result<(), ImapError> {
        ImapClient::store(self, mailbox, uids, add, flags).await
    }

    async fn move_to(
        &self,
        mailbox: &str,
        uids: &UidSet,
        to: &str,
    ) -> Result<Option<CopyUid>, ImapError> {
        ImapClient::move_to(self, mailbox, uids, to).await
    }

    async fn copy_to(
        &self,
        mailbox: &str,
        uids: &UidSet,
        to: &str,
    ) -> Result<Option<CopyUid>, ImapError> {
        ImapClient::copy_to(self, mailbox, uids, to).await
    }

    async fn expunge(&self, mailbox: &str, uids: &UidSet) -> Result<(), ImapError> {
        ImapClient::expunge(self, mailbox, uids).await
    }

    async fn append(
        &self,
        mailbox: &str,
        flags: &[String],
        raw: &[u8],
    ) -> Result<Option<AppendUid>, ImapError> {
        ImapClient::append(self, mailbox, flags, raw).await
    }

    async fn create(&self, mailbox: &str) -> Result<(), ImapError> {
        ImapClient::create(self, mailbox).await
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), ImapError> {
        ImapClient::rename(self, from, to).await
    }

    async fn delete(&self, mailbox: &str) -> Result<(), ImapError> {
        ImapClient::delete(self, mailbox).await
    }

    async fn idle(&self, mailbox: &str, limit: Duration) -> Result<Woke, ImapError> {
        ImapClient::idle(self, mailbox, limit).await
    }
}

impl Submit for SmtpClient {
    async fn submit(&self, from: &str, to: &[String], raw: &[u8]) -> Result<(), ImapError> {
        SmtpClient::submit(self, from, to, raw).await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mailrs_discover::{Security, Server, UserName};
    use mailrs_imap::{ImapClient, Login, SmtpClient, UidSet};

    use super::{ImapApi, Submit};

    #[test]
    fn the_real_clients_serve_the_traits() {
        fn serves<I: ImapApi, S: Submit>() {}
        serves::<mailrs_imap::ImapClient, mailrs_imap::SmtpClient>();
    }

    /// Neither client dials until a call runs, so building the futures
    /// here needs no connection. Calling through `ImapApi`/`Submit` by
    /// fully qualified syntax, rather than `client.method()`, reaches the
    /// trait method instead of the client's own same-named one: the
    /// property under test is that delegating one call to one method
    /// keeps every future the trait promises `Send`.
    #[test]
    fn every_future_a_trait_method_returns_is_send() {
        fn sendable<T: Send>(_: T) {}
        let server = Server {
            host: "imap.example".into(),
            port: 993,
            security: Security::Tls,
            user_name: UserName::Address,
        };
        let login = Login::new("user", "password");
        let client = ImapClient::new(server.clone(), login.clone());
        let smtp = SmtpClient::new(&server, &login).expect("a client that has not dialed yet");
        let uids = UidSet::from_uid(1);

        sendable(ImapApi::capabilities(&client));
        sendable(ImapApi::list(&client));
        sendable(ImapApi::select(&client, "INBOX", None));
        sendable(ImapApi::flags(&client, "INBOX", &uids, None));
        sendable(ImapApi::search(&client, "INBOX", "ALL"));
        sendable(ImapApi::headers(&client, "INBOX", &uids));
        sendable(ImapApi::body(&client, "INBOX", 1, ""));
        sendable(ImapApi::structure(&client, "INBOX", 1));
        sendable(ImapApi::store(&client, "INBOX", &uids, true, &[]));
        sendable(ImapApi::move_to(&client, "INBOX", &uids, "Archive"));
        sendable(ImapApi::copy_to(&client, "INBOX", &uids, "Archive"));
        sendable(ImapApi::expunge(&client, "INBOX", &uids));
        sendable(ImapApi::append(&client, "INBOX", &[], b"raw"));
        sendable(ImapApi::create(&client, "INBOX"));
        sendable(ImapApi::rename(&client, "INBOX", "Old"));
        sendable(ImapApi::delete(&client, "INBOX"));
        sendable(ImapApi::idle(&client, "INBOX", Duration::from_secs(1)));
        sendable(Submit::submit(&smtp, "from@example", &[], b"raw"));
    }
}
