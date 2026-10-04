//! ManageSieve's answers (RFC 5804 section 1.2): data lines of quoted
//! strings, literals and atoms, ended by an OK, NO or BYE line with an
//! optional response code in parentheses and an optional string.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Ok,
    No,
    Bye,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Status {
    pub kind: Kind,
    pub code: Option<String>,
    pub text: String,
}

/// The status a line holds, or `None` for a data line.
pub(crate) fn status(line: &str) -> Option<Status> {
    let line = line.trim_end();
    let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
    let kind = match word.to_ascii_uppercase().as_str() {
        "OK" => Kind::Ok,
        "NO" => Kind::No,
        "BYE" => Kind::Bye,
        _ => return None,
    };
    let rest = rest.trim_start();
    let (code, rest) = match rest.strip_prefix('(') {
        Some(inner) => match inner.split_once(')') {
            Some((code, after)) => (Some(code.to_string()), after.trim_start()),
            None => (None, rest),
        },
        None => (None, rest),
    };
    let text = strings(rest).0.into_iter().next().unwrap_or_default();
    Some(Status { kind, code, text })
}

/// The quoted strings on a line, unescaped, and the atoms beside them.
pub(crate) fn strings(line: &str) -> (Vec<String>, Vec<String>) {
    let (mut quoted, mut atoms) = (Vec::new(), Vec::new());
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            '"' => {
                chars.next();
                let mut text = String::new();
                while let Some(ch) = chars.next() {
                    match ch {
                        '\\' => text.extend(chars.next()),
                        '"' => break,
                        other => text.push(other),
                    }
                }
                quoted.push(text);
            }
            c if c.is_whitespace() => {
                chars.next();
            }
            _ => {
                let mut atom = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_whitespace() || ch == '"' {
                        break;
                    }
                    atom.push(ch);
                    chars.next();
                }
                atoms.push(atom);
            }
        }
    }
    (quoted, atoms)
}

/// The byte count of a literal header, `{42}` or `{42+}`.
pub(crate) fn literal(line: &str) -> Option<usize> {
    line.trim_end()
        .strip_prefix('{')?
        .strip_suffix('}')?
        .trim_end_matches('+')
        .parse()
        .ok()
}

/// A string as ManageSieve quotes it.
pub(crate) fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_line_reads_its_kind_code_and_words() {
        assert_eq!(
            status("NO (QUOTA/MAXSIZE) \"Too big.\"").unwrap(),
            Status {
                kind: Kind::No,
                code: Some("QUOTA/MAXSIZE".into()),
                text: "Too big.".into()
            }
        );
        assert_eq!(status("OK").unwrap().kind, Kind::Ok);
        assert!(
            status("\"SIEVE\" \"x\"").is_none(),
            "a data line is no status"
        );
    }

    #[test]
    fn a_quoted_string_reads_its_escapes() {
        assert_eq!(
            strings("\"a \\\"b\\\" c\" ACTIVE"),
            (vec!["a \"b\" c".to_string()], vec!["ACTIVE".to_string()])
        );
    }

    #[test]
    fn a_literal_header_says_its_length() {
        assert_eq!(literal("{42}"), Some(42));
        assert_eq!(literal("{42+}"), Some(42));
        assert_eq!(literal("\"x\""), None);
    }
}
