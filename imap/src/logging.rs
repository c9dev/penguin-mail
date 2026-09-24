//! Keeps what async-imap and imap-proto log out of the log. async-imap
//! logs every command and answer at trace level through the `log` crate:
//! LOGIN's password, AUTHENTICATE PLAIN's base64, and every byte of mail.
//! The binaries pass `log` records on to tracing, so `RUST_LOG=trace`
//! would put all of that in the journal.

use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt;

/// Crates whose log lines never leave the process.
const SILENT: [&str; 2] = ["async_imap", "imap_proto"];

/// `subscriber` with the lines of [`SILENT`]'s crates dropped before any
/// other filter sees them, so no `RUST_LOG` directive, however specific,
/// turns them back on.
pub fn quiet<S>(subscriber: S) -> impl tracing::Subscriber + Send + Sync + 'static
where
    S: tracing::Subscriber + Send + Sync + 'static,
{
    subscriber.with(filter_fn(|metadata| !silent(metadata.target())))
}

fn silent(target: &str) -> bool {
    SILENT.iter().any(|name| {
        target
            .strip_prefix(name)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
    })
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex, PoisonError};

    use tracing_subscriber::EnvFilter;

    use super::quiet;

    #[derive(Clone, Default)]
    struct Written(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Written {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// `log` records reach tracing under their module path as target, the
    /// way the binaries' LogTracer passes async-imap's on.
    #[test]
    fn no_log_setting_shows_what_async_imap_and_imap_proto_log() {
        for directives in [
            "trace",
            "async_imap=trace,imap_proto=trace,mailrs_imap=trace",
            "async_imap::imap_stream=trace,imap_proto::parser=trace,mailrs_imap=trace",
        ] {
            let written = Written::default();
            let writer = written.clone();
            let subscriber = quiet(
                tracing_subscriber::fmt()
                    .with_env_filter(EnvFilter::new(directives))
                    .with_writer(move || writer.clone())
                    .finish(),
            );
            tracing::subscriber::with_default(subscriber, || {
                tracing::trace!(target: "async_imap::imap_stream", "A0001 LOGIN ann pässword");
                tracing::trace!(target: "imap_proto::parser", "mail body");
                tracing::trace!(target: "mailrs_imap", "still here");
            });
            let text = String::from_utf8(written.0.lock().unwrap().clone()).unwrap();
            assert!(text.contains("still here"), "{directives}: {text}");
            assert!(!text.contains("pässword"), "{directives}: {text}");
            assert!(!text.contains("mail body"), "{directives}: {text}");
        }
    }
}
