//! Shared by the integration tests that stop the agent gpg or gpgsm started.

use std::path::Path;

/// gpgconf beside the program these tests found, or the bare name for
/// PATH to resolve: Gpg4win keeps itself off PATH but puts the whole
/// family in one directory.
pub fn gpgconf(program: &Path) -> std::path::PathBuf {
    let beside = program.with_file_name("gpgconf");
    if beside.exists() || beside.with_extension("exe").exists() {
        beside
    } else {
        "gpgconf".into()
    }
}
