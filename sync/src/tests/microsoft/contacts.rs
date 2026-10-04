use crate::ContactsService;

#[tokio::test]
async fn contacts_are_not_wired_yet() {
    let h = super::outlook().await;
    let contacts = h.sync.services().contacts.clone().unwrap();
    assert!(matches!(contacts.connections(None, None).await, Err(crate::BackendError::Unsupported)));
}
