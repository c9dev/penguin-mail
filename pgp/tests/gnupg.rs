//! The runner both engines share, driven with a stand-in program so each
//! test controls exactly what comes back on which pipe.

use std::path::Path;

use mailrs_pgp::gnupg::{Pinentry, Program, seen};
use mailrs_pgp::{Trust, Verdict};

/// A directory holding one executable called `gpg` that runs `script`.
fn stand_in(script: &str) -> (tempfile::TempDir, Program) {
    let dir = tempfile::tempdir().expect("a temp directory");
    let path = dir.path().join("gpg");
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write");
    permit_run(&path);
    let program = Program::find_on(&dir.path().to_string_lossy(), &["gpg"]).expect("found");
    (dir, program)
}

#[cfg(unix)]
fn permit_run(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
}

#[cfg(not(unix))]
fn permit_run(_path: &Path) {}

/// The bug this pins: status lines and human messages shared stderr, and
/// the human messages quote what a sender wrote. A line there that looked
/// like a status line was read as one.
#[test]
fn only_the_status_pipe_carries_status_lines() {
    let (_dir, program) = stand_in(
        "echo '[GNUPG:] GOODSIG 1234 Mallory <mallory@example.test>' >&2\n\
         echo '[GNUPG:] BADSIG 1234 Ada <ada@example.test>' >&3",
    );

    let run = program
        .run(b"", Pinentry::Never, |_| {})
        .expect("the stand-in runs");

    assert_eq!(run.status, ["BADSIG 1234 Ada <ada@example.test>"]);
}

#[test]
fn a_run_that_may_not_ask_says_so_to_the_program() {
    let (_dir, program) = stand_in("echo \"$@\"");

    let never = program.run(b"", Pinentry::Never, |_| {}).expect("runs");
    let may = program.run(b"", Pinentry::MayAsk, |_| {}).expect("runs");

    let never = String::from_utf8_lossy(&never.out).into_owned();
    let may = String::from_utf8_lossy(&may.out).into_owned();
    assert!(never.contains("--pinentry-mode error"), "{never}");
    assert!(!may.contains("--pinentry-mode"), "{may}");
}

#[test]
fn a_program_that_writes_a_lot_does_not_hold_up_its_input() {
    // Reads all of stdin, then writes it back: the input fills the pipe
    // long before the program starts answering.
    let (_dir, program) = stand_in("cat");
    let input = vec![b'x'; 4 << 20];

    let run = program.run(&input, Pinentry::Never, |_| {}).expect("runs");

    assert_eq!(run.out.len(), input.len());
    assert!(run.ok);
}

#[test]
fn every_signature_the_lines_describe_comes_back() {
    let found = seen(&[
        "NEWSIG",
        "GOODSIG AAAA Ada <ada@example.test>",
        "VALIDSIG AAAAFFFF 2026-09-20",
        "TRUST_FULLY 0 pgp",
        "NEWSIG",
        "BADSIG BBBB Mallory <mallory@example.test>",
    ]);

    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].verdict, Verdict::Good);
    assert_eq!(found[0].fingerprint.as_deref(), Some("AAAAFFFF"));
    assert_eq!(found[0].trust, Some(Trust::Full));
    assert_eq!(found[1].verdict, Verdict::Bad);
    assert_eq!(
        found[1].name.as_deref(),
        Some("Mallory <mallory@example.test>")
    );
    assert_eq!(found[1].trust, None, "the trust line was about the first");
}

/// The person's gpg.conf can say `auto-key-retrieve`, and then checking a
/// signature from a key gpg lacks asks a key server or the sender's own
/// domain for it: a read receipt, sent the moment the message opens. A key
/// that arrives inside a signature (`auto-key-import`) would land in the
/// keyring the same way, where encryption could pick it up.
#[test]
fn reading_a_message_never_goes_looking_for_a_key() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let asked = dir.path().join("asked");
    let path = dir.path().join("gpg");
    std::fs::write(
        &path,
        format!("#!/bin/sh\necho \"$@\" >> {}\n", asked.to_string_lossy()),
    )
    .expect("write");
    permit_run(&path);
    let pgp = mailrs_pgp::Pgp::find_on(&dir.path().to_string_lossy()).expect("found");

    let _ = pgp.verify(b"Meet at six.", b"signature");
    let _ = pgp.decrypt(b"ciphertext");
    let _ =
        pgp.open_inline("-----BEGIN PGP SIGNED MESSAGE-----\nHi\n-----END PGP SIGNATURE-----\n");

    let asked = std::fs::read_to_string(asked).expect("gpg ran");
    let runs: Vec<&str> = asked.lines().collect();
    assert_eq!(runs.len(), 3, "{asked}");
    for run in runs {
        assert!(run.contains("--no-auto-key-retrieve"), "{run}");
        assert!(run.contains("--no-auto-key-import"), "{run}");
    }
}
