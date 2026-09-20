#[derive(Debug, Clone, thiserror::Error)]
pub enum PgpError {
    /// Nothing on PATH answers to `gpg`. A caller that gets this leaves the
    /// encryption controls out rather than offering something that cannot work.
    #[error("no gpg on this computer; install GnuPG to read or send OpenPGP mail")]
    NoGpg,
    #[error("could not run {program}: {reason}")]
    CannotRun { program: String, reason: String },
    /// Handing gpg a detached signature means putting it in a file first.
    #[error("could not write a temporary file: {0}")]
    Temp(String),
    /// The message was encrypted to keys this computer holds no secret half of.
    #[error("this message is encrypted to a key this computer does not hold")]
    NotForYou,
    /// gpg holds no secret key for the address the message is being sent from.
    #[error("no secret key to sign as {0}")]
    CannotSign(String),
    /// A recipient has no key, or only keys that cannot encrypt.
    #[error("no usable encryption key for {0}")]
    NoKeyFor(String),
    /// A body that was meant to hold inline PGP holds none.
    #[error("this text holds no PGP block")]
    NotPgp,
    #[error("gpg failed: {0}")]
    Gpg(String),
}
