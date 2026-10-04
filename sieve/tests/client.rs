//! The ManageSieve dialogue after TLS: capabilities, AUTHENTICATE PLAIN,
//! LISTSCRIPTS, GETSCRIPT with a literal, PUTSCRIPT and a refusal.

use mailrs_sieve::client::testing::session_over;
use mailrs_sieve::client::{Login, SieveError};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const CAPS: &str = "\"IMPLEMENTATION\" \"Dovecot Pigeonhole\"\r\n\"SIEVE\" \"fileinto vacation imap4flags include\"\r\n\"SASL\" \"PLAIN\"\r\n\"VERSION\" \"1.0\"\r\nOK \"TLS ready.\"\r\n";

/// A server that sends the capabilities, then answers each line it reads
/// with the next of `answers`, and hands back what it read.
async fn server(mut stream: tokio::io::DuplexStream, answers: Vec<&'static str>) -> Vec<String> {
    stream.write_all(CAPS.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(stream);
    let mut heard = Vec::new();
    for answer in answers {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        // A PUTSCRIPT line ends with a literal: read its bytes too.
        if let Some(len) = line
            .trim_end()
            .strip_suffix("+}")
            .and_then(|l| l.rsplit('{').next())
            .and_then(|n| n.parse::<usize>().ok())
        {
            let mut body = vec![0u8; len + 2];
            reader.read_exact(&mut body).await.unwrap();
            line.push_str(&String::from_utf8_lossy(&body));
        }
        heard.push(line);
        reader.get_mut().write_all(answer.as_bytes()).await.unwrap();
    }
    heard
}

#[tokio::test]
async fn a_session_logs_in_lists_reads_and_writes() {
    let (client, far) = tokio::io::duplex(1 << 16);
    let answers = vec![
        "OK \"Logged in.\"\r\n",
        "\"penguin-mail\" ACTIVE\r\n\"old\"\r\nOK \"Listscripts completed.\"\r\n",
        "{12}\r\nkeep;\r\nstop;\r\nOK \"Getscript completed.\"\r\n",
        "OK \"Putscript completed.\"\r\n",
        "OK \"Logout completed.\"\r\n",
    ];
    let heard = tokio::spawn(server(far, answers));
    let mut session = session_over(client, &Login::new("me@example.test", "pw"))
        .await
        .unwrap();
    assert!(session.capabilities().sieve.has("include"));
    let listed = session.scripts().await.unwrap();
    assert_eq!(
        (listed[0].name.as_str(), listed[0].active),
        ("penguin-mail", true)
    );
    assert_eq!(session.get("penguin-mail").await.unwrap(), "keep;\r\nstop;");
    session.put("penguin-mail", "keep;\n").await.unwrap();
    session.logout().await;
    let heard = heard.await.unwrap();
    // "\0me@example.test\0pw" in base64.
    assert_eq!(
        heard[0],
        "AUTHENTICATE \"PLAIN\" \"AG1lQGV4YW1wbGUudGVzdABwdw==\"\r\n"
    );
    assert!(heard[3].starts_with("PUTSCRIPT \"penguin-mail\" {6+}\r\nkeep;\n"));
}

#[tokio::test]
async fn a_refused_script_says_the_server_s_words() {
    let (client, far) = tokio::io::duplex(1 << 16);
    let heard = tokio::spawn(server(
        far,
        vec!["OK\r\n", "NO \"line 3: unknown command 'fileintoo'\"\r\n"],
    ));
    let mut session = session_over(client, &Login::new("me", "pw")).await.unwrap();
    let refused = session
        .put("penguin-mail", "fileintoo \"x\";")
        .await
        .unwrap_err();
    assert!(
        matches!(refused, SieveError::Refused(ref words) if words.contains("fileintoo")),
        "{refused:?}"
    );
    drop(heard);
}

#[tokio::test]
async fn a_wrong_password_is_auth() {
    let (client, far) = tokio::io::duplex(1 << 16);
    let _heard = tokio::spawn(server(far, vec!["NO \"Authentication failed.\"\r\n"]));
    let refused = session_over(client, &Login::new("me", "bad"))
        .await
        .err()
        .unwrap();
    assert!(matches!(refused, SieveError::Auth(_)), "{refused:?}");
}

#[tokio::test]
async fn a_literal_past_the_limit_is_too_large() {
    let (client, far) = tokio::io::duplex(1 << 16);
    let big: &'static str =
        Box::leak(format!("{{{}}}\r\n", mailrs_sieve::MOST_SCRIPT_BYTES + 1).into_boxed_str());
    let _heard = tokio::spawn(server(far, vec!["OK\r\n", big]));
    let mut session = session_over(client, &Login::new("me", "pw")).await.unwrap();
    assert!(matches!(
        session.get("huge").await,
        Err(SieveError::TooLarge)
    ));
}
