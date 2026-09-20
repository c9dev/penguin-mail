#[derive(Debug, Clone, thiserror::Error)]
pub enum SmimeError {
    /// Nothing on PATH answers to `gpgsm`. A caller that gets this leaves
    /// the S/MIME controls out rather than offering something that cannot
    /// work.
    #[error("no gpgsm on this computer; install GnuPG to read or send S/MIME mail")]
    NoGpgsm,
    #[error("could not run {program}: {reason}")]
    CannotRun { program: String, reason: String },
    /// Handing gpgsm a detached signature means putting it in a file first.
    #[error("could not write a temporary file: {0}")]
    Temp(String),
    /// The message was enveloped to certificates this computer holds no
    /// secret key for.
    #[error("this message is encrypted to a certificate this computer does not hold")]
    NotForYou,
    /// gpgsm holds no secret key for the address the message is being sent
    /// from.
    #[error("no secret key to sign as {0}")]
    CannotSign(String),
    /// A recipient has no certificate, or only ones that cannot encrypt.
    #[error("no usable certificate for {0}")]
    NoCertificateFor(String),
    /// A part that was meant to hold CMS holds none.
    #[error("this part holds no S/MIME data")]
    NotSmime,
    #[error("gpgsm failed: {0}")]
    Gpgsm(String),
}
