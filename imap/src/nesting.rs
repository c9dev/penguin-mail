//! A cap on how deep a server's parentheses nest. imap-proto parses nested
//! lists (BODYSTRUCTURE, body extensions, THREAD) by recursion with no
//! limit, and async-imap parses whatever the server sends, so a few
//! kilobytes of `(` from a hostile server would overflow the stack in the
//! parse or in dropping what it built. The cap reads the bytes under
//! async-imap and fails the read before anything parses them.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use mailrs_mime::MAX_DEPTH;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// The deepest nesting a response may reach. A structure this crate keeps
/// whole, [`MAX_DEPTH`] parts deep, nests one level per part, one for the
/// FETCH list and up to three for the parameters and disposition of its
/// innermost part. A debug build spends about 20 KB of stack a level
/// parsing BODYSTRUCTURE, so this many levels fit a 2 MB tokio worker.
pub(crate) const MAX_NESTING: usize = MAX_DEPTH + 8;

/// A stream whose answers may nest at most [`MAX_NESTING`] deep.
#[derive(Debug)]
pub(crate) struct Nesting<S> {
    inner: S,
    scan: Scan,
}

impl<S> Nesting<S> {
    pub(crate) fn new(inner: S) -> Self {
        Nesting {
            inner,
            scan: Scan::default(),
        }
    }
}

/// Enough of a response's start to hold a tag and a status word.
const HEAD: usize = 24;

/// Where the reader stands in the server's bytes: how deep the current
/// line nests, and whether it is inside a quoted string or a literal,
/// whose parentheses are text.
#[derive(Debug, Default)]
struct Scan {
    depth: usize,
    quoted: bool,
    escaped: bool,
    /// Bytes of a literal still to come.
    literal: u64,
    /// The digits of a `{n}` being read.
    digits: Option<u64>,
    /// A `{n}` just closed, so a line end here starts an `n`-byte literal.
    announced: Option<u64>,
    /// The first bytes of the response, to tell a status response or a
    /// continuation, whose text runs to the line end with any `{n}` in it.
    head: [u8; HEAD],
    head_len: usize,
    refused: bool,
}

impl Scan {
    fn feed(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut rest = bytes;
        while !rest.is_empty() {
            if self.refused {
                return Err(too_deep());
            }
            if self.literal > 0 {
                let skip = usize::try_from(self.literal)
                    .unwrap_or(usize::MAX)
                    .min(rest.len());
                self.literal -= skip as u64;
                rest = &rest[skip..];
                continue;
            }
            let byte = rest[0];
            rest = &rest[1..];
            self.step(byte);
        }
        if self.refused {
            return Err(too_deep());
        }
        Ok(())
    }

    fn step(&mut self, byte: u8) {
        if self.head_len < HEAD {
            self.head[self.head_len] = byte;
            self.head_len += 1;
        }
        if byte == b'\n' {
            // A line ends a response unless a literal follows it.
            match self.announced.take() {
                Some(length) if !self.in_text() => self.literal = length,
                _ => {
                    self.depth = 0;
                    self.head_len = 0;
                }
            }
            self.quoted = false;
            self.escaped = false;
            self.digits = None;
            return;
        }
        if self.quoted {
            match (self.escaped, byte) {
                (true, _) => self.escaped = false,
                (false, b'\\') => self.escaped = true,
                (false, b'"') => self.quoted = false,
                _ => {}
            }
            return;
        }
        if byte != b'\r' {
            self.announced = None;
        }
        match (self.digits, byte) {
            (Some(n), b'0'..=b'9') => {
                self.digits = Some(n.saturating_mul(10).saturating_add(u64::from(byte - b'0')));
                return;
            }
            (Some(n), b'}') => {
                self.digits = None;
                self.announced = Some(n);
                return;
            }
            _ => self.digits = None,
        }
        match byte {
            b'"' => self.quoted = true,
            b'{' => self.digits = Some(0),
            b'(' => {
                self.depth += 1;
                if self.depth > MAX_NESTING {
                    self.refused = true;
                }
            }
            b')' => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
    }
}

impl Scan {
    /// Whether the response is a continuation or a status response (RFC
    /// 3501's resp-text), which imap-proto reads as text to the line end.
    fn in_text(&self) -> bool {
        let head = &self.head[..self.head_len];
        if head.first() == Some(&b'+') {
            return true;
        }
        let Some(space) = head.iter().position(|&b| b == b' ') else {
            return false;
        };
        let word = head[space + 1..]
            .split(|&b| matches!(b, b' ' | b'\r' | b'\n' | b'['))
            .next()
            .unwrap_or_default();
        ["OK", "NO", "BAD", "BYE", "PREAUTH"]
            .iter()
            .any(|status| word.eq_ignore_ascii_case(status.as_bytes()))
    }
}

fn too_deep() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the server nested an answer more than {MAX_NESTING} levels deep"),
    )
}

impl<S: AsyncRead + Unpin> AsyncRead for Nesting<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        let start = buf.filled().len();
        ready!(Pin::new(&mut this.inner).poll_read(cx, buf))?;
        Poll::Ready(this.scan.feed(&buf.filled()[start..]))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Nesting<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(bytes: &[u8]) -> Result<usize, ()> {
        let mut scan = Scan::default();
        scan.feed(bytes).map_err(|_| ())?;
        Ok(scan.depth)
    }

    #[test]
    fn parentheses_count_until_the_line_ends() {
        assert_eq!(scan(b"* 1 FETCH (FLAGS (\\Seen"), Ok(2));
        assert_eq!(scan(b"* 1 FETCH (FLAGS (\\Seen))\r\n"), Ok(0));
        assert_eq!(scan(b"* 1 FETCH (UID 1 BODY[] {3}\r\n"), Ok(1));
    }

    #[test]
    fn parentheses_in_quoted_strings_and_literals_do_not_count() {
        let line = format!(
            "* 1 FETCH (ENVELOPE (\"{}\\\"(\" NIL) BODY[] {{4}}\r\n((((",
            "(".repeat(500)
        );
        assert_eq!(scan(line.as_bytes()), Ok(1));
    }

    #[test]
    fn a_line_nested_past_the_cap_is_refused() {
        let at_cap = "(".repeat(MAX_NESTING);
        assert_eq!(scan(at_cap.as_bytes()), Ok(MAX_NESTING));
        let past = "(".repeat(MAX_NESTING + 1);
        assert_eq!(scan(past.as_bytes()), Err(()));
    }

    #[test]
    fn the_count_carries_across_reads() {
        let mut scan = Scan::default();
        for chunk in [&b"* 1 FETCH (BODY[] {"[..], b"2}\r", b"\n)", b")(", b"\r\n"] {
            scan.feed(chunk).unwrap();
        }
        assert_eq!(scan.depth, 0);
        scan.feed("(".repeat(MAX_NESTING).as_bytes()).unwrap();
        assert!(scan.feed(b"(").is_err());
        // Once refused, the stream stays refused.
        assert!(scan.feed(b"\r\n").is_err());
    }

    /// imap-proto reads a status line's text to the line end, braces
    /// included, so `{n}` there must not hide the next `n` bytes.
    #[test]
    fn braces_ending_a_status_line_start_no_literal() {
        let deep = "(".repeat(MAX_NESTING + 1);
        for line in [
            "* OK x{99999}\r\n",
            "* bye {99999}\r\n",
            "A0001 NO [ALERT] {99999}\r\n",
            "+ go {99999}\r\n",
        ] {
            assert_eq!(
                scan(format!("{line}{deep}").as_bytes()),
                Err(()),
                "{line:?}"
            );
        }
        assert_eq!(
            scan(format!("* LIST () \"/\" {{{}}}\r\n{deep}", deep.len()).as_bytes()),
            Ok(0),
            "a literal outside a status line holds text"
        );
    }
}
