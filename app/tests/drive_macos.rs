//! The driver must report the app's exit status and stop only its own process.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn the_driver_preserves_failure_and_limits_the_watchdog_to_its_child() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for name in ["scripts", "bin", "target/debug"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
    }
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/drive-macos.sh"),
        root.join("scripts/drive-macos.sh"),
    ).unwrap();
    executable(&root.join("bin/cargo"), "#!/bin/sh\nexit 0\n");
    executable(&root.join("bin/sleep"), "#!/bin/sh\nexec /bin/sleep 0.2\n");
    std::fs::write(root.join("steps.txt"), "quit\n").unwrap();
    let run = || Command::new("sh")
        .arg(root.join("scripts/drive-macos.sh"))
        .arg(root.join("steps.txt"))
        .env("PATH", format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap()))
        .output().unwrap();
    let app = root.join("target/debug/penguin-mail");
    executable(&app, "#!/bin/sh\necho drive: done\nexit 0\n");
    assert!(run().status.success());
    executable(&app, "#!/bin/sh\nexit 42\n");
    assert_eq!(run().status.code(), Some(42));
    executable(&app, "#!/bin/sh\nexec /bin/sleep 30\n");
    let mut other = Command::new(&app).spawn().unwrap();
    let status = run().status;
    let alive = other.try_wait().unwrap().is_none();
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(!status.success(), "a watchdog timeout fails the run");
    assert!(alive, "another copy of the app must stay running");
}
