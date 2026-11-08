//! Signatures written in HTML.
//!
//! A signature laid out in a table, with a logo and colored rules, cannot
//! be written in Markdown. When the signature someone saved is HTML, the
//! composer shows its text where the signature goes, and the message that
//! goes out carries the HTML in that place instead. That holds only while
//! the text stays as the composer put it: a signature the writer edited is
//! theirs, and goes out as they left it.
//!
//! Pictures the HTML carries as `data:` URIs or local files go out as parts
//! of the message, shown through `cid:`, since Gmail drops `data:` pictures
//! and a file path means nothing on the reader's computer.

use std::collections::HashMap;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use super::{
    OutgoingAttachment, html_to_text, plain_as_markdown, quote_after_signature,
    rich_signature_change, signature_starts_at,
};
use crate::richtext::{self, Block, BlockKind, RichBody, Span};

/// An HTML signature for one sending address, its tokens filled in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HtmlSignature {
    /// The signature as the composer shows it: the text of the HTML,
    /// written as Markdown that reads back as that same text.
    pub shown: String,
    /// What goes out in its place.
    pub html: String,
}

/// Stands in for the signature while the rest of the body becomes HTML. It
/// holds no character Markdown or HTML would read as a mark.
const MARK: &str = "PenguinMailHtmlSignatureGoesHere";

/// Whether `template` is HTML rather than Markdown: it opens with a tag
/// and closes one. A Markdown signature that starts with `<email>` is not.
pub fn is_html(template: &str) -> bool {
    let trimmed = template.trim_start();
    let Some(rest) = trimmed.strip_prefix('<') else {
        return false;
    };
    if trimmed.starts_with("<email>") || trimmed.starts_with("<domain>") {
        return false;
    }
    rest.starts_with(|c: char| c.is_ascii_alphabetic() || c == '!') && trimmed.contains("</")
}

/// Whether the HTML signature `template` names a picture on this computer,
/// which every message sent with it would then carry. The assistant may not
/// save such a signature: a message it read could ask for one that sends a
/// private picture out with all mail, and the HTML it shows for approval
/// does not make that plain.
pub fn names_local_file(template: &str) -> bool {
    is_html(template) && sources(template).any(|source| local_path(source).is_some())
}

/// The quoted values of the `src` attributes in `html`.
fn sources(html: &str) -> impl Iterator<Item = &str> {
    let lower = html.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = lower[at..].find("src=") {
        let start = at + offset + "src=".len();
        at = start;
        let Some(quote) = html[start..]
            .chars()
            .next()
            .filter(|c| *c == '"' || *c == '\'')
        else {
            continue;
        };
        let Some(len) = html[start + 1..].find(quote) else {
            break;
        };
        found.push(&html[start + 1..start + 1 + len]);
        at = start + 1 + len;
    }
    found.into_iter()
}

/// The file `source` names, when it is a `file:` URL or an absolute path.
fn local_path(source: &str) -> Option<std::path::PathBuf> {
    match url::Url::parse(source) {
        Ok(url) if url.scheme() == "file" => url.to_file_path().ok(),
        Ok(_) => None,
        Err(_) if source.starts_with('/') => Some(std::path::PathBuf::from(source)),
        Err(_) => None,
    }
}

/// The signature `template` makes for `email`, when it is HTML.
///
/// `<email>` and `<domain>` are filled in as they are in Markdown. A whole
/// document, such as the `.htm` file another mail program saves, gives up
/// everything outside its `<body>`.
pub fn expand(template: &str, email: &str) -> Option<HtmlSignature> {
    if !is_html(template) {
        return None;
    }
    let domain = email.rsplit_once('@').map_or("", |(_, domain)| domain);
    let filled = template
        .replace("<email>", &richtext::escape(email))
        .replace("<domain>", &richtext::escape(domain));
    let html = body_of(&filled).trim().to_string();
    let shown = plain_as_markdown(text_of(&html).trim())
        .trim_end()
        .to_string();
    Some(HtmlSignature { shown, html })
}

/// The text of a signature, each table cell on a line of its own. Laid out
/// side by side, a name and a phone number read as one word otherwise.
fn text_of(html: &str) -> String {
    let mut cells = String::with_capacity(html.len());
    let lower = html.to_ascii_lowercase();
    let mut at = 0;
    while let Some(found) = lower[at..].find("</td") {
        cells.push_str(&html[at..at + found]);
        cells.push_str("<br>");
        at += found;
        let tag_end = lower[at..]
            .find('>')
            .map_or(lower.len(), |end| at + end + 1);
        cells.push_str(&html[at..tag_end]);
        at = tag_end;
    }
    cells.push_str(&html[at..]);
    html_to_text(&cells)
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// What sits between `<body …>` and `</body>`, or all of `html` when it
/// has no body tag.
fn body_of(html: &str) -> &str {
    // ASCII lowercase keeps every byte where it was, so the offsets found
    // in it hold for `html`.
    let lower = html.to_ascii_lowercase();
    let Some(open) = lower.find("<body") else {
        return html;
    };
    let Some(open_end) = lower[open..].find('>') else {
        return html;
    };
    let start = open + open_end + 1;
    let end = lower[start..]
        .find("</body")
        .map_or(html.len(), |end| start + end);
    &html[start..end]
}

/// The body with the signature `shown` swapped for [`MARK`], in whichever
/// form decides what goes out. `None` when the writer changed or removed
/// the signature, which then goes out as they left it.
pub(super) fn marked(
    markdown: &str,
    rich: Option<&RichBody>,
    shown: &str,
) -> Option<(String, Option<RichBody>)> {
    if shown.trim().is_empty() {
        return None;
    }
    match rich {
        Some(rich) => {
            let (first, len, _) = rich_signature_change(rich.blocks.iter().cloned(), shown, shown)?;
            let mut marked = rich.clone();
            marked.blocks.splice(
                first..first + len,
                [
                    Block::default(),
                    Block::new(BlockKind::Paragraph, vec![Span::plain(MARK)]),
                    Block::default(),
                ],
            );
            Some((markdown.to_string(), Some(marked)))
        }
        None => {
            let end = quote_after_signature(markdown, shown).unwrap_or(markdown.len());
            let head = markdown[..end].trim_end();
            let start = signature_starts_at(head, shown)?;
            let rest = &markdown[end..];
            let marked = match rest.is_empty() {
                true => format!("{}\n\n{MARK}\n", &markdown[..start]),
                false => format!("{}\n\n{MARK}\n\n{rest}", &markdown[..start]),
            };
            Some((marked, None))
        }
    }
}

/// `html` with the signature where [`MARK`] stood. The paragraph the mark
/// sat in goes too, since a table inside a paragraph is not valid HTML and
/// readers close the paragraph early in ways that move the layout.
pub(super) fn put(html: &str, signature: &str) -> String {
    let paragraph = format!("<p style=\"{}\">{MARK}</p>", richtext::PARAGRAPH);
    match html.contains(&paragraph) {
        true => html.replacen(&paragraph, signature, 1),
        false => html.replacen(MARK, signature, 1),
    }
}

/// `html` with each picture held in a `data:` URI or a local file swapped
/// for a `cid:` reference, and the pictures as parts to send with it. A
/// picture that cannot be read keeps its address, so the rest still goes.
pub(super) fn with_inline_pictures(html: &str) -> (String, Vec<OutgoingAttachment>) {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut pictures: Vec<OutgoingAttachment> = Vec::new();
    let mut known: HashMap<String, String> = HashMap::new();
    let mut at = 0;
    while let Some(found) = lower[at..].find("src=") {
        let value_at = at + found + "src=".len();
        let Some(quote) = html[value_at..]
            .chars()
            .next()
            .filter(|c| *c == '"' || *c == '\'')
        else {
            out.push_str(&html[at..value_at]);
            at = value_at;
            continue;
        };
        let start = value_at + 1;
        let Some(len) = html[start..].find(quote) else {
            break;
        };
        let source = &html[start..start + len];
        out.push_str(&html[at..start]);
        let cid = match known.get(source) {
            Some(cid) => Some(cid.clone()),
            None => picture(source, pictures.len() + 1).map(|picture| {
                let cid = picture
                    .content_id
                    .clone()
                    .expect("a signature picture has a content id");
                pictures.push(picture);
                known.insert(source.to_string(), cid.clone());
                cid
            }),
        };
        match cid {
            Some(cid) => out.push_str(&format!("cid:{cid}")),
            None => out.push_str(source),
        }
        at = start + len;
    }
    out.push_str(&html[at..]);
    (out, pictures)
}

/// The picture at `source`, when it is one the message has to carry.
fn picture(source: &str, number: usize) -> Option<OutgoingAttachment> {
    let (data, mime_type, filename) = match source.get(..5) {
        Some(scheme) if scheme.eq_ignore_ascii_case("data:") => {
            let (header, payload) = source[5..].split_once(',')?;
            let (mime_type, encoding) = header.split_once(';')?;
            if !encoding.eq_ignore_ascii_case("base64") || !mime_type.starts_with("image/") {
                return None;
            }
            let payload: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
            let data = base64::engine::general_purpose::STANDARD
                .decode(payload)
                .ok()?;
            let extension = mime_type
                .trim_start_matches("image/")
                .replace("svg+xml", "svg");
            (
                data,
                mime_type.to_string(),
                format!("signature{number}.{extension}"),
            )
        }
        _ => {
            let path = local_path(source)?;
            let extension = path.extension()?.to_str()?.to_ascii_lowercase();
            let mime_type = match extension.as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                "svg" => "image/svg+xml",
                "bmp" => "image/bmp",
                _ => return None,
            };
            let data = std::fs::read(&path).ok()?;
            let filename = path.file_name()?.to_str()?.to_string();
            (data, mime_type.to_string(), filename)
        }
    };
    Some(OutgoingAttachment {
        filename,
        mime_type,
        data: data.into(),
        content_id: Some(format!(
            "signature{number}.{:016x}@penguin-mail",
            rand::random::<u64>()
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARD: &str = "<table><tr><td><b>David Friedrich</b><br>Project Manager</td>\
        <td style=\"border-left:1px solid #e30613\"><a href=\"mailto:<email>\"><email></a></td></tr></table>";

    #[test]
    fn html_is_told_apart_from_markdown() {
        assert!(is_html(CARD));
        assert!(is_html(
            "<!DOCTYPE html><html><body><p>Dana</p></body></html>"
        ));
        assert!(!is_html("<email>\n**Sales**"));
        assert!(!is_html("**Dana**\n<https://example.com>"));
        assert!(!is_html("Dana"));
        assert!(!is_html("<b>"));
    }

    #[test]
    fn a_saved_document_gives_up_everything_outside_its_body() {
        let signature = expand(
            "<html><head><style>p{color:red}</style></head><BODY lang=de><p>Dana</p></BODY></html>",
            "dana@example.com",
        )
        .expect("an HTML signature");
        assert_eq!(signature.html, "<p>Dana</p>");
        assert_eq!(signature.shown, "Dana");
    }

    #[test]
    fn the_sender_tokens_are_filled_in() {
        let signature = expand(CARD, "dana@example.com").expect("an HTML signature");
        assert!(signature.html.contains("href=\"mailto:dana@example.com\""));
        assert!(signature.html.contains(">dana@example.com</a>"));
        assert!(!signature.html.contains("<email>"));
        // Each cell reads as its own line.
        assert_eq!(
            signature.shown.replace('\\', ""),
            "David Friedrich\nProject Manager\ndana@example.com"
        );
    }

    #[test]
    fn data_pictures_become_parts_and_repeats_share_one() {
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG not really");
        let html = format!(
            "<img src=\"data:image/png;base64,{png}\"><img src='data:image/png;base64,{png}'>\
             <img src=\"https://example.com/logo.png\">"
        );
        let (out, pictures) = with_inline_pictures(&html);
        assert_eq!(pictures.len(), 1);
        let cid = pictures[0].content_id.clone().unwrap();
        assert_eq!(out.matches(&format!("cid:{cid}")).count(), 2);
        assert!(out.contains("src=\"https://example.com/logo.png\""));
        assert_eq!(&*pictures[0].data, b"\x89PNG not really");
        assert_eq!(pictures[0].mime_type, "image/png");
    }

    fn draft_with(body: &str, rich: bool) -> super::super::Draft {
        let from = mailrs_domain::Address {
            name: None,
            email: "dana@example.com".into(),
        };
        let mut draft = super::super::Draft::new(1, from);
        let template = format!(
            "{CARD}<img src=\"data:image/png;base64,{}\">",
            base64::engine::general_purpose::STANDARD.encode(b"logo")
        );
        let signature = expand(&template, "dana@example.com").unwrap();
        draft.markdown = super::super::with_signature(body, &signature.shown);
        if rich {
            draft.rich = Some(RichBody::from_markdown(&draft.markdown));
        }
        draft.html_signature = Some(signature);
        draft
    }

    fn sent(draft: &super::super::Draft) -> String {
        let raw = super::super::build_mime(draft, 0, "id@example.com").unwrap();
        String::from_utf8(raw).unwrap()
    }

    #[test]
    fn an_untouched_signature_goes_out_as_its_html() {
        for rich in [true, false] {
            let mime = sent(&draft_with("Hello Ann", rich));
            assert!(mime.contains("#e30613"), "{mime}");
            assert!(!mime.contains(MARK), "{mime}");
            assert!(mime.contains("multipart/related"), "{mime}");
            assert!(mime.contains("Content-ID: <signature1."), "{mime}");
            assert!(mime.contains("Hello Ann"), "{mime}");
            // The text part keeps the words.
            assert!(mime.contains("David Friedrich"), "{mime}");
        }
    }

    #[test]
    fn a_signature_the_writer_changed_goes_out_as_they_left_it() {
        for rich in [true, false] {
            let mut draft = draft_with("Hello Ann", rich);
            draft.markdown = draft.markdown.replace("Project Manager", "Boss");
            if rich {
                draft.rich = Some(RichBody::from_markdown(&draft.markdown));
            }
            let mime = sent(&draft);
            assert!(!mime.contains("#e30613"), "{mime}");
            assert!(!mime.contains("multipart/related"), "{mime}");
            assert!(mime.contains("Boss"), "{mime}");
        }
    }

    #[test]
    fn a_draft_kept_in_drafts_keeps_the_text() {
        let draft = draft_with("Hello Ann", true);
        let raw = super::super::build_saved_draft(&draft, 0, "id@example.com").unwrap();
        let mime = String::from_utf8(raw).unwrap();
        assert!(!mime.contains("#e30613"), "{mime}");
        assert!(mime.contains("David Friedrich"), "{mime}");
    }

    #[test]
    fn a_local_picture_goes_with_the_message() {
        let dir = std::env::temp_dir().join(format!("penguin-sig-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let logo = dir.join("logo.PNG");
        std::fs::write(&logo, b"logo").unwrap();
        let url = url::Url::from_file_path(&logo).unwrap();
        let html = format!(
            "<img src=\"{url}\"><img src=\"{}\"><img src=\"{}/missing.png\">",
            logo.display(),
            dir.display()
        );
        let (out, pictures) = with_inline_pictures(&html);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(pictures.len(), 2);
        assert_eq!(pictures[0].filename, "logo.PNG");
        assert_eq!(&*pictures[1].data, b"logo");
        assert!(out.contains("missing.png"), "{out}");
    }

    #[test]
    fn only_a_picture_on_this_computer_counts_as_a_local_file() {
        assert!(names_local_file(
            "<div><img src=\"/home/dana/Pictures/id.png\"></div>"
        ));
        assert!(names_local_file(
            "<div><IMG SRC='file:///home/dana/logo.png'></div>"
        ));
        assert!(!names_local_file(
            "<div><img src=\"https://example.com/logo.png\"><img src=\"data:image/png;base64,bG9nbw==\"></div>"
        ));
        assert!(!names_local_file("**Dana**\n/home/dana/logo.png"));
    }
}
