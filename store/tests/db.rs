mod common;

use common::{meta, store};
use mailrs_store::{Db, StoreError, accounts, open_connection};

fn open() -> (Db, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (Db::open(&dir.path().join("mail.db")).unwrap(), dir)
}

#[tokio::test]
async fn writes_are_visible_to_reads() {
    let (db, _dir) = open();
    let id = db
        .write(|c| accounts::insert_account(c, "me@example.com", 0))
        .await
        .unwrap();
    let all = db.read(accounts::list_accounts).await.unwrap();
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
    assert!(db.read(accounts::list_accounts).await.unwrap().is_empty());
}

#[tokio::test]
async fn reads_are_read_only() {
    let (db, _dir) = open();
    let result = db
        .read(|c| accounts::insert_account(c, "me@example.com", 0))
        .await;
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
                db.write(move |c| {
                    accounts::insert_account(c, &format!("u{i}@example.com"), 0).map(|_| ())
                })
                .await
                .unwrap();
            } else {
                db.read(|c| accounts::list_accounts(c).map(|all| all.len()))
                    .await
                    .unwrap();
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(db.read(accounts::list_accounts).await.unwrap().len(), 10);
}

#[tokio::test]
async fn data_survives_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let db = Db::open(&path).unwrap();
    db.write(|c| accounts::insert_account(c, "me@example.com", 0))
        .await
        .unwrap();
    drop(db);
    let db = Db::open(&path).unwrap();
    assert_eq!(db.read(accounts::list_accounts).await.unwrap().len(), 1);
}

/// A burst of reads shares a few connections rather than opening one each.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_of_reads_opens_no_more_than_the_pool() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (db, _dir) = open();
    let (open_now, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let mut tasks = Vec::new();
    for _ in 0..24 {
        let (db, open_now, most) = (db.clone(), Arc::clone(&open_now), Arc::clone(&most));
        tasks.push(tokio::spawn(async move {
            db.read(move |c| {
                let now = open_now.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(20));
                open_now.fetch_sub(1, Ordering::SeqCst);
                accounts::list_accounts(c)
            })
            .await
            .unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let most = most.load(Ordering::SeqCst);
    assert!((1..=4).contains(&most), "{most} reads ran at once");
}

/// A read that panics gives its connection back, so later reads still run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_read_leaves_the_pool_whole() {
    let (db, _dir) = open();
    for _ in 0..8 {
        let failed = db
            .read(|_| -> Result<(), StoreError> { panic!("a read went wrong") })
            .await;
        assert!(matches!(failed, Err(StoreError::Closed)));
    }
    assert!(db.read(accounts::list_accounts).await.unwrap().is_empty());
}

/// Which tables SQLite holds statistics for, so it plans from real row
/// counts instead of guesses.
fn analyzed(conn: &rusqlite::Connection) -> Vec<String> {
    conn.prepare("SELECT DISTINCT tbl FROM sqlite_stat1 ORDER BY tbl")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()
        })
        .unwrap_or_default()
}

fn some_mail(conn: &rusqlite::Connection, account: i64, count: usize) {
    let mail: Vec<_> = (0..count)
        .map(|n| {
            meta(
                account,
                &format!("m{n}"),
                &format!("t{}", n / 2),
                n as i64,
                &["INBOX"],
            )
        })
        .collect();
    store(conn, &mail);
}

#[test]
fn a_store_opened_again_has_statistics_to_plan_with() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let conn = open_connection(&path).unwrap();
    let id = accounts::insert_account(&conn, "me@example.com", 0).unwrap();
    some_mail(&conn, id, 200);
    drop(conn);
    let conn = open_connection(&path).unwrap();
    let tables = analyzed(&conn);
    assert!(tables.contains(&"messages".to_string()), "{tables:?}");
    assert!(
        tables.contains(&"thread_mailboxes".to_string()),
        "{tables:?}"
    );
}

#[test]
fn the_write_ahead_log_shrinks_back_after_a_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let conn = open_connection(&dir.path().join("mail.db")).unwrap();
    let limit: i64 = conn
        .pragma_query_value(None, "journal_size_limit", |row| row.get(0))
        .unwrap();
    assert!((1..=64 << 20).contains(&limit), "{limit}");
}

/// A bootstrap writes thousands of rows into a store that had none, and
/// the plans made from the empty tables' statistics would be wrong.
#[tokio::test]
async fn a_large_write_leaves_statistics_behind() {
    let (db, _dir) = open();
    let id = db
        .write(|c| accounts::insert_account(c, "me@example.com", 0))
        .await
        .unwrap();
    db.write(move |c| {
        some_mail(c, id, 3000);
        Ok(())
    })
    .await
    .unwrap();
    // The writer takes jobs in order, so once this one returns it has
    // finished whatever followed the last.
    db.write(|_| Ok(())).await.unwrap();
    let tables = db.read(|c| Ok(analyzed(c))).await.unwrap();
    assert!(tables.contains(&"messages".to_string()), "{tables:?}");
}
