//! [`outline`] read from a seekable source, such as a stored message's
//! blob, a piece at a time. The structure and the text come out as they do
//! from the bytes in memory, but a file is never held: its headers are read,
//! its end is found by scanning for the boundary in chunks, and its decoded
//! size is counted as the chunks pass. Peak memory is the text parts, the
//! headers and one chunk, not the message.

use std::io::{self, Read, Seek, SeekFrom};

use mail_parser::MessageParser;

use super::{
    Described, MAX_DEPTH, Part, Parts, Skim, Spans, Transfer, decoded_size, describe,
    is_attachment, is_calendar, is_readable, mime_type, numbered, undo_transfer_encoding, unfold,
};

/// How much a scan reads at once.
const CHUNK: usize = 64 << 10;

/// The most a header block may take before the part is read whole instead.
const HEAD_LIMIT: usize = 1 << 20;

/// The most of one text part's encoded bytes the outline keeps to show. The
/// part's size and span still cover all of it, so saving it or showing the
/// original reads the stored message and loses nothing.
const TEXT_KEPT: usize = 8 << 20;

/// [`super::outline`] of the `len` bytes `source` holds. A part that cannot
/// be walked from the stream, one with no headers or an enormous header
/// block, is read whole and walked in memory.
pub fn outline_from<R: Read + Seek>(
    source: &mut R,
    len: u64,
) -> io::Result<Option<(Parts, Spans)>> {
    let mut walk = Walk {
        source,
        skim: Skim {
            forwarded: true,
            spans: Vec::new(),
        },
    };
    let Some(head) = walk.head(0, len)? else {
        let raw = walk.read(0, len)?;
        return Ok(super::outline(&raw));
    };
    let Some(message) = MessageParser::default().parse_headers(&head) else {
        return Ok(None);
    };
    let headers = message
        .headers()
        .iter()
        .map(|header| {
            (
                header.raw_name().to_string(),
                unfold(&String::from_utf8_lossy(header.raw_value())),
            )
        })
        .collect();
    let root_path = match mime_type(&message.root_part()).starts_with("multipart/") {
        true => String::new(),
        false => "1".to_string(),
    };
    let root = walk.part(0, len, root_path, 0)?;
    Ok(Some((
        Parts {
            headers,
            root,
            incomplete: false,
        },
        Spans(walk.skim.spans),
    )))
}

struct Walk<'r, R> {
    source: &'r mut R,
    /// Holds the spans, and walks the pieces that are read whole.
    skim: Skim,
}

impl<R: Read + Seek> Walk<'_, R> {
    /// The bytes from `start` to `end`.
    fn read(&mut self, start: u64, end: u64) -> io::Result<Vec<u8>> {
        let length = usize::try_from(end.saturating_sub(start))
            .map_err(|_| io::Error::other("a part longer than memory"))?;
        self.source.seek(SeekFrom::Start(start))?;
        let mut bytes = vec![0; length];
        self.source.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    /// The header block of the part at `start..end`, with its blank line,
    /// or `None` when it passes [`HEAD_LIMIT`] without one.
    fn head(&mut self, start: u64, end: u64) -> io::Result<Option<Vec<u8>>> {
        let mut head: Vec<u8> = Vec::new();
        let mut at = start;
        self.source.seek(SeekFrom::Start(start))?;
        while at < end {
            let want = ((end - at) as usize).min(CHUNK);
            let old = head.len();
            head.resize(old + want, 0);
            let got = self.source.read(&mut head[old..])?;
            head.truncate(old + got);
            if got == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            at += got as u64;
            // The blank line may straddle the old end.
            let from = old.saturating_sub(3);
            if let Some(stop) = blank_line(&head, from) {
                head.truncate(stop);
                return Ok(Some(head));
            }
            if head.len() > HEAD_LIMIT {
                return Ok(None);
            }
        }
        Ok(Some(head))
    }

    /// The part at `start..end` of the source, `depth` levels below the root.
    fn part(&mut self, start: u64, end: u64, path: String, depth: usize) -> io::Result<Part> {
        let Some(head) = self.head(start, end)? else {
            return self.whole(start, end, path, depth);
        };
        let parser = MessageParser::default();
        let Some(message) = parser.parse_headers(&head) else {
            return self.whole(start, end, path, depth);
        };
        let body_start = start + u64::from(message.root_part().offset_body());
        let Described {
            mut skimmed,
            boundary,
            encoding,
        } = describe(&message);
        let encoding = encoding.as_deref();
        let transfer = Transfer::of(encoding);
        if skimmed.mime_type.starts_with("multipart/") {
            if let (Some(boundary), true) = (boundary, depth < MAX_DEPTH) {
                let ranges = self.children(body_start, end, boundary.as_bytes())?;
                for (i, (from, to)) in ranges.into_iter().enumerate() {
                    skimmed
                        .children
                        .push(self.part(from, to, numbered(&path, i), depth + 1)?);
                }
            }
        } else if skimmed.mime_type == "message/rfc822" {
            skimmed.size = self.size(encoding, body_start, end)? as i64;
            self.note(&path, body_start, end, transfer, skimmed.size as u64);
            if depth < MAX_DEPTH {
                self.forwarded(&mut skimmed, body_start, end, encoding, &path, depth)?;
            }
        } else {
            let name = skimmed.filename.as_deref().unwrap_or_default();
            let wanted = (is_readable(&skimmed.mime_type) && !is_attachment(&skimmed))
                || is_calendar(&skimmed.mime_type, name);
            if wanted {
                if end - body_start > TEXT_KEPT as u64 {
                    // Keep whole lines of the start, so a transfer encoding
                    // decodes cleanly; the size counts all of the part.
                    let mut body = self.read(body_start, body_start + TEXT_KEPT as u64)?;
                    if let Some(newline) = body.iter().rposition(|b| *b == b'\n') {
                        body.truncate(newline + 1);
                    }
                    skimmed.data = undo_transfer_encoding(encoding, &body);
                    skimmed.size = self.size(encoding, body_start, end)? as i64;
                } else {
                    let body = self.read(body_start, end)?;
                    skimmed.data = undo_transfer_encoding(encoding, &body);
                    skimmed.size = skimmed.data.as_ref().map_or(0, |d| d.len() as i64);
                }
            } else {
                skimmed.size = self.size(encoding, body_start, end)? as i64;
            }
            self.note(&path, body_start, end, transfer, skimmed.size as u64);
        }
        skimmed.path = path;
        Ok(skimmed)
    }

    /// The part read into memory and walked there.
    fn whole(&mut self, start: u64, end: u64, path: String, depth: usize) -> io::Result<Part> {
        let bytes = self.read(start, end)?;
        Ok(self.skim.part(&bytes, Some(start as usize), path, depth))
    }

    fn note(&mut self, path: &str, start: u64, end: u64, transfer: Transfer, size: u64) {
        let span = super::Span {
            start,
            end,
            transfer,
            size,
        };
        self.skim.spans.push((path.to_string(), span));
    }

    /// The parts of the multipart body at `start..end` as ranges, cut the
    /// way [`Skim::children`] cuts them.
    fn children(&mut self, start: u64, end: u64, boundary: &[u8]) -> io::Result<Vec<(u64, u64)>> {
        let mut scan = Scan::new(self.source, start, end);
        let mut ranges = Vec::new();
        if !scan.seek_next_part(boundary)? {
            return Ok(ranges);
        }
        scan.skip_crlf()?;
        loop {
            let from = scan.offset();
            let (to, found) = scan.seek_part_end(boundary)?;
            ranges.push((from, to.max(from)));
            if !found || scan.is_multipart_end()? {
                return Ok(ranges);
            }
        }
    }

    /// The parts of the message a `message/rfc822` part holds, as
    /// [`Skim::forwarded_message`] reads them.
    fn forwarded(
        &mut self,
        skimmed: &mut Part,
        start: u64,
        end: u64,
        encoding: Option<&str>,
        path: &str,
        depth: usize,
    ) -> io::Result<()> {
        let inner = match Transfer::of(encoding) {
            Transfer::Plain => self.head(start, end)?,
            _ => None,
        };
        let Some(head) = inner else {
            let body = self.read(start, end)?;
            self.skim.forwarded_message(
                skimmed,
                &body,
                Some(start as usize),
                encoding,
                path,
                depth,
            );
            return Ok(());
        };
        let Some(headers) = MessageParser::default().parse_headers(&head) else {
            return Ok(());
        };
        skimmed.subject = headers.subject().map(str::to_string);
        skimmed.children = match mime_type(&headers.root_part()).starts_with("multipart/") {
            true => self.part(start, end, path.to_string(), depth + 1)?.children,
            false => vec![self.part(start, end, format!("{path}.1"), depth + 1)?],
        };
        Ok(())
    }

    /// The size of the bytes at `start..end` once `encoding` is undone,
    /// counted from chunks when they are base64.
    fn size(&mut self, encoding: Option<&str>, start: u64, end: u64) -> io::Result<usize> {
        match Transfer::of(encoding) {
            Transfer::Plain => Ok((end - start) as usize),
            Transfer::Base64 => match self.base64_letters(start, end)? {
                Some(size) => Ok(size),
                None => Ok(decoded_size(encoding, &self.read(start, end)?)),
            },
            Transfer::QuotedPrintable => Ok(decoded_size(encoding, &self.read(start, end)?)),
        }
    }

    /// [`super::base64_size`] over the bytes at `start..end`, a chunk at a time.
    fn base64_letters(&mut self, start: u64, end: u64) -> io::Result<Option<usize>> {
        let (mut letters, mut padding) = (0usize, 0usize);
        let mut chunk = vec![0u8; CHUNK.min((end - start) as usize)];
        self.source.seek(SeekFrom::Start(start))?;
        let mut left = end - start;
        while left > 0 {
            let want = (left as usize).min(chunk.len());
            self.source.read_exact(&mut chunk[..want])?;
            left -= want as u64;
            for &b in &chunk[..want] {
                match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' if padding == 0 => {
                        letters += 1
                    }
                    b'=' => padding += 1,
                    b if b.is_ascii_whitespace() => {}
                    _ => return Ok(None),
                }
            }
        }
        let whole =
            letters % 4 != 1 && padding <= 2 && (padding == 0 || (letters + padding) % 4 == 0);
        Ok(whole.then_some(letters * 3 / 4))
    }
}

/// Where the first blank line at or after `from` ends in `head`.
fn blank_line(head: &[u8], from: usize) -> Option<usize> {
    // A part may open with its blank line, when it has no headers.
    if from == 0 {
        if head.starts_with(b"\r\n") {
            return Some(2);
        }
        if head.starts_with(b"\n") {
            return Some(1);
        }
    }
    let mut at = from;
    while let Some(nl) = head[at..].iter().position(|b| *b == b'\n') {
        let line_end = at + nl + 1;
        let next = &head[line_end..];
        if next.starts_with(b"\n") {
            return Some(line_end + 1);
        }
        if next.starts_with(b"\r\n") {
            return Some(line_end + 2);
        }
        at = line_end;
    }
    None
}

/// A forward-only reader of the bytes at `start..end` of a source, with the
/// operations of mail-parser's stream that cut a multipart body.
pub(super) struct Scan<'r, R> {
    source: &'r mut R,
    buf: Vec<u8>,
    /// Where `buf` starts in the source.
    base: u64,
    pos: u64,
    end: u64,
}

impl<'r, R: Read + Seek> Scan<'r, R> {
    pub(super) fn new(source: &'r mut R, start: u64, end: u64) -> Self {
        Scan {
            source,
            buf: Vec::new(),
            base: start,
            pos: start,
            end,
        }
    }

    pub(super) fn offset(&self) -> u64 {
        self.pos.min(self.end)
    }

    /// Makes the bytes from `pos` on available, up to `want` of them.
    fn fill(&mut self, want: usize) -> io::Result<()> {
        let target = (self.pos + want as u64).min(self.end);
        // A lookahead that stepped back to a saved offset may land before
        // bytes a refill already dropped; read from there again.
        if self.pos < self.base {
            self.buf.clear();
            self.base = self.pos;
        }
        while self.base + (self.buf.len() as u64) < target {
            let consumed = (self.pos - self.base) as usize;
            if consumed >= CHUNK {
                self.buf.drain(..consumed);
                self.base = self.pos;
            }
            let at = self.base + self.buf.len() as u64;
            let room = ((self.end - at) as usize).min(CHUNK);
            self.source.seek(SeekFrom::Start(at))?;
            let old = self.buf.len();
            self.buf.resize(old + room, 0);
            let got = self.source.read(&mut self.buf[old..])?;
            self.buf.truncate(old + got);
            if got == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
        }
        Ok(())
    }

    fn peek(&mut self) -> io::Result<Option<u8>> {
        if self.pos >= self.end {
            return Ok(None);
        }
        self.fill(1)?;
        Ok(self.buf.get((self.pos - self.base) as usize).copied())
    }

    fn next(&mut self) -> io::Result<Option<u8>> {
        let byte = self.peek()?;
        if byte.is_some() {
            self.pos += 1;
        }
        Ok(byte)
    }

    /// Skips `needle` when it is next.
    fn try_skip(&mut self, needle: &[u8]) -> io::Result<bool> {
        if self.pos + needle.len() as u64 > self.end {
            return Ok(false);
        }
        self.fill(needle.len())?;
        let at = (self.pos - self.base) as usize;
        let matches = self.buf.get(at..at + needle.len()) == Some(needle);
        if matches {
            self.pos += needle.len() as u64;
        }
        Ok(matches)
    }

    pub(super) fn skip_crlf(&mut self) -> io::Result<()> {
        while let Some(ch) = self.peek()? {
            match ch {
                b'\r' | b' ' | b'\t' => self.pos += 1,
                b'\n' => {
                    self.pos += 1;
                    break;
                }
                _ => break,
            }
        }
        Ok(())
    }

    /// Moves past the buffered bytes that are neither `a` nor `b`, and
    /// answers the last two of them, if there were that many. Scanning a
    /// byte at a time through `next` costs a refill check for each.
    fn skip_to(&mut self, a: u8, b: u8) -> io::Result<(Option<u8>, Option<u8>)> {
        if self.pos >= self.end {
            return Ok((None, None));
        }
        self.fill(1)?;
        let rest = &self.buf[(self.pos - self.base) as usize..];
        let skipped = rest
            .iter()
            .position(|c| *c == a || *c == b)
            .unwrap_or(rest.len());
        let passed = (
            skipped.checked_sub(2).map(|i| rest[i]),
            skipped.checked_sub(1).map(|i| rest[i]),
        );
        self.pos += skipped as u64;
        Ok(passed)
    }

    pub(super) fn seek_next_part(&mut self, boundary: &[u8]) -> io::Result<bool> {
        if boundary.is_empty() {
            return Ok(false);
        }
        let saved = self.pos;
        let mut last = 0;
        loop {
            if let (_, Some(passed)) = self.skip_to(b'-', b'-')? {
                last = passed;
            }
            let Some(ch) = self.next()? else { break };
            if ch == b'-' && last == b'-' && self.try_skip(boundary)? {
                return Ok(true);
            }
            last = ch;
        }
        self.pos = saved;
        Ok(false)
    }

    pub(super) fn seek_part_end(&mut self, boundary: &[u8]) -> io::Result<(u64, bool)> {
        let (mut last, mut before) = (b'\n', 0u8);
        let mut end_pos = self.offset();
        loop {
            match self.skip_to(b'\n', b'-')? {
                (Some(second_last), Some(passed)) => (before, last) = (second_last, passed),
                (None, Some(passed)) => (before, last) = (last, passed),
                _ => {}
            }
            let Some(ch) = self.next()? else { break };
            if ch == b'\n' {
                end_pos = match last == b'\r' {
                    true => self.offset() - 2,
                    false => self.offset() - 1,
                };
            } else if ch == b'-' && last == b'-' && self.try_skip(boundary)? {
                if before != b'\n' {
                    end_pos = self.offset() - boundary.len() as u64 - 2;
                }
                return Ok((end_pos, true));
            }
            before = last;
            last = ch;
        }
        Ok((self.offset(), false))
    }

    pub(super) fn is_multipart_end(&mut self) -> io::Result<bool> {
        let saved = self.pos;
        match (self.next()?, self.peek()?) {
            (Some(b'\r'), Some(b'\n')) => {
                self.next()?;
                Ok(false)
            }
            (Some(b'-'), Some(b'-')) => {
                self.next()?;
                Ok(true)
            }
            (Some(b'\n'), _) => Ok(false),
            (Some(a), _) if a.is_ascii_whitespace() => {
                self.skip_crlf()?;
                Ok(false)
            }
            _ => {
                self.pos = saved;
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// A source that remembers the largest single read asked of it.
    struct Counting {
        inner: Cursor<Vec<u8>>,
        largest: usize,
        total: usize,
    }

    impl Read for Counting {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.largest = self.largest.max(buf.len());
            let got = self.inner.read(buf)?;
            self.total += got;
            Ok(got)
        }
    }

    impl Seek for Counting {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.inner.seek(to)
        }
    }

    fn same_outline(raw: &[u8]) {
        let from_memory = super::super::outline(raw);
        let from_stream = outline_from(&mut Cursor::new(raw.to_vec()), raw.len() as u64).unwrap();
        assert_eq!(
            from_stream,
            from_memory,
            "{:?}",
            String::from_utf8_lossy(&raw[..raw.len().min(600)])
        );
    }

    fn b64(n: usize) -> String {
        let letters = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for i in 0..n {
            out.push(letters[(i * 7 + i / 3) % 64] as char);
            if i % 76 == 75 {
                out.push_str("\r\n");
            }
        }
        out
    }

    fn mixed(file: &str, nl: &str) -> Vec<u8> {
        format!(
            "From: a@example.com{nl}To: b@example.com{nl}Subject: Files{nl}MIME-Version: 1.0{nl}Content-Type: multipart/mixed; boundary=\"outer\"{nl}{nl}\
             preamble{nl}--outer{nl}Content-Type: multipart/alternative; boundary=inner{nl}{nl}\
             --inner{nl}Content-Type: text/plain; charset=utf-8{nl}{nl}Hello there{nl}--inner{nl}Content-Type: text/html{nl}{nl}<p>Hello there</p>{nl}--inner--{nl}\
             --outer{nl}Content-Type: application/pdf; name=\"a.pdf\"{nl}Content-Disposition: attachment; filename=\"a.pdf\"{nl}Content-Transfer-Encoding: base64{nl}{nl}{file}{nl}\
             --outer{nl}Content-Type: image/png{nl}Content-Id: <x@y>{nl}Content-Transfer-Encoding: base64{nl}{nl}iVBORw0KGgo={nl}--outer--{nl}epilogue{nl}"
        )
        .into_bytes()
    }

    #[test]
    fn a_message_with_files_outlines_as_it_does_from_memory() {
        same_outline(&mixed(&b64(5000), "\r\n"));
        same_outline(&mixed(&b64(5000), "\n"));
        same_outline(&mixed("", "\r\n"));
    }

    #[test]
    fn a_plain_message_and_a_message_without_a_body_outline_alike() {
        same_outline(b"From: a@example.com\r\nSubject: Hi\r\n\r\nHello\r\n");
        same_outline(b"From: a@example.com\r\nSubject: Hi\r\n");
        same_outline(b"From: a@example.com\r\nContent-Type: text/html\r\n\r\n<b>x</b>");
        same_outline(b"");
        same_outline(b"\r\nno headers at all");
    }

    #[test]
    fn broken_multiparts_outline_alike() {
        // No end boundary; a boundary that is a prefix of another; a
        // boundary line with trailing blanks; an empty part; a part with
        // no headers; a boundary that never appears.
        same_outline(b"Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nOne\r\n--b\r\nContent-Type: text/plain\r\n\r\nTwo");
        same_outline(b"Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nOne\r\n--bb\r\nContent-Type: text/plain\r\n\r\nTwo\r\n--b--\r\n");
        same_outline(b"Content-Type: multipart/mixed; boundary=b\r\n\r\n--b  \r\nContent-Type: text/plain\r\n\r\nOne\r\n--b \t\r\n\r\n\r\n--b\r\n\r\nplain without headers\r\n--b--\r\n");
        same_outline(b"Content-Type: multipart/mixed; boundary=zzz\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nOne\r\n--b--\r\n");
        same_outline(b"Content-Type: multipart/mixed; boundary=b\r\n\r\n--b--\r\n");
        same_outline(b"Content-Type: multipart/mixed; boundary=b\r\n\r\n");
        same_outline(b"Content-Type: multipart/mixed\r\n\r\n--b\r\n\r\nx");
    }

    #[test]
    fn forwarded_messages_outline_alike() {
        let inner = mixed(&b64(900), "\r\n");
        let wrap = |encoding: &str, body: &[u8]| {
            let mut raw = format!(
                "From: a@example.com\r\nContent-Type: multipart/mixed; boundary=w\r\n\r\n--w\r\nContent-Type: text/plain\r\n\r\nSee below\r\n--w\r\nContent-Type: message/rfc822\r\n{encoding}\r\n"
            )
            .into_bytes();
            raw.extend_from_slice(body);
            raw.extend_from_slice(b"\r\n--w--\r\n");
            raw
        };
        same_outline(&wrap("", &inner));
        let encoded = {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(&inner)
        };
        same_outline(&wrap(
            "Content-Transfer-Encoding: base64\r\n",
            encoded.as_bytes(),
        ));
        same_outline(&wrap(
            "",
            b"From: x@example.com\r\nSubject: inner\r\n\r\nplain inner",
        ));
        same_outline(&wrap("", b"nothing headerlike"));
    }

    #[test]
    fn odd_file_encodings_outline_alike() {
        let part = |encoding: &str, body: &str| {
            format!(
                "Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: application/octet-stream; name=f.bin\r\nContent-Transfer-Encoding: {encoding}\r\n\r\n{body}\r\n--b--\r\n"
            )
            .into_bytes()
        };
        same_outline(&part("base64", "QUJD\r\nREVG\r\n"));
        same_outline(&part("base64", "QUJDR"));
        same_outline(&part("base64", "QUJD=A=="));
        same_outline(&part("base64", "QU*JD"));
        same_outline(&part("quoted-printable", "a=3Db=\r\nc"));
        same_outline(&part("7bit", "plain bytes"));
        same_outline(&part("binary", ""));
    }

    /// A random message tree, to compare the two readers on shapes nobody
    /// wrote by hand.
    #[test]
    fn random_messages_outline_alike() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut roll = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for case in 0..400 {
            let nl = if roll(2) == 0 { "\r\n" } else { "\n" };
            let mut raw = String::from("From: a@example.com\r\nSubject: s\r\n");
            let mut depth = 0;
            let mut stack: Vec<String> = Vec::new();
            let open = |raw: &mut String,
                        stack: &mut Vec<String>,
                        depth: &mut usize,
                        roll: &mut dyn FnMut(u64) -> u64| {
                let boundary = format!("bd{depth}x{}", roll(3));
                raw.push_str(&format!(
                    "Content-Type: multipart/mixed; boundary=\"{boundary}\"{nl}{nl}"
                ));
                stack.push(boundary);
                *depth += 1;
            };
            open(&mut raw, &mut stack, &mut depth, &mut roll);
            for _ in 0..roll(8) {
                let boundary = stack.last().unwrap().clone();
                raw.push_str(&format!(
                    "{}--{boundary}{}{nl}",
                    if roll(4) == 0 { "pre" } else { "" },
                    if roll(5) == 0 { "  " } else { "" }
                ));
                match roll(7) {
                    0 if stack.len() < 4 => open(&mut raw, &mut stack, &mut depth, &mut roll),
                    1 => raw.push_str(&format!("Content-Type: text/plain{nl}{nl}text {}{nl}", roll(100))),
                    2 => raw.push_str(&format!("Content-Type: application/zip; name=z.zip{nl}Content-Transfer-Encoding: base64{nl}{nl}{}{nl}", b64(roll(400) as usize))),
                    3 => raw.push_str(&format!("{nl}headerless {}{nl}", roll(9))),
                    4 => raw.push_str(&format!("Content-Type: message/rfc822{nl}{nl}Subject: in{nl}Content-Type: text/plain{nl}{nl}fwd{nl}")),
                    5 => {
                        if stack.len() > 1 {
                            let closing = stack.pop().unwrap();
                            raw.push_str(&format!("--{closing}--{nl}"));
                        }
                    }
                    _ => raw.push_str(&format!("Content-Type: image/gif{nl}{nl}GIF{nl}")),
                }
            }
            if roll(3) != 0 {
                while let Some(boundary) = stack.pop() {
                    raw.push_str(&format!("--{boundary}--{nl}"));
                }
            }
            let raw = raw.replace("\r\n\n", "\r\n").into_bytes();
            let from_memory = super::super::outline(&raw);
            let from_stream =
                outline_from(&mut Cursor::new(raw.clone()), raw.len() as u64).unwrap();
            assert_eq!(
                from_stream,
                from_memory,
                "case {case}: {:?}",
                String::from_utf8_lossy(&raw)
            );
        }
    }

    #[test]
    fn a_large_file_is_scanned_in_chunks_and_never_held() {
        let raw = mixed(&b64(40_000_000), "\r\n");
        let mut source = Counting {
            inner: Cursor::new(raw.clone()),
            largest: 0,
            total: 0,
        };
        let (parts, spans) = outline_from(&mut source, raw.len() as u64)
            .unwrap()
            .unwrap();
        assert!(
            source.largest <= CHUNK,
            "one read asked for {} bytes",
            source.largest
        );
        let file = parts.find("2").expect("the pdf");
        assert!(file.data.is_none() && file.size > 29_000_000);
        assert_eq!(spans.get("2").map(|s| s.size), Some(file.size as u64));
    }

    /// A lookahead that has to refill the buffer must not lose the byte it
    /// stepped back to.
    #[test]
    fn stepping_back_across_a_refill_keeps_the_byte() {
        let mut data = vec![b'x'; CHUNK + 10];
        data[CHUNK - 1] = b'y';
        let len = data.len() as u64;
        let mut cursor = Cursor::new(data);
        let mut scan = Scan::new(&mut cursor, 0, len);
        scan.fill(1).unwrap();
        scan.pos = (CHUNK - 1) as u64;
        assert!(!scan.is_multipart_end().unwrap());
        assert_eq!(scan.peek().unwrap(), Some(b'y'));
    }

    #[test]
    fn a_boundary_line_at_a_chunk_edge_outlines_as_it_does_from_memory() {
        for pad in CHUNK - 40..CHUNK + 40 {
            for nl in ["\r\n", "\n"] {
                let mut raw = format!(
                    "Content-Type: multipart/mixed; boundary=b{nl}{nl}--b{nl}Content-Type: text/plain{nl}{nl}"
                )
                .into_bytes();
                raw.resize(raw.len() + pad, b'a');
                raw.extend_from_slice(
                    format!("{nl}--b{nl}Content-Type: text/plain{nl}{nl}second{nl}--b--{nl}")
                        .as_bytes(),
                );
                let from_memory = super::super::outline(&raw);
                let from_stream =
                    outline_from(&mut Cursor::new(raw.clone()), raw.len() as u64).unwrap();
                assert_eq!(from_stream, from_memory, "pad {pad}");
            }
        }
    }

    #[test]
    fn a_boundary_that_never_comes_in_a_long_body_leaves_the_scan_usable() {
        let mut data = vec![b'x'; 3 * CHUNK];
        data.extend_from_slice(b"--b");
        let len = data.len() as u64;
        let mut cursor = Cursor::new(data);
        let mut scan = Scan::new(&mut cursor, 0, len);
        assert!(!scan.seek_next_part(b"zz").unwrap());
        assert_eq!(scan.peek().unwrap(), Some(b'x'));
    }

    #[test]
    fn a_huge_text_part_keeps_a_capped_preview_and_its_whole_span() {
        let line = "a".repeat(76) + "\r\n";
        let text = line.repeat(TEXT_KEPT / line.len() + 1000);
        let raw = format!(
            "Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\n{text}\r\n--b\r\nContent-Type: text/plain\r\n\r\nsecond\r\n--b--\r\n"
        )
        .into_bytes();
        let (parts, spans) = outline_from(&mut Cursor::new(raw.clone()), raw.len() as u64)
            .unwrap()
            .unwrap();
        let first = parts.find("1").expect("the long text");
        let kept = first.data.as_ref().expect("a preview");
        assert!(
            kept.len() <= TEXT_KEPT && kept.len() > TEXT_KEPT / 2,
            "{} bytes kept",
            kept.len()
        );
        assert!(text.as_bytes().starts_with(kept));
        assert_eq!(
            first.size,
            text.len() as i64,
            "the size is the whole part's"
        );
        let span = spans.get("1").expect("its span");
        assert_eq!(span.size, text.len() as u64);
        let whole = super::super::decode_span(&mut Cursor::new(raw), span)
            .unwrap()
            .expect("the whole part");
        assert_eq!(whole, text.as_bytes());
        assert_eq!(
            parts.find("2").and_then(|p| p.data.clone()),
            Some(b"second".to_vec())
        );
    }
}
