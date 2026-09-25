use mailrs_domain::mailbox::keyword;
use mailrs_domain::{AccountState, Address, LabelKind, Memberships, MessageMeta, Role};

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
    for kind in [LabelKind::System, LabelKind::User, LabelKind::Group] {
        assert_eq!(kind.as_str().parse::<LabelKind>(), Ok(kind));
    }
}

#[test]
fn address_display_prefers_the_name() {
    let named = Address {
        name: Some("Ann Lee".into()),
        email: "ann@example.com".into(),
    };
    let bare = Address {
        name: None,
        email: "bob@example.com".into(),
    };
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
        held: Memberships {
            mailboxes: vec!["INBOX".into()],
            keywords: vec![keyword::SEEN.into()],
            categories: vec![],
        },
        roles: vec![Role::Inbox],
        list_unsubscribe: None,
        one_click: false,
    };
    assert!(!meta.is_unread());
    assert!(meta.in_role(Role::Inbox));
    meta.held.keywords.retain(|k| k != keyword::SEEN);
    assert!(meta.is_unread());
}
