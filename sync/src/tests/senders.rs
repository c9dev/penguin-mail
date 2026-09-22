//! What the app does about one sender: Categorize Sender.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::{
    Address, Category, Filter, FilterAction, FilterCriteria, MessageMeta, system_label,
};

use super::{Connected, Harness, harness};
use crate::fake::meta;
use crate::{MailActions, Permitted, now_millis};

fn actions(h: &Harness) -> MailActions<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    MailActions::new(Arc::new(Connected(connected)), h.db.clone())
}

/// A message from `email`, in the inbox under `category`.
fn from(email: &str, id: &str, thread: &str, category: &str) -> MessageMeta {
    MessageMeta {
        from: Some(Address {
            name: None,
            email: email.into(),
        }),
        ..meta(id, thread, now_millis(), &[system_label::INBOX, category])
    }
}

/// A rule that puts mail from `email` under `label`.
fn sorts(email: &str, label: &str) -> Filter {
    Filter {
        id: None,
        criteria: FilterCriteria {
            from: Some(email.into()),
            ..FilterCriteria::default()
        },
        action: FilterAction {
            add_label_ids: vec![label.into()],
            ..FilterAction::default()
        },
    }
}

#[tokio::test]
async fn categorizing_a_sender_moves_their_mail_and_replaces_their_rule() {
    let h = harness().await;
    let shop = "shop@example.com";
    h.fake.seed(from(shop, "a", "t1", "CATEGORY_UPDATES"));
    h.fake.seed(from(shop, "b", "t2", "CATEGORY_SOCIAL"));
    h.fake
        .seed(from("ann@example.com", "c", "t3", "CATEGORY_UPDATES"));
    h.bootstrap_all().await;
    h.sync
        .create_filter(sorts("SHOP@example.com", "CATEGORY_SOCIAL"))
        .await
        .unwrap();
    let blocked = h.sync.create_filter(Filter::block(shop)).await.unwrap();

    let done = actions(&h)
        .categorize_sender(h.account_id, shop, None, Category::Promotions)
        .await;
    assert_eq!(done.moved.done.len(), 2, "{:?}", done.moved);
    assert!(matches!(done.sorted, Ok(Permitted::Done(()))));

    for id in ["a", "b"] {
        let labels = h.labels_of(id).await;
        assert!(
            labels.contains(&"CATEGORY_PROMOTIONS".to_string()),
            "{labels:?}"
        );
        let categories = labels.iter().filter(|l| system_label::is_category(l));
        assert_eq!(categories.count(), 1, "{labels:?}");
    }
    assert!(
        h.labels_of("c")
            .await
            .contains(&"CATEGORY_UPDATES".to_string()),
        "another sender's mail stays where it was"
    );
    let rules = h.fake.with(|s| s.filters.clone());
    assert_eq!(rules.len(), 2, "{rules:?}");
    assert!(
        rules.contains(&blocked),
        "a rule that is no category rule stays"
    );
    let new = rules.iter().find(|r| r.id != blocked.id).unwrap();
    assert_eq!(new.criteria.from.as_deref(), Some(shop));
    assert_eq!(new.action.add_label_ids, ["CATEGORY_PROMOTIONS"]);
}

#[tokio::test]
async fn categorizing_without_the_settings_permission_still_moves_the_mail() {
    let h = harness().await;
    let shop = "shop@example.com";
    h.fake.seed(from(shop, "a", "t1", "CATEGORY_UPDATES"));
    h.fake
        .seed(from("ann@example.com", "c", "t3", "CATEGORY_UPDATES"));
    h.bootstrap_all().await;
    h.fake.withhold(mailrs_gmail::SETTINGS_SCOPE);

    let done = actions(&h)
        .categorize_sender(h.account_id, shop, Some("t3"), Category::Social)
        .await;
    assert!(matches!(done.sorted, Ok(Permitted::NeedsPermission)));
    assert_eq!(
        done.moved.done.len(),
        2,
        "the sender's thread and the one asked for"
    );
    for id in ["a", "c"] {
        assert!(
            h.labels_of(id)
                .await
                .contains(&"CATEGORY_SOCIAL".to_string())
        );
    }
    assert!(h.fake.with(|s| s.filters.is_empty()));
}
