//! Plays gpg for the runner tests in `tests/gnupg.rs`, so each test
//! decides what its program writes to which pipe without needing a shell.
//!
//! The runner hands over its status pipe as an extra channel beside
//! stdin, stdout and stderr — a file descriptor on Unix, an inherited
//! HANDLE on Windows — and names it in the `--status-fd` argument.
//!
//! What this does each run comes from `gnupg-stand-in.conf` beside the
//! binary, which the tests fill in before they call the engine: the
//! engine's own calls carry no flags of ours, so the script has to sit
//! with the program like a shell script's body would. A run can also get
//! flags in its own arguments when the test drives `Program::run`
//! itself. Both use the same vocabulary: a flag names one thing to do,
//! runs in the order given, and takes the rest of its line as a value
//! when it wants one:
//!
//! - `--x-args` writes the whole argument list to stdout, one line.
//! - `--x-args-to <file>` appends that same line to `file`.
//! - `--x-out <text>` writes `text` to stdout, one line.
//! - `--x-stderr <text>` writes `text` to stderr, one line.
//! - `--x-status <text>` writes `[GNUPG:] text` to the status channel.
//! - `--x-copy` copies stdin to stdout until stdin ends.
//! - `--x-emit <n>` writes `n` zero bytes to stdout.
//! - `--x-sleep-ms <n>` waits `n` milliseconds.
//! - `--x-pid <file>` writes this process's pid to `file`.
//! - `--x-child <file>` starts a copy of itself that sleeps 30 seconds,
//!   inheriting every pipe, and writes the child's pid to `file`, the
//!   way gpgsm's dirmngr outlives the run that started it.

use std::io::Write as _;
use std::process::Command;

/// One flag and the value it takes, if it wants one.
struct Step<'a> {
    flag: &'a str,
    value: Option<&'a str>,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let conf = conf();
    let mut status = status_channel(&args);
    let mut stdout = std::io::stdout().lock();
    for Step { flag, value } in steps(&args, &conf) {
        match flag {
            "--x-args" => writeln!(stdout, "{}", args.join(" ")).expect("write the args"),
            "--x-args-to" => {
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(value.expect("--x-args-to a file"))
                    .expect("open the args file");
                writeln!(file, "{}", args.join(" ")).expect("write the args");
            }
            "--x-out" => writeln!(stdout, "{}", value.expect("--x-out text")).expect("write"),
            "--x-stderr" => eprintln!("{}", value.expect("--x-stderr text")),
            "--x-status" => {
                if let Some(status) = &mut status {
                    writeln!(status, "[GNUPG:] {}", value.expect("--x-status text"))
                        .expect("write the status line");
                }
            }
            "--x-copy" => {
                let _ = std::io::copy(&mut std::io::stdin().lock(), &mut stdout);
            }
            "--x-emit" => {
                let mut left: usize = value.expect("--x-emit a count").parse().expect("a count");
                let zeros = [0u8; 1 << 20];
                while left > 0 {
                    let chunk = left.min(zeros.len());
                    stdout.write_all(&zeros[..chunk]).expect("write zeros");
                    left -= chunk;
                }
            }
            "--x-sleep-ms" => std::thread::sleep(std::time::Duration::from_millis(
                value
                    .expect("--x-sleep-ms a count")
                    .parse()
                    .expect("a count"),
            )),
            "--x-pid" => std::fs::write(
                value.expect("--x-pid a file"),
                std::process::id().to_string(),
            )
            .expect("write the pid"),
            "--x-child" => {
                std::fs::write(
                    value.expect("--x-child a file"),
                    spawn_sleeper().to_string(),
                )
                .expect("write the child's pid");
            }
            _ => {}
        }
    }
}

/// The flags this run's arguments ask for, each with the value after it.
fn argv_steps(args: &[String]) -> Vec<Step<'_>> {
    let mut steps = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index].starts_with("--x-") {
            steps.push(Step {
                flag: args[index].as_str(),
                value: args.get(index + 1).map(String::as_str),
            });
            index += 1;
        }
        index += 1;
    }
    steps
}

/// The conf lines as flags: `flag` first, the rest of the line its value.
fn conf_steps(conf: &str) -> Vec<Step<'_>> {
    conf.lines()
        .filter_map(|line| {
            let line = line.trim();
            (!line.is_empty() && !line.starts_with('#')).then(|| {
                let (flag, value) = line
                    .split_once(' ')
                    .map_or((line, ""), |(f, v)| (f, v.trim()));
                Step {
                    flag,
                    value: (!value.is_empty()).then_some(value),
                }
            })
        })
        .collect()
}

fn steps<'a>(args: &'a [String], conf: &'a str) -> Vec<Step<'a>> {
    let mut steps = argv_steps(args);
    steps.extend(conf_steps(conf));
    steps
}

/// The pid of a copy of this program that sleeps on the run's pipes.
/// Nothing waits on it: the point of the test is whether the runner's
/// kill reaches it, after which the system's init reaps it.
fn spawn_sleeper() -> u32 {
    let child = Command::new(std::env::current_exe().expect("this program"))
        .args(["--x-sleep-ms", "30000"])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("the child starts");
    let pid = child.id();
    std::mem::forget(child);
    pid
}

/// The conf file beside this binary, if one was written.
fn conf() -> String {
    let exe = std::env::current_exe().expect("this program");
    std::fs::read_to_string(exe.with_file_name("gnupg-stand-in.conf")).unwrap_or_default()
}

/// The status channel the runner named in `--status-fd`, if it gave one:
/// the file descriptor a Unix runner dup2'd into place, the inherited
/// HANDLE a Windows one did.
#[cfg(unix)]
fn status_channel(args: &[String]) -> Option<std::fs::File> {
    use std::os::fd::FromRawFd;
    let fd: i32 = args
        .iter()
        .position(|arg| arg == "--status-fd")
        .and_then(|at| args.get(at + 1))
        .and_then(|value| value.parse().ok())?;
    // SAFETY: the runner dup2'd its status pipe to this descriptor before
    // exec, so the number names a live channel this process owns.
    Some(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(windows)]
fn status_channel(args: &[String]) -> Option<std::fs::File> {
    use std::os::windows::io::FromRawHandle;
    let handle: usize = args
        .iter()
        .position(|arg| arg == "--status-fd")
        .and_then(|at| args.get(at + 1))
        .and_then(|value| value.parse().ok())?;
    // SAFETY: the runner marked its status pipe's write end inheritable
    // for the spawn, so the number names a live channel this process owns.
    Some(unsafe { std::fs::File::from_raw_handle(handle as _) })
}

#[cfg(not(any(unix, windows)))]
fn status_channel(_args: &[String]) -> Option<std::fs::File> {
    None
}
