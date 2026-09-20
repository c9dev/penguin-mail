//! Finding the person's `gpg` and running it.
//!
//! Every call in this crate ends up here. Nothing reads what gpg prints for
//! a human, because that text is translated and changes between releases;
//! the machine-readable `--status-fd` lines are the interface.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::PgpError;

/// The prefix gpg puts on every status line.
const STATUS: &str = "[GNUPG:] ";

/// The person's own GnuPG: their keys, their agent, their pinentry, their
/// trust database. Penguin Mail never holds a key or a passphrase itself.
#[derive(Debug, Clone)]
pub struct Pgp {
    program: PathBuf,
    home: Option<PathBuf>,
}

impl Pgp {
    /// Looks for `gpg` on PATH, then `gpg2` for the distributions that still
    /// name it that way.
    pub fn find() -> Result<Pgp, PgpError> {
        Pgp::find_on(&std::env::var("PATH").unwrap_or_default())
    }

    /// Like [`Pgp::find`], but searches `path` instead of the environment's.
    /// Tests use this to see what an empty PATH does.
    pub fn find_on(path: &str) -> Result<Pgp, PgpError> {
        let program = ["gpg", "gpg2"]
            .into_iter()
            .find_map(|name| lookup(path, name))
            .ok_or(PgpError::NoGpg)?;
        Ok(Pgp {
            program,
            home: None,
        })
    }

    /// The binary the calls run.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Runs against another GNUPGHOME than the person's own. Tests keep a
    /// keyring of their own this way.
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    /// Runs gpg with `input` on its stdin. `args` adds what the one call
    /// needs; the flags every call wants are already set.
    pub(crate) fn run(
        &self,
        input: &[u8],
        args: impl FnOnce(&mut Command),
    ) -> Result<Run, PgpError> {
        let mut command = Command::new(&self.program);
        command.args(["--batch", "--no-tty", "--status-fd", "2"]);
        if let Some(home) = &self.home {
            command.arg("--homedir").arg(home);
        }
        args(&mut command);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|err| self.cannot_run(&err))?;
        let mut stdin = child.stdin.take().expect("stdin is a pipe");
        // Writing the whole input before reading a byte deadlocks as soon as
        // gpg fills the output pipe, which a message of any size does. One
        // thread writes while this one reads both of gpg's pipes.
        let output = std::thread::scope(|scope| {
            scope.spawn(move || {
                // gpg closing the pipe early, as it does when it turns the
                // arguments down, is not worth reporting: the status lines
                // below say why.
                let _ = stdin.write_all(input);
            });
            child.wait_with_output()
        })
        .map_err(|err| self.cannot_run(&err))?;
        Ok(Run {
            out: output.stdout,
            status: String::from_utf8_lossy(&output.stderr)
                .lines()
                .filter_map(|line| line.strip_prefix(STATUS).map(str::to_string))
                .collect(),
            ok: output.status.success(),
        })
    }

    fn cannot_run(&self, err: &std::io::Error) -> PgpError {
        PgpError::CannotRun {
            program: self.program.display().to_string(),
            reason: err.to_string(),
        }
    }
}

/// One run of gpg.
pub(crate) struct Run {
    /// What gpg wrote to stdout: a signature, the plain text, a key listing.
    pub out: Vec<u8>,
    /// The `[GNUPG:]` lines it wrote beside that, in order, without the prefix.
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

    /// What to report when gpg would not do what it was asked. The status
    /// lines name the reason; the exit code on its own never does.
    pub fn failure(&self) -> PgpError {
        if self.says("DECRYPTION_FAILED") && self.says("NO_SECKEY") {
            return PgpError::NotForYou;
        }
        if self.says("NODATA") {
            return PgpError::NotPgp;
        }
        if let Some(rest) = self.field("INV_RECP") {
            // `INV_RECP <reason> <recipient>`.
            let who = rest.split_once(' ').map_or(rest, |(_, who)| who);
            return PgpError::NoKeyFor(who.to_string());
        }
        PgpError::Gpg(match self.field("FAILURE") {
            Some(rest) => rest.to_string(),
            None => self.status.join("; "),
        })
    }
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
