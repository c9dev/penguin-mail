//! Address-list parsing, enough to display senders and recipients.
//! Gmail decodes RFC 2047 encoded words before returning headers.

use mailrs_domain::Address;

/// Parses a `From`, `To`, or `Cc` header. Entries without an address, such as
/// group syntax, are dropped.
pub fn parse_address_list(input: &str) -> Vec<Address> {
    split_top_level(input)
        .into_iter()
        .filter_map(parse_one)
        .collect()
}

/// Like [`parse_address_list`], but keeps entries that are not addresses as
/// they were typed, so a form can point out the mistake instead of losing it.
pub fn parse_address_list_keeping_invalid(input: &str) -> Vec<Address> {
    split_top_level(input)
        .into_iter()
        .filter_map(|raw| {
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then(|| {
                parse_one(trimmed).unwrap_or_else(|| Address {
                    name: None,
                    email: trimmed.to_string(),
                })
            })
        })
        .collect()
}

/// Splits on commas outside quotes and angle brackets.
fn split_top_level(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut in_quotes = false;
    let mut escaped = false;
    let mut angle_depth = 0u32;
    let mut start = 0;
    for (i, c) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            '<' if !in_quotes => angle_depth += 1,
            '>' if !in_quotes && angle_depth > 0 => angle_depth -= 1,
            ',' if !in_quotes && angle_depth == 0 => {
                parts.push(&input[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&input[start..]);
    parts
}

fn parse_one(raw: &str) -> Option<Address> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let (Some(open), Some(close)) = (raw.rfind('<'), raw.rfind('>'))
        && open < close
    {
        let email = raw[open + 1..close].trim().to_string();
        if email.is_empty() {
            return None;
        }
        let name = unquote(raw[..open].trim());
        return Some(Address {
            name: (!name.is_empty()).then_some(name),
            email,
        });
    }
    raw.contains('@').then(|| Address {
        name: None,
        email: raw.to_string(),
    })
}

fn unquote(s: &str) -> String {
    let s = s
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s);
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}
