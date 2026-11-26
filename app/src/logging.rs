//! Where the app's log lines go, and what a release build keeps out of them.
//!
//! The log lands in the system journal, which any program running as the
//! person can read, and it outlives the mail. Error text from Google and
//! from sending can carry addresses, so a release build masks every email
//! address on its way out: `dana.reyes@example.com` becomes
//! `d…@example.com`. A debug build keeps them, and so does a release build
//! started with `PENGUIN_MAIL_LOG_DETAILS=1`, for looking into a problem on
//! an installed copy.

use std::io::{self, Write};

use tracing_subscriber::fmt::MakeWriter;

/// The variable that keeps addresses in a release build's log.
const DETAILS: &str = "PENGUIN_MAIL_LOG_DETAILS";

/// Whether this run's log keeps addresses whole.
pub fn details() -> bool {
    cfg!(debug_assertions) || std::env::var_os(DETAILS).is_some_and(|v| v != "0")
}

/// Writes each log event to standard error, masking addresses unless
/// `details` is set.
#[derive(Clone, Copy)]
pub struct Writer {
    pub details: bool,
}

impl<'a> MakeWriter<'a> for Writer {
    type Writer = Event;

    fn make_writer(&'a self) -> Event {
        Event {
            details: self.details,
            line: Vec::new(),
        }
    }
}

/// One event's text, held until the formatter is done with it so an
/// address split across two writes is still seen whole.
pub struct Event {
    details: bool,
    line: Vec<u8>,
}

impl Write for Event {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.line.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        let text = String::from_utf8_lossy(&self.line);
        let text = if self.details {
            text.into_owned()
        } else {
            mask_addresses(&text)
        };
        let _ = io::stderr().lock().write_all(text.as_bytes());
        #[cfg(windows)]
        if let Some(file) = log_file() {
            let _ = file
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .write_all(without_colours(&text).as_bytes());
        }
    }
}

/// `text` without the ANSI colour codes the console formatter adds, which a
/// log file opened in Notepad would show as noise.
#[cfg_attr(not(windows), allow(dead_code))]
fn without_colours(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // A colour code is ESC, `[`, digits and `;`, then a letter.
            for code in chars.by_ref() {
                if code.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The log file of a Windows run, `%LOCALAPPDATA%\penguin-mail\logs\penguin-mail.log`.
/// The release build has no console, so standard error goes nowhere and
/// this file is the only record a person can send. The run before it stays
/// as `penguin-mail.1.log`. The file is opened for sharing reads only, so a
/// second copy that hands over to this one can neither move nor reopen it
/// and writes none. `None` when the folder or file cannot be made.
#[cfg(windows)]
fn log_file() -> Option<&'static std::sync::Mutex<std::fs::File>> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::sync::{Mutex, OnceLock};
    static FILE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    FILE.get_or_init(|| {
        let dir = crate::exe::local_data()?.join("logs");
        std::fs::create_dir_all(&dir).ok()?;
        let log = dir.join("penguin-mail.log");
        let _ = std::fs::rename(&log, dir.join("penguin-mail.1.log"));
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .share_mode(1) // FILE_SHARE_READ
            .open(&log)
            .ok()
            .map(Mutex::new)
    })
    .as_ref()
}

/// `text` with the local part of each email address cut to its first
/// letter. The domain stays, since which provider failed is worth knowing
/// and names nobody.
pub fn mask_addresses(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let local = |c: char| c.is_alphanumeric() || "._%+-".contains(c);
    let domain = |c: char| c.is_alphanumeric() || ".-".contains(c);
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' {
            let start = (0..i).rev().take_while(|&j| local(chars[j])).last();
            let end = (i + 1..chars.len())
                .take_while(|&j| domain(chars[j]))
                .last();
            if let (Some(start), Some(end)) = (start, end) {
                let host: String = chars[i + 1..=end].iter().collect();
                let host = host.trim_end_matches('.');
                if host.contains('.') && !host.starts_with('.') {
                    // The local part is already in `out`; take it back.
                    for _ in start..i {
                        out.pop();
                    }
                    out.push(chars[start]);
                    out.push('…');
                    out.push('@');
                    out.push_str(host);
                    i = i + 1 + host.chars().count();
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{mask_addresses, without_colours};

    #[test]
    fn a_log_file_line_carries_no_colour_codes() {
        assert_eq!(
            without_colours("\x1b[2m2026-11-26\x1b[0m \x1b[33m WARN\x1b[0m could not draw"),
            "2026-11-26  WARN could not draw"
        );
        assert_eq!(without_colours("plain line"), "plain line");
    }

    #[test]
    fn an_address_keeps_its_first_letter_and_its_domain() {
        assert_eq!(
            mask_addresses("could not start syncing account=dana.reyes@example.com"),
            "could not start syncing account=d…@example.com"
        );
    }

    #[test]
    fn every_address_in_a_line_is_masked() {
        assert_eq!(
            mask_addresses("rejected <ann+news@mail.example.org>, bo@x.co."),
            "rejected <a…@mail.example.org>, b…@x.co."
        );
    }

    #[test]
    fn text_without_an_address_is_left_alone() {
        for text in [
            "an @ on its own",
            "user@localhost",
            "handle @dana",
            "error: HTTP 403 for https://gmail.googleapis.com/gmail/v1/users/me",
            "ümlaut ünd 日本語",
        ] {
            assert_eq!(mask_addresses(text), text);
        }
    }
}
