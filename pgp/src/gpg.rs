//! Finding the person's `gpg` and running it.
//!
//! Every call in this crate ends up here. Nothing reads what gpg prints for
//! a human, because that text is translated and changes between releases;
//! the machine-readable `--status-fd` lines are the interface.

use std::path::{Path, PathBuf};

use crate::error::PgpError;

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
