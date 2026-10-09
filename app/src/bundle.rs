//! Finds the GTK data shipped beside the executable in a macOS release.

use std::path::Path;

use sha2::{Digest, Sha256};

pub fn prepare() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let Some(contents) = exe.parent().and_then(Path::parent) else {
        return Ok(());
    };
    let resources = contents.join("Resources");
    let template = resources.join("loaders.cache.in");
    if !template.is_file() {
        return Ok(());
    }
    // The signed bundle is read-only; each location gets its own loader cache.
    let cache = gtk::glib::user_cache_dir().join("penguin-mail/bundle");
    std::fs::create_dir_all(&cache)?;
    let hash: String = Sha256::digest(contents.as_os_str().as_encoded_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let file = cache.join(format!("{hash}.cache"));
    let temporary = cache.join(format!("{hash}-{}.tmp", std::process::id()));
    let text = loader_paths(&std::fs::read_to_string(template)?, contents);
    {
        use std::io::Write;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        output.write_all(text.as_bytes())?;
    }
    std::fs::rename(&temporary, &file)?;
    // Startup, before GTK or any worker can read the environment.
    unsafe {
        std::env::set_var("XDG_DATA_DIRS", resources.join("share"));
        std::env::set_var(
            "GSETTINGS_SCHEMA_DIR",
            resources.join("share/glib-2.0/schemas"),
        );
        std::env::set_var("PENGUIN_MAIL_LOCALE_DIR", resources.join("share/locale"));
        std::env::set_var("GDK_PIXBUF_MODULE_FILE", file);
        std::env::set_var("GIO_MODULE_DIR", resources.join("gio/modules"));
        std::env::set_var("FONTCONFIG_PATH", resources.join("etc/fonts"));
        std::env::set_var("FONTCONFIG_FILE", resources.join("etc/fonts/fonts.conf"));
    }
    Ok(())
}

fn loader_paths(template: &str, contents: &Path) -> String {
    let path = contents
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    template.replace("@CONTENTS@", &path)
}

#[cfg(test)]
mod tests {
    #[test]
    fn loader_paths_keep_spaces_and_escape_quotes_and_backslashes() {
        let path = std::path::Path::new("/Applications/A \"mail\"\\copy.app/Contents");
        assert_eq!(
            super::loader_paths("\"@CONTENTS@/Frameworks/svg.so\"\n", path),
            "\"/Applications/A \\\"mail\\\"\\\\copy.app/Contents/Frameworks/svg.so\"\n"
        );
    }
}
