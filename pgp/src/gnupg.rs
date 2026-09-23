//! Running a GnuPG program, `gpg` or `gpgsm`, the one way both engines do.
//!
//! Every call in `mailrs_pgp` and `mailrs_smime` ends up here. Nothing reads
//! what the programs print for a human, because that text is translated and
//! changes between releases; the machine-readable status lines are the
//! interface. The two programs report through the same library, so the
//! lines, the pipes and the rule about asking the person anything are the
//! same for both, and live here once.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::status::{Trust, Verdict};

/// The prefix GnuPG puts on every status line.
const STATUS: &str = "[GNUPG:] ";

/// Whether a run may put a pinentry window in front of the person.
///
/// Reading a message must never do that: a stranger's signature asks
/// gpg-agent to mark a root trusted, and gpg-agent asks through a window,
/// so opening the inbox would mean a window per message. Only a run that
/// needs the person's own secret key, to decrypt or to sign, may ask for
/// the passphrase that unlocks it. Every run names one of the two, so a new
/// call cannot forget the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pinentry {
    /// `--pinentry-mode error`: a request becomes an error, and the
    /// status lines say what that left unanswered.
    Never,
    /// The person's own pinentry, for the passphrase of their secret key.
    MayAsk,
}

/// One GnuPG binary, run against the person's own home or another one.
#[derive(Debug, Clone)]
pub struct Program {
    path: PathBuf,
    home: Option<PathBuf>,
}

impl Program {
    /// The first of `names` on `path`, in the order given.
    pub fn find_on(path: &str, names: &[&str]) -> Option<Program> {
        let path = names.iter().find_map(|name| lookup(path, name))?;
        Some(Program { path, home: None })
    }

    /// The binary the runs start.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs against another GNUPGHOME than the person's own. Tests keep a
    /// keyring of their own this way.
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    /// Runs the program with `input` on its stdin. `args` adds what the one
    /// call needs; the flags every call wants are already set, and
    /// `pinentry` says whether this run may ask the person anything.
    ///
    /// The status lines come back on a pipe of their own rather than
    /// beside the human messages on stderr. Those messages quote whatever
    /// a sender put in a user id, a subject or a file name, and a line in
    /// one that looked like a status line would otherwise be read as one.
    pub fn run(
        &self,
        input: &[u8],
        pinentry: Pinentry,
        args: impl FnOnce(&mut Command),
    ) -> std::io::Result<Run> {
        let (mut status_reader, status_writer) = std::io::pipe()?;
        let mut command = Command::new(&self.path);
        command.args(["--batch", "--no-tty"]);
        status_fd(&mut command, &status_writer)?;
        if let Some(home) = &self.home {
            command.arg("--homedir").arg(home);
        }
        if pinentry == Pinentry::Never {
            command.args(["--pinentry-mode", "error"]);
        }
        args(&mut command);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let spawned = command.spawn();
        // The child holds its own copy now. Keeping this one open would
        // leave the status pipe without an end, and the read below would
        // wait for ever.
        drop(status_writer);
        drop(command);
        let mut child = spawned?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("no stdin to write to"))?;
        // Writing the whole input before reading a byte deadlocks as soon as
        // the program fills an output pipe, which a message of any size
        // does. One thread writes and one reads the status lines while this
        // one reads stdout.
        let (output, status) = std::thread::scope(|scope| {
            scope.spawn(move || {
                // The program closing the pipe early, as it does when it
                // turns the arguments down, is not worth reporting: the
                // status lines say why.
                let _ = stdin.write_all(input);
            });
            let status = scope.spawn(move || {
                let mut lines = Vec::new();
                let _ = std::io::Read::read_to_end(&mut status_reader, &mut lines);
                lines
            });
            let output = child.wait_with_output();
            (output, status.join().unwrap_or_default())
        });
        let output = output?;
        Ok(Run {
            out: output.stdout,
            status: String::from_utf8_lossy(&status)
                .lines()
                .filter_map(|line| line.strip_prefix(STATUS).map(str::to_string))
                .collect(),
            ok: output.status.success(),
        })
    }
}

/// Hands the write end of the status pipe to the child as its fd 3 and
/// tells the program to write there.
#[cfg(unix)]
fn status_fd(command: &mut Command, writer: &std::io::PipeWriter) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    const FD: i32 = 3;
    let fd = writer.as_raw_fd();
    command.arg("--status-fd").arg(FD.to_string());
    // SAFETY: the closure runs in the child between fork and exec, where
    // only async-signal-safe calls are allowed; dup2 and fcntl are, and it
    // touches no memory the parent shares. dup2 leaves the copy without
    // close-on-exec, so fd 3 survives into the program; when the pipe
    // already sits at fd 3 there is nothing to copy and the flag comes off
    // by hand.
    unsafe {
        command.pre_exec(move || {
            let done = if fd == FD {
                libc::fcntl(FD, libc::F_SETFD, 0)
            } else {
                libc::dup2(fd, FD)
            };
            match done {
                -1 => Err(std::io::Error::last_os_error()),
                _ => Ok(()),
            }
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn status_fd(_command: &mut Command, _writer: &std::io::PipeWriter) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "GnuPG status lines need a pipe of their own, which this system cannot give",
    ))
}

/// One run of gpg or gpgsm.
pub struct Run {
    /// What the program wrote to stdout: a signature, the plain text, a
    /// listing.
    pub out: Vec<u8>,
    /// The status lines it wrote, in order, without the prefix.
    pub status: Vec<String>,
    pub ok: bool,
}

impl Run {
    /// The rest of the first status line that starts with `keyword`.
    pub fn field(&self, keyword: &str) -> Option<&str> {
        self.status.iter().find_map(|line| {
            line.strip_prefix(keyword)
                .filter(|rest| rest.is_empty() || rest.starts_with(' '))
                .map(str::trim)
        })
    }

    pub fn says(&self, keyword: &str) -> bool {
        self.field(keyword).is_some()
    }

    /// Every signature the status lines describe, in order.
    pub fn signatures(&self) -> Vec<Seen> {
        seen(&self.status)
    }
}

/// One signature as the status lines describe it, before either engine
/// says what it means: gpg names a key by its id and a user id, gpgsm a
/// certificate by its fingerprint and its subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub verdict: Verdict,
    /// The first word after the verdict: a key id for gpg, a fingerprint
    /// for gpgsm.
    pub id: Option<String>,
    /// The rest of that line: a user id or a subject.
    pub name: Option<String>,
    /// From `VALIDSIG`, for a signature the program could check.
    pub fingerprint: Option<String>,
    /// From the `TRUST_` line, or `None` when there was none.
    pub trust: Option<Trust>,
}

/// Every signature in `status`, in order. Each `NEWSIG` starts the next
/// one, and the lines before the first belong to one of their own, which
/// is how a program that writes no `NEWSIG` reads.
pub fn seen<S: AsRef<str>>(status: &[S]) -> Vec<Seen> {
    let mut groups: Vec<Vec<&str>> = vec![Vec::new()];
    for line in status {
        let line = line.as_ref();
        if split(line).0 == "NEWSIG" {
            groups.push(Vec::new());
        } else if let Some(group) = groups.last_mut() {
            group.push(line);
        }
    }
    groups.into_iter().filter_map(|group| one(&group)).collect()
}

/// The signature one group of lines describes, if any.
fn one(lines: &[&str]) -> Option<Seen> {
    let mut found: Option<Seen> = None;
    let mut fingerprint = None;
    let mut trust = None;
    for line in lines {
        let (keyword, rest) = split(line);
        let verdict = match keyword {
            "GOODSIG" => Verdict::Good,
            "BADSIG" => Verdict::Bad,
            "EXPKEYSIG" => Verdict::ExpiredKey,
            "REVKEYSIG" => Verdict::RevokedKey,
            "EXPSIG" => Verdict::Expired,
            // `ERRSIG <keyid> <algo> <hash> <class> <time> <rc>`, where 9
            // means gpg has no key for the signer.
            "ERRSIG" => match rest.split_whitespace().nth(5) {
                Some("9") => Verdict::NoKey,
                _ => Verdict::Unchecked,
            },
            // gpg says this beside `ERRSIG 9`, and on its own when it read
            // a signature it could not even look a key up for.
            "NO_PUBKEY" => Verdict::NoKey,
            // gpgsm reports a certificate it could not find against the
            // step that went looking, rather than as an `ERRSIG`.
            "ERROR" if rest.starts_with("verify.findkey") => {
                found.get_or_insert(Seen {
                    verdict: Verdict::NoKey,
                    id: None,
                    name: None,
                    fingerprint: None,
                    trust: None,
                });
                continue;
            }
            "VALIDSIG" => {
                fingerprint = rest.split_whitespace().next().map(str::to_string);
                continue;
            }
            "TRUST_UNDEFINED" | "TRUST_NEVER" | "TRUST_MARGINAL" | "TRUST_FULLY"
            | "TRUST_ULTIMATE" => {
                trust = Some(trust_of(keyword));
                continue;
            }
            _ => continue,
        };
        // A verdict line settles the signature; `NO_PUBKEY` and `ERRSIG`
        // arrive as a pair about the same one, in either order.
        let settled = found
            .as_ref()
            .is_some_and(|seen| !matches!(seen.verdict, Verdict::NoKey | Verdict::Unchecked));
        if settled {
            continue;
        }
        let (id, name) = match rest.split_once(' ') {
            Some((id, name)) => (id, Some(name.trim().to_string())),
            None => (rest, None),
        };
        let unnamed = matches!(verdict, Verdict::NoKey | Verdict::Unchecked);
        let verdict = match (&found, verdict) {
            // gpg knew the key was missing before it gave up.
            (Some(seen), Verdict::Unchecked) if seen.verdict == Verdict::NoKey => Verdict::NoKey,
            (_, verdict) => verdict,
        };
        found = Some(Seen {
            verdict,
            id: (!id.is_empty()).then(|| id.to_string()),
            name: name.filter(|_| !unnamed),
            fingerprint: None,
            trust: None,
        });
    }
    found.map(|seen| Seen {
        fingerprint,
        trust,
        ..seen
    })
}

fn trust_of(keyword: &str) -> Trust {
    match keyword {
        "TRUST_NEVER" => Trust::Never,
        "TRUST_MARGINAL" => Trust::Marginal,
        "TRUST_FULLY" => Trust::Full,
        "TRUST_ULTIMATE" => Trust::Ultimate,
        _ => Trust::Unknown,
    }
}

fn split(line: &str) -> (&str, &str) {
    match line.split_once(' ') {
        Some((keyword, rest)) => (keyword, rest.trim_start()),
        None => (line, ""),
    }
}

/// The address at the end of a status line that leads with a reason code,
/// such as `INV_RECP <reason> <recipient>`.
pub fn named(rest: &str) -> String {
    rest.split_once(' ')
        .map_or(rest, |(_, who)| who)
        .trim()
        .to_string()
}

/// How a program should be asked for the key or certificate belonging to
/// one address. The angle brackets make it an exact match on the address
/// rather than a search for that text anywhere in a user id, so asking for
/// `ann@example.com` cannot land on one that spells itself
/// `ann@example.com.example.net`.
pub fn user_id(address: &str) -> String {
    format!("<{}>", address.trim().trim_matches(['<', '>']))
}

/// The first entry of `path` holding an executable file called `name`.
fn lookup(path: &str, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_program(candidate))
}

#[cfg(unix)]
fn is_program(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_program(path: &Path) -> bool {
    path.is_file()
}
