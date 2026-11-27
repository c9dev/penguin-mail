use std::fmt::Write as _;
use std::process::Command;

fn main() {
    let out = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let status = Command::new("glib-compile-resources")
        .args([
            "--sourcedir",
            "data",
            "--target",
            &format!("{out}/penguin-mail.gresource"),
            "data/penguin-mail.gresource.xml",
        ])
        .status()
        .expect("glib-compile-resources runs; install libglib2.0-dev-bin");
    assert!(status.success(), "glib-compile-resources failed");
    println!("cargo:rerun-if-changed=data");
    write_language_names(&out);
    println!("cargo:rerun-if-changed=../po");
    embed_windows_icon();
}

/// Embeds the app icon and the version block in the Windows binary, so
/// Explorer, the taskbar and the notification area draw the icon without a
/// file beside the exe, and Windows names the app "Penguin Mail" (in Open
/// With, the firewall and Task Manager) rather than by its file name.
/// Missing windres or rc.exe skips them rather than breaking a build.
#[cfg(windows)]
fn embed_windows_icon() {
    let out = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let rc = format!("{out}/penguin-mail.rc");
    std::fs::write(&rc, windows_resources()).expect("write the resource script");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        // GNU ld reads a COFF resource object straight from windres;
        // embed-resource hands it an MSVC-format library instead.
        let res = format!("{out}/penguin-mail.res");
        let made = Command::new("windres")
            .args([rc.as_str(), "-O", "coff", "-o", &res])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if made {
            println!("cargo:rustc-link-arg={res}");
        } else {
            println!("cargo:warning=the Windows icon was not embedded: windres failed");
        }
        return;
    }
    let result = embed_resource::compile(&rc, embed_resource::NONE);
    if result != embed_resource::CompilationResult::Ok {
        println!("cargo:warning=the Windows icon was not embedded: {result:?}");
    }
}

/// The resource script: the two icons by their full path, since the
/// script sits in OUT_DIR, and a version block from Cargo's version.
#[cfg(windows)]
fn windows_resources() -> String {
    let var = |name: &str| std::env::var(name).expect("cargo sets the package variables");
    let dir = var("CARGO_MANIFEST_DIR").replace('\\', "/");
    let version = var("CARGO_PKG_VERSION");
    let numbers = format!(
        "{},{},{},0",
        var("CARGO_PKG_VERSION_MAJOR"),
        var("CARGO_PKG_VERSION_MINOR"),
        var("CARGO_PKG_VERSION_PATCH")
    );
    format!(
        r#"1 ICON "{dir}/data/win32/penguin-mail.ico"
2 ICON "{dir}/data/win32/penguin-mail-unread.ico"
1 VERSIONINFO
FILEVERSION {numbers}
PRODUCTVERSION {numbers}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Penguin Mail"
      VALUE "FileDescription", "Penguin Mail"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "penguin-mail"
      VALUE "LegalCopyright", "GPL-3.0-or-later"
      VALUE "OriginalFilename", "penguin-mail.exe"
      VALUE "ProductName", "Penguin Mail"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    )
}

#[cfg(not(windows))]
fn embed_windows_icon() {}

/// Writes the code and the name of every translation in `po/`, so the
/// Language preference can name a language in its own language. Each `.po`
/// carries both in its header: `Language:` and `X-Language-Name:`. Adding
/// a translation with those two lines adds a row, and nothing else here
/// knows which languages exist.
fn write_language_names(out: &str) {
    let entries = std::fs::read_dir("../po").expect("the po directory sits beside the crates");
    let mut files: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|kind| kind == "po"))
        .collect();
    files.sort();
    let mut rows = String::new();
    for path in files {
        let text = std::fs::read_to_string(&path).expect("a po file is UTF-8");
        let header = |name: &str| {
            text.lines()
                .find_map(|line| line.trim_matches('"').strip_prefix(name))
                .map(|value| value.trim().trim_end_matches("\\n").to_string())
        };
        let (Some(code), Some(name)) = (header("Language:"), header("X-Language-Name:")) else {
            println!(
                "cargo:warning={} has no Language or X-Language-Name header",
                path.display()
            );
            continue;
        };
        writeln!(rows, "    ({code:?}, {name:?}),").expect("a string takes what it is given");
    }
    let table = format!(
        "/// Every translation in `po/`, by code and by its own name.\n\
         pub const TRANSLATED: &[(&str, &str)] = &[\n{rows}];\n"
    );
    std::fs::write(format!("{out}/languages.rs"), table).expect("OUT_DIR is writable");
}
