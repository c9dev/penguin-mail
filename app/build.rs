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
}
