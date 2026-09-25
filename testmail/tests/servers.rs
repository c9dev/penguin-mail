//! The harness on its own: the servers start and answer, doveadm reads and
//! changes what they hold, and no container outlives the test.

use std::process::Command;

use mailrs_testmail::{Certs, Dovecot, Mailpit, Profile, Submission};

const USER: &str = "me@example.test";

#[test]
fn servers_start_answer_and_leave_no_container_behind() {
    if mailrs_testmail::docker().is_none() {
        return;
    }
    let Some(certs) = Certs::make() else {
        return;
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let ids = runtime.block_on(async {
        let password = mailrs_testmail::password();
        let dovecot = Dovecot::start(&certs, Profile::Bare, &password).await?;
        let sink = Mailpit::start(&certs, Submission::Tls, USER, &password).await?;

        assert_eq!(dovecot.connections(USER).await, 0);
        dovecot.create_mailbox(USER, "Receipts").await;
        dovecot
            .save(
                USER,
                "Receipts",
                b"Subject: Kept\r\nMessage-ID: <kept@example.test>\r\n\r\nKept\r\n",
                Some(1_704_103_200),
            )
            .await;
        let kept = dovecot
            .find(USER, "Receipts", "kept@example.test")
            .await
            .expect("doveadm finds what it saved");
        assert_eq!(kept.uid, 1);

        dovecot
            .add_flags(USER, "Receipts", kept.uid, "\\Flagged")
            .await;
        let flagged = dovecot
            .find(USER, "Receipts", "kept@example.test")
            .await
            .expect("still there");
        assert!(flagged.has_flag("\\Flagged"), "{flagged:?}");

        let before = dovecot.uidvalidity(USER, "Receipts").await;
        dovecot.set_uidvalidity(USER, "Receipts", before + 7).await;
        assert_eq!(dovecot.uidvalidity(USER, "Receipts").await, before + 7);

        dovecot.expunge(USER, "Receipts", kept.uid).await;
        assert!(dovecot.messages(USER, "Receipts").await.is_empty());

        assert_eq!(sink.count().await, 0);
        Some([dovecot.id().to_string(), sink.id().to_string()])
    });
    let Some(ids) = ids else {
        return;
    };
    for id in ids {
        let listed = Command::new("docker")
            .args([
                "ps",
                "--all",
                "--quiet",
                "--no-trunc",
                "--filter",
                &format!("id={id}"),
            ])
            .output()
            .expect("docker runs");
        assert!(
            String::from_utf8_lossy(&listed.stdout).trim().is_empty(),
            "container {id} is still there"
        );
    }
}
