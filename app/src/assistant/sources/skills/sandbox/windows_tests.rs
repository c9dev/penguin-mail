//! Native regressions for AppContainer scopes and process-tree cleanup.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{Jail, Limits, Run, run};

fn shell() -> Option<PathBuf> {
    let found = super::windows::find_busybox();
    assert!(
        found.is_some() || std::env::var_os("PENGUIN_MAIL_REQUIRE_SANDBOX").is_none(),
        "native sandbox tests require BusyBox"
    );
    found
}

fn quote(path: &Path) -> String {
    format!(
        "'{}'",
        path.to_string_lossy()
            .replace('\\', "/")
            .replace('\'', "'\\''")
    )
}

async fn command(shell: &Path, skill: &Path, work: &Path, text: &str, time: Duration) -> Run {
    run(
        &Jail {
            program: shell,
            skill,
            work,
            network: false,
        },
        text,
        None,
        Limits { time, output: 4096 },
    )
    .await
    .unwrap()
}

fn folders(parent: &Path, name: &str) -> (PathBuf, PathBuf) {
    let skill = parent.join(format!("skill-{name}"));
    let work = parent.join(format!("work-{name}"));
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    (skill, work)
}

#[tokio::test]
async fn another_invocation_cannot_read_or_write_a_previous_jail() {
    let Some(shell) = shell() else { return };
    let root = tempfile::tempdir().unwrap();
    let (skill_a, work_a) = folders(root.path(), "a");
    let (skill_b, work_b) = folders(root.path(), "b");
    let secret = work_a.join("private.txt");
    std::fs::write(&secret, "private").unwrap();
    let first = command(
        &shell,
        &skill_a,
        &work_a,
        "printf ready",
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(first.exit_code, Some(0), "{}", first.output);
    let text = format!(
        "if {} cat {}; then exit 23; fi; if printf changed > {}; then exit 24; fi; printf isolated",
        quote(&shell),
        quote(&secret),
        quote(&secret)
    );
    let second = command(&shell, &skill_b, &work_b, &text, Duration::from_secs(5)).await;
    assert_eq!(second.exit_code, Some(0), "{}", second.output);
    assert_eq!(std::fs::read_to_string(secret).unwrap(), "private");
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_invocations_keep_their_grants_separate() {
    let Some(shell) = shell() else { return };
    let root = tempfile::tempdir().unwrap();
    let (skill_a, work_a) = folders(root.path(), "a");
    let (skill_b, work_b) = folders(root.path(), "b");
    let secret = work_a.join("private.txt");
    std::fs::write(&secret, "private").unwrap();
    let ready = work_a.join("ready");
    let first = tokio::spawn({
        let shell = shell.clone();
        let skill = skill_a.clone();
        let work = work_a.clone();
        async move {
            let text = format!(
                "printf ready > \"$WORK/ready\"; {} sleep 3; printf done",
                quote(&shell)
            );
            command(&shell, &skill, &work, &text, Duration::from_secs(8)).await
        }
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ready.exists(), "the first invocation never started");
    let text = format!(
        "if {} cat {}; then exit 23; fi; printf isolated",
        quote(&shell),
        quote(&secret)
    );
    let second = command(&shell, &skill_b, &work_b, &text, Duration::from_secs(5)).await;
    assert_eq!(second.exit_code, Some(0), "{}", second.output);
    assert_eq!(first.await.unwrap().exit_code, Some(0));
}

#[tokio::test]
async fn the_skill_is_read_only_and_the_work_folder_is_writable() {
    let Some(shell) = shell() else { return };
    let root = tempfile::tempdir().unwrap();
    let (skill, work) = folders(root.path(), "one");
    let file = skill.join("read-only");
    std::fs::write(&file, "skill").unwrap();
    let text = format!(
        "{} cat \"$SKILL/read-only\" || exit 21; if printf changed > \"$SKILL/read-only\"; then exit 22; fi; printf written > \"$WORK/result\"",
        quote(&shell)
    );
    let answer = command(&shell, &skill, &work, &text, Duration::from_secs(5)).await;
    assert_eq!(answer.exit_code, Some(0), "{}", answer.output);
    assert_eq!(std::fs::read_to_string(file).unwrap(), "skill");
    assert_eq!(
        std::fs::read_to_string(work.join("result")).unwrap(),
        "written"
    );
}

#[tokio::test]
async fn a_background_child_cannot_outlive_the_shell_or_its_deadline() {
    let Some(shell) = shell() else { return };
    let root = tempfile::tempdir().unwrap();
    let (skill, work) = folders(root.path(), "one");
    let text = format!(
        "({} sleep 1; printf escaped > \"$WORK/late\") & printf parent; exit 0",
        quote(&shell)
    );
    let answer = command(&shell, &skill, &work, &text, Duration::from_millis(200)).await;
    assert_eq!(answer.exit_code, Some(0), "{}", answer.output);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(
        !work.join("late").exists(),
        "a background child survived the command"
    );
}

#[tokio::test]
async fn a_timed_out_command_cannot_leave_a_child_running() {
    let Some(shell) = shell() else { return };
    let root = tempfile::tempdir().unwrap();
    let (skill, work) = folders(root.path(), "one");
    let text = format!(
        "({} sleep 1; printf escaped > \"$WORK/late\") & {} sleep 5",
        quote(&shell),
        quote(&shell)
    );
    let answer = command(&shell, &skill, &work, &text, Duration::from_millis(200)).await;
    assert!(answer.timed_out);
    assert_eq!(answer.exit_code, None);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(!work.join("late").exists());
}
