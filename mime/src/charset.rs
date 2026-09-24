//! Reading bytes in the charset a message declares.

/// Text from `bytes`, read in the charset the part declares. The bytes win
/// over the label when they are valid UTF-8 and hold a character above
/// ASCII: plenty of mailers send UTF-8 under `iso-8859-1` or `us-ascii`,
/// and obeying the label is what turns "Direção" into "DireÃ§Ã£o". A part
/// that says UTF-8 but is not falls the other way, to windows-1252, rather
/// than showing a row of replacement characters.
pub fn decode_charset(bytes: &[u8], charset: Option<&str>) -> String {
    let declared = charset
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    if declared != encoding_rs::UTF_8
        && let Ok(text) = std::str::from_utf8(bytes)
        && !text.is_ascii()
    {
        return text.to_string();
    }
    let (text, _, replaced) = declared.decode(bytes);
    if replaced && declared == encoding_rs::UTF_8 {
        return encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned();
    }
    text.into_owned()
}

/// The `charset` parameter of a `Content-Type` value, without quotes.
pub fn charset_param(content_type: &str) -> Option<&str> {
    param(content_type, "charset")
}

/// One parameter of a header value, without its quotes.
fn param<'a>(value: &'a str, name: &str) -> Option<&'a str> {
    value.split(';').skip(1).find_map(|parameter| {
        let (key, value) = parameter.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().trim_matches('"'))
    })
}
