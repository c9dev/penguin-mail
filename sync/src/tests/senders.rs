//! What the app does about one sender: Categorize Sender, and leaving
//! their mailing list.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::{
    Address, Category, Filter, FilterAction, FilterCriteria, MailSet, MessageMeta, system_label,
};
use mailrs_store::unsubscribes::{self, How};

use super::{Connected, Harness, harness};
use crate::fake::meta;
use crate::unsubscribe::choose;
use crate::{
    Leave, MailActions, OneClick, Permitted, RulesService, SyncError, Unsubscribe, now_millis,
};

#[tokio::test]
async fn a_one_click_list_hears_from_the_app_at_once() {
    let h = harness().await;
    let url = "https://news.example/u/1";
    let how = choose(&format!("<{url}>"), true).unwrap();

    let left = actions(&h).unsubscribe(h.account_id, how).await.unwrap();
    assert_eq!(left, Leave::Done);
    assert_eq!(h.one_click.posted(), [url]);
}

#[tokio::test]
async fn a_one_click_list_that_refuses_says_so() {
    let h = harness().await;
    h.one_click.refuse_next();
    let how = Unsubscribe::OneClick("https://news.example/u/1".into());

    let err = actions(&h).unsubscribe(h.account_id, how.clone()).await;
    let said = err.expect_err("the list refused").to_string();
    // The list's own server answered, so the message names it and not
    // Gmail, which took no part.
    assert_eq!(said, "news.example refused the request to unsubscribe (HTTP 500)");
    assert!(h.one_click.posted().is_empty());
    let nobody = actions(&h).unsubscribe(99, how).await;
    assert!(matches!(nobody, Err(SyncError::UnknownAccount(99))));
}

#[tokio::test]
async fn a_request_by_mail_or_a_page_is_left_to_the_app() {
    let h = harness().await;
    let by_mail = choose("<mailto:leave@news.example?subject=Remove%20me>", true).unwrap();
    let left = actions(&h)
        .unsubscribe(h.account_id, by_mail)
        .await
        .unwrap();
    assert_eq!(
        left,
        Leave::Send {
            to: "leave@news.example".into(),
            subject: "Remove me".into(),
            body: "unsubscribe".into(),
        }
    );

    let page = choose("<http://news.example/u>", true).unwrap();
    let left = actions(&h).unsubscribe(h.account_id, page).await.unwrap();
    assert_eq!(left, Leave::Open("http://news.example/u".into()));
    assert!(h.one_click.posted().is_empty(), "nothing posted");
}

#[tokio::test]
async fn a_list_left_is_kept_for_its_sender() {
    let h = harness().await;
    actions(&h)
        .left(h.account_id, "News@Trail.example", How::OneClick)
        .await
        .unwrap();

    let id = h.account_id;
    let left = h
        .db
        .read(move |c| unsubscribes::left(c, id, "news@trail.example"))
        .await
        .unwrap();
    assert_eq!(left.map(|l| l.how), Some(How::OneClick));
}

fn actions(h: &Harness) -> MailActions<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    MailActions::new(
        Arc::new(Connected(connected)),
        h.db.clone(),
        OneClick::Fake(Arc::clone(&h.one_click)),
    )
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

/// A rule that sorts mail from `email` into the category `label`.
fn sorts(email: &str, label: &str) -> Filter {
    Filter {
        id: None,
        criteria: FilterCriteria {
            from: Some(email.into()),
            ..FilterCriteria::default()
        },
        action: FilterAction {
            add: vec![MailSet::Category(label.into())],
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
    let rules = h.sync.services().rules.clone();
    rules
        .create_filter(&sorts("SHOP@example.com", "CATEGORY_SOCIAL"))
        .await
        .unwrap();
    let blocked = rules.create_filter(&Filter::block(shop)).await.unwrap();

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
    assert_eq!(new.action.add, [MailSet::Category("CATEGORY_PROMOTIONS".into())]);
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
