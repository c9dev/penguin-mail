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
}

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
