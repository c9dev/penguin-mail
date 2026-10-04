use crate::MailBackend;

#[tokio::test]
async fn sending_is_not_wired_yet() {
    let h = super::outlook().await;
    let sent = h.sync.services().mail.send(b"Subject: x\r\n\r\n", None).await;
    assert!(matches!(sent, Err(crate::BackendError::Unsupported)));
}
