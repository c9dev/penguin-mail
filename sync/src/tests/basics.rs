use std::time::Duration;

use mailrs_gmail::{GmailError, HistoryChange};

use crate::api::GmailApi;
use crate::fake::{FakeGmail, meta};
use crate::{TriageAction, backoff_delay};

fn close(actual: Duration, expected: Duration) -> bool {
    (actual.as_secs_f64() - expected.as_secs_f64()).abs() < 1e-6
}

#[test]
fn backoff_doubles_and_caps() {
    let max = Duration::from_secs(300);
    assert!(close(backoff_delay(0, max, 0.0), Duration::from_secs(1)));
    assert!(close(backoff_delay(3, max, 0.0), Duration::from_secs(8)));
    assert!(close(backoff_delay(30, max, 0.0), max));
    assert!(close(
        backoff_delay(0, Duration::from_millis(20), 0.0),
        Duration::from_millis(20)
    ));
}

#[test]
fn backoff_jitter_stays_within_twenty_percent() {
    let max = Duration::from_secs(300);
    assert!(close(
        backoff_delay(0, max, 1.0),
        Duration::from_millis(1200)
    ));
    assert!(close(
        backoff_delay(0, max, -1.0),
        Duration::from_millis(800)
    ));
    assert!(close(
        backoff_delay(0, max, 7.0),
        Duration::from_millis(1200)
    ));
}

#[test]
fn triage_actions_parse_and_map_to_labels() {
    let archive: TriageAction = "archive".parse().unwrap();
    assert_eq!(archive.label_delta(), (vec![], vec!["INBOX".to_string()]));
    assert_eq!(
        "label:Label_1".parse::<TriageAction>().unwrap(),
        TriageAction::AddLabel("Label_1".into())
    );
    assert_eq!(
        "unlabel:Label_1".parse::<TriageAction>().unwrap(),
        TriageAction::RemoveLabel("Label_1".into())
    );
    assert_eq!(
        TriageAction::Trash.label_delta(),
        (vec!["TRASH".to_string()], vec!["INBOX".to_string()])
    );
    assert_eq!(TriageAction::MarkRead.describe(), "Mark read");
    assert!("explode".parse::<TriageAction>().is_err());
    assert!("label:".parse::<TriageAction>().is_err());
}

#[tokio::test]
async fn the_fake_pages_history_and_hands_back_the_errors_it_is_given() {
    let fake = FakeGmail::new();
    for (i, id) in ["a", "b", "c"].into_iter().enumerate() {
        fake.deliver(meta(id, "t", 100 - i as i64, &["INBOX"]));
    }
    let history = fake.history(100, None).await.unwrap();
    assert_eq!(
        history.changes[0],
        HistoryChange::MessageAdded {
            id: "a".into(),
            thread_id: "t".into()
        }
    );
    assert_eq!(history.history_id, 103);

    fake.expire_history();
    assert!(matches!(
        fake.history(103, None).await,
        Err(GmailError::NotFound)
    ));
    fake.fail_next(GmailError::Network("down".into()));
    assert!(matches!(fake.profile().await, Err(GmailError::Network(_))));
    assert!(fake.profile().await.is_ok());
}
