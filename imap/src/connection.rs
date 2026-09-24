//! One signed-in IMAP connection and the commands the client sends on it.
//! It runs over any stream, so a test drives it through an in-memory pipe.
//!
//! Commands go out through async-imap's `run_command` and their answers
//! come back through the readers in `parse`: async-imap's own command
//! methods drop the parts of an answer this client needs, such as
//! VANISHED during SELECT, COPYUID after MOVE and APPENDUID after APPEND.

use std::fmt;
use std::time::Duration;

use async_imap::extensions::idle::IdleResponse;
use async_imap::imap_proto::{RequestId, Response, ResponseCode, Status};
use async_imap::{Authenticator, Client, Session};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::time::Instant;

use crate::guard::{Guarded, IDLE_BYTES};
use crate::parse::{
    AppendUidReader, CapabilityReader, CopyUidReader, FlagsReader, HeadersReader, ListReader,
    Reads, SearchReader, SectionReader, SelectReader, StructureReader,
};
use crate::refusal::{Doing, Refusal, from_async_imap, from_io, refusal};
use crate::{
    AppendUid, BodyStructure, Capabilities, CopyUid, Fetched, FlagsOf, HEADER_FIELDS, ImapError,
    Listed, Login, Selected, Since, UidSet, Woke,
};

/// What a connection runs over: TLS in the app, a pipe in tests.
pub(crate) trait Stream:
    AsyncRead + AsyncWrite + Unpin + fmt::Debug + Send + 'static
{
}

impl<T: AsyncRead + AsyncWrite + Unpin + fmt::Debug + Send + 'static> Stream for T {}

/// The tag of the commands sent before async-imap takes over the stream.
/// async-imap numbers its own `A0001` and up, so the two never meet.
const RAW_TAG: &str = "PM0";

pub(crate) struct Conn<S: Stream> {
    session: Session<Guarded<S>>,
    pub(crate) capabilities: Capabilities,
    /// The mailbox the server has selected on this connection.
    selected: Option<String>,
    /// When a command last went out, to tell a connection a router may
    /// have dropped in silence.
    last_used: Instant,
}

/// SASL PLAIN (RFC 4616): no authorization identity, then the user and
/// password. It carries UTF-8, which LOGIN's quoted strings do not.
struct Plain<'a>(&'a Login);

impl Authenticator for Plain<'_> {
    type Response = String;

    fn process(&mut self, _challenge: &[u8]) -> String {
        format!("\0{}\0{}", self.0.user, self.0.password)
    }
}

impl<S: Stream> Conn<S> {
    /// Signs in on `stream`. `greeted` says the greeting was read already,
    /// as it is when STARTTLS ran before TLS. Reads the capabilities again
    /// after login, and turns QRESYNC on where the server offers it.
    pub(crate) async fn login(
        stream: S,
        greeted: bool,
        login: &Login,
    ) -> Result<Conn<S>, ImapError> {
        let mut client = Client::new(Guarded::new(stream));
        let mut before = CapabilityReader::default();
        if !greeted {
            let greeting = client
                .read_response()
                .await
                .map_err(from_io)?
                .ok_or_else(closed)?;
            match greeting.parsed() {
                Response::Data {
                    status: Status::Ok, ..
                } => before.read(greeting.parsed()),
                Response::Data {
                    status: Status::Bye,
                    information,
                    ..
                } => {
                    return Err(refusal(
                        Doing::Greeting,
                        Refusal::Bye,
                        false,
                        information.as_deref().unwrap_or_default(),
                    ));
                }
                other => {
                    return Err(ImapError::Protocol(format!(
                        "the greeting is not one this client accepts: {other:?}"
                    )));
                }
            }
        }
        if before.atoms.is_empty() {
            raw_capability(&mut client, &mut before).await?;
        }
        let has = |atom: &str| before.atoms.iter().any(|a| a == atom);
        let session = if has("AUTH=PLAIN") {
            client
                .authenticate("PLAIN", Plain(login))
                .await
                .map_err(|(err, _)| from_async_imap(err, Doing::Login))?
        } else if has("LOGINDISABLED") {
            return Err(ImapError::Protocol(
                "the server takes neither LOGIN nor AUTHENTICATE PLAIN".into(),
            ));
        } else {
            client
                .login(&login.user, &login.password)
                .await
                .map_err(|(err, _)| from_async_imap(err, Doing::Login))?
        };
        let mut conn = Conn {
            session,
            capabilities: Capabilities::default(),
            selected: None,
            last_used: Instant::now(),
        };
        let mut after = CapabilityReader::default();
        conn.exec("CAPABILITY", Doing::Other, &mut after).await?;
        let mut capabilities = Capabilities::from_atoms(after.atoms.iter().map(String::as_str));
        if capabilities.qresync {
            // RFC 7162 section 3.2.3: QRESYNC does nothing until enabled.
            capabilities.qresync = match conn.exec("ENABLE QRESYNC", Doing::Other, &mut ()).await {
                Ok(()) => true,
                Err(err) if err.drops_connection() => return Err(err),
                Err(_) => false,
            };
        }
        conn.capabilities = capabilities;
        Ok(conn)
    }

    /// How long since a command last went out on this connection.
    pub(crate) fn unused_for(&self) -> Duration {
        self.last_used.elapsed()
    }

    pub(crate) async fn noop(&mut self) -> Result<(), ImapError> {
        self.exec("NOOP", Doing::Other, &mut ()).await
    }

    pub(crate) async fn list(&mut self) -> Result<Vec<Listed>, ImapError> {
        let command = match self.capabilities.special_use {
            true => "LIST \"\" \"*\" RETURN (SPECIAL-USE)",
            false => "LIST \"\" \"*\"",
        };
        let mut reader = ListReader::default();
        self.exec(command, Doing::Other, &mut reader).await?;
        Ok(reader.listed)
    }

    /// Selects `mailbox`. With QRESYNC on and a `since` to start from, the
    /// answer carries what changed; otherwise `since` goes unused.
    pub(crate) async fn select(
        &mut self,
        mailbox: &str,
        since: Option<&Since>,
    ) -> Result<Selected, ImapError> {
        let name = quoted(mailbox)?;
        // RFC 7162 wants a MODSEQ of at least 1 here.
        let since = since.filter(|s| self.capabilities.qresync && s.modseq > 0);
        let command = match since {
            Some(since) => {
                let known = since
                    .known
                    .as_ref()
                    .filter(|k| !k.is_empty())
                    .map(|k| format!(" {k}"))
                    .unwrap_or_default();
                format!(
                    "SELECT {name} (QRESYNC ({} {}{known}))",
                    since.uidvalidity, since.modseq
                )
            }
            None if self.capabilities.condstore => format!("SELECT {name} (CONDSTORE)"),
            None => format!("SELECT {name}"),
        };
        // A failed SELECT leaves no mailbox selected (RFC 3501 section 6.3.1).
        self.selected = None;
        let mut reader = SelectReader::new(since.and_then(|s| s.known.as_ref()));
        self.exec(&command, Doing::Mailbox(mailbox), &mut reader)
            .await?;
        self.selected = Some(mailbox.to_string());
        reader.finish()
    }

    async fn ensure_selected(&mut self, mailbox: &str) -> Result<(), ImapError> {
        if self.selected.as_deref() != Some(mailbox) {
            self.select(mailbox, None).await?;
        }
        Ok(())
    }

    pub(crate) async fn flags(
        &mut self,
        mailbox: &str,
        uids: &UidSet,
        changed_since: Option<u64>,
    ) -> Result<Vec<FlagsOf>, ImapError> {
        if changed_since.is_some() && !self.capabilities.condstore {
            return Err(ImapError::Unsupported("CONDSTORE"));
        }
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_selected(mailbox).await?;
        let items = match self.capabilities.condstore {
            true => "(UID FLAGS MODSEQ)",
            false => "(UID FLAGS)",
        };
        let modifier = changed_since
            .map(|m| format!(" (CHANGEDSINCE {m})"))
            .unwrap_or_default();
        let mut reader = FlagsReader::new(uids);
        self.exec(
            &format!("UID FETCH {uids} {items}{modifier}"),
            Doing::Mailbox(mailbox),
            &mut reader,
        )
        .await?;
        Ok(reader.finish())
    }

    pub(crate) async fn search(
        &mut self,
        mailbox: &str,
        keys: &str,
    ) -> Result<Vec<u32>, ImapError> {
        self.ensure_selected(mailbox).await?;
        let (head, literals) = search_command(keys)?;
        let literals: Vec<(&[u8], String)> = literals
            .iter()
            .map(|(bytes, after)| (bytes.as_slice(), after.clone()))
            .collect();
        let mut reader = SearchReader::default();
        self.exec_with_literals(&head, &literals, Doing::Mailbox(mailbox), &mut reader)
            .await?;
        Ok(reader.finish())
    }

    pub(crate) async fn headers(
        &mut self,
        mailbox: &str,
        uids: &UidSet,
    ) -> Result<Vec<Fetched>, ImapError> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_selected(mailbox).await?;
        let modseq = match self.capabilities.condstore {
            true => " MODSEQ",
            false => "",
        };
        let command = format!(
            "UID FETCH {uids} (UID FLAGS INTERNALDATE RFC822.SIZE{modseq} BODY.PEEK[HEADER.FIELDS ({HEADER_FIELDS})])"
        );
        let mut reader = HeadersReader::new(uids);
        self.exec(&command, Doing::Mailbox(mailbox), &mut reader)
            .await?;
        Ok(reader.finish())
    }

    /// `BODY.PEEK[<section>]` of one message: `""` for the whole message,
    /// `"HEADER"`, or a part path such as `"1.2"`. `None` when the
    /// mailbox holds no message with that UID.
    pub(crate) async fn body(
        &mut self,
        mailbox: &str,
        uid: u32,
        section: &str,
    ) -> Result<Option<Vec<u8>>, ImapError> {
        if !section
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.')
        {
            return Err(ImapError::Protocol(format!(
                "{section:?} is not a section this client fetches"
            )));
        }
        self.ensure_selected(mailbox).await?;
        let mut reader = SectionReader::new(uid);
        self.exec(
            &format!("UID FETCH {uid} (UID BODY.PEEK[{section}])"),
            Doing::Mailbox(mailbox),
            &mut reader,
        )
        .await?;
        Ok(reader.bytes)
    }

    pub(crate) async fn structure(
        &mut self,
        mailbox: &str,
        uid: u32,
    ) -> Result<Option<BodyStructure>, ImapError> {
        self.ensure_selected(mailbox).await?;
        let mut reader = StructureReader::new(uid);
        self.exec(
            &format!("UID FETCH {uid} (UID BODYSTRUCTURE)"),
            Doing::Mailbox(mailbox),
            &mut reader,
        )
        .await?;
        Ok(reader.structure)
    }

    /// Adds or takes away `flags` on `uids`, without the server echoing
    /// every message back.
    pub(crate) async fn store(
        &mut self,
        mailbox: &str,
        uids: &UidSet,
        add: bool,
        flags: &[String],
    ) -> Result<(), ImapError> {
        if uids.is_empty() || flags.is_empty() {
            return Ok(());
        }
        let list = flag_list(flags)?;
        self.ensure_selected(mailbox).await?;
        let sign = match add {
            true => '+',
            false => '-',
        };
        self.exec(
            &format!("UID STORE {uids} {sign}FLAGS.SILENT ({list})"),
            Doing::Mailbox(mailbox),
            &mut (),
        )
        .await
    }

    pub(crate) async fn move_to(
        &mut self,
        mailbox: &str,
        uids: &UidSet,
        to: &str,
    ) -> Result<Option<CopyUid>, ImapError> {
        if !self.capabilities.moves {
            return Err(ImapError::Unsupported("MOVE"));
        }
        self.transfer("MOVE", mailbox, uids, to).await
    }

    pub(crate) async fn copy_to(
        &mut self,
        mailbox: &str,
        uids: &UidSet,
        to: &str,
    ) -> Result<Option<CopyUid>, ImapError> {
        self.transfer("COPY", mailbox, uids, to).await
    }

    async fn transfer(
        &mut self,
        verb: &str,
        mailbox: &str,
        uids: &UidSet,
        to: &str,
    ) -> Result<Option<CopyUid>, ImapError> {
        if uids.is_empty() {
            return Ok(None);
        }
        let target = quoted(to)?;
        self.ensure_selected(mailbox).await?;
        let mut reader = CopyUidReader::default();
        self.exec(
            &format!("UID {verb} {uids} {target}"),
            Doing::Into(to),
            &mut reader,
        )
        .await?;
        Ok(reader.copy_uid)
    }

    /// Expunges `uids` alone, which only UIDPLUS's `UID EXPUNGE` can do:
    /// a plain EXPUNGE would take every message anyone marked deleted.
    pub(crate) async fn expunge(&mut self, mailbox: &str, uids: &UidSet) -> Result<(), ImapError> {
        if !self.capabilities.uidplus {
            return Err(ImapError::Unsupported("UIDPLUS"));
        }
        if uids.is_empty() {
            return Ok(());
        }
        self.ensure_selected(mailbox).await?;
        self.exec(
            &format!("UID EXPUNGE {uids}"),
            Doing::Mailbox(mailbox),
            &mut (),
        )
        .await
    }

    pub(crate) async fn append(
        &mut self,
        mailbox: &str,
        flags: &[String],
        raw: &[u8],
    ) -> Result<Option<AppendUid>, ImapError> {
        let head = format!("APPEND {} ({}) ", quoted(mailbox)?, flag_list(flags)?);
        let mut reader = AppendUidReader::default();
        self.exec_with_literals(
            &head,
            &[(raw, String::new())],
            Doing::Into(mailbox),
            &mut reader,
        )
        .await?;
        Ok(reader.append_uid)
    }

    pub(crate) async fn create(&mut self, mailbox: &str) -> Result<(), ImapError> {
        self.exec(
            &format!("CREATE {}", quoted(mailbox)?),
            Doing::Other,
            &mut (),
        )
        .await
    }

    pub(crate) async fn rename(&mut self, from: &str, to: &str) -> Result<(), ImapError> {
        let command = format!("RENAME {} {}", quoted(from)?, quoted(to)?);
        if self.selected.as_deref() == Some(from) {
            self.selected = None;
        }
        self.exec(&command, Doing::Mailbox(from), &mut ()).await
    }

    pub(crate) async fn delete(&mut self, mailbox: &str) -> Result<(), ImapError> {
        let command = format!("DELETE {}", quoted(mailbox)?);
        if self.selected.as_deref() == Some(mailbox) {
            self.selected = None;
        }
        self.exec(&command, Doing::Mailbox(mailbox), &mut ()).await
    }

    /// Waits in IDLE on `mailbox` until the server reports a change or
    /// `limit` passes, then ends IDLE and hands the connection back.
    pub(crate) async fn idle(
        mut self,
        mailbox: &str,
        limit: Duration,
    ) -> Result<(Conn<S>, Woke), ImapError> {
        self.ensure_selected(mailbox).await?;
        self.session.get_mut().expect(IDLE_BYTES);
        let Conn {
            session,
            capabilities,
            selected,
            ..
        } = self;
        let mut handle = session.idle();
        handle
            .init()
            .await
            .map_err(|err| from_async_imap(err, Doing::Other))?;
        let woke = {
            // Dropping the stop source ends the wait at once, and async-imap
            // reports that as `ManualInterrupt`, the answer it also gives when
            // the server closes the connection. Held here, it never drops
            // early, so `ManualInterrupt` can only mean a closed connection.
            let (wait, _stop) = handle.wait_with_timeout(limit);
            match wait
                .await
                .map_err(|err| from_async_imap(err, Doing::Other))?
            {
                IdleResponse::NewData(data) => match data.parsed() {
                    Response::Data {
                        status: Status::Bye,
                        information,
                        ..
                    } => {
                        return Err(ImapError::Network(
                            information.as_deref().unwrap_or_default().to_string(),
                        ));
                    }
                    _ => Woke::Changed,
                },
                IdleResponse::Timeout => Woke::TimedOut,
                IdleResponse::ManualInterrupt => return Err(closed()),
            }
        };
        let session = handle
            .done()
            .await
            .map_err(|err| from_async_imap(err, Doing::Other))?;
        // async-imap queues what arrived during IDLE for a reader this
        // client does not have; the adapter syncs the mailbox instead.
        while session.unsolicited_responses.try_recv().is_ok() {}
        Ok((
            Conn {
                session,
                capabilities,
                selected,
                last_used: Instant::now(),
            },
            woke,
        ))
    }

    /// Sends `command` and reads its answer to the end, handing every
    /// response to `reader`.
    async fn exec(
        &mut self,
        command: &str,
        doing: Doing<'_>,
        reader: &mut impl Reads,
    ) -> Result<(), ImapError> {
        self.last_used = Instant::now();
        self.session.get_mut().expect(reader.bytes());
        let tag = self
            .session
            .run_command(command)
            .await
            .map_err(|err| from_async_imap(err, doing))?;
        self.finish(&tag, doing, reader).await
    }

    /// Sends `head`, then each literal and the text after it, waiting for
    /// the server's continuation before each literal as RFC 3501 asks.
    async fn exec_with_literals(
        &mut self,
        head: &str,
        literals: &[(&[u8], String)],
        doing: Doing<'_>,
        reader: &mut impl Reads,
    ) -> Result<(), ImapError> {
        let Some((first, _)) = literals.first() else {
            return self.exec(head, doing, reader).await;
        };
        self.last_used = Instant::now();
        self.session.get_mut().expect(reader.bytes());
        let tag = self
            .session
            .run_command(format!("{head}{{{}}}", first.len()))
            .await
            .map_err(|err| from_async_imap(err, doing))?;
        let mut answers = 0;
        for (i, (literal, after)) in literals.iter().enumerate() {
            self.wait_for_continuation(&tag, doing, reader, &mut answers)
                .await?;
            let next = literals
                .get(i + 1)
                .map(|(l, _)| format!("{{{}}}", l.len()))
                .unwrap_or_default();
            let stream = self.session.get_mut();
            stream.write_all(literal).await.map_err(from_io)?;
            stream.write_all(after.as_bytes()).await.map_err(from_io)?;
            stream.write_all(next.as_bytes()).await.map_err(from_io)?;
            stream.write_all(b"\r\n").await.map_err(from_io)?;
            stream.flush().await.map_err(from_io)?;
        }
        self.finish_counted(&tag, doing, reader, answers).await
    }

    async fn wait_for_continuation(
        &mut self,
        tag: &RequestId,
        doing: Doing<'_>,
        reader: &mut impl Reads,
        answers: &mut usize,
    ) -> Result<(), ImapError> {
        loop {
            let response = self
                .session
                .read_response()
                .await
                .map_err(from_io)?
                .ok_or_else(closed)?;
            match response.parsed() {
                Response::Continue { .. } => return Ok(()),
                parsed => {
                    if let Some(result) = ended(parsed, tag, doing) {
                        // The server refused before taking the literal.
                        return result.and(Err(ImapError::Protocol(
                            "the server ended the command early".into(),
                        )));
                    }
                    count(answers, reader)?;
                    reader.read(parsed);
                    reader.check()?;
                }
            }
        }
    }

    async fn finish(
        &mut self,
        tag: &RequestId,
        doing: Doing<'_>,
        reader: &mut impl Reads,
    ) -> Result<(), ImapError> {
        self.finish_counted(tag, doing, reader, 0).await
    }

    /// Reads the command's answers to its tagged one, `answers` of them
    /// read already.
    async fn finish_counted(
        &mut self,
        tag: &RequestId,
        doing: Doing<'_>,
        reader: &mut impl Reads,
        mut answers: usize,
    ) -> Result<(), ImapError> {
        loop {
            let response = self
                .session
                .read_response()
                .await
                .map_err(from_io)?
                .ok_or_else(closed)?;
            let parsed = response.parsed();
            if matches!(parsed, Response::Done { tag: done, .. } if done == tag) {
                reader.read(parsed);
            }
            if let Some(result) = ended(parsed, tag, doing) {
                return result;
            }
            count(&mut answers, reader)?;
            let Some(wanted) = reader.wants(parsed) else {
                reader.read(parsed);
                reader.check()?;
                continue;
            };
            // The literal sits in the buffer async-imap read the answer
            // into, which the answer owns. Taking that buffer instead of
            // copying the literal out keeps one copy of a large body.
            match within(response.borrow_owner(), wanted) {
                Some(span) => {
                    let mut buffer = response.into_owner();
                    drop(buffer.split_to(span.start));
                    buffer.truncate(span.len());
                    reader.keep(Vec::from(buffer));
                }
                None => reader.keep(wanted.to_vec()),
            }
        }
    }
}

/// How `response` ends the command tagged `tag`, or `None` when it does
/// not: the tagged answer, or a BYE that closes the connection under it.
fn ended(
    response: &Response<'_>,
    tag: &RequestId,
    doing: Doing<'_>,
) -> Option<Result<(), ImapError>> {
    let text = |information: &Option<std::borrow::Cow<'_, str>>| {
        information.as_deref().unwrap_or_default().to_string()
    };
    match response {
        Response::Done { tag: other, .. } if other != tag => Some(Err(ImapError::Protocol(
            format!("the server answered {} while {} ran", other.0, tag.0),
        ))),
        Response::Done {
            tag: done,
            status,
            code,
            information,
        } if done == tag => Some(match status {
            Status::Ok => Ok(()),
            Status::No => Err(refusal(
                doing,
                Refusal::No,
                matches!(code, Some(ResponseCode::TryCreate)),
                &text(information),
            )),
            _ => Err(refusal(doing, Refusal::Bad, false, &text(information))),
        }),
        Response::Data {
            status: Status::Bye,
            information,
            ..
        } => Some(Err(refusal(doing, Refusal::Bye, false, &text(information)))),
        _ => None,
    }
}

/// Asks for CAPABILITY before login, for a server whose greeting lists
/// none. async-imap has no way to ask before login, so the command goes
/// straight onto the stream.
async fn raw_capability<S: Stream>(
    client: &mut Client<Guarded<S>>,
    reader: &mut CapabilityReader,
) -> Result<(), ImapError> {
    let stream = client.get_mut();
    stream
        .write_all(format!("{RAW_TAG} CAPABILITY\r\n").as_bytes())
        .await
        .map_err(from_io)?;
    stream.flush().await.map_err(from_io)?;
    loop {
        let response = client
            .read_response()
            .await
            .map_err(from_io)?
            .ok_or_else(closed)?;
        reader.read(response.parsed());
        if let Response::Done {
            tag,
            status,
            information,
            ..
        } = response.parsed()
            && tag.0 == RAW_TAG
        {
            return match status {
                Status::Ok => Ok(()),
                _ => Err(ImapError::Protocol(
                    information.as_deref().unwrap_or_default().to_string(),
                )),
            };
        }
    }
}

/// Counts one more untagged answer against what the command may bring.
fn count(answers: &mut usize, reader: &impl Reads) -> Result<(), ImapError> {
    *answers += 1;
    match *answers > reader.most() {
        true => Err(ImapError::Protocol(format!(
            "the server sent more than {} answers to one command",
            reader.most()
        ))),
        false => Ok(()),
    }
}

/// Where `part` lies inside `whole`, when it is a slice of it.
fn within(whole: &[u8], part: &[u8]) -> Option<std::ops::Range<usize>> {
    let start = part.as_ptr().addr().checked_sub(whole.as_ptr().addr())?;
    let end = start.checked_add(part.len())?;
    (end <= whole.len()).then_some(start..end)
}

fn closed() -> ImapError {
    ImapError::Network("the server closed the connection".into())
}

/// A mailbox name as a quoted string. Names travel in modified UTF-7, so
/// one that is not ASCII did not come from the server or from
/// [`crate::utf7::encode`].
fn quoted(name: &str) -> Result<String, ImapError> {
    if !name.is_ascii() || name.contains(['\r', '\n', '\0']) {
        return Err(ImapError::Protocol(format!(
            "{name:?} is not a mailbox name in modified UTF-7"
        )));
    }
    Ok(format!(
        "\"{}\"",
        name.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

/// Flags as a space-separated list, each checked to be an IMAP atom with
/// an optional leading backslash.
fn flag_list(flags: &[String]) -> Result<String, ImapError> {
    for flag in flags {
        let atom = flag.strip_prefix('\\').unwrap_or(flag);
        let bad = |c: char| !c.is_ascii_graphic() || "(){%*\"\\]".contains(c);
        if atom.is_empty() || atom.chars().any(bad) {
            return Err(ImapError::Protocol(format!("{flag:?} is not a flag")));
        }
    }
    Ok(flags.join(" "))
}

/// A command's text before its first literal, and each literal with the
/// text after it.
pub(crate) type SearchParts = (String, Vec<(Vec<u8>, String)>);

/// `UID SEARCH keys`, split for sending: IMAP's quoted strings carry
/// 7-bit text without line breaks, so each quoted string holding more
/// than ASCII, or a CR or LF, goes as a literal, and the search names
/// UTF-8 as its charset. A line break outside quotes would end the command
/// and start another, and no IMAP string carries NUL, so either fails.
/// Returns the text before the first literal and each literal with the
/// text after it.
pub(crate) fn search_command(keys: &str) -> Result<SearchParts, ImapError> {
    if keys.contains('\0') {
        return Err(ImapError::Protocol("a search holds a NUL".into()));
    }
    let mut head = String::new();
    let mut literals: Vec<(Vec<u8>, String)> = Vec::new();
    let mut chars = keys.chars().peekable();
    let push = |text: &str, head: &mut String, literals: &mut Vec<(Vec<u8>, String)>| match literals
        .last_mut()
    {
        Some((_, after)) => after.push_str(text),
        None => head.push_str(text),
    };
    while let Some(c) = chars.next() {
        if matches!(c, '\r' | '\n') {
            return Err(ImapError::Protocol(
                "a search holds a line break outside quotes".into(),
            ));
        }
        if c != '"' {
            push(c.encode_utf8(&mut [0; 4]), &mut head, &mut literals);
            continue;
        }
        let mut quoted = String::new();
        let mut value = String::new();
        quoted.push('"');
        while let Some(c) = chars.next() {
            quoted.push(c);
            match c {
                '\\' => {
                    if let Some(next) = chars.next() {
                        quoted.push(next);
                        value.push(next);
                    }
                }
                '"' => break,
                c => value.push(c),
            }
        }
        match value.is_ascii() && !value.contains(['\r', '\n']) {
            true => push(&quoted, &mut head, &mut literals),
            false => literals.push((value.into_bytes(), String::new())),
        }
    }
    let charset = match literals.is_empty() {
        true => "",
        false => "CHARSET UTF-8 ",
    };
    Ok((format!("UID SEARCH {charset}{head}"), literals))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    use super::{Conn, search_command};
    use crate::testing::{pipe, selected, server};
    use crate::{CopyUid, ImapError, Login, Since, UidSet, Woke};

    const GREETING: &str = "* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN] ready";
    const ALL: &str = "IDLE QRESYNC CONDSTORE MOVE UIDPLUS SPECIAL-USE ENABLE";

    fn log() -> Arc<Mutex<Vec<String>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn seen(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        log.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn ann() -> Login {
        Login::new("ann@example.com", "pässword")
    }

    #[tokio::test]
    async fn signs_in_with_authenticate_plain_when_offered() {
        let log = log();
        let stream = pipe(GREETING, server(ALL, log.clone(), |_| vec![]));
        Conn::login(stream, false, &ann()).await.unwrap();
        let commands = seen(&log);
        // base64 of "\0ann@example.com\0pässword"
        assert_eq!(
            commands[0],
            "AUTHENTICATE PLAIN AGFubkBleGFtcGxlLmNvbQBww6Rzc3dvcmQ="
        );
        assert!(!commands.iter().any(|c| c.starts_with("LOGIN")));
    }

    #[tokio::test]
    async fn asks_for_capabilities_when_the_greeting_has_none_and_falls_back_to_login() {
        let log = log();
        let stream = pipe("* OK ready", move |command: &str| {
            log.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(command.to_string());
            match command {
                "CAPABILITY" => vec!["* CAPABILITY IMAP4rev1 IDLE".into(), "{tag} OK done".into()],
                c if c.starts_with("LOGIN") => vec!["{tag} OK in".into()],
                _ => vec!["{tag} BAD unknown".into()],
            }
        });
        let conn = Conn::login(stream, false, &Login::new("ann", "pw"))
            .await
            .unwrap();
        assert!(conn.capabilities.idle);
        assert!(!conn.capabilities.qresync);
    }

    #[tokio::test]
    async fn a_refused_password_is_an_auth_error_in_the_servers_words() {
        let stream = pipe(GREETING, |command: &str| match command {
            c if c.starts_with("AUTHENTICATE") => {
                vec!["{tag} NO [AUTHENTICATIONFAILED] Invalid credentials (Failure)".into()]
            }
            _ => vec!["{tag} OK".into()],
        });
        let err = Conn::login(stream, false, &ann()).await.err();
        assert_eq!(
            err,
            Some(ImapError::Auth {
                text: "[AUTHENTICATIONFAILED] Invalid credentials (Failure)".into()
            })
        );
    }

    #[tokio::test]
    async fn a_greeting_that_says_bye_over_the_limit_is_too_many_connections() {
        let stream = pipe(
            "* BYE Maximum number of connections from user+IP exceeded",
            |_: &str| vec![],
        );
        let err = Conn::login(stream, false, &ann()).await.err();
        assert!(
            matches!(err, Some(ImapError::TooManyConnections { .. })),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn qresync_is_turned_on_when_offered() {
        let log = log();
        let stream = pipe(GREETING, server(ALL, log.clone(), |_| vec![]));
        let conn = Conn::login(stream, false, &ann()).await.unwrap();
        assert!(conn.capabilities.qresync);
        assert!(seen(&log).contains(&"ENABLE QRESYNC".to_string()));
    }

    #[tokio::test]
    async fn qresync_stays_off_when_enable_is_refused() {
        let stream = pipe(GREETING, move |command: &str| match command {
            "CAPABILITY" => vec!["* CAPABILITY IMAP4rev1 QRESYNC".into(), "{tag} OK".into()],
            "ENABLE QRESYNC" => vec!["{tag} NO not today".into()],
            _ => vec!["{tag} OK".into()],
        });
        let conn = Conn::login(stream, false, &ann()).await.unwrap();
        assert!(!conn.capabilities.qresync);
        assert!(conn.capabilities.condstore);
    }

    #[tokio::test]
    async fn select_since_sends_qresync_parameters_and_reads_what_vanished() {
        let log = log();
        let stream = pipe(
            GREETING,
            server(ALL, log.clone(), |command| match command {
                c if c.starts_with("SELECT") => {
                    let mut lines = vec![
                        "* VANISHED (EARLIER) 2".to_string(),
                        "* 1 FETCH (UID 1 FLAGS (\\Seen) MODSEQ (91))".to_string(),
                    ];
                    lines.extend(selected());
                    lines
                }
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let since = Since {
            uidvalidity: 7,
            modseq: 80,
            known: Some(UidSet::from_uids([1, 2, 3])),
        };
        let state = conn.select("INBOX", Some(&since)).await.unwrap();
        assert!(seen(&log).contains(&"SELECT \"INBOX\" (QRESYNC (7 80 1:3))".to_string()));
        assert_eq!(state.uidvalidity, 7);
        assert_eq!(state.highestmodseq, Some(90));
        assert_eq!(state.vanished, UidSet::from_uids([2]));
        assert_eq!(state.changed.len(), 1);
    }

    #[tokio::test]
    async fn selecting_a_missing_mailbox_names_it() {
        let stream = pipe(
            GREETING,
            server(ALL, log(), |command| match command {
                c if c.starts_with("SELECT") => {
                    vec!["{tag} NO [NONEXISTENT] Mailbox doesn't exist: Gone".into()]
                }
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        assert_eq!(
            conn.select("Gone", None).await.err(),
            Some(ImapError::NoMailbox("Gone".into()))
        );
    }

    #[tokio::test]
    async fn an_open_ended_fetch_drops_the_message_below_its_start() {
        let stream = pipe(
            GREETING,
            server(ALL, log(), |command| match command {
                c if c.starts_with("SELECT") => selected(),
                // RFC 3501: 101:* names the highest message even at UID 100.
                c if c.starts_with("UID FETCH 101:*") => vec![
                    "* 3 FETCH (UID 100 FLAGS () BODY[HEADER.FIELDS (SUBJECT)] {12}".into(),
                    "Subject: x".into(),
                    ")".into(),
                    "{tag} OK".into(),
                ],
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let fetched = conn.headers("INBOX", &UidSet::from_uid(101)).await.unwrap();
        assert!(fetched.is_empty());
    }

    #[tokio::test]
    async fn a_search_with_accents_sends_them_as_a_literal() {
        let log = log();
        let stream = pipe(
            GREETING,
            server(ALL, log.clone(), |command| match command {
                c if c.starts_with("SELECT") => selected(),
                c if c.starts_with("UID SEARCH") => vec!["* SEARCH 4 2".into(), "{tag} OK".into()],
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let uids = conn
            .search("INBOX", "FROM \"joão\" SUBJECT \"hi\"")
            .await
            .unwrap();
        assert_eq!(uids, [2, 4]);
        assert!(
            seen(&log)
                .contains(&"UID SEARCH CHARSET UTF-8 FROM {5}joão SUBJECT \"hi\"".to_string())
        );
    }

    #[tokio::test]
    async fn append_sends_the_message_as_a_literal_and_reads_appenduid() {
        let log = log();
        let stream = pipe(
            GREETING,
            server(ALL, log.clone(), |command| match command {
                c if c.starts_with("APPEND") => vec!["{tag} OK [APPENDUID 7 12] done".into()],
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let raw = b"Subject: sent\r\n\r\nhello\r\n";
        let uid = conn
            .append("Sent", &["\\Seen".to_string()], raw)
            .await
            .unwrap();
        assert_eq!(
            uid,
            Some(crate::AppendUid {
                uidvalidity: 7,
                uid: 12
            })
        );
        assert!(
            seen(&log).contains(
                &"APPEND \"Sent\" (\\Seen) {24}Subject: sent\r\n\r\nhello\r\n".to_string()
            )
        );
    }

    #[tokio::test]
    async fn move_reads_copyuid_from_the_untagged_ok() {
        let stream = pipe(
            GREETING,
            server(ALL, log(), |command| match command {
                c if c.starts_with("SELECT") => selected(),
                c if c.starts_with("UID MOVE") => vec![
                    "* OK [COPYUID 44 3:4 20:21] moved".into(),
                    "* VANISHED 3:4".into(),
                    "{tag} OK done".into(),
                ],
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let copied = conn
            .move_to("INBOX", &UidSet::range(3, 4), "Archive")
            .await
            .unwrap();
        assert_eq!(
            copied,
            Some(CopyUid {
                uidvalidity: 44,
                pairs: vec![(3, 20), (4, 21)]
            })
        );
    }

    #[tokio::test]
    async fn move_without_the_extension_sends_nothing() {
        let log = log();
        let stream = pipe(
            GREETING,
            server("IDLE", log.clone(), |_| vec!["{tag} OK".into()]),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let err = conn
            .move_to("INBOX", &UidSet::range(3, 4), "Archive")
            .await
            .err();
        assert_eq!(err, Some(ImapError::Unsupported("MOVE")));
        assert!(!seen(&log).iter().any(|c| c.contains("MOVE")));
    }

    #[tokio::test]
    async fn a_flag_that_is_not_an_atom_is_refused_before_sending() {
        let stream = pipe(GREETING, server(ALL, log(), |_| vec!["{tag} OK".into()]));
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let err = conn
            .store(
                "INBOX",
                &UidSet::range(1, 1),
                true,
                &["bad flag)".to_string()],
            )
            .await;
        assert!(matches!(err, Err(ImapError::Protocol(_))));
    }

    #[tokio::test]
    async fn idle_wakes_when_the_server_reports_new_mail() {
        let stream = pipe(
            GREETING,
            server(ALL, log(), |command| match command {
                c if c.starts_with("SELECT") => selected(),
                "IDLE" => vec!["* 4 EXISTS".into()],
                _ => vec!["{tag} OK".into()],
            }),
        );
        let conn = Conn::login(stream, false, &ann()).await.unwrap();
        let (mut conn, woke) = conn.idle("INBOX", Duration::from_secs(60)).await.unwrap();
        assert_eq!(woke, Woke::Changed);
        conn.noop().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn idle_times_out_when_nothing_happens() {
        let stream = pipe(
            GREETING,
            server(ALL, log(), |command| match command {
                c if c.starts_with("SELECT") => selected(),
                "IDLE" => vec![],
                _ => vec!["{tag} OK".into()],
            }),
        );
        let conn = Conn::login(stream, false, &ann()).await.unwrap();
        let (_, woke) = conn.idle("INBOX", Duration::from_secs(60)).await.unwrap();
        assert_eq!(woke, Woke::TimedOut);
    }

    #[tokio::test]
    async fn idle_on_a_closed_connection_is_a_network_error() {
        let stream = pipe(
            GREETING,
            server(ALL, log(), |command| match command {
                c if c.starts_with("SELECT") => selected(),
                "IDLE" => vec!["<close>".into()],
                _ => vec!["{tag} OK".into()],
            }),
        );
        let conn = Conn::login(stream, false, &ann()).await.unwrap();
        let err = conn.idle("INBOX", Duration::from_secs(60)).await.err();
        assert!(matches!(err, Some(ImapError::Network(_))), "{err:?}");
    }

    #[tokio::test]
    async fn list_asks_for_special_use_when_the_server_has_it() {
        let log = log();
        let stream = pipe(
            GREETING,
            server(ALL, log.clone(), |command| match command {
                c if c.starts_with("LIST") => vec![
                    "* LIST (\\HasNoChildren) \"/\" INBOX".into(),
                    "* LIST (\\Sent) \"/\" \"Sent Items\"".into(),
                    "{tag} OK".into(),
                ],
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let listed = conn.list().await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[1].special_use, Some(crate::SpecialUse::Sent));
        assert!(seen(&log).contains(&"LIST \"\" \"*\" RETURN (SPECIAL-USE)".to_string()));
    }

    #[tokio::test]
    async fn flags_changed_since_a_modseq_ask_with_changedsince() {
        let log = log();
        let stream = pipe(
            GREETING,
            server(ALL, log.clone(), |command| match command {
                c if c.starts_with("SELECT") => selected(),
                c if c.starts_with("UID FETCH") => vec![
                    "* 1 FETCH (UID 1 FLAGS (\\Seen) MODSEQ (95))".into(),
                    "{tag} OK".into(),
                ],
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let flags = conn
            .flags("INBOX", &UidSet::from_uid(1), Some(90))
            .await
            .unwrap();
        assert_eq!(flags[0].modseq, Some(95));
        assert!(
            seen(&log).contains(&"UID FETCH 1:* (UID FLAGS MODSEQ) (CHANGEDSINCE 90)".to_string())
        );
    }

    #[tokio::test]
    async fn flags_changed_since_need_condstore() {
        let stream = pipe(GREETING, server("IDLE", log(), |_| vec!["{tag} OK".into()]));
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let err = conn
            .flags("INBOX", &UidSet::from_uid(1), Some(90))
            .await
            .err();
        assert_eq!(err, Some(ImapError::Unsupported("CONDSTORE")));
    }

    #[tokio::test]
    async fn body_and_structure_fetch_one_message() {
        let log = log();
        let stream = pipe(
            GREETING,
            server(ALL, log.clone(), |command| {
                match command {
            c if c.starts_with("SELECT") => selected(),
            "UID FETCH 5 (UID BODY.PEEK[1.2])" => vec![
                "* 2 FETCH (UID 5 BODY[1.2] {5}".into(),
                "hello)".into(),
                "{tag} OK".into(),
            ],
            "UID FETCH 5 (UID BODYSTRUCTURE)" => vec![
                "* 2 FETCH (UID 5 BODYSTRUCTURE (\"text\" \"plain\" NIL NIL NIL \"7bit\" 5 1 NIL NIL NIL NIL))".into(),
                "{tag} OK".into(),
            ],
            _ => vec!["{tag} OK".into()],
        }
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        assert_eq!(
            conn.body("INBOX", 5, "1.2").await.unwrap().as_deref(),
            Some(&b"hello"[..])
        );
        let structure = conn.structure("INBOX", 5).await.unwrap().unwrap();
        assert_eq!(structure.root.mime_type, "text/plain");
        assert_eq!(conn.body("INBOX", 6, "1").await.unwrap(), None);
        // One SELECT serves every command on the same mailbox.
        assert_eq!(
            seen(&log)
                .iter()
                .filter(|c| c.starts_with("SELECT"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn a_section_with_spaces_is_refused_before_sending() {
        let stream = pipe(GREETING, server(ALL, log(), |_| vec!["{tag} OK".into()]));
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let err = conn.body("INBOX", 5, "HEADER.FIELDS (FROM)").await;
        assert!(matches!(err, Err(ImapError::Protocol(_))));
    }

    #[tokio::test]
    async fn copy_reads_copyuid_and_a_missing_target_is_named() {
        let stream = pipe(
            GREETING,
            server(ALL, log(), |command| match command {
                c if c.starts_with("SELECT") => selected(),
                "UID COPY 3 \"Archive\"" => vec!["{tag} OK [COPYUID 44 3 20] copied".into()],
                "UID COPY 3 \"Gone\"" => vec!["{tag} NO [TRYCREATE] no such mailbox".into()],
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let copied = conn
            .copy_to("INBOX", &UidSet::from_uids([3]), "Archive")
            .await
            .unwrap();
        assert_eq!(
            copied,
            Some(CopyUid {
                uidvalidity: 44,
                pairs: vec![(3, 20)]
            })
        );
        let err = conn
            .copy_to("INBOX", &UidSet::from_uids([3]), "Gone")
            .await
            .err();
        assert_eq!(err, Some(ImapError::NoMailbox("Gone".into())));
    }

    #[tokio::test]
    async fn expunge_names_its_uids_and_needs_uidplus() {
        let commands = log();
        let stream = pipe(
            GREETING,
            server(ALL, commands.clone(), |command| match command {
                c if c.starts_with("SELECT") => selected(),
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        conn.expunge("INBOX", &UidSet::from_uids([3, 4]))
            .await
            .unwrap();
        assert!(seen(&commands).contains(&"UID EXPUNGE 3:4".to_string()));
        let bare = pipe(GREETING, server("IDLE", log(), |_| vec!["{tag} OK".into()]));
        let mut bare = Conn::login(bare, false, &ann()).await.unwrap();
        let err = bare.expunge("INBOX", &UidSet::from_uids([3])).await.err();
        assert_eq!(err, Some(ImapError::Unsupported("UIDPLUS")));
    }

    #[tokio::test]
    async fn create_rename_and_delete_quote_their_names() {
        let log = log();
        let stream = pipe(
            GREETING,
            server(ALL, log.clone(), |_| vec!["{tag} OK".into()]),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        conn.create("Work/Q\"3\"").await.unwrap();
        conn.rename("Work", "Job").await.unwrap();
        conn.delete("Job").await.unwrap();
        let commands = seen(&log);
        assert!(commands.contains(&"CREATE \"Work/Q\\\"3\\\"\"".to_string()));
        assert!(commands.contains(&"RENAME \"Work\" \"Job\"".to_string()));
        assert!(commands.contains(&"DELETE \"Job\"".to_string()));
        assert!(conn.unused_for() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_mailbox_name_that_is_not_ascii_never_reaches_the_server() {
        let stream = pipe(GREETING, server(ALL, log(), |_| vec!["{tag} OK".into()]));
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        assert!(matches!(
            conn.create("Envoyés").await,
            Err(ImapError::Protocol(_))
        ));
        conn.create(&crate::utf7::encode("Envoyés")).await.unwrap();
    }

    #[test]
    fn a_search_in_ascii_goes_as_one_line() {
        let (head, literals) =
            search_command("OR FROM \"ann\" SUBJECT \"say \\\"hi\\\"\"").unwrap();
        assert_eq!(
            head,
            "UID SEARCH OR FROM \"ann\" SUBJECT \"say \\\"hi\\\"\""
        );
        assert!(literals.is_empty());
    }

    #[test]
    fn each_non_ascii_string_becomes_a_literal_with_the_text_after_it() {
        let (head, literals) = search_command("FROM \"joão\" TEXT \"reunião\" UNSEEN").unwrap();
        assert_eq!(head, "UID SEARCH CHARSET UTF-8 FROM ");
        assert_eq!(
            literals,
            [
                ("joão".as_bytes().to_vec(), " TEXT ".to_string()),
                ("reunião".as_bytes().to_vec(), " UNSEEN".to_string())
            ]
        );
    }

    #[tokio::test]
    async fn an_empty_uid_set_never_reaches_the_server() {
        let log = log();
        let stream = pipe(
            GREETING,
            server(ALL, log.clone(), |_| {
                vec!["{tag} BAD an empty set is no sequence set".into()]
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let signed_in = seen(&log).len();
        let none = UidSet::new();
        assert!(conn.flags("INBOX", &none, None).await.unwrap().is_empty());
        assert!(
            conn.flags("INBOX", &none, Some(9))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(conn.headers("INBOX", &none).await.unwrap().is_empty());
        conn.store("INBOX", &none, true, &["\\Seen".to_string()])
            .await
            .unwrap();
        assert_eq!(conn.move_to("INBOX", &none, "Archive").await.unwrap(), None);
        assert_eq!(conn.copy_to("INBOX", &none, "Archive").await.unwrap(), None);
        conn.expunge("INBOX", &none).await.unwrap();
        assert_eq!(seen(&log)[signed_in..], [] as [String; 0]);
    }

    #[tokio::test]
    async fn select_since_leaves_out_an_empty_set_of_known_uids() {
        let log = log();
        let stream = pipe(
            GREETING,
            server(ALL, log.clone(), |command| match command {
                c if c.starts_with("SELECT") => selected(),
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let since = Since {
            uidvalidity: 7,
            modseq: 80,
            known: Some(UidSet::new()),
        };
        conn.select("INBOX", Some(&since)).await.unwrap();
        assert!(seen(&log).contains(&"SELECT \"INBOX\" (QRESYNC (7 80))".to_string()));
    }

    /// A multipart BODYSTRUCTURE `levels` parentheses deep.
    fn nested_structure(levels: usize) -> String {
        let leaf = "(\"text\" \"plain\" NIL NIL NIL \"7bit\" 5 1)";
        format!(
            "* 2 FETCH (UID 5 BODYSTRUCTURE {}{leaf}{})",
            "(".repeat(levels - 1),
            " \"mixed\")".repeat(levels - 1)
        )
    }

    /// Runs `test` on a 2 MB stack, the size of a tokio worker's, so an
    /// answer that makes imap-proto recurse too deep crashes the test
    /// binary instead of passing.
    fn on_a_small_stack<F: std::future::Future<Output = ()>>(
        test: impl FnOnce() -> F + Send + 'static,
    ) {
        std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(test())
            })
            .unwrap()
            .join()
            .unwrap();
    }

    /// A connection whose server answers `UID FETCH 5 (UID BODYSTRUCTURE)`
    /// with `lines`, then OK.
    async fn answering_structure(lines: Vec<String>) -> Conn<tokio::io::DuplexStream> {
        let stream = pipe(
            GREETING,
            server(ALL, log(), move |command| match command {
                c if c.starts_with("SELECT") => selected(),
                "UID FETCH 5 (UID BODYSTRUCTURE)" => {
                    let mut answer = lines.clone();
                    answer.push("{tag} OK".into());
                    answer
                }
                _ => vec!["{tag} OK".into()],
            }),
        );
        Conn::login(stream, false, &ann()).await.unwrap()
    }

    /// imap-proto parses nested lists by recursion and async-imap parses
    /// whatever arrives, so without the cap this answer overflows the
    /// stack before any depth check of this crate runs.
    #[test]
    fn a_structure_nested_past_the_cap_fails_without_overflowing_the_stack() {
        on_a_small_stack(|| async {
            // One level for the FETCH list itself.
            let at_cap = nested_structure(crate::guard::MAX_NESTING - 1);
            let mut conn = answering_structure(vec![at_cap]).await;
            assert!(conn.structure("INBOX", 5).await.unwrap().is_some());
            let mut conn = answering_structure(vec![nested_structure(10_000)]).await;
            let err = conn.structure("INBOX", 5).await.err();
            assert!(matches!(err, Some(ImapError::Protocol(_))), "{err:?}");
        });
    }

    /// Each first line ends in what the cap could take for a literal and
    /// imap-proto reads as text, so without the refusal the deep line
    /// after it reaches the parser uncounted.
    #[test]
    fn a_deep_line_behind_a_status_line_literal_is_refused() {
        on_a_small_stack(|| async {
            let hiding = [
                vec![format!("{} OK x{{200000}}", "A".repeat(30))],
                vec!["* OK [BADCHARSET ({4}".into(), "AAAA)] x{200000}".into()],
                vec!["+ [BADCHARSET ({4}".into(), "AAAA)] x{200000}".into()],
            ];
            for mut lines in hiding {
                let first = lines[0].clone();
                lines.push(nested_structure(10_000));
                let mut conn = answering_structure(lines).await;
                let err = conn.structure("INBOX", 5).await.err();
                assert!(
                    matches!(err, Some(ImapError::Protocol(_))),
                    "{first}: {err:?}"
                );
            }
        });
    }

    /// A structure as a mail client meets one: an alternative inside a
    /// mixed, a PDF whose quoted name holds parentheses and quotes, and a
    /// forwarded message whose subject is a literal full of parentheses.
    #[test]
    fn a_realistic_structure_passes_the_cap() {
        on_a_small_stack(|| async {
            let lines = vec![
                concat!(
                    "* 12 FETCH (UID 5 BODYSTRUCTURE (",
                    "((\"text\" \"plain\" (\"charset\" \"utf-8\") NIL NIL \"quoted-printable\" 1204 31 NIL NIL NIL NIL)",
                    "(\"text\" \"html\" (\"charset\" \"utf-8\") NIL NIL \"quoted-printable\" 5120 104 NIL NIL NIL NIL)",
                    " \"alternative\" (\"boundary\" \"b2\") NIL NIL NIL)",
                    "(\"application\" \"pdf\" (\"name\" \"Invoice (final) \\\"v2\\\".pdf\") NIL NIL \"base64\" 88422 NIL",
                    " (\"attachment\" (\"filename\" \"Invoice (final) \\\"v2\\\".pdf\" \"size\" \"64620\")) NIL NIL)",
                    "(\"message\" \"rfc822\" NIL NIL NIL \"7bit\" 3000 (\"Mon, 7 Feb 2026 10:00:00 +0000\" {12}",
                )
                .to_string(),
                concat!(
                    "Subj ((((( ) ((\"Ann (work)\" NIL \"ann\" \"example.com\")) ((\"Ann\" NIL \"ann\" \"example.com\"))",
                    " ((\"Ann\" NIL \"ann\" \"example.com\")) ((NIL NIL \"bob\" \"example.com\")) NIL NIL NIL \"<x@y>\")",
                    " (\"text\" \"plain\" (\"charset\" \"us-ascii\") NIL NIL \"7bit\" 10 1 NIL NIL NIL NIL) 40 NIL",
                    " (\"attachment\" (\"filename\" \"fwd (1).eml\")) NIL NIL)",
                    " \"mixed\" (\"boundary\" \"b1\") NIL (\"en\") NIL))",
                )
                .to_string(),
            ];
            let mut conn = answering_structure(lines).await;
            let structure = conn.structure("INBOX", 5).await.unwrap().unwrap();
            assert_eq!(structure.root.mime_type, "multipart/mixed");
            assert_eq!(structure.root.children.len(), 3);
        });
    }

    #[tokio::test]
    async fn an_answer_tagged_for_another_command_is_a_protocol_error() {
        let stream = pipe(
            GREETING,
            server(ALL, log(), |command| match command {
                "NOOP" => vec!["A9999 OK not yours".into()],
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        // Read as news, the stray answer would leave the client waiting for
        // its own tag forever.
        let answer = tokio::time::timeout(Duration::from_secs(5), conn.noop()).await;
        assert!(
            matches!(answer, Ok(Err(ImapError::Protocol(_)))),
            "{answer:?}"
        );
    }

    #[test]
    fn a_line_break_in_a_search_string_goes_inside_a_literal() {
        let (head, literals) = search_command("FROM \"x\r\nA9 DELETE INBOX\r\n\"").unwrap();
        assert_eq!(head, "UID SEARCH CHARSET UTF-8 FROM ");
        assert_eq!(
            literals,
            [(b"x\r\nA9 DELETE INBOX\r\n".to_vec(), String::new())]
        );
    }

    #[test]
    fn a_line_break_outside_quotes_or_a_nul_anywhere_is_refused() {
        for keys in ["ALL\r\nA9 DELETE INBOX", "ALL\nX", "FROM \"a\0b\"", "ALL\0"] {
            assert!(
                matches!(search_command(keys), Err(ImapError::Protocol(_))),
                "{keys:?}"
            );
        }
    }

    #[tokio::test]
    async fn an_injected_search_sends_one_command() {
        let log = log();
        let stream = pipe(
            GREETING,
            server(ALL, log.clone(), |command| match command {
                c if c.starts_with("SELECT") => selected(),
                c if c.starts_with("UID SEARCH") => vec!["* SEARCH".into(), "{tag} OK".into()],
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        conn.search("INBOX", "FROM \"x\r\nA9 DELETE INBOX\r\n\"")
            .await
            .unwrap();
        let commands = seen(&log);
        assert!(
            !commands.iter().any(|c| c.starts_with("DELETE")),
            "{commands:?}"
        );
        assert!(
            commands.contains(
                &"UID SEARCH CHARSET UTF-8 FROM {20}x\r\nA9 DELETE INBOX\r\n".to_string()
            )
        );
    }

    #[tokio::test]
    async fn a_command_that_brings_too_many_answers_is_refused() {
        let stream = pipe(
            GREETING,
            server(ALL, log(), |command| match command {
                c if c.starts_with("SELECT") => selected(),
                c if c.starts_with("UID FETCH") => {
                    let mut lines: Vec<String> = (0..1_100)
                        .map(|_| "* 1 FETCH (UID 9 FLAGS (\\Seen))".to_string())
                        .collect();
                    lines.push("{tag} OK".into());
                    lines
                }
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let err = conn.flags("INBOX", &UidSet::from_uid(1), None).await.err();
        // An open-ended set may bring answers up to the cap for any command.
        assert_eq!(err, None);
        let err = conn
            .flags("INBOX", &UidSet::from_uids([1]), None)
            .await
            .err();
        assert!(matches!(err, Some(ImapError::Protocol(_))), "{err:?}");
    }

    #[tokio::test]
    async fn a_large_body_comes_back_whole() {
        let body: String = (0..200_000u32)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let answer = body.clone();
        let stream = pipe(
            GREETING,
            server(ALL, log(), move |command| match command {
                c if c.starts_with("SELECT") => selected(),
                c if c.starts_with("UID FETCH 5") => vec![
                    format!("* 2 FETCH (UID 5 BODY[] {{{}}}", answer.len()),
                    format!("{answer})"),
                    // News about another message in the same read.
                    "* 3 EXISTS".into(),
                    "{tag} OK".into(),
                ],
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let fetched = conn.body("INBOX", 5, "").await.unwrap().unwrap();
        assert_eq!(fetched, body.as_bytes());
        conn.noop().await.unwrap();
    }

    #[tokio::test]
    async fn a_search_of_200_000_uids_comes_back_whole() {
        let stream = pipe(
            GREETING,
            server(ALL, log(), |command| match command {
                c if c.starts_with("SELECT") => selected(),
                c if c.starts_with("UID SEARCH") => {
                    let uids: Vec<String> = (1..=200_000).map(|u| u.to_string()).collect();
                    vec![format!("* SEARCH {}", uids.join(" ")), "{tag} OK".into()]
                }
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let uids = conn.search("INBOX", "ALL").await.unwrap();
        assert_eq!(uids.len(), 200_000);
        assert_eq!(uids.last(), Some(&200_000));
    }

    #[tokio::test]
    async fn a_literal_larger_than_the_commands_budget_is_refused_before_it_arrives() {
        let stream = pipe(
            GREETING,
            server(ALL, log(), |command| match command {
                c if c.starts_with("SELECT") => selected(),
                // 64 MiB announced during a command that may bring 32 MiB;
                // three bytes follow, then nothing.
                c if c.starts_with("UID FETCH") => {
                    vec!["* 1 FETCH (UID 1 BODY[] {67108864}".into(), "abc".into()]
                }
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let answer = tokio::time::timeout(
            Duration::from_secs(5),
            conn.flags("INBOX", &UidSet::from_uids([1]), None),
        )
        .await;
        assert!(
            matches!(answer, Ok(Err(ImapError::Protocol(_)))),
            "{answer:?}"
        );
    }

    #[tokio::test]
    async fn idle_refuses_a_literal_past_its_small_budget() {
        let stream = pipe(
            GREETING,
            server(ALL, log(), |command| match command {
                c if c.starts_with("SELECT") => selected(),
                "IDLE" => vec!["* 1 FETCH (UID 1 BODY[] {8388608}".into(), "abc".into()],
                _ => vec!["{tag} OK".into()],
            }),
        );
        let conn = Conn::login(stream, false, &ann()).await.unwrap();
        let answer = tokio::time::timeout(
            Duration::from_secs(5),
            conn.idle("INBOX", Duration::from_secs(60)),
        )
        .await
        .map(|result| result.err());
        assert!(
            matches!(answer, Ok(Some(ImapError::Protocol(_)))),
            "{answer:?}"
        );
    }

    #[tokio::test]
    async fn a_flags_answer_past_the_flag_cap_is_a_protocol_error() {
        let stream = pipe(
            GREETING,
            server(ALL, log(), |command| match command {
                c if c.starts_with("SELECT") => selected(),
                c if c.starts_with("UID FETCH") => {
                    let flags: Vec<String> = (0..=crate::parse::MAX_FLAGS)
                        .map(|i| format!("k{i}"))
                        .collect();
                    vec![
                        format!("* 1 FETCH (UID 1 FLAGS ({}))", flags.join(" ")),
                        "{tag} OK".into(),
                    ]
                }
                _ => vec!["{tag} OK".into()],
            }),
        );
        let mut conn = Conn::login(stream, false, &ann()).await.unwrap();
        let err = conn
            .flags("INBOX", &UidSet::from_uids([1]), None)
            .await
            .err();
        assert!(matches!(err, Some(ImapError::Protocol(_))), "{err:?}");
    }
}
