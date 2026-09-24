//! A scripted IMAP server on an in-memory pipe, so a test runs a real
//! session without a socket. The script is a closure from each command,
//! without its tag, to the lines to send back; `{tag}` in a line becomes
//! the command's tag, a line `<close>` hangs up, and a line `<pause>`
//! waits a tenth of a second before the lines after it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::{
    AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf,
};

/// Heap bytes the current thread holds, and the most it has held since
/// the last [`HeapMark`]. Counted per thread, so tests running beside
/// each other add nothing to one another's peak, and a measurement that
/// spawns its server on a multi-thread runtime's worker sees the client
/// alone.
#[derive(Clone, Copy)]
struct Heap {
    live: usize,
    peak: usize,
}

thread_local! {
    static HEAP: Cell<Heap> = const { Cell::new(Heap { live: 0, peak: 0 }) };
}

fn heap_add(bytes: usize) {
    let _ = HEAP.try_with(|heap| {
        let mut h = heap.get();
        h.live = h.live.saturating_add(bytes);
        h.peak = h.peak.max(h.live);
        heap.set(h);
    });
}

fn heap_sub(bytes: usize) {
    let _ = HEAP.try_with(|heap| {
        let mut h = heap.get();
        h.live = h.live.saturating_sub(bytes);
        heap.set(h);
    });
}

/// The system allocator, counting what each thread holds, so a test can
/// measure what a path costs instead of estimating it.
struct Counting;

// SAFETY: every call goes to the system allocator with the same layout;
// the counting touches only a thread-local cell.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            heap_add(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        heap_sub(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            heap_sub(layout.size());
            heap_add(new_size);
        }
        new
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// A point from which the thread's heap growth is measured.
pub(crate) struct HeapMark {
    base: usize,
}

impl HeapMark {
    /// Starts measuring: the peak from here on counts bytes above what
    /// the thread holds now.
    pub(crate) fn start() -> Self {
        let base = HEAP.with(|heap| {
            let mut h = heap.get();
            h.peak = h.live;
            heap.set(h);
            h.live
        });
        HeapMark { base }
    }

    /// The most bytes the thread has held above the mark's start.
    pub(crate) fn peak(&self) -> usize {
        HEAP.with(|heap| heap.get().peak.saturating_sub(self.base))
    }

    /// The bytes the thread holds now above the mark's start.
    pub(crate) fn held(&self) -> usize {
        HEAP.with(|heap| heap.get().live.saturating_sub(self.base))
    }
}

/// The client's end of a pipe to a server that sends `greeting` (none when
/// empty) and answers through `answer`.
pub(crate) fn pipe(
    greeting: &str,
    answer: impl FnMut(&str) -> Vec<String> + Send + 'static,
) -> DuplexStream {
    let (client, server) = tokio::io::duplex(1 << 20);
    tokio::spawn(serve(server, greeting.to_string(), answer));
    client
}

/// Answers LOGIN, AUTHENTICATE PLAIN, CAPABILITY and ENABLE as a server
/// offering `caps` after login does, and hands every other command to
/// `rest`. Records every command it sees in `seen`.
pub(crate) fn server(
    caps: &'static str,
    seen: Arc<Mutex<Vec<String>>>,
    mut rest: impl FnMut(&str) -> Vec<String> + Send + 'static,
) -> impl FnMut(&str) -> Vec<String> + Send + 'static {
    move |command: &str| {
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(command.to_string());
        let upper = command.to_ascii_uppercase();
        if upper.starts_with("LOGIN ") || upper.starts_with("AUTHENTICATE PLAIN ") {
            return vec!["{tag} OK signed in".into()];
        }
        if upper == "CAPABILITY" {
            return vec![
                format!("* CAPABILITY IMAP4rev1 {caps}"),
                "{tag} OK done".into(),
            ];
        }
        if upper.starts_with("ENABLE ") {
            return vec!["* ENABLED QRESYNC".into(), "{tag} OK enabled".into()];
        }
        rest(command)
    }
}

/// Lines for a SELECT of a mailbox with UIDVALIDITY 7, UIDNEXT 10 and
/// HIGHESTMODSEQ 90.
pub(crate) fn selected() -> Vec<String> {
    [
        "* 3 EXISTS",
        "* OK [UIDVALIDITY 7] ok",
        "* OK [UIDNEXT 10] ok",
        "* OK [HIGHESTMODSEQ 90] ok",
        "* OK [PERMANENTFLAGS (\\Seen \\Deleted \\*)] ok",
        "{tag} OK [READ-WRITE] selected",
    ]
    .map(String::from)
    .to_vec()
}

/// A FETCH line carrying the BODYSTRUCTURE of a multipart/mixed message
/// of `count` PDF attachments, each with a long file name in its
/// parameters and its disposition, as a server sends it.
pub(crate) fn structure_of_attachments(count: usize) -> String {
    let mut line = String::from("* 1 FETCH (UID 1 BODYSTRUCTURE (");
    for i in 0..count {
        let name = format!(
            "Quarterly report {i} (final, reviewed by the whole team) with appendices A to F and the signed cover letter.pdf"
        );
        line.push_str(&format!(
            "(\"application\" \"pdf\" (\"name\" \"{name}\") NIL NIL \"base64\" 123456 NIL (\"attachment\" (\"filename\" \"{name}\" \"size\" \"123456\")) NIL NIL)"
        ));
    }
    line.push_str(" \"mixed\" (\"boundary\" \"b\") NIL NIL NIL))\r\n");
    line
}

async fn serve(
    server: DuplexStream,
    greeting: String,
    mut answer: impl FnMut(&str) -> Vec<String>,
) {
    let (read, mut write) = tokio::io::split(server);
    let mut read = BufReader::new(read);
    if !greeting.is_empty() && send(&mut write, &greeting).await.is_none() {
        return;
    }
    while let Some(command) = read_command(&mut read, &mut write).await {
        let (tag, body) = command.split_once(' ').unwrap_or((command.as_str(), ""));
        let replies = if body.eq_ignore_ascii_case("AUTHENTICATE PLAIN") {
            if send(&mut write, "+ ").await.is_none() {
                return;
            }
            let Some(credentials) = read_line(&mut read).await else {
                return;
            };
            answer(&format!("AUTHENTICATE PLAIN {credentials}"))
        } else if body.eq_ignore_ascii_case("IDLE") {
            if send(&mut write, "+ idling").await.is_none() {
                return;
            }
            // The client ends IDLE with DONE, which may come while the
            // server still has lines to send.
            let lines = answer("IDLE");
            let feed = async {
                for line in lines {
                    if line == "<pause>" {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        continue;
                    }
                    if line == "<close>" || send(&mut write, &line).await.is_none() {
                        return false;
                    }
                }
                true
            };
            let done = read_line(&mut read);
            tokio::pin!(feed, done);
            let ended = tokio::select! {
                fed = &mut feed => match fed {
                    true => done.await,
                    false => None,
                },
                line = &mut done => line,
            };
            if ended.is_none() {
                return;
            }
            vec!["{tag} OK IDLE done".to_string()]
        } else {
            answer(body)
        };
        for line in replies {
            if line == "<close>" {
                return;
            }
            if line == "<pause>" {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
            if send(&mut write, &line.replace("{tag}", tag))
                .await
                .is_none()
            {
                return;
            }
        }
    }
}

async fn send(write: &mut WriteHalf<DuplexStream>, line: &str) -> Option<()> {
    write.write_all(format!("{line}\r\n").as_bytes()).await.ok()
}

async fn read_line(read: &mut BufReader<ReadHalf<DuplexStream>>) -> Option<String> {
    let mut bytes = Vec::new();
    if read.read_until(b'\n', &mut bytes).await.ok()? == 0 {
        return None;
    }
    Some(
        String::from_utf8_lossy(&bytes)
            .trim_end_matches(['\r', '\n'])
            .to_string(),
    )
}

/// One command, with each synchronizing literal (`{5}` at a line's end)
/// answered with a continuation and read into the command's text.
async fn read_command(
    read: &mut BufReader<ReadHalf<DuplexStream>>,
    write: &mut WriteHalf<DuplexStream>,
) -> Option<String> {
    let mut command = String::new();
    loop {
        let line = read_line(read).await?;
        command.push_str(&line);
        let Some(length) = literal_length(&line) else {
            return Some(command);
        };
        send(write, "+ go ahead").await?;
        let mut bytes = vec![0; length];
        read.read_exact(&mut bytes).await.ok()?;
        command.push_str(&String::from_utf8_lossy(&bytes));
    }
}

fn literal_length(line: &str) -> Option<usize> {
    let open = line.rfind('{')?;
    line.strip_suffix('}')?.get(open + 1..)?.parse().ok()
}

/// What a scripted SMTP server saw: each command line, and each message
/// as it arrived after DATA, doubled dots included.
#[derive(Clone, Default)]
pub(crate) struct SmtpSeen {
    pub(crate) commands: Arc<Mutex<Vec<String>>>,
    pub(crate) messages: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl SmtpSeen {
    pub(crate) fn commands(&self) -> Vec<String> {
        self.commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn messages(&self) -> Vec<Vec<u8>> {
        self.messages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The client's end of a pipe to an SMTP server that sends `greeting`
/// (none when empty) and answers each command line through `answer`. When
/// its answer to DATA starts with 354, the server reads the message up to
/// the line holding a single dot and answers with `answer(".")`. The pipe
/// holds 64 KiB each way, so a measurement of the client sees little of it.
pub(crate) fn smtp_pipe(
    greeting: &str,
    seen: SmtpSeen,
    answer: impl FnMut(&str) -> Vec<String> + Send + 'static,
) -> DuplexStream {
    smtp_pipe_paced(greeting, seen, std::time::Duration::ZERO, answer)
}

/// [`smtp_pipe`] with a server that waits `pace` after each line of a
/// message it reads, as a slow link does.
pub(crate) fn smtp_pipe_paced(
    greeting: &str,
    seen: SmtpSeen,
    pace: std::time::Duration,
    answer: impl FnMut(&str) -> Vec<String> + Send + 'static,
) -> DuplexStream {
    let (client, server) = tokio::io::duplex(64 << 10);
    tokio::spawn(serve_smtp(server, greeting.to_string(), seen, pace, answer));
    client
}

/// Answers as a submission server offering AUTH PLAIN and LOGIN, 8BITMIME
/// and SMTPUTF8 that takes any sign-in and any message.
pub(crate) fn smtp_server(command: &str) -> Vec<String> {
    let upper = command.to_ascii_uppercase();
    let line = if upper.starts_with("EHLO") {
        return vec![
            "250-smtp.example.com".into(),
            "250-8BITMIME".into(),
            "250-SMTPUTF8".into(),
            "250 AUTH PLAIN LOGIN".into(),
        ];
    } else if upper.starts_with("AUTH") {
        "235 2.7.0 Accepted"
    } else if upper == "DATA" {
        "354 Go ahead"
    } else if upper == "QUIT" {
        "221 Bye"
    } else {
        "250 OK"
    };
    vec![line.into()]
}

async fn serve_smtp(
    server: DuplexStream,
    greeting: String,
    seen: SmtpSeen,
    pace: std::time::Duration,
    mut answer: impl FnMut(&str) -> Vec<String>,
) {
    let (read, mut write) = tokio::io::split(server);
    let mut read = BufReader::new(read);
    if !greeting.is_empty() && send(&mut write, &greeting).await.is_none() {
        return;
    }
    while let Some(command) = read_line(&mut read).await {
        seen.commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(command.clone());
        let replies = answer(&command);
        let data = command.eq_ignore_ascii_case("DATA")
            && replies.first().is_some_and(|line| line.starts_with("354"));
        for line in replies {
            if line == "<close>" || send(&mut write, &line).await.is_none() {
                return;
            }
        }
        if !data {
            continue;
        }
        let mut message = Vec::new();
        loop {
            let start = message.len();
            match read.read_until(b'\n', &mut message).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if &message[start..] == b".\r\n" {
                message.truncate(start);
                break;
            }
            if !pace.is_zero() {
                tokio::time::sleep(pace).await;
            }
        }
        seen.messages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(message);
        for line in answer(".") {
            if line == "<close>" || send(&mut write, &line).await.is_none() {
                return;
            }
        }
    }
}

/// Every form a server could repeat the password in: as typed, as LOGIN
/// quotes it, as Debug escapes it, and in AUTH's base64.
pub(crate) fn forms(user: &str, password: &str) -> Vec<String> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64;
    vec![
        password.to_string(),
        password.replace('\\', "\\\\").replace('"', "\\\""),
        format!("{password:?}").trim_matches('"').to_string(),
        BASE64.encode(format!("\0{user}\0{password}")),
        BASE64.encode(password),
    ]
}
