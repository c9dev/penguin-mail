#[tokio::test]
async fn writing_mail_is_not_wired_yet() {
    let h = super::outlook().await;
    let refused = crate::MailBackend::apply(&h.sync.services().mail, &["m".into()], &[crate::MailOp::Destroy]).await;
    assert!(matches!(refused, Err(u) if matches!(u.error, crate::BackendError::Unsupported)));
}
