//! The one Sieve script Penguin Mail keeps on a server: its rules and the
//! automatic reply, each a block under a comment that names the neutral
//! rule as JSON and hashes the statement it wrote. Reading the script
//! back trusts a block whose statement still hashes the same and keeps
//! any other text, a hand-edited block of ours included, as foreign, so
//! nothing written elsewhere is lost when the script is written again.

use std::collections::BTreeSet;

use chrono::{Local, TimeZone};
use mailrs_domain::{Filter, MailSet, Role, Vacation};

const HEADER: &str = "# Penguin Mail keeps this script and writes it whole when a rule or the\n\
# automatic reply changes. Blocks edited or added by hand are kept as they\n\
# are, and show in Penguin Mail as rules written elsewhere.\n";
const RULE: &str = "# penguin-mail rule ";
const VACATION: &str = "# penguin-mail vacation ";
const OFF: &str = "off";

/// The Sieve extensions a server offers, from ManageSieve's `SIEVE`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extensions(BTreeSet<String>);

impl Extensions {
    pub fn parse(listed: &str) -> Extensions {
        Extensions(listed.split_whitespace().map(str::to_ascii_lowercase).collect())
    }

    pub fn has(&self, name: &str) -> bool {
        self.0.contains(name)
    }

    /// What Penguin Mail needs before it keeps rules on the server.
    pub fn usable(&self) -> bool {
        self.has("fileinto") && self.has("vacation")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Script {
    pub rules: Vec<Filter>,
    pub vacation: Option<Vacation>,
    /// Text nobody here wrote, or a block of ours edited by hand, kept
    /// verbatim after the app's own blocks.
    pub foreign: Vec<String>,
    /// The script the person already ran, included (RFC 6609).
    pub include: Option<String>,
    /// Extensions the foreign text requires.
    pub kept_requires: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteError {
    /// The rule needs this Sieve extension, which the server lacks.
    Needs(&'static str),
    /// The rule files into a folder the account has not listed.
    NoFolder(String),
    /// Sieve has no way to say this part of the rule.
    Unsayable(Unsayable),
}

/// What a Sieve rule cannot say, for sync to word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsayable {
    NothingToMatch,
    NothingToDo,
    Importance,
    NeverSpam,
    TwoFolders,
}

/// FNV-1a over the statement, as hex: enough to tell an edit, not a secret.
fn hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.trim().bytes() {
        h ^= u64::from(byte);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// A Sieve quoted string (RFC 5228 section 2.4.2).
fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn rule_statement(
    filter: &Filter,
    folder: &dyn Fn(&MailSet) -> Option<String>,
    ext: &Extensions,
    needs: &mut BTreeSet<&'static str>,
) -> Result<String, WriteError> {
    let mut need = |name: &'static str| {
        if ext.has(name) {
            needs.insert(name);
            Ok(())
        } else {
            Err(WriteError::Needs(name))
        }
    };
    let c = &filter.criteria;
    let mut tests = Vec::new();
    if let Some(from) = &c.from {
        tests.push(format!("address :all :contains \"from\" {}", quote(from)));
    }
    if let Some(to) = &c.to {
        tests.push(format!("address :all :contains [\"to\", \"cc\"] {}", quote(to)));
    }
    if let Some(subject) = &c.subject {
        tests.push(format!("header :contains \"subject\" {}", quote(subject)));
    }
    if let Some(words) = &c.query {
        need("body")?;
        tests.push(format!("body :text :contains {}", quote(&mailrs_domain::query::plain(words))));
    }
    if let Some(words) = &c.negated_query {
        need("body")?;
        tests.push(format!("not body :text :contains {}", quote(&mailrs_domain::query::plain(words))));
    }
    if let Some(size) = c.size {
        // Gmail's "smaller" is Sieve's `:under`; anything else reads as larger.
        let way = if c.size_comparison.as_deref() == Some("smaller") { ":under" } else { ":over" };
        tests.push(format!("size {way} {size}"));
    }
    if c.has_attachment {
        need("mime")?;
        tests.push("header :mime :anychild :contains \"Content-Disposition\" \"attachment\"".to_string());
    }
    let condition = match tests.len() {
        0 => return Err(WriteError::Unsayable(Unsayable::NothingToMatch)),
        1 => tests.remove(0),
        _ => format!("allof ({})", tests.join(", ")),
    };
    let a = &filter.action;
    let mut actions = Vec::new();
    if a.remove.contains(&MailSet::Unseen) {
        need("imap4flags")?;
        actions.push("addflag \"\\\\Seen\";".to_string());
    }
    if a.add.contains(&MailSet::flagged()) {
        need("imap4flags")?;
        actions.push("addflag \"\\\\Flagged\";".to_string());
    }
    if let Some(to) = &a.forward {
        need("copy")?;
        actions.push(format!("redirect :copy {};", quote(to)));
    }
    let places: Vec<&MailSet> = a
        .add
        .iter()
        .filter(|set| matches!(set, MailSet::Mailbox(_) | MailSet::Role(Role::Trash | Role::Junk | Role::Archive)))
        .collect();
    if a.add.iter().chain(&a.remove).any(|set| matches!(set, MailSet::Role(Role::Important) | MailSet::Category(_))) {
        return Err(WriteError::Unsayable(Unsayable::Importance));
    }
    if a.remove.contains(&MailSet::Role(Role::Junk)) {
        return Err(WriteError::Unsayable(Unsayable::NeverSpam));
    }
    match places.as_slice() {
        [] if a.remove.contains(&MailSet::Role(Role::Inbox)) => {
            need("fileinto")?;
            match folder(&MailSet::Role(Role::Archive)) {
                Some(name) => actions.push(format!("fileinto {};", quote(&name))),
                None => {
                    need("mailbox")?;
                    actions.push("fileinto :create \"Archive\";".to_string());
                }
            }
        }
        [] => {}
        [MailSet::Role(Role::Trash)] => match folder(&MailSet::Role(Role::Trash)) {
            Some(name) => {
                need("fileinto")?;
                actions.push(format!("fileinto {};", quote(&name)));
            }
            None => actions.push("discard;".to_string()),
        },
        [set] => {
            need("fileinto")?;
            let name = folder(set).ok_or_else(|| WriteError::NoFolder(format!("{set:?}")))?;
            actions.push(format!("fileinto {};", quote(&name)));
        }
        _ => return Err(WriteError::Unsayable(Unsayable::TwoFolders)),
    }
    if actions.is_empty() {
        return Err(WriteError::Unsayable(Unsayable::NothingToDo));
    }
    Ok(format!("if {condition} {{\n    {}\n}}", actions.join("\n    ")))
}

fn day(at: mailrs_domain::EpochMillis) -> String {
    Local.timestamp_millis_opt(at).single().map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

fn vacation_statement(v: &Vacation, address: &str, ext: &Extensions, needs: &mut BTreeSet<&'static str>) -> Result<String, WriteError> {
    if !ext.has("vacation") {
        return Err(WriteError::Needs("vacation"));
    }
    needs.insert("vacation");
    let reply = format!("vacation :days 1 :subject {} :addresses [{}] {};", quote(&v.subject), quote(address), quote(&v.body));
    let mut tests = Vec::new();
    if let Some(start) = v.start {
        tests.push(format!("currentdate :value \"ge\" \"date\" {}", quote(&day(start))));
    }
    if let Some(end) = v.end {
        // The end is the moment replies stop: the first day not answered.
        tests.push(format!("currentdate :value \"lt\" \"date\" {}", quote(&day(end))));
    }
    if tests.is_empty() {
        return Ok(reply);
    }
    for name in ["date", "relational"] {
        if !ext.has(name) {
            return Err(WriteError::Needs(name));
        }
        needs.insert(name);
    }
    let condition = if tests.len() == 1 { tests.remove(0) } else { format!("allof ({})", tests.join(", ")) };
    Ok(format!("if {condition} {{\n    {reply}\n}}"))
}

pub fn write(script: &Script, address: &str, folder: &dyn Fn(&MailSet) -> Option<String>, ext: &Extensions) -> Result<String, WriteError> {
    let mut needs: BTreeSet<&'static str> = BTreeSet::new();
    let mut blocks = Vec::new();
    if let Some(v) = &script.vacation {
        let json = serde_json::to_string(v).unwrap_or_default();
        if v.enabled {
            let statement = vacation_statement(v, address, ext, &mut needs)?;
            blocks.push(format!("{VACATION}{} {json}\n{statement}", hash(&statement)));
        } else {
            blocks.push(format!("{VACATION}{OFF} {json}"));
        }
    }
    for rule in &script.rules {
        let statement = rule_statement(rule, folder, ext, &mut needs)?;
        let json = serde_json::to_string(rule).unwrap_or_default();
        blocks.push(format!("{RULE}{} {json}\n{statement}", hash(&statement)));
    }
    if script.include.is_some() {
        if !ext.has("include") {
            return Err(WriteError::Needs("include"));
        }
        needs.insert("include");
    }
    let mut requires: BTreeSet<String> = needs.iter().map(|n| n.to_string()).collect();
    requires.extend(script.kept_requires.iter().cloned());
    let mut text = String::from(HEADER);
    if !requires.is_empty() {
        let listed: Vec<String> = requires.iter().map(|r| quote(r)).collect();
        text.push_str(&format!("require [{}];\n", listed.join(", ")));
    }
    if let Some(name) = &script.include {
        text.push_str(&format!("include :personal {};\n", quote(name)));
    }
    for block in blocks.iter().chain(&script.foreign) {
        text.push('\n');
        text.push_str(block.trim_end());
        text.push('\n');
    }
    Ok(text)
}

/// One top-level statement and the comment lines just above it.
struct Piece {
    comments: Vec<String>,
    statement: String,
}

/// Splits a script into top-level statements. A statement ends at a `;`
/// or at a `}` that closes its last block, unless `elsif` or `else`
/// follows. Strings, `text:` literals and comments cannot end one.
fn pieces(text: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut comments = Vec::new();
    let mut statement = String::new();
    let mut depth = 0i32;
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if statement.is_empty() && (trimmed.starts_with('#') || trimmed.is_empty()) {
            if trimmed.starts_with('#') {
                comments.push(trimmed.to_string());
            }
            continue;
        }
        statement.push_str(line);
        statement.push('\n');
        // `text:` runs to a line holding a lone dot.
        if trimmed.ends_with("text:") {
            for more in lines.by_ref() {
                statement.push_str(more);
                statement.push('\n');
                if more.trim_end() == "." {
                    break;
                }
            }
        }
        let (mut in_string, mut escaped, mut ended) = (false, false, false);
        for ch in line.chars() {
            match (in_string, escaped, ch) {
                (true, true, _) => escaped = false,
                (true, false, '\\') => escaped = true,
                (true, false, '"') => in_string = false,
                (false, _, '"') => in_string = true,
                (false, _, '#') => break,
                (false, _, '{') => depth += 1,
                (false, _, '}') => {
                    depth -= 1;
                    ended = depth <= 0;
                }
                (false, _, ';') if depth == 0 => ended = true,
                _ => {}
            }
        }
        let continues = lines.peek().is_some_and(|next| {
            let next = next.trim_start();
            next.starts_with("elsif") || next.starts_with("else")
        });
        if ended && depth <= 0 && !continues {
            out.push(Piece { comments: std::mem::take(&mut comments), statement: std::mem::take(&mut statement) });
            depth = 0;
        }
    }
    if !statement.trim().is_empty() || !comments.is_empty() {
        out.push(Piece { comments, statement });
    }
    out
}

/// The names a `require` statement lists.
fn required(statement: &str) -> Vec<String> {
    statement
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

pub fn read(text: &str) -> Script {
    let mut script = Script::default();
    let mut kept: BTreeSet<String> = BTreeSet::new();
    for piece in pieces(text) {
        let statement = piece.statement.trim();
        if statement.starts_with("require") {
            kept.extend(required(statement));
            continue;
        }
        if let Some(name) = statement.strip_prefix("include :personal ").and_then(|r| r.trim_end_matches(';').trim().strip_prefix('"')).and_then(|r| r.strip_suffix('"')) {
            script.include = Some(name.to_string());
            continue;
        }
        // An "off" reply has no statement, so its comment can sit in the
        // same piece as the next block's.
        for comment in &piece.comments {
            let off = comment.strip_prefix(VACATION).and_then(|r| r.strip_prefix(OFF)).and_then(|j| serde_json::from_str::<Vacation>(j.trim()).ok());
            if off.is_some() {
                script.vacation = off;
            }
        }
        let off_prefix = format!("{VACATION}{OFF} ");
        let mine = piece.comments.iter().rev().find(|c| (c.starts_with(RULE) || c.starts_with(VACATION)) && !c.starts_with(&off_prefix));
        let ours = mine.and_then(|comment| {
            let (kind, rest) = comment.strip_prefix(RULE).map(|r| ("rule", r)).or_else(|| comment.strip_prefix(VACATION).map(|r| ("vacation", r)))?;
            let (mark, json) = rest.split_once(' ')?;
            if mark != hash(statement) {
                return None;
            }
            match kind {
                "vacation" => serde_json::from_str::<Vacation>(json).ok().map(|v| (None, Some(v))),
                // A rule read back from our own script is never read-only.
                _ => serde_json::from_str::<Filter>(json).ok().map(|f| (Some(Filter { read_only: false, ..f }), None)),
            }
        });
        match ours {
            Some((Some(rule), _)) => script.rules.push(rule),
            Some((None, Some(vacation))) => script.vacation = Some(vacation),
            _ if statement.is_empty() => {}
            _ => {
                let own_comments: Vec<&String> = piece
                    .comments
                    .iter()
                    .filter(|c| !c.starts_with("# Penguin Mail keeps") && !c.starts_with("# automatic reply") && !c.starts_with("# are, and show"))
                    .filter(|c| !c.starts_with(RULE) && !c.starts_with(VACATION))
                    .collect();
                let mut block = own_comments.iter().map(|c| format!("{c}\n")).collect::<String>();
                block.push_str(statement);
                script.foreign.push(block);
            }
        }
    }
    // What the foreign text needs stays required; what only our blocks
    // need comes back from the writer.
    script.kept_requires = if script.foreign.is_empty() { Vec::new() } else { kept.into_iter().collect() };
    script
}

#[cfg(test)]
mod extension_tests {
    use super::*;

    #[test]
    fn a_server_needs_fileinto_and_vacation() {
        assert!(Extensions::parse("fileinto reject vacation").usable());
        assert!(!Extensions::parse("fileinto reject").usable());
        assert!(Extensions::parse("FileInto Vacation").has("vacation"));
    }
}
