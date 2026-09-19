use mailrs_store::{Db, StoreError, accounts};

fn open() -> (Db, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (Db::open(&dir.path().join("mail.db")).unwrap(), dir)
}

#[tokio::test]
async fn writes_are_visible_to_reads() {
    let (db, _dir) = open();
    let id = db.write(|c| accounts::insert_account(c, "me@example.com", 0)).await.unwrap();
    let all = db.read(|c| accounts::list_accounts(c)).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, id);
}

#[tokio::test]
async fn a_failed_write_rolls_back() {
    let (db, _dir) = open();
    let result: Result<(), StoreError> = db
        .write(|c| {
            accounts::insert_account(c, "me@example.com", 0)?;
            Err(StoreError::Closed)
        })
        .await;
    assert!(result.is_err());
    assert!(db.read(|c| accounts::list_accounts(c)).await.unwrap().is_empty());
}

#[tokio::test]
async fn reads_are_read_only() {
    let (db, _dir) = open();
    let result = db.read(|c| accounts::insert_account(c, "me@example.com", 0)).await;
    assert!(matches!(result, Err(StoreError::Sqlite(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reads_and_writes_interleave() {
    let (db, _dir) = open();
    let mut tasks = Vec::new();
    for i in 0..20 {
        let db = db.clone();
        tasks.push(tokio::spawn(async move {
            if i % 2 == 0 {
                db.write(move |c| accounts::insert_account(c, &format!("u{i}@example.com"), 0).map(|_| ()))
                    .await
                    .unwrap();
            } else {
                db.read(|c| accounts::list_accounts(c).map(|all| all.len())).await.unwrap();
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(db.read(|c| accounts::list_accounts(c)).await.unwrap().len(), 10);
}

#[tokio::test]
async fn data_survives_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let db = Db::open(&path).unwrap();
    db.write(|c| accounts::insert_account(c, "me@example.com", 0)).await.unwrap();
    drop(db);
    let db = Db::open(&path).unwrap();
    assert_eq!(db.read(|c| accounts::list_accounts(c)).await.unwrap().len(), 1);
}
