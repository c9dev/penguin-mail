use mailrs_store::servers::{self, Saved, Security, Servers};
use mailrs_store::{accounts, open_in_memory};

fn fastmail() -> Servers {
    Servers {
        imap: Saved {
            host: "imap.fastmail.com".into(),
            port: 993,
            security: Security::Tls,
            user_name: "dana@fastmail.com".into(),
        },
        smtp: Saved {
            host: "smtp.fastmail.com".into(),
            port: 465,
            security: Security::Tls,
            user_name: "dana@fastmail.com".into(),
        },
    }
}

#[test]
fn servers_come_back_as_they_were_saved() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_imap_account(&conn, "dana@fastmail.com", "Fastmail", 0)
        .unwrap()
        .unwrap();
    servers::save(&conn, id, &fastmail()).unwrap();
    assert_eq!(servers::load(&conn, id).unwrap(), Some(fastmail()));
}

#[test]
fn saving_again_replaces_both_servers() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_imap_account(&conn, "dana@example.org", "example.org", 0)
        .unwrap()
        .unwrap();
    servers::save(&conn, id, &fastmail()).unwrap();
    let mut moved = fastmail();
    moved.imap.host = "mail.example.org".into();
    moved.smtp.port = 587;
    moved.smtp.security = Security::StartTls;
    servers::save(&conn, id, &moved).unwrap();
    assert_eq!(servers::load(&conn, id).unwrap(), Some(moved));
}

#[test]
fn an_account_without_servers_has_none() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_account(&conn, "me@gmail.com", 0).unwrap();
    assert_eq!(servers::load(&conn, id).unwrap(), None);
}

#[test]
fn removing_the_account_removes_its_servers() {
    let conn = open_in_memory().unwrap();
    let id = accounts::insert_imap_account(&conn, "dana@fastmail.com", "Fastmail", 0)
        .unwrap()
        .unwrap();
    servers::save(&conn, id, &fastmail()).unwrap();
    accounts::delete_account(&conn, id).unwrap();
    let left: i64 = conn
        .query_row("SELECT COUNT(*) FROM account_servers", [], |row| row.get(0))
        .unwrap();
    assert_eq!(left, 0);
}

/// The password lives in the keyring. A column that could hold one is
/// a column someone would one day fill.
#[test]
fn no_column_of_the_servers_table_holds_a_password() {
    let conn = open_in_memory().unwrap();
    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('account_servers')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        columns,
        [
            "account_id",
            "role",
            "host",
            "port",
            "security",
            "user_name",
            "pinned_certificate"
        ]
    );
}
