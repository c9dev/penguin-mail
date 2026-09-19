use mailrs_domain::{AccountState, Address, LabelKind, MessageMeta};

#[test]
fn account_state_round_trips_through_strings() {
    for state in AccountState::ALL {
        assert_eq!(state.as_str().parse::<AccountState>(), Ok(state));
    }
}

#[test]
fn unknown_account_state_is_rejected() {
    assert!("sleeping".parse::<AccountState>().is_err());
}

#[test]
fn label_kind_round_trips_through_strings() {
    for kind in [LabelKind::System, LabelKind::User] {
        assert_eq!(kind.as_str().parse::<LabelKind>(), Ok(kind));
    }
}

#[test]
fn address_display_prefers_the_name() {
    let named = Address { name: Some("Ann Lee".into()), email: "ann@example.com".into() };
    let bare = Address { name: None, email: "bob@example.com".into() };
    assert_eq!(named.display(), "Ann Lee");
    assert_eq!(bare.display(), "bob@example.com");
}

#[test]
fn unread_follows_the_unread_label() {
    let mut meta = MessageMeta {
        account_id: 1,
        id: "m1".into(),
        thread_id: "t1".into(),
        rfc822_msgid: None,
        from: None,
        to: vec![],
        cc: vec![],
        subject: String::new(),
        date: 0,
        snippet: String::new(),
        size: 0,
        has_attachments: false,
        label_ids: vec!["INBOX".into()],
    };
    assert!(!meta.is_unread());
    assert!(meta.has_label("INBOX"));
    meta.label_ids.push("UNREAD".into());
    assert!(meta.is_unread());
}
