//! The YAML front matter at the top of a `SKILL.md`.
//!
//! Skills only need two strings from it, `name` and `description`, so this
//! reads the scalar forms people write by hand rather than pulling in a
//! YAML library: plain text that may run on over indented lines, single and
//! double quotes, and the `|` and `>` blocks. A key whose value is a nested
//! list or map is skipped, since neither field can be one.

use std::collections::BTreeMap;

/// The top-level keys with a text value, and the Markdown after the front
/// matter.
#[derive(Debug, PartialEq, Eq)]
pub struct FrontMatter<'a> {
    pub fields: BTreeMap<String, String>,
    pub body: &'a str,
}

/// Splits `text` into its front matter and body, or says why it cannot.
pub fn parse(text: &str) -> Result<FrontMatter<'_>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().unwrap_or("");
    if first.trim_end() != "---" {
        return Err("the file does not start with a --- line".into());
    }
    let mut offset = first.len();
    let mut header = Vec::new();
    let mut closed = false;
    for line in lines {
        offset += line.len();
        let bare = line.trim_end_matches(['\n', '\r']);
        if bare.trim_end() == "---" || bare.trim_end() == "..." {
            closed = true;
            break;
        }
        header.push(bare);
    }
    if !closed {
        return Err("the front matter has no closing --- line".into());
    }
    Ok(FrontMatter {
        fields: fields(&header)?,
        body: &text[offset..],
    })
}

fn fields(lines: &[&str]) -> Result<BTreeMap<String, String>, String> {
    let mut fields = BTreeMap::new();
    let mut at = 0;
    while at < lines.len() {
        let line = lines[at];
        at += 1;
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            return Err(format!("line {} is indented under nothing", at + 1));
        }
        let Some((key, rest)) = line.split_once(':') else {
            return Err(format!("line {} has no colon after a key", at + 1));
        };
        let key = key.trim().trim_matches(['"', '\'']).to_string();
        let rest = rest.trim();
        // Every line indented under this key belongs to its value.
        let start = at;
        while at < lines.len() && (lines[at].trim().is_empty() || indented(lines[at])) {
            at += 1;
        }
        let more = &lines[start..at];
        if let Some(value) = scalar(rest, more)? {
            fields.insert(key, value);
        }
    }
    Ok(fields)
}

fn indented(line: &str) -> bool {
    line.starts_with([' ', '\t'])
}

/// The text of one value, or `None` for a list or a map.
fn scalar(first: &str, more: &[&str]) -> Result<Option<String>, String> {
    if first.starts_with('"') {
        return double_quoted(first, more).map(Some);
    }
    if first.starts_with('\'') {
        return single_quoted(first, more).map(Some);
    }
    if let Some(style) = first.chars().next().filter(|c| *c == '|' || *c == '>') {
        return Ok(Some(block(style, more)));
    }
    if first.is_empty() {
        let Some(next) = more.iter().find(|line| !line.trim().is_empty()) else {
            return Ok(Some(String::new()));
        };
        let next = next.trim();
        if next.starts_with("- ") || next == "-" || next.ends_with(':') || next.contains(": ") {
            return Ok(None);
        }
    }
    if first.starts_with('[') || first.starts_with('{') {
        return Ok(None);
    }
    // Plain text: a comment ends it, and lines under it fold into one.
    let mut words = vec![uncomment(first)];
    words.extend(more.iter().map(|line| uncomment(line.trim())));
    Ok(Some(fold(&words)))
}

fn uncomment(text: &str) -> &str {
    match text.find(" #") {
        Some(at) => text[..at].trim_end(),
        None => text,
    }
}

/// Joins lines the way YAML folds them: one space between lines, and a
/// line break for each blank line.
fn fold(lines: &[&str]) -> String {
    let mut out = String::new();
    let mut breaks = 0;
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            breaks += 1;
            continue;
        }
        if !out.is_empty() {
            if breaks > 0 {
                out.push_str(&"\n".repeat(breaks));
            } else {
                out.push(' ');
            }
        }
        breaks = 0;
        out.push_str(line);
    }
    out
}

/// A `|` block keeps its line breaks and a `>` block folds them.
fn block(style: char, lines: &[&str]) -> String {
    let indent = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let body: Vec<&str> = lines
        .iter()
        .map(|line| line.get(indent..).unwrap_or("").trim_end())
        .collect();
    if style == '|' {
        body.join("\n").trim_end().to_string()
    } else {
        fold(&body)
    }
}

fn double_quoted(first: &str, more: &[&str]) -> Result<String, String> {
    let text = joined(&first[1..], more);
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Ok(out),
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let code = u32::from_str_radix(&hex, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .ok_or_else(|| format!("\\u{hex} is not a character"))?;
                    out.push(code);
                }
                Some(other) => out.push(other),
                None => break,
            },
            other => out.push(other),
        }
    }
    Err("a double-quoted value has no closing quote".into())
}

fn single_quoted(first: &str, more: &[&str]) -> Result<String, String> {
    let text = joined(&first[1..], more);
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\'' {
            if chars.peek() == Some(&'\'') {
                chars.next();
                out.push('\'');
                continue;
            }
            return Ok(out);
        }
        out.push(c);
    }
    Err("a single-quoted value has no closing quote".into())
}

/// A quoted value may carry on over several lines, which fold like plain
/// text before the escapes are read, so an escaped `\n` survives.
fn joined(first: &str, more: &[&str]) -> String {
    let mut lines = vec![first];
    lines.extend(more.iter().copied());
    fold(&lines)
}
