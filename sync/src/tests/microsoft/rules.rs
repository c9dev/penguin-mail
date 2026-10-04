use crate::RulesService;

#[tokio::test]
async fn rules_are_not_wired_yet() {
    let h = super::outlook().await;
    let rules = h.sync.services().rules.clone().unwrap();
    assert!(matches!(rules.filters().await, Err(crate::BackendError::Unsupported)));
}
