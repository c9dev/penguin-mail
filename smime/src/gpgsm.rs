//! Finding the person's `gpgsm` and running it.
//!
//! Every call in this crate ends up here. Nothing reads what gpgsm prints
//! for a human, because that text is translated and changes between
//! releases; the machine-readable `--status-fd` lines are the interface.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::SmimeError;

/// The prefix gpgsm puts on every status line.
const STATUS: &str = "[GNUPG:] ";

/// The person's own GnuPG: their certificates, their agent, their pinentry,
/// their list of roots to trust. Penguin Mail never holds a secret key or a
/// passphrase itself.
#[derive(Debug, Clone)]
pub struct Smime {
    program: PathBuf,
    home: Option<PathBuf>,
}

impl Smime {
    /// Looks for `gpgsm` on PATH. It ships with GnuPG, so a computer with
    /// gpg on it usually has this too, but the two are packaged apart often
    /// enough to be worth asking separately.
    pub fn find() -> Result<Smime, SmimeError> {
        Smime::find_on(&std::env::var("PATH").unwrap_or_default())
    }

    /// Like [`Smime::find`], but searches `path` instead of the
    /// environment's. Tests use this to see what an empty PATH does.
    pub fn find_on(path: &str) -> Result<Smime, SmimeError> {
        let program = lookup(path, "gpgsm").ok_or(SmimeError::NoGpgsm)?;
        Ok(Smime {
            program,
            home: None,
        })
    }

    /// The binary the calls run.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Runs against another GNUPGHOME than the person's own. Tests keep
    /// certificates of their own this way.
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    /// Runs gpgsm with `input` on its stdin. `args` adds what the one call
    /// needs; the flags every call wants are already set.
    pub(crate) fn run(
        &self,
        input: &[u8],
        args: impl FnOnce(&mut Command),
    ) -> Result<Run, SmimeError> {
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
        // Writing the whole input before reading a byte deadlocks as soon
        // as gpgsm fills the output pipe, which a message of any size does.
        // One thread writes while this one reads both of gpgsm's pipes.
        let output = std::thread::scope(|scope| {
            scope.spawn(move || {
                // gpgsm closing the pipe early, as it does when it turns
                // the arguments down, is not worth reporting: the status
                // lines below say why.
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

    fn cannot_run(&self, err: &std::io::Error) -> SmimeError {
        SmimeError::CannotRun {
            program: self.program.display().to_string(),
            reason: err.to_string(),
        }
    }
}

/// One run of gpgsm.
pub(crate) struct Run {
    /// What gpgsm wrote to stdout: a signature, the plain text, a listing.
    pub out: Vec<u8>,
    /// The `[GNUPG:]` lines it wrote beside that, in order, without the
    /// prefix.
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

    /// What to report when gpgsm would not do what it was asked. The status
    /// lines name the reason; the exit code on its own never does.
    pub fn failure(&self) -> SmimeError {
        // gpgsm says this for a message enveloped to somebody else and for
        // one that arrived damaged alike. The first is what people meet, so
        // it is what the sentence names.
        if self.says("DECRYPTION_FAILED") {
            return SmimeError::NotForYou;
        }
        if self.says("NODATA") {
            return SmimeError::NotSmime;
        }
        // An address gpgsm cannot sign as is also an address it cannot
        // encrypt to, so it writes both lines and the signer comes first.
        if let Some(rest) = self.field("INV_SGNR") {
            return SmimeError::CannotSign(named(rest));
        }
        if let Some(rest) = self.field("INV_RECP") {
            return SmimeError::NoCertificateFor(named(rest));
        }
        SmimeError::Gpgsm(match self.field("FAILURE") {
            Some(rest) => rest.to_string(),
            None => self.status.join("; "),
        })
    }
}

/// The address at the end of a status line that leads with a reason code.
fn named(rest: &str) -> String {
    rest.split_once(' ')
        .map_or(rest, |(_, who)| who)
        .trim()
        .to_string()
}

/// How gpgsm should be asked for the certificate belonging to one address.
/// The angle brackets make it an exact match on the address rather than a
/// search for that text anywhere in a subject, so asking for
/// `ann@example.com` cannot land on a certificate for
/// `ann@example.com.example.net`.
pub(crate) fn user_id(address: &str) -> String {
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
