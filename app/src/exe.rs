//! Where the running program lives on disk.

use std::io;
use std::path::PathBuf;

/// The path to start Penguin Mail from. Installing a new build replaces
/// the file under a running copy, and Linux then reports that copy's
/// executable as `<path> (deleted)`. Starting that path fails, so this
/// answers with the new file that took its place.
pub fn path() -> io::Result<PathBuf> {
    std::env::current_exe().map(replaced)
}

/// The file a login item names and an update restarts into. An AppImage
/// runs from a mount that disappears when it quits and still holds the old
/// version after an update, so this is the AppImage file itself, which the
/// AppImage runtime names in `$APPIMAGE`.
pub fn launcher() -> io::Result<PathBuf> {
    if crate::packaging::BUILT_FOR == crate::packaging::Packaging::AppImage
        && let Some(file) = std::env::var_os("APPIMAGE")
    {
        return Ok(PathBuf::from(file));
    }
    path()
}

fn replaced(exe: PathBuf) -> PathBuf {
    match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(installed) => PathBuf::from(installed),
        None => exe,
    }
}

#[cfg(test)]
mod tests {
    use super::replaced;
    use std::path::PathBuf;

    #[test]
    fn a_replaced_executable_resolves_to_the_file_that_replaced_it() {
        assert_eq!(
            replaced(PathBuf::from("/home/ann/.local/bin/penguin-mail (deleted)")),
            PathBuf::from("/home/ann/.local/bin/penguin-mail")
        );
    }

    #[test]
    fn an_executable_still_on_disk_keeps_its_path() {
        assert_eq!(
            replaced(PathBuf::from("/usr/bin/penguin-mail")),
            PathBuf::from("/usr/bin/penguin-mail")
        );
    }
}
