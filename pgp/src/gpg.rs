//! Finding the person's `gpg` and running it through [`crate::gnupg`].

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::PgpError;
use crate::gnupg::{Pinentry, Program, Run, named};

/// The person's own GnuPG: their keys, their agent, their pinentry, their
/// trust database. Penguin Mail never holds a key or a passphrase itself.
#[derive(Debug, Clone)]
pub struct Pgp {
    program: Program,
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
        let program = Program::find_on(path, &["gpg", "gpg2"]).ok_or(PgpError::NoGpg)?;
        Ok(Pgp { program })
    }

    /// The binary the calls run.
    pub fn program(&self) -> &Path {
        self.program.path()
    }

    /// Runs against another GNUPGHOME than the person's own. Tests keep a
    /// keyring of their own this way.
    pub fn with_home(self, home: impl Into<PathBuf>) -> Self {
        Pgp {
            program: self.program.with_home(home),
        }
    }

    /// Runs gpg with `input` on its stdin; see [`Program::run`].
    pub(crate) fn run(
        &self,
        input: &[u8],
        pinentry: Pinentry,
        args: impl FnOnce(&mut Command),
    ) -> Result<Run, PgpError> {
        self.program
            .run(input, pinentry, args)
            .map_err(|err| PgpError::CannotRun {
                program: self.program.path().display().to_string(),
                reason: err.to_string(),
            })
    }
}

/// What every run that reads a message adds. The person's gpg.conf may
/// say `auto-key-retrieve`, and then a signature from a key gpg lacks sends
/// it to a key server or to the sender's own domain for one: a read
/// receipt, sent the moment the message opens. `auto-key-import` would put
/// a key that travelled inside the signature into the keyring, where the
/// composer would offer it for encryption. Opening a message does neither.
pub(crate) fn reading(command: &mut Command) {
    command.args(["--no-auto-key-retrieve", "--no-auto-key-import"]);
}

/// What to report when gpg would not do what it was asked. The status
/// lines name the reason; the exit code on its own never does.
pub(crate) fn failure(run: &Run) -> PgpError {
    if run.says("DECRYPTION_FAILED") && run.says("NO_SECKEY") {
        return PgpError::NotForYou;
    }
    if run.says("NODATA") {
        return PgpError::NotPgp;
    }
    // `INV_RECP <reason> <recipient>`, and the same shape for the signer.
    if let Some(rest) = run.field("INV_RECP") {
        return PgpError::NoKeyFor(named(rest));
    }
    if let Some(rest) = run.field("INV_SGNR") {
        return PgpError::CannotSign(named(rest));
    }
    PgpError::Gpg(match run.field("FAILURE") {
        Some(rest) => rest.to_string(),
        None => run.status.join("; "),
    })
}
