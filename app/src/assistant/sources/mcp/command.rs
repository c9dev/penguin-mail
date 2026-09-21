//! A server's command line, read and written the way a shell quotes one,
//! since that is how READMEs print them: `npx -y @scope/server "/my dir"`,
//! with `NAME=value` words in front for the environment.

use std::collections::BTreeMap;

/// The pieces of a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine {
    pub env: BTreeMap<String, String>,
    pub command: String,
    pub args: Vec<String>,
}

/// Splits a command line into words, honouring single quotes, double
/// quotes and backslashes, then takes the `NAME=value` words in front as
/// the environment.
pub fn split_command_line(line: &str) -> Result<CommandLine, String> {
    let words = words(line)?;
    let mut env = BTreeMap::new();
    let mut rest = words.into_iter().peekable();
    while let Some((name, value)) = rest.peek().and_then(|word| assignment(word)) {
        env.insert(name, value);
        rest.next();
    }
    let command = rest.next().ok_or("There is no command.")?;
    Ok(CommandLine {
        env,
        command,
        args: rest.collect(),
    })
}

/// `NAME=value`, when the word is one.
fn assignment(word: &str) -> Option<(String, String)> {
    let (name, value) = word.split_once('=')?;
    let first = name.chars().next()?;
    let valid = (first.is_ascii_alphabetic() || first == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then(|| (name.to_string(), value.to_string()))
}

fn words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    // A word can be empty and still be a word, as `''` is.
    let mut started = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("A single quote is never closed.".into()),
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return Err("A double quote is never closed.".into()),
                        },
                        Some(c) => word.push(c),
                        None => return Err("A double quote is never closed.".into()),
                    }
                }
            }
            '\\' => {
                started = true;
                if let Some(c) = chars.next() {
                    word.push(c);
                }
            }
            c => {
                started = true;
                word.push(c);
            }
        }
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

/// Writes a command line back, quoting only the words that need it, so
/// that [`split_command_line`] reads it back the same.
pub fn join_command_line(env: &BTreeMap<String, String>, command: &str, args: &[String]) -> String {
    env.iter()
        .map(|(name, value)| format!("{name}={}", quote(value)))
        .chain(std::iter::once(quote(command)))
        .chain(args.iter().map(|arg| quote(arg)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_~".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}
