use mailrs_gmail::{KeyringTokenStore, MemoryTokenStore, TokenStore};

fn round_trip(store: &dyn TokenStore) {
    let email = "roundtrip@example.com";
    store.delete(email).unwrap();
    assert_eq!(store.load(email).unwrap(), None);
    store.save(email, "rt-1").unwrap();
    assert_eq!(store.load(email).unwrap().as_deref(), Some("rt-1"));
    store.save(email, "rt-2").unwrap();
    assert_eq!(store.load(email).unwrap().as_deref(), Some("rt-2"));
    store.delete(email).unwrap();
    assert_eq!(store.load(email).unwrap(), None);
}

#[test]
fn memory_store_round_trips() {
    round_trip(&MemoryTokenStore::default());
}

/// Uses the desktop keyring. Run it by hand:
/// `cargo test -p mailrs-gmail --test token_store -- --ignored`
#[test]
#[ignore]
fn keyring_store_round_trips() {
    round_trip(&KeyringTokenStore::with_service("mailrs-test"));
}
