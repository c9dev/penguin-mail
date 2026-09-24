//! A scripted IMAP server on an in-memory pipe, so a test runs a real
//! session without a socket. The script is a closure from each command,
//! without its tag, to the lines to send back; `{tag}` in a line becomes
//! the command's tag, a line `<close>` hangs up, and a line `<pause>`
//! waits a tenth of a second before the lines after it.

use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::{
    AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf,
};

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
            for line in answer("IDLE") {
                if line == "<close>" || send(&mut write, &line).await.is_none() {
                    return;
                }
            }
            // The client ends IDLE with DONE.
            if read_line(&mut read).await.is_none() {
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
