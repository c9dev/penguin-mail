use mailrs_domain::LabelKind;

use super::harness;
use crate::fake::meta;
use crate::now_millis;

#[tokio::test]
async fn labels_are_created_renamed_with_children_and_deleted() {
    let h = harness().await;
    h.bootstrap_all().await;
    let work = h.sync.create_label("Work").await.unwrap();
    let clients = h.sync.create_label("Work/Clients").await.unwrap();
    assert_eq!(work.kind, LabelKind::User);

    h.sync.rename_label(&work.id, "Job").await.unwrap();
    let names = h.fake.with(|s| {
        let mut names: Vec<String> = s.labels.iter().map(|l| l.name.clone()).collect();
        names.sort();
        names
    });
    assert!(names.contains(&"Job".to_string()));
    assert!(names.contains(&"Job/Clients".to_string()));

    h.fake.deliver(meta(
        "m1",
        "t1",
        now_millis(),
        &["INBOX", clients.id.as_str()],
    ));
    h.sync.ensure_thread("t1").await.unwrap();
    assert!(h.labels_of("m1").await.contains(&clients.id));
    h.sync.delete_label(&clients.id).await.unwrap();
    assert!(!h.labels_of("m1").await.contains(&clients.id));
    assert!(h.fake.with(|s| s.labels.iter().all(|l| l.id != clients.id)));
}

#[tokio::test]
async fn a_label_colour_is_kept() {
    let h = harness().await;
    h.bootstrap_all().await;
    let label = h.sync.create_label("Travel").await.unwrap();
    let color = mailrs_gmail::LabelColor {
        background_color: "#16a766".into(),
        text_color: "#ffffff".into(),
    };
    h.sync.set_label_color(&label.id, color).await.unwrap();
    let stored =
        h.db.read(|c| mailrs_store::labels::list_labels(c, 1))
            .await
            .unwrap();
    let travel = stored.iter().find(|l| l.id == label.id).unwrap();
    assert_eq!(travel.color.as_deref(), Some("#16a766"));
}

#[tokio::test]
async fn a_name_gmail_keeps_for_itself_is_refused_before_gmail_is_asked() {
    let h = harness().await;
    h.bootstrap_all().await;
    let before = h.fake.with(|s| s.labels.len());
    let err = h.sync.create_label(" important ").await.unwrap_err();
    assert!(matches!(err, crate::SyncError::ReservedLabel(ref name) if name == "important"));
    assert_eq!(h.fake.with(|s| s.labels.len()), before);
}

#[tokio::test]
async fn a_label_cannot_be_renamed_to_a_name_gmail_keeps() {
    let h = harness().await;
    h.bootstrap_all().await;
    let work = h.sync.create_label("Work").await.unwrap();
    let err = h.sync.rename_label(&work.id, "Starred").await.unwrap_err();
    assert!(matches!(err, crate::SyncError::ReservedLabel(_)));
    assert!(h.fake.with(|s| s.labels.iter().any(|l| l.name == "Work")));
}

// ---- Labels changed on the web -------------------------------------------

async fn stored(h: &super::Harness) -> Vec<(String, String, Option<String>)> {
    let mut labels: Vec<_> =
        h.db.read(|c| mailrs_store::labels::list_labels(c, 1))
            .await
            .unwrap()
            .into_iter()
            .map(|l| (l.id, l.name, l.color))
            .collect();
    labels.sort();
    labels
}

fn label_events(h: &super::Harness) -> usize {
    h.drain()
        .iter()
        .filter(|e| matches!(e, mailrs_domain::ChangeEvent::LabelsChanged { .. }))
        .count()
}

/// A label made, renamed or recoloured in the browser reaches the sidebar
/// on the next check, for one `labels.list` and nothing else.
#[tokio::test]
async fn labels_changed_on_the_web_are_picked_up() {
    let h = harness().await;
    h.bootstrap_all().await;
    h.fake.with(|s| {
        s.labels.push(mailrs_gmail::RemoteLabel {
            id: "Label_9".into(),
            name: "Made on the web".into(),
            kind: Some("user".into()),
            color: None,
        });
        let one = s.labels.iter_mut().find(|l| l.id == "Label_1").unwrap();
        one.name = "Renamed".into();
        one.color = Some(mailrs_gmail::LabelColor {
            background_color: "#16a766".into(),
            text_color: "#ffffff".into(),
        });
    });
    h.fake.reset_usage();

    assert!(h.sync.refresh_labels().await.unwrap());

    let labels = stored(&h).await;
    assert!(labels.contains(&("Label_9".into(), "Made on the web".into(), None)));
    assert!(labels.contains(&("Label_1".into(), "Renamed".into(), Some("#16a766".into()))));
    assert_eq!(label_events(&h), 1);
    assert_eq!(h.fake.usage().calls, 1);
    assert_eq!(h.fake.usage().units, 1);

    assert!(!h.sync.refresh_labels().await.unwrap(), "nothing new");
    assert_eq!(label_events(&h), 0, "and nothing said");
}

/// A label deleted on the web leaves the sidebar and the mail that had it.
#[tokio::test]
async fn a_label_deleted_on_the_web_leaves_its_mail() {
    let h = harness().await;
    h.fake
        .seed(meta("m1", "t1", now_millis(), &["INBOX", "Label_1"]));
    h.bootstrap_all().await;
    h.fake.with(|s| s.labels.retain(|l| l.id != "Label_1"));

    assert!(h.sync.refresh_labels().await.unwrap());

    assert!(stored(&h).await.iter().all(|(id, _, _)| id != "Label_1"));
    assert_eq!(h.labels_of("m1").await, ["INBOX"]);
}

/// History naming a label the store has never seen means one was made
/// elsewhere, so the replay lists the labels once.
#[tokio::test]
async fn history_naming_an_unknown_label_lists_the_labels() {
    let h = harness().await;
    h.fake.seed(meta("m1", "t1", now_millis(), &["INBOX"]));
    h.bootstrap_all().await;
    h.fake.with(|s| {
        s.labels.push(mailrs_gmail::RemoteLabel {
            id: "Label_9".into(),
            name: "Receipts".into(),
            kind: Some("user".into()),
            color: None,
        })
    });
    h.fake.remote_relabel("m1", &["Label_9"], &[]);
    h.fake.reset_usage();

    h.sync.incremental().await.unwrap();

    assert!(
        stored(&h)
            .await
            .contains(&("Label_9".into(), "Receipts".into(), None))
    );
    assert_eq!(h.fake.usage().calls_to("users.labels.list"), 1);
    assert!(label_events(&h) >= 1);

    h.fake.remote_relabel("m1", &["STARRED"], &[]);
    h.fake.reset_usage();
    h.sync.incremental().await.unwrap();
    assert_eq!(
        h.fake.usage().calls_to("users.labels.list"),
        0,
        "known labels cost nothing more"
    );
}
