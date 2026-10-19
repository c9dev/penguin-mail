//! Read receipts (RFC 8098): sent only when the person says so, never on
//! their own. The server's own way where it has one (`MDN/send`), a
//! `multipart/report` sent as ordinary mail everywhere else. Either answer
//! is kept on this computer, which is what stops the question coming
//! back, and marks `$mdnsent` on IMAP and JMAP for the person's other
//! mail programs.
//!
//! A receipt goes out at most once. The record is written before the send,
//! and only by the first answer, so a lost answer, a restart or a second
//! click cannot send a second one. A send the server refused outright
//! takes the record back, so the person can try again; one that may have
//! gone keeps it. A receipt also goes only to the
//! sender's own address: one to anyone else tells a stranger the mail was
//! opened.

use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address as MimeAddress;
use mail_builder::headers::content_type::ContentType;
use mail_builder::mime::MimePart;
use mailrs_domain::mailbox::keyword::MDN_SENT;
use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{EpochMillis, MessageMeta};
use mailrs_store::receipts::{self, Answered};
use mailrs_store::{accounts, bodies, messages};

use super::AccountSync;
use crate::{AnyMail, BackendError, IdentityService, MailOp, ReceiptOut, SyncError, now_millis};

/// "Read: Allotment rota".
fn receipt_subject(subject: &str) -> String {
    fill(&gettext("Read: {subject}"), &[("subject", subject)])
}

/// What the sender reads. RFC 8098 asks that a receipt claim no more than
/// that the message was shown.
fn receipt_text(me: &str) -> String {
    fill(
        &gettext(
            "Your message to {address} was shown on screen. This receipt does not say whether it was read or understood.",
        ),
        &[("address", me)],
    )
}

/// The receipt as mail: the person's words, then the machine's.
fn receipt_mail(
    receipt: &ReceiptOut,
    original: Option<&str>,
    now: EpochMillis,
) -> Result<Vec<u8>, String> {
    let mut report = format!(
        "Reporting-UA: Penguin Mail\r\nFinal-Recipient: rfc822;{}\r\n",
        receipt.from
    );
    if let Some(id) = original
        .map(|id| id.trim().trim_matches(['<', '>']))
        .filter(|id| !id.is_empty())
    {
        report.push_str(&format!("Original-Message-ID: <{id}>\r\n"));
    }
    report.push_str("Disposition: manual-action/MDN-sent-manually; displayed\r\n");
    let domain = receipt
        .from
        .rsplit_once('@')
        .map_or("penguin-mail.local", |(_, d)| d);
    MessageBuilder::new()
        .from(MimeAddress::from(receipt.from.clone()))
        .to(MimeAddress::from(receipt.to.clone()))
        .subject(receipt.subject.clone())
        .date(now / 1000)
        .message_id(format!("{}.mdn@{domain}", mailrs_gmail::random_token(12)))
        .body(MimePart::new(
            ContentType::new("multipart/report")
                .attribute("report-type", "disposition-notification"),
            vec![
                MimePart::new("text/plain", receipt.text.clone()),
                MimePart::new("message/disposition-notification", report),
            ],
        ))
        .write_to_vec()
        .map_err(|err| err.to_string())
}

/// Whether a failed send of a receipt proves it never reached the server:
/// a refusal, or a sign-in or permission the server turned down before it
/// acted. A lost connection proves nothing, since the request may have
/// gone.
fn nothing_went(err: &SyncError) -> bool {
    matches!(
        err,
        SyncError::Backend(
            BackendError::Refused(_)
                | BackendError::NeedsReauth
                | BackendError::NeedsPermission
                | BackendError::NotFound
                | BackendError::RateLimited(_)
                | BackendError::Unsupported
        )
    )
}

impl AccountSync {
    /// Whether the person already answered the receipt `message_id` asks
    /// for, here or, where the server keeps `$mdnsent`, anywhere.
    pub async fn receipt_answered(&self, message_id: &str) -> Result<bool, SyncError> {
        let (account, id) = (self.account_id, message_id.to_string());
        Ok(self
            .db
            .read(move |c| {
                if receipts::answered(c, account, &id)?.is_some() {
                    return Ok(true);
                }
                Ok(messages::by_ids(c, account, std::slice::from_ref(&id))?
                    .iter()
                    .any(|m| m.held.keywords.iter().any(|k| k == MDN_SENT)))
            })
            .await?)
    }

    /// Whether the receipt `message_id` asks for still waits on the
    /// person: nobody answered it, and the message reached one of the
    /// account's addresses or masked addresses, which the receipt would go
    /// from. Mail that reached none of them, through a list or a Bcc, asks
    /// nothing: a receipt from another address would name it to the sender.
    pub async fn receipt_open(&self, message_id: &str) -> Result<bool, SyncError> {
        if self.receipt_answered(message_id).await? {
            return Ok(false);
        }
        let (account, id) = (self.account_id, message_id.to_string());
        let meta = self
            .db
            .read(move |c| messages::by_ids(c, account, std::slice::from_ref(&id)))
            .await?;
        let Some(meta) = meta.into_iter().next() else {
            return Ok(false);
        };
        Ok(self.receipt_sender(&meta).await.is_some())
    }

    /// The address a receipt for `meta` goes from: the one of the
    /// account's addresses or masked addresses the message was delivered
    /// to, as for a reply. A server that would not list them leaves only
    /// the account's own address.
    async fn receipt_sender(&self, meta: &MessageMeta) -> Option<String> {
        let account = self.account_id;
        let mut mine: Vec<String> = match self.db.read(move |c| accounts::account(c, account)).await
        {
            Ok(Some(me)) => vec![me.email],
            _ => Vec::new(),
        };
        match self.services.identities.identities().await {
            Ok(listed) => mine.extend(listed.into_iter().map(|a| a.email)),
            Err(_) => tracing::info!(
                account,
                "could not list the account's addresses for a receipt"
            ),
        }
        if let AnyMail::Jmap(jmap) = self.mail.untranslated()
            && jmap.masked_email()
        {
            match jmap.masked_addresses().await {
                Ok(masked) => mine.extend(masked.into_iter().map(|m| m.email)),
                Err(_) => tracing::info!(
                    account,
                    "could not list the account's masked addresses for a receipt"
                ),
            }
        }
        crate::masked::delivered_to(meta.to.iter().chain(&meta.cc), &mine)
    }

    /// Sends the receipt `message_id` asks for, or declines it, and
    /// records the answer. A message already answered, here or on the
    /// server, is left alone. The receipt goes from the address the
    /// message was delivered to, and a message that reached none of the
    /// account's addresses is refused.
    pub async fn answer_receipt(&self, message_id: &str, send: bool) -> Result<(), SyncError> {
        let (account, id) = (self.account_id, message_id.to_string());
        let (meta, body) = self
            .db
            .read(move |c| {
                Ok((
                    messages::by_ids(c, account, std::slice::from_ref(&id))?
                        .into_iter()
                        .next(),
                    bodies::peek_body(c, account, &id)?,
                ))
            })
            .await?;
        let meta = meta.ok_or(BackendError::NotFound)?;
        let no_receipt =
            || BackendError::Refused(gettext("This message asks for no read receipt."));
        let to = body.and_then(|b| b.receipt_to).ok_or_else(no_receipt)?;
        // Defence in depth: the card offers a receipt only to the sender.
        if !meta
            .from
            .as_ref()
            .is_some_and(|from| from.email.eq_ignore_ascii_case(&to))
        {
            return Err(no_receipt().into());
        }
        if self.receipt_answered(message_id).await? {
            return Ok(());
        }
        let me = match send {
            true => self.receipt_sender(&meta).await.ok_or_else(no_receipt)?,
            false => String::new(),
        };
        let answered = if send {
            Answered::Sent
        } else {
            Answered::Declined
        };
        let (id, now) = (message_id.to_string(), now_millis());
        // Only the answer this call keeps goes on: a second click, or the
        // card and the assistant at once, find the first one kept.
        if !self
            .db
            .write(move |c| receipts::record(c, account, &id, answered, now))
            .await?
        {
            return Ok(());
        }
        if send {
            let receipt = ReceiptOut {
                message_id: message_id.to_string(),
                from: me.clone(),
                to,
                subject: receipt_subject(&meta.subject),
                text: receipt_text(&me),
            };
            let sent = match self.mail.send_receipt(&receipt).await {
                Err(SyncError::Backend(BackendError::Unsupported)) => {
                    match receipt_mail(&receipt, meta.rfc822_msgid.as_deref(), now_millis()) {
                        Ok(raw) => self.send(raw, None, None).await.map(drop),
                        Err(_) => Err(no_receipt().into()),
                    }
                }
                sent => sent,
            };
            if let Err(err) = sent {
                // A failure that proves nothing went leaves the question
                // open, so the person can send it again. Any other may
                // follow a receipt that went, and the answer stays.
                if nothing_went(&err) {
                    let id = message_id.to_string();
                    self.db
                        .write(move |c| receipts::withdraw(c, account, &id))
                        .await?;
                }
                return Err(err);
            }
        }
        self.mark_mdn_sent(message_id).await;
        Ok(())
    }

    /// `$mdnsent` on the server, on IMAP and JMAP, for the person's other
    /// mail programs. Gmail and Outlook keep no such keyword. The record on
    /// this computer is what this app reads, so a refusal only goes to the log.
    async fn mark_mdn_sent(&self, message_id: &str) {
        // Gmail over IMAP keeps no such keyword either.
        if !matches!(
            self.mail.untranslated(),
            AnyMail::Imap(_) | AnyMail::Jmap(_)
        ) {
            return;
        }
        let op = MailOp::SetKeyword {
            keyword: MDN_SENT.to_string(),
            on: true,
        };
        let moves = self.mail.hold_moves().await;
        let marked = match moves.named(&[message_id.to_string()]).await {
            Ok(named) => moves.apply(&named, &[op]).await.is_ok(),
            Err(_) => false,
        };
        if !marked {
            // No server text, address or message id in the log.
            tracing::info!(
                account = self.account_id,
                "could not mark a message $mdnsent"
            );
        }
    }
}
