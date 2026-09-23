use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

use mailrs_ai::ToolOutcome;
use serde_json::json;

use super::front_matter;
use super::sandbox::{self, Jail, Limits};
use super::shell::{Runnable, Shell};
use super::{Origin, Skill, Skills, discover, read_file};
use crate::assistant::sources::{Source, system_prompt};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

fn skill_md(name: &str, description: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n\nDo the thing.\n")
}

#[test]
fn front_matter_reads_the_forms_people_write() {
    let text = "---\n\
        name: \"pdf tools\"\n\
        description: >\n  Reads PDFs\n  and forms.\n\n  Second paragraph.\n\
        license: 'it''s MIT' # a comment\n\
        allowed-tools:\n  - Bash\n  - Read\n\
        metadata:\n  version: 2\n\
        notes: |\n  line one\n  line two\n\
        plain: runs on\n  over two lines\n\
        ---\nBody\n";
    let front = front_matter::parse(text).expect("parses");
    let field = |key: &str| front.fields.get(key).map(String::as_str);
    assert_eq!(field("name"), Some("pdf tools"));
    assert_eq!(
        field("description"),
        Some("Reads PDFs and forms.\nSecond paragraph.")
    );
    assert_eq!(field("license"), Some("it's MIT"));
    assert_eq!(field("allowed-tools"), None);
    assert_eq!(field("metadata"), None);
    assert_eq!(field("notes"), Some("line one\nline two"));
    assert_eq!(field("plain"), Some("runs on over two lines"));
    assert_eq!(front.body, "Body\n");

    let escaped = front_matter::parse("---\r\nname: \"a \\\"b\\\" \\u00e9\"\r\n---\r\n")
        .expect("CRLF parses");
    assert_eq!(
        escaped.fields.get("name").map(String::as_str),
        Some("a \"b\" é")
    );
    assert!(front_matter::parse("# Title\n").is_err());
    assert!(front_matter::parse("---\nname: x\n").is_err());
    assert!(front_matter::parse("---\nname \"x\"\n---\n").is_err());
    assert!(front_matter::parse("---\nname: \"x\n---\n").is_err());
}

#[test]
fn discovery_keeps_good_skills_and_skips_broken_ones() {
    let home = tempfile::tempdir().expect("tempdir");
    let own = home.path().join("own");
    let claude = home.path().join("claude");
    let elsewhere = home.path().join("elsewhere");

    write(
        &own.join("notes/SKILL.md"),
        &skill_md("notes", "Writes notes."),
    );
    write(
        &own.join("receipts/SKILL.md"),
        &skill_md("receipts", "Totals receipts."),
    );
    write(
        &own.join("receipts/scripts/total.txt"),
        "not a script by name",
    );
    write(&own.join("broken/SKILL.md"), "---\nname: broken\n");
    write(
        &own.join("nameless/SKILL.md"),
        "---\ndescription: No name.\n---\n",
    );
    write(&own.join("no-skill-file/README.md"), "hello");
    write(&own.join("stray.md"), "a file, not a folder");

    // A skill reached through a link, as Claude Code's folder often holds.
    write(
        &elsewhere.join("linked/SKILL.md"),
        &skill_md("linked", "Lives elsewhere."),
    );
    let tool = elsewhere.join("linked/run");
    write(&tool, "#!/bin/sh\necho hi\n");
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::create_dir_all(&claude).expect("mkdir");
    symlink(elsewhere.join("linked"), claude.join("linked")).expect("symlink");
    write(
        &claude.join("helper/SKILL.md"),
        &skill_md("helper", "Has a Python file."),
    );
    write(&claude.join("helper/tool.py"), "print('hi')\n");

    let skills = discover(&[
        (Origin::PenguinMail, own.clone()),
        (Origin::ClaudeCode, claude.clone()),
        (Origin::ClaudeCode, home.path().join("missing")),
    ]);
    let summary: Vec<(&str, &str, bool)> = skills
        .iter()
        .map(|s| (s.id.as_str(), s.name.as_str(), s.has_scripts))
        .collect();
    assert_eq!(
        summary,
        [
            ("penguin-mail/notes", "notes", false),
            ("penguin-mail/receipts", "receipts", true),
            ("claude-code/helper", "helper", true),
            ("claude-code/linked", "linked", true),
        ]
    );
    let linked = &skills[3];
    assert_eq!(
        linked.folder,
        elsewhere.join("linked").canonicalize().expect("canonical")
    );
    assert_eq!(linked.description, "Lives elsewhere.");
}

fn skill_at(folder: &Path, name: &str, has_scripts: bool) -> Skill {
    Skill {
        id: format!("penguin-mail/{name}"),
        origin: Origin::PenguinMail,
        name: name.into(),
        description: format!("The {name} skill."),
        folder: folder.canonicalize().expect("canonical"),
        has_scripts,
    }
}

#[test]
fn a_skill_file_read_stays_inside_the_folder() {
    let home = tempfile::tempdir().expect("tempdir");
    let folder = home.path().join("skill");
    write(&folder.join("SKILL.md"), &skill_md("s", "S."));
    write(&folder.join("docs/guide.md"), "Guide text.");
    write(&home.path().join("secret.txt"), "private");
    symlink(home.path().join("secret.txt"), folder.join("escape.txt")).expect("symlink");
    symlink(folder.join("docs/guide.md"), folder.join("inside.md")).expect("symlink");

    let read = |path: &str| read_file(&folder, path).map(|text| text.text);
    assert_eq!(read("docs/guide.md").as_deref(), Ok("Guide text."));
    assert_eq!(read("./docs/guide.md").as_deref(), Ok("Guide text."));
    assert_eq!(read("inside.md").as_deref(), Ok("Guide text."));
    assert!(read("../secret.txt").is_err());
    assert!(read("docs/../../secret.txt").is_err());
    let absolute = home.path().join("secret.txt");
    assert!(read(&absolute.to_string_lossy()).is_err());
    assert!(read("/etc/passwd").is_err());
    assert!(read("escape.txt").is_err());
    assert!(read("docs").is_err());
    assert!(read("").is_err());

    // A big file comes back cut at the cap.
    write(
        &folder.join("big.txt"),
        &"a".repeat(super::FILE_CAP as usize + 10),
    );
    let big = read_file(&folder, "big.txt").expect("reads");
    assert!(big.cut);
    assert_eq!(big.text.len(), super::FILE_CAP as usize);
}

#[tokio::test]
async fn the_skills_source_lists_itself_and_hands_out_its_text() {
    let home = tempfile::tempdir().expect("tempdir");
    let folder = home.path().join("receipts");
    write(
        &folder.join("SKILL.md"),
        &skill_md("receipts", "Totals receipts."),
    );
    write(&folder.join("reference/rates.md"), "VAT is 23%.");
    let source = Skills::new(vec![skill_at(&folder, "receipts", false)]);

    let prompt = system_prompt("Base.", &[std::sync::Arc::new(source) as _]);
    assert!(prompt.starts_with("Base.\n\nSkills:"));
    assert!(prompt.contains("\n- receipts: The receipts skill."));

    let source = Skills::new(vec![skill_at(&folder, "receipts", false)]);
    assert!(
        source
            .ask("use_skill", &json!({"name": "receipts"}))
            .is_none()
    );
    let ToolOutcome::Ok(used) = source
        .call("use_skill".into(), json!({"name": "receipts"}))
        .await
    else {
        panic!("use_skill failed");
    };
    assert_eq!(used["instructions"], json!("# receipts\n\nDo the thing."));
    assert_eq!(
        used["files"],
        json!([{"path": "reference/rates.md", "script": false}])
    );
    let ToolOutcome::Ok(file) = source
        .call(
            "read_skill_file".into(),
            json!({"name": "receipts", "path": "reference/rates.md"}),
        )
        .await
    else {
        panic!("read_skill_file failed");
    };
    assert_eq!(file["text"], json!("VAT is 23%."));
    assert!(matches!(
        source
            .call("use_skill".into(), json!({"name": "other"}))
            .await,
        ToolOutcome::Err(_)
    ));
    assert!(Skills::new(Vec::new()).specs().is_empty());
    assert!(Skills::new(Vec::new()).prompt().is_none());
}

/// Bubblewrap, or `None` with a note when this computer has none that
/// works, in which case the sandbox tests prove nothing and say so.
fn bwrap() -> Option<PathBuf> {
    match sandbox::check() {
        Ok(path) => Some(path),
        Err(reason) => {
            eprintln!("skipping the sandbox test: {reason}");
            if std::env::var_os("PENGUIN_MAIL_REQUIRE_SANDBOX").is_some() {
                panic!("PENGUIN_MAIL_REQUIRE_SANDBOX is set and the sandbox does not work");
            }
            None
        }
    }
}

struct Dirs {
    _home: tempfile::TempDir,
    skill: PathBuf,
    work: PathBuf,
}

fn sandbox_dirs() -> Dirs {
    let home = tempfile::tempdir().expect("tempdir");
    let skill = home.path().join("skill");
    let work = home.path().join("work");
    write(&skill.join("SKILL.md"), &skill_md("s", "S."));
    write(&skill.join("hello.sh"), "echo hello from the skill\n");
    std::fs::create_dir_all(&work).expect("mkdir");
    Dirs {
        _home: home,
        skill,
        work,
    }
}

async fn run_in(dirs: &Dirs, bwrap: &Path, command: &str, limits: Limits) -> sandbox::Run {
    let jail = Jail {
        bwrap,
        skill: &dirs.skill,
        work: &dirs.work,
        network: false,
    };
    sandbox::run(&jail, command, None, limits)
        .await
        .expect("bwrap starts")
}

#[tokio::test]
async fn the_sandbox_sees_the_system_and_the_skill_but_not_the_home_folder() {
    let Some(bwrap) = bwrap() else { return };
    let dirs = sandbox_dirs();
    let limits = Limits::DEFAULT;

    let listed = run_in(&dirs, &bwrap, "ls /", limits).await;
    assert_eq!(listed.exit_code, Some(0), "{}", listed.output);
    for name in ["usr", "skill", "work", "tmp"] {
        assert!(
            listed.output.lines().any(|l| l == name),
            "{}",
            listed.output
        );
    }
    assert!(!listed.output.lines().any(|l| l == "home"));

    let script = run_in(&dirs, &bwrap, "bash /skill/hello.sh", limits).await;
    assert_eq!(script.output, "hello from the skill\n");

    let home = std::env::var("HOME").expect("HOME is set");
    let peek = run_in(
        &dirs,
        &bwrap,
        &format!("cat ~/.ssh/id_ed25519; ls {home}; ls {home}/.ssh"),
        limits,
    )
    .await;
    assert_ne!(peek.exit_code, Some(0));
    assert!(peek.output.contains("No such file"), "{}", peek.output);

    let env = run_in(&dirs, &bwrap, "echo $HOME; env | wc -l", limits).await;
    assert!(env.output.starts_with("/work\n"), "{}", env.output);

    let write_out = run_in(
        &dirs,
        &bwrap,
        "touch /usr/x; touch /skill/x; touch /x; touch /etc/x; echo $?",
        limits,
    )
    .await;
    assert!(write_out.output.ends_with("1\n"), "{}", write_out.output);
    assert!(!dirs.skill.join("x").exists());
}

#[tokio::test]
async fn the_work_folder_lasts_across_calls_in_one_chat() {
    let Some(bwrap) = bwrap() else { return };
    let dirs = sandbox_dirs();
    let skill = skill_at(&dirs.skill, "s", true);
    let shell = Shell::with(
        bwrap,
        vec![Runnable {
            skill,
            network: false,
        }],
        dirs.work.clone(),
        Limits::DEFAULT,
    );
    let question = shell
        .ask(
            "run_command",
            &json!({"skill": "s", "command": "echo kept > note"}),
        )
        .expect("it always asks");
    assert!(question.contains("echo kept > note"));
    assert!(question.contains("cannot reach the internet"));
    assert!(
        shell
            .ask("run_command", &json!({"skill": "nope", "command": "ls"}))
            .is_none()
    );

    let first = shell
        .call(
            "run_command".into(),
            json!({"skill": "s", "command": "echo kept > note; pwd"}),
        )
        .await;
    let ToolOutcome::Ok(first) = first else {
        panic!("{first:?}")
    };
    assert_eq!(first["output"], json!("/work\n"));
    let second = shell
        .call(
            "run_command".into(),
            json!({"skill": "s", "command": "cat note; cat", "stdin": "fed in\n"}),
        )
        .await;
    let ToolOutcome::Ok(second) = second else {
        panic!("{second:?}")
    };
    assert_eq!(second["output"], json!("kept\nfed in\n"));
    assert_eq!(second["exit_code"], json!(0));

    // The scratch folder goes with the chat.
    let made: Vec<PathBuf> = std::fs::read_dir(&dirs.work)
        .expect("read")
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(made.len(), 1);
    let mode = std::fs::metadata(&made[0])
        .expect("meta")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    drop(shell);
    assert!(!made[0].exists());
}

#[tokio::test]
async fn the_network_is_off_unless_the_skill_allows_it() {
    let Some(bwrap) = bwrap() else { return };
    let dirs = sandbox_dirs();
    let connect = "exec 3<>/dev/tcp/1.1.1.1/53 && echo connected";
    let cut_off = run_in(&dirs, &bwrap, connect, Limits::DEFAULT).await;
    assert_ne!(cut_off.exit_code, Some(0), "{}", cut_off.output);
    assert!(!cut_off.output.contains("connected"));
    let interfaces = run_in(&dirs, &bwrap, "cat /proc/net/dev", Limits::DEFAULT).await;
    assert!(
        !interfaces
            .output
            .lines()
            .skip(2)
            .any(|l| !l.trim_start().starts_with("lo:")),
        "{}",
        interfaces.output
    );
}

#[tokio::test]
async fn the_time_limit_kills_everything_the_command_started() {
    let Some(bwrap) = bwrap() else { return };
    let dirs = sandbox_dirs();
    let limits = Limits {
        time: Duration::from_secs(2),
        output: 1024,
    };
    // A marker no other process on the machine carries.
    let marker = format!(
        "{}.{}",
        4000 + std::process::id() % 1000,
        rand::random::<u16>()
    );
    let started = std::time::Instant::now();
    let run = run_in(
        &dirs,
        &bwrap,
        &format!("(setsid sleep {marker} &) ; sleep {marker}"),
        limits,
    )
    .await;
    assert!(run.timed_out);
    assert_eq!(run.exit_code, None);
    assert!(started.elapsed() < Duration::from_secs(20));
    // The kernel may take a moment to reap the namespace.
    let mut alive = true;
    for _ in 0..50 {
        alive = still_running(&marker);
        if !alive {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!alive, "a sleep {marker} outlived the sandbox");
}

fn still_running(marker: &str) -> bool {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| std::fs::read(entry.path().join("cmdline")).ok())
        .any(|cmdline| {
            let line = String::from_utf8_lossy(&cmdline).replace('\0', " ");
            line.starts_with("sleep ") && line.contains(marker)
        })
}

#[tokio::test]
async fn output_past_the_limit_is_cut_and_says_so() {
    let Some(bwrap) = bwrap() else { return };
    let dirs = sandbox_dirs();
    let limits = Limits {
        time: Duration::from_secs(30),
        output: 1000,
    };
    let run = run_in(
        &dirs,
        &bwrap,
        "yes 0123456789 | head -c 500000; echo err >&2",
        limits,
    )
    .await;
    assert!(run.truncated);
    assert_eq!(run.output.len(), 1000);
    assert_eq!(run.exit_code, Some(0));
    let small = run_in(&dirs, &bwrap, "echo out; echo err >&2", limits).await;
    assert!(!small.truncated);
    assert_eq!(small.output, "out\nerr\n");
    let failed = run_in(&dirs, &bwrap, "exit 3", limits).await;
    assert_eq!(failed.exit_code, Some(3));
}

#[test]
fn network_only_brings_the_files_a_lookup_needs() {
    let path = PathBuf::from("/x");
    let jail = |network| Jail {
        bwrap: &path,
        skill: &path,
        work: &path,
        network,
    };
    let text = |network| -> Vec<String> {
        sandbox::arguments(&jail(network))
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    };
    let off = text(false);
    let on = text(true);
    assert!(off.contains(&"--unshare-all".to_string()));
    assert!(!off.contains(&"--share-net".to_string()));
    assert!(!off.iter().any(|a| a == "/etc/resolv.conf"));
    assert!(on.contains(&"--share-net".to_string()));
    assert!(on.iter().any(|a| a == "/etc/resolv.conf"));
    for flag in ["--die-with-parent", "--new-session", "--clearenv"] {
        assert!(off.contains(&flag.to_string()), "{flag}");
    }
}

/// A toolbox over one shell with the skill `s`, and the keys of the
/// questions it asks, each answered with `verdict`. The shell's sandbox
/// program does not exist, so no command runs.
fn shell_toolbox(
    allowed: &[&str],
    verdict: crate::assistant::sources::Verdict,
) -> (
    crate::assistant::sources::Toolbox,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    tempfile::TempDir,
) {
    use crate::assistant::sources::{ApprovalRequest, Toolbox};
    let dir = tempfile::tempdir().expect("tempdir");
    let skill = skill_at(dir.path(), "s", true);
    let shell = Shell::with(
        PathBuf::from("/nonexistent/bwrap"),
        vec![Runnable {
            skill,
            network: false,
        }],
        dir.path().join("work"),
        Limits::DEFAULT,
    );
    let (requests, _received) = async_channel::unbounded();
    let (approvals, asked) = async_channel::unbounded::<ApprovalRequest>();
    let keys = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = std::sync::Arc::clone(&keys);
    tokio::spawn(async move {
        while let Ok(request) = asked.recv().await {
            seen.lock().expect("lock").push(request.key.clone());
            let _ = request.reply.send(verdict).await;
        }
    });
    let toolbox = Toolbox::new(
        crate::assistant::Host::new(Vec::new(), requests),
        vec![std::sync::Arc::new(shell)],
        approvals,
        allowed.iter().map(|k| k.to_string()),
    );
    (toolbox, keys, dir)
}

#[tokio::test]
async fn always_allowing_one_command_leaves_other_commands_asking() {
    use crate::assistant::sources::Verdict;
    use mailrs_ai::ToolHost;
    let (toolbox, keys, _dir) = shell_toolbox(&[], Verdict::Always);
    let run = |command: &str| {
        toolbox.call(
            "run_command".into(),
            json!({"skill": "s", "command": command}),
        )
    };
    run("scripts/total.sh").await;
    run("scripts/total.sh").await;
    assert_eq!(
        keys.lock().expect("lock").len(),
        1,
        "the same command asks once"
    );
    run("rm -rf /work").await;
    assert_eq!(
        keys.lock().expect("lock").len(),
        2,
        "another command asks again"
    );
    let keys = keys.lock().expect("lock").clone();
    assert_ne!(keys[0], keys[1]);
}

#[tokio::test]
async fn an_always_answer_from_before_the_fix_allows_no_command() {
    use crate::assistant::sources::Verdict;
    use mailrs_ai::ToolHost;
    let (toolbox, keys, _dir) = shell_toolbox(&["shell/run_command"], Verdict::Deny);
    let outcome = toolbox
        .call(
            "run_command".into(),
            json!({"skill": "s", "command": "scripts/total.sh"}),
        )
        .await;
    assert_eq!(outcome, ToolOutcome::Err("The user declined.".into()));
    assert_eq!(keys.lock().expect("lock").len(), 1);
}
