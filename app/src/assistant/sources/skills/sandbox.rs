//! Running one command inside a sandbox.
//!
//! The sandbox sees the system's programs read-only and the skill's folder
//! read-only, and one scratch folder it may write. It has no home folder,
//! so no mail database, keys, tokens or SSH keys; no session bus or
//! display; and no network unless the skill is allowed one. Whatever the
//! command started dies with it, however the processes detached
//! themselves.

use std::io::Read;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::{arguments, find_bwrap};

#[cfg(unix)]
pub use unix::run;
#[cfg(windows)]
pub use windows::run;

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
    /// The sandboxing program: bubblewrap on Unix, busybox on Windows.
    pub program: &'a std::path::Path,
    /// The skill's folder, read-only inside.
    pub skill: &'a std::path::Path,
    /// The scratch folder, read-write inside and the working directory.
    pub work: &'a std::path::Path,
    pub network: bool,
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

/// Whether the sandbox works here: its program is installed and can start
/// a command with the isolation above. The answer is kept for the rest of
/// the run.
pub fn check() -> Result<PathBuf, String> {
    static ANSWER: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    #[cfg(unix)]
    {
        ANSWER.get_or_init(unix::probe).clone()
    }
    #[cfg(windows)]
    {
        ANSWER.get_or_init(windows::probe).clone()
    }
}
