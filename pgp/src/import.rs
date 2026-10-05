//! Putting the keys in a file the person picked into their keyring.

use crate::error::PgpError;
use crate::gnupg::{Import, Pinentry, import_counts, imports};
use crate::gpg::{Pgp, failure};
use crate::keys::{first_user_id, split_user_id};

impl Pgp {
    /// Imports every key in `file`, an armored or binary OpenPGP file, and
    /// says what each one brought, with the name and address on it.
    ///
    /// The person picked the file, so the run may ask them something.
    /// gpg 2.4 takes a secret key without its passphrase, keeping the
    /// protection the key came with, and asks for it the first time the
    /// key signs or decrypts.
    pub fn import(&self, file: &[u8]) -> Result<Vec<Import>, PgpError> {
        let run = self.run(file, Pinentry::MayAsk, |command| {
            command.arg("--import");
        })?;
        let mut found = imports(&run.status);
        if found.is_empty() {
            let considered = import_counts(&run.status).map_or(0, |counts| counts.considered);
            return Err(match run.says("NODATA") || considered == 0 {
                true => PgpError::NotAKey,
                false => failure(&run),
            });
        }
        self.name_imports(&mut found);
        Ok(found)
    }

    /// Fills in who each imported key names, from one listing of them all.
    /// A listing that fails leaves the names out rather than the import.
    fn name_imports(&self, found: &mut [Import]) {
        let Ok(run) = self.run(&[], Pinentry::Never, |command| {
            command.args(["--with-colons", "--list-keys", "--"]);
            command.args(found.iter().map(|import| import.fingerprint.as_str()));
        }) else {
            return;
        };
        let listing = String::from_utf8_lossy(&run.out);
        for import in found {
            if let Some(uid) = first_user_id(&listing, &import.fingerprint) {
                (import.name, import.address) = split_user_id(&uid);
            }
        }
    }
}
