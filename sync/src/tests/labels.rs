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
