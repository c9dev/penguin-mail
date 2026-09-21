//! Running one command under bubblewrap.
//!
//! The sandbox sees the system's programs and libraries read-only, the
//! skill's folder read-only at `/skill`, and one scratch folder it may
//! write at `/work`. It has no home folder, so no mail database, keys,
//! tokens or SSH keys; no session bus or display, since the environment is
//! cleared and `/tmp` and `/run` are empty; and no network unless the skill
//! is allowed one. Every namespace bubblewrap can make is unshared, so the
//! command cannot see or signal the app's own processes either.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

/// How long a command may run and how much of what it prints is kept.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub time: Duration,
    pub output: usize,
}

impl Limits {
    pub const DEFAULT: Limits = Limits {
        time: Duration::from_secs(60),
        output: 1024 * 1024,
    };
}

/// What one command did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Standard output and standard error together, in the order written.
    pub output: String,
    /// The output went past the limit and the rest was dropped.
    pub truncated: bool,
    /// `None` when the command was killed rather than exiting.
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

/// Where one command runs and what it may reach.
#[derive(Debug, Clone)]
pub struct Jail<'a> {
    pub bwrap: &'a Path,
    /// Mounted read-only at `/skill`.
    pub skill: &'a Path,
    /// Mounted read-write at `/work`, the working directory.
    pub work: &'a Path,
    pub network: bool,
}

/// The programs a command may start, and nothing from the person's own
/// PATH, which could point into their home folder.
const PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// The top-level folders that merged-usr systems such as Ubuntu keep as
/// links into `/usr`.
const USR_LINKS: &[&str] = &["bin", "sbin", "lib", "lib32", "lib64", "libx32"];

/// Files under /etc a program needs to look up a host name and check a
/// certificate. Only a skill allowed the network gets them.
const NETWORK_ETC: &[&str] = &[
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/nsswitch.conf",
    "/etc/ssl",
    "/etc/ca-certificates",
];

/// Everything bubblewrap is told, ahead of the command itself.
pub fn arguments(jail: &Jail) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |parts: &[&str]| args.extend(parts.iter().map(OsString::from));
    push(&["--unshare-all"]);
    if jail.network {
        push(&["--share-net"]);
    }
    push(&["--die-with-parent", "--new-session", "--clearenv"]);
    push(&["--setenv", "PATH", PATH]);
    push(&["--setenv", "HOME", "/work"]);
    push(&["--setenv", "LANG", "C.UTF-8"]);
    push(&["--ro-bind", "/usr", "/usr"]);
    for name in USR_LINKS {
        let top = Path::new("/").join(name);
        match std::fs::read_link(&top) {
            Ok(target) => {
                let target = target.to_string_lossy().into_owned();
                push(&["--symlink", &target, &format!("/{name}")]);
            }
            // A system that is not merged-usr keeps real folders here.
            Err(_) if top.is_dir() => {
                let top = top.to_string_lossy().into_owned();
                push(&["--ro-bind", &top, &top]);
            }
            Err(_) => {}
        }
    }
    // Debian points commands such as awk at their chosen program through
    // here, so without it they would not start.
    if Path::new("/etc/alternatives").is_dir() {
        push(&["--ro-bind", "/etc/alternatives", "/etc/alternatives"]);
    }
    if jail.network {
        for path in NETWORK_ETC {
            push(&["--ro-bind-try", path, path]);
        }
    }
    push(&["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"]);
    args.push("--ro-bind".into());
    args.push(jail.skill.as_os_str().to_owned());
    args.push("/skill".into());
    args.push("--bind".into());
    args.push(jail.work.as_os_str().to_owned());
    args.push("/work".into());
    // bubblewrap builds the sandbox on a fresh tmpfs, which would let a
    // command write anywhere outside the mounts above. Making it read-only
    // last leaves /work and /tmp as the only places to write.
    args.extend(["--remount-ro", "/", "--chdir", "/work"].map(OsString::from));
    args
}

/// Runs `bash -c command` in the sandbox, feeding it `stdin`.
///
/// When the time runs out, bubblewrap is killed. `--die-with-parent` takes
/// the sandbox's first process with it, and since that process is the
/// first in its own PID namespace, the kernel then kills every process the
/// command started, however it detached them.
pub async fn run(
    jail: &Jail<'_>,
    command: &str,
    stdin: Option<&str>,
    limits: Limits,
) -> std::io::Result<Run> {
    let (reader, writer) = std::io::pipe()?;
    let mut child = {
        // The command holds its own copies of the pipe's writing end until
        // it is dropped, and the reader sees the end of the output only once
        // every copy is closed, so it lives in this block alone.
        let mut command_line = tokio::process::Command::new(jail.bwrap);
        command_line
            .args(arguments(jail))
            .arg("--")
            .args(["/bin/bash", "-c", command])
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(writer.try_clone()?)
            .stderr(writer)
            .kill_on_drop(true);
        command_line.spawn()?
    };
    let cap = limits.output;
    let collected = tokio::task::spawn_blocking(move || collect(reader, cap));
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let text = text.to_string();
        // A command that never reads its input must not hold up the rest.
        tokio::spawn(async move {
            let _ = pipe.write_all(text.as_bytes()).await;
        });
    }
    let (status, timed_out) = match tokio::time::timeout(limits.time, child.wait()).await {
        Ok(status) => (Some(status?), false),
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            (None, true)
        }
    };
    let (bytes, truncated) = collected.await.map_err(std::io::Error::other)?;
    Ok(Run {
        output: String::from_utf8_lossy(&bytes).into_owned(),
        truncated,
        exit_code: status.and_then(|status| status.code()),
        timed_out,
    })
}

/// Reads until the last writer closes, keeping the first `cap` bytes. It
/// goes on reading past the cap so a chatty command does not block on a
/// full pipe before the time limit can stop it.
fn collect(mut reader: std::io::PipeReader, cap: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let room = cap.saturating_sub(kept.len());
                kept.extend_from_slice(&buffer[..read.min(room)]);
                truncated |= read > room;
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    (kept, truncated)
}

/// Where bubblewrap is, if it is installed.
pub fn find_bwrap() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join("bwrap"));
    std::iter::once(PathBuf::from("/usr/bin/bwrap"))
        .chain(on_path)
        .find(|path| path.is_file())
}

/// Whether the sandbox works here: bubblewrap is installed and can start a
/// command with the arguments above. Some systems forbid the unprivileged
/// namespaces it needs, and there it starts and fails at once. The answer
/// is kept for the rest of the run.
pub fn check() -> Result<PathBuf, String> {
    static ANSWER: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    ANSWER.get_or_init(probe).clone()
}

fn probe() -> Result<PathBuf, String> {
    let Some(bwrap) = find_bwrap() else {
        return Err(mailrs_domain::translate::gettext(
            "Skill scripts need bubblewrap, which is not installed. Install the bubblewrap package to run them.",
        ));
    };
    let empty = std::env::temp_dir();
    let jail = Jail {
        bwrap: &bwrap,
        skill: &empty,
        work: &empty,
        network: false,
    };
    let output = std::process::Command::new(&bwrap)
        .args(arguments(&jail))
        .args(["--", "/bin/true"])
        .stdin(Stdio::null())
        .output();
    match output {
        Ok(output) if output.status.success() => Ok(bwrap),
        Ok(output) => Err(mailrs_domain::translate::fill(
            &mailrs_domain::translate::gettext(
                "Skill scripts cannot run: bubblewrap failed to start a sandbox ({reason}).",
            ),
            &[("reason", String::from_utf8_lossy(&output.stderr).trim())],
        )),
        Err(err) => Err(mailrs_domain::translate::fill(
            &mailrs_domain::translate::gettext(
                "Skill scripts cannot run: bubblewrap failed to start a sandbox ({reason}).",
            ),
            &[("reason", &err.to_string())],
        )),
    }
}
