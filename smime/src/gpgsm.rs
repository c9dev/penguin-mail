//! Finding the person's `gpgsm` and running it through
//! [`mailrs_pgp::gnupg`], the runner both engines share.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use mailrs_pgp::gnupg::{Pinentry, Program, Run, named};

use crate::error::SmimeError;

/// The person's own GnuPG: their certificates, their agent, their pinentry,
/// their list of roots to trust. Penguin Mail never holds a secret key or a
/// passphrase itself.
#[derive(Debug, Clone)]
pub struct Smime {
    program: Program,
    /// How long checking a signature may wait on dirmngr's revocation
    /// check before it goes ahead without one.
    revocation_wait: Duration,
}

/// How long a signature check waits on the certificate authority. A CRL
/// server that answers at all answers well inside this; one that holds the
/// connection open without a word would otherwise hold the message closed
/// for as long as it likes.
pub const REVOCATION_WAIT: Duration = Duration::from_secs(10);

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
        let program = Program::find_on(path, &["gpgsm"]).ok_or(SmimeError::NoGpgsm)?;
        Ok(Smime {
            program,
            revocation_wait: REVOCATION_WAIT,
        })
    }

    /// The binary the calls run.
    pub fn program(&self) -> &Path {
        self.program.path()
    }

    /// Runs against another GNUPGHOME than the person's own. Tests keep
    /// certificates of their own this way.
    pub fn with_home(self, home: impl Into<PathBuf>) -> Self {
        Smime {
            program: self.program.with_home(home),
            ..self
        }
    }

    /// Waits `wait` rather than [`REVOCATION_WAIT`] on a revocation check.
    /// Tests use this to see the limit work without waiting ten seconds.
    pub fn with_revocation_wait(self, wait: Duration) -> Self {
        Smime {
            revocation_wait: wait,
            ..self
        }
    }

    /// When the keybox or the trust list last changed; see
    /// [`mailrs_pgp::gnupg::Program::keyring_stamp`].
    pub fn keyring_stamp(&self) -> Option<std::time::SystemTime> {
        self.program.keyring_stamp()
    }

    /// Runs gpgsm with `input` on its stdin; see
    /// [`mailrs_pgp::gnupg::Program::run`].
    ///
    /// Verifying a signature whose root nobody here vouches for makes
    /// gpgsm ask gpg-agent to mark that root trusted, and gpg-agent puts
    /// that question on the screen. Most mail signed by a company arrives
    /// from a root the reader has never seen, so every call that only reads
    /// passes [`Pinentry::Never`]: the request becomes an error gpgsm
    /// swallows, and the verdict comes back the same, not trusted, which is
    /// the honest answer and the one the card should show.
    pub(crate) fn run(
        &self,
        input: &[u8],
        pinentry: Pinentry,
        args: impl FnOnce(&mut Command),
    ) -> Result<Run, SmimeError> {
        self.program
            .run(input, pinentry, args)
            .map_err(|err| self.cannot_run(&err))
    }

    /// Like [`Smime::run`] for a read, giving gpgsm the revocation wait to
    /// answer in. An error of kind [`std::io::ErrorKind::TimedOut`] means
    /// it did not, and nothing of that run is left behind; see
    /// [`mailrs_pgp::gnupg::Program::run_within`].
    pub(crate) fn run_limited(
        &self,
        input: &[u8],
        args: impl FnOnce(&mut Command),
    ) -> std::io::Result<Run> {
        self.program
            .run_within(self.revocation_wait, input, Pinentry::Never, args)
    }

    pub(crate) fn cannot_run(&self, err: &std::io::Error) -> SmimeError {
        SmimeError::CannotRun {
            program: self.program.path().display().to_string(),
            reason: err.to_string(),
        }
    }
}

/// What to report when gpgsm would not do what it was asked. The status
/// lines name the reason; the exit code on its own never does.
pub(crate) fn failure(run: &Run) -> SmimeError {
    // gpgsm says this for a message enveloped to somebody else and for one
    // that arrived damaged alike. The first is what people meet, so it is
    // what the sentence names.
    if run.says("DECRYPTION_FAILED") {
        return SmimeError::NotForYou;
    }
    if run.says("NODATA") {
        return SmimeError::NotSmime;
    }
    // An address gpgsm cannot sign as is also an address it cannot encrypt
    // to, so it writes both lines and the signer comes first.
    if let Some(rest) = run.field("INV_SGNR") {
        return SmimeError::CannotSign(named(rest));
    }
    if let Some(rest) = run.field("INV_RECP") {
        return SmimeError::NoCertificateFor(named(rest));
    }
    SmimeError::Gpgsm(match run.field("FAILURE") {
        Some(rest) => rest.to_string(),
        None => run.status.join("; "),
    })
}
