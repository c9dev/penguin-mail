//! The runner both engines share, driven with a stand-in program so each
//! test controls exactly what comes back on which pipe.
//!
//! The stand-in is `gnupg-stand-in`, a little binary this crate builds
//! for the tests; each run's flags say what it writes where, and the
//! runner's status pipe reaches it as a file descriptor on Unix and an
//! inherited HANDLE on Windows.

use mailrs_pgp::gnupg::{Pinentry, Program, seen};
use mailrs_pgp::{Trust, Verdict};

/// A directory holding one executable called `gpg`: a copy of the
/// stand-in binary, so `find_on` discovers it the way it would the real
/// one. A test that wants it to do something on every run, the way a
/// shell script carried its body, writes `gnupg-stand-in.conf` beside
/// it; a test driving `Program::run` can hand it flags in the arguments.
fn stand_in() -> (tempfile::TempDir, Program) {
    let dir = tempfile::tempdir().expect("a temp directory");
    let name = if cfg!(windows) { "gpg.exe" } else { "gpg" };
    std::fs::copy(env!("CARGO_BIN_EXE_gnupg-stand-in"), dir.path().join(name))
        .expect("the stand-in copies");
    let program = Program::find_on(&dir.path().to_string_lossy(), &["gpg"]).expect("found");
    (dir, program)
}

/// The same directory and binary, found through `Pgp` the way the
/// engine's own calls find it.
fn stand_in_pgp() -> (tempfile::TempDir, mailrs_pgp::Pgp) {
    let (dir, _program) = stand_in();
    let pgp = mailrs_pgp::Pgp::find_on(&dir.path().to_string_lossy()).expect("found");
    (dir, pgp)
}

/// The bug this pins: status lines and human messages shared stderr, and
/// the human messages quote what a sender wrote. A line there that looked
/// like a status line was read as one.
#[test]
fn only_the_status_pipe_carries_status_lines() {
    let (_dir, program) = stand_in();

    let run = program
        .run(b"", Pinentry::Never, |command| {
            command
                .arg("--x-stderr")
                .arg("[GNUPG:] GOODSIG 1234 Mallory <mallory@example.test>")
                .arg("--x-status")
                .arg("BADSIG 1234 Ada <ada@example.test>");
        })
        .expect("the stand-in runs");

    assert_eq!(run.status, ["BADSIG 1234 Ada <ada@example.test>"]);
}

#[test]
fn a_run_that_may_not_ask_says_so_to_the_program() {
    let (_dir, program) = stand_in();
    let args = |command: &mut std::process::Command| {
        command.arg("--x-args");
    };

    let never = program.run(b"", Pinentry::Never, args).expect("runs");
    let may = program.run(b"", Pinentry::MayAsk, args).expect("runs");

    let never = String::from_utf8_lossy(&never.out).into_owned();
    let may = String::from_utf8_lossy(&may.out).into_owned();
    assert!(never.contains("--pinentry-mode error"), "{never}");
    assert!(!may.contains("--pinentry-mode"), "{may}");
}

#[test]
fn a_program_that_writes_a_lot_does_not_hold_up_its_input() {
    // Reads all of stdin, then writes it back: the input fills the pipe
    // long before the program starts answering.
    let (_dir, program) = stand_in();
    let input = vec![b'x'; 4 << 20];

    let run = program
        .run(&input, Pinentry::Never, |command| {
            command.arg("--x-copy");
        })
        .expect("runs");

    assert_eq!(run.out.len(), input.len());
    assert!(run.ok);
}

/// A message can carry compressed data that a few kilobytes of mail expand
/// into gigabytes. The runner stops reading at its cap, stops the program,
/// and says the output was too large, rather than holding all of it.
#[cfg(any(target_os = "linux", windows))]
#[test]
fn a_program_that_writes_past_the_cap_is_stopped() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let pid = dir.path().join("pid");
    // About 95 MiB, then a long wait: a runner that reads everything
    // waits out the sleep and answers with all of it.
    let (_stand_in, program) = stand_in();
    let started = std::time::Instant::now();

    let err = match program.run(b"", Pinentry::Never, |command| {
        command
            .arg("--x-pid")
            .arg(&pid)
            .args(["--x-emit", "100000000", "--x-sleep-ms", "30000"]);
    }) {
        Ok(run) => panic!("read {} bytes past the cap", run.out.len()),
        Err(err) => err,
    };

    assert_eq!(err.kind(), std::io::ErrorKind::FileTooLarge, "{err}");
    let waited = started.elapsed();
    assert!(
        waited < std::time::Duration::from_secs(10),
        "gave up after {waited:?}"
    );
    let pid = std::fs::read_to_string(&pid).expect("the stand-in wrote its pid");
    let gone = (0..100).any(|_| {
        if running(pid.trim()) {
            std::thread::sleep(std::time::Duration::from_millis(20));
            false
        } else {
            true
        }
    });
    assert!(gone, "the stand-in was left behind");
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn a_run_with_a_limit_stops_a_program_that_writes_past_the_cap() {
    let (_dir, program) = stand_in();
    let started = std::time::Instant::now();

    let err = match program.run_within(
        std::time::Duration::from_secs(20),
        b"",
        Pinentry::Never,
        |command| {
            command.args(["--x-emit", "100000000", "--x-sleep-ms", "30000"]);
        },
    ) {
        Ok(run) => panic!("read {} bytes past the cap", run.out.len()),
        Err(err) => err,
    };

    assert_eq!(err.kind(), std::io::ErrorKind::FileTooLarge, "{err}");
    let waited = started.elapsed();
    assert!(
        waited < std::time::Duration::from_secs(10),
        "gave up after {waited:?}"
    );
}

/// Whether the process `pid` still runs. One that has exited and not been
/// reaped yet is a zombie, which runs nothing, so it counts as gone here;
/// the tests that care about zombies look for the entry itself.
#[cfg(target_os = "linux")]
fn running(pid: &str) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            let after_name = stat.rsplit_once(')')?.1.trim_start().to_string();
            after_name.chars().next()
        })
        .is_some_and(|state| state != 'Z')
}

#[cfg(windows)]
fn running(pid: &str) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let pid: u32 = pid.parse().expect("a pid");
    // SAFETY: plain calls, on a handle this call owns and closes.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let mut code = 0;
        let alive = GetExitCodeProcess(process, &mut code) != 0 && code == STILL_ACTIVE as u32;
        CloseHandle(process);
        alive
    }
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn a_run_with_a_limit_stops_a_program_that_never_answers() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let pid = dir.path().join("pid");
    let child = dir.path().join("child");
    // The stand-in starts a child of its own, the way gpgsm starts
    // dirmngr, and that child holds stdout open. Killing the stand-in
    // alone would leave the child running and the pipe with a writer.
    let (_stand_in, program) = stand_in();
    let started = std::time::Instant::now();

    let err = match program.run_within(
        std::time::Duration::from_millis(500),
        b"",
        Pinentry::Never,
        |command| {
            command
                .arg("--x-pid")
                .arg(&pid)
                .arg("--x-child")
                .arg(&child)
                .args(["--x-sleep-ms", "30000"]);
        },
    ) {
        Ok(_) => panic!("a program that sleeps for 30 seconds answered in half of one"),
        Err(err) => err,
    };

    let waited = started.elapsed();
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");
    assert!(
        waited < std::time::Duration::from_secs(3),
        "gave up after {waited:?}"
    );
    let pid = std::fs::read_to_string(&pid).expect("the stand-in wrote its pid");
    let pid = pid.trim();
    // Reaped rather than left a zombie: its entry is gone altogether.
    let gone = (0..100).any(|_| {
        if running(pid) {
            std::thread::sleep(std::time::Duration::from_millis(20));
            false
        } else {
            true
        }
    });
    assert!(gone, "the stand-in {pid} was left behind");
    let child = std::fs::read_to_string(&child).expect("the stand-in wrote its child's pid");
    let child = child.trim();
    // The child's new parent reaps it, which takes a moment.
    let gone = (0..100).any(|_| {
        if running(child) {
            std::thread::sleep(std::time::Duration::from_millis(20));
            false
        } else {
            true
        }
    });
    assert!(gone, "the stand-in's child {child} still runs");
}

#[test]
fn a_run_that_finishes_inside_its_limit_answers_as_any_run_does() {
    let (_dir, program) = stand_in();

    let run = program
        .run_within(
            std::time::Duration::from_secs(10),
            b"Meet at six.",
            Pinentry::Never,
            |command| {
                command
                    .arg("--x-status")
                    .arg("GOODSIG 1234 Ada <ada@example.test>")
                    .arg("--x-copy");
            },
        )
        .expect("the stand-in answers in time");

    assert_eq!(run.status, ["GOODSIG 1234 Ada <ada@example.test>"]);
    assert_eq!(run.out, b"Meet at six.");
    assert!(run.ok);
}

/// gpg decrypting a large message, or waiting on the person's pinentry,
/// takes as long as it takes, so a run without a limit waits for it.
#[test]
fn a_run_without_a_limit_waits_for_a_slow_program() {
    let (_dir, program) = stand_in();
    let started = std::time::Instant::now();

    let run = program
        .run(b"", Pinentry::Never, |command| {
            command
                .args(["--x-sleep-ms", "1000"])
                .arg("--x-status")
                .arg("GOODSIG 1234 Ada <ada@example.test>")
                .arg("--x-out")
                .arg("done");
        })
        .expect("the stand-in runs");

    assert!(started.elapsed() >= std::time::Duration::from_secs(1));
    assert_eq!(run.status, ["GOODSIG 1234 Ada <ada@example.test>"]);
    assert_eq!(run.out, b"done\n");
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

/// The composer asks this each time a recipient changes, so a draft to ten
/// people should start gpg once, not ten times.
#[test]
fn every_address_is_looked_up_in_one_run() {
    let (dir, pgp) = stand_in_pgp();
    let asked = dir.path().join("asked");
    std::fs::write(
        dir.path().join("gnupg-stand-in.conf"),
        format!("--x-args-to {}", asked.display()),
    )
    .expect("the conf");
    let addresses: Vec<String> = ["ada", "bo", "cy"]
        .iter()
        .map(|name| format!("{name}@example.test"))
        .collect();

    let held = pgp.keys_for(&addresses).expect("an answer");

    assert_eq!(held.len(), 3);
    let asked = std::fs::read_to_string(&asked).expect("gpg ran");
    assert_eq!(asked.lines().count(), 1, "{asked}");
    assert!(asked.contains("<cy@example.test>"), "{asked}");
}

/// The person's gpg.conf can say `auto-key-retrieve`, and then checking a
/// signature from a key gpg lacks asks a key server or the sender's own
/// domain for it: a read receipt, sent the moment the message opens. A key
/// that arrives inside a signature (`auto-key-import`) would land in the
/// keyring the same way, where encryption could pick it up.
#[test]
fn reading_a_message_never_goes_looking_for_a_key() {
    let (dir, pgp) = stand_in_pgp();
    let asked = dir.path().join("asked");
    std::fs::write(
        dir.path().join("gnupg-stand-in.conf"),
        format!("--x-args-to {}", asked.display()),
    )
    .expect("the conf");

    let _ = pgp.verify(b"Meet at six.", b"signature");
    let _ = pgp.decrypt(b"ciphertext");
    let _ =
        pgp.open_inline("-----BEGIN PGP SIGNED MESSAGE-----\nHi\n-----END PGP SIGNATURE-----\n");

    let asked = std::fs::read_to_string(&asked).expect("gpg ran");
    let runs: Vec<&str> = asked.lines().collect();
    assert_eq!(runs.len(), 3, "{asked}");
    for run in runs {
        assert!(run.contains("--no-auto-key-retrieve"), "{run}");
        assert!(run.contains("--no-auto-key-import"), "{run}");
        // gpg's own limit on what it writes, beside the runner's cap.
        assert!(run.contains("--max-output"), "{run}");
    }
}
