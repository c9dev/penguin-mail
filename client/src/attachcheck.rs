//! Whether a message promises a file, so the composer can ask about it
//! before the message goes out without one.
//!
//! The phrases come in English and Portuguese, and each one is matched
//! whole word by whole word: a house with an attached garage promises
//! nothing, and neither does a link that happens to spell one out. Only
//! what the writer typed counts, so the quoted original, a forwarded
//! message, and the signature are cut off first. Nothing here touches
//! GTK, so the whole decision runs on strings.

/// A promise of a file in what the writer typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promise {
    /// The sentence that made it, as the writer typed it, for the dialog
    /// to quote back.
    pub sentence: String,
    /// Whether the sentence names a file rather than something to look
    /// at, which decides whether a pasted image keeps the promise.
    pub names_a_file: bool,
}

/// Word sequences that promise a file. Bare "attached" is not among them,
/// because an attached garage is a building, not an attachment; it
/// promises only in the company below.
const PROMISES: &[&[&str]] = &[
    &["see", "attached"],
    &["find", "attached"],
    &["attached", "is"],
    &["attached", "are"],
    &["attached", "you"],
    &["attached", "please"],
    &["attached", "to", "this"],
    &["have", "attached"],
    &["ve", "attached"],
    &["i", "attach"],
    &["attaching"],
    &["enclosed"],
    &["anexo"],
    &["anexos"],
    &["anexei"],
    &["anexado"],
    &["anexada"],
    &["anexados"],
    &["anexadas"],
    &["anexando"],
];

/// Articles that may stand inside a phrase, so "find attached" still reads
/// "please find the attached invoice".
const ARTICLES: &[&str] = &[
    "a", "an", "the", "o", "os", "as", "um", "uma", "este", "esta",
];

/// Words for something that arrives as a file. A picture can be pasted
/// into the text instead, so these are what an inline image cannot settle.
const FILES: &[&str] = &[
    "file",
    "files",
    "doc",
    "docs",
    "document",
    "documents",
    "pdf",
    "pdfs",
    "spreadsheet",
    "spreadsheets",
    "ficheiro",
    "ficheiros",
    "arquivo",
    "arquivos",
    "documento",
    "documentos",
    "planilha",
    "planilhas",
];

/// Subjects that carry someone else's words, so the subject line says
/// nothing about what this writer promised.
const PASSED_ON: &[&str] = &["re:", "fwd:", "fw:", "enc:"];

/// The promise this message makes, if it makes one. The body is what the
/// composer holds as Markdown, quoted original and all.
pub fn promised(subject: &str, body: &str) -> Option<Promise> {
    let typed = typed(body);
    let subject = subject.trim();
    let lower = subject.to_lowercase();
    let subject = (!PASSED_ON.iter().any(|p| lower.starts_with(p))).then_some(subject);
    sentences(typed)
        .into_iter()
        .chain(subject)
        .find_map(|sentence| {
            let words = words(sentence);
            PROMISES
                .iter()
                .any(|phrase| says(&words, phrase))
                .then(|| Promise {
                    sentence: sentence.to_string(),
                    names_a_file: words.iter().any(|w| FILES.contains(&w.as_str())),
                })
        })
}

/// What the writer typed: everything above the quoted original, above a
/// forwarded message, and above the signature.
///
/// A forward carries its original in `Draft::forwarded` and `build_mime`
/// appends it, so it never reaches here; a draft that came back from Gmail
/// can still hold the header block in its text, and that ends the writer's
/// words too.
fn typed(body: &str) -> &str {
    let mut at = 0;
    let mut attribution = None;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('>') {
            return &body[..attribution.unwrap_or(at)];
        }
        if trimmed == "--" || forward_header(trimmed) {
            return &body[..at];
        }
        if !trimmed.is_empty() {
            attribution = trimmed.ends_with("wrote:").then_some(at);
        }
        at += line.len();
    }
    body
}

/// The line that opens a forwarded message, dashes and all.
fn forward_header(line: &str) -> bool {
    line.starts_with("---") && line.to_lowercase().contains("forwarded message")
}

/// The sentences of `text`. A full stop ends one when a space or the end
/// of the text follows, which leaves the dots inside a web address alone.
fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let ends = match c {
            '\n' => true,
            '.' | '!' | '?' => chars.peek().is_none_or(|(_, next)| next.is_whitespace()),
            _ => false,
        };
        if ends {
            let sentence = text[start..at + c.len_utf8()].trim();
            if !sentence.is_empty() {
                out.push(sentence);
            }
            start = at + c.len_utf8();
        }
    }
    let last = text[start..].trim();
    if !last.is_empty() {
        out.push(last);
    }
    out
}

/// The words of one sentence, lower case and without their accents. What
/// reads as a web address or a mail address drops out whole, so a link
/// that spells out a phrase does not make a promise.
fn words(sentence: &str) -> Vec<String> {
    sentence
        .split_whitespace()
        .filter(|token| !is_link(token))
        .flat_map(|token| token.split(|c: char| !c.is_alphanumeric()))
        .filter(|word| !word.is_empty())
        .map(fold)
        .collect()
}

fn is_link(token: &str) -> bool {
    let lower = token.to_lowercase();
    lower.contains("://")
        || lower.contains("www.")
        || lower.contains('@')
        || (lower.contains('/') && lower.contains('.'))
}

fn fold(word: &str) -> String {
    word.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

/// Whether `words` says `phrase` anywhere.
fn says(words: &[String], phrase: &[&str]) -> bool {
    (0..words.len()).any(|start| opens_with(&words[start..], phrase))
}

/// Whether `words` opens with `phrase`, articles aside.
fn opens_with(words: &[String], phrase: &[&str]) -> bool {
    let mut at = 0;
    for (index, word) in phrase.iter().enumerate() {
        // An article stands between a phrase's words, never before it, so
        // "the attached" is still a promise and "a see" is not a start.
        if index > 0 {
            while words
                .get(at)
                .is_some_and(|w| ARTICLES.contains(&w.as_str()))
            {
                at += 1;
            }
        }
        if words.get(at).map(String::as_str) != Some(*word) {
            return false;
        }
        at += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sentence(subject: &str, body: &str) -> Option<String> {
        promised(subject, body).map(|p| p.sentence)
    }

    #[test]
    fn the_english_phrases_all_promise_a_file() {
        for body in [
            "Hi Ann, see attached.",
            "Please find attached the invoice.",
            "Please find the attached invoice.",
            "I attach the invoice.",
            "I have attached the invoice.",
            "I've attached the invoice.",
            "I'm attaching the invoice.",
            "Attached is the invoice.",
            "Attached are the invoices.",
            "Attached you will find the invoice.",
            "The invoice is enclosed.",
        ] {
            assert!(promised("Invoice", body).is_some(), "{body}");
        }
    }

    #[test]
    fn the_portuguese_phrases_all_promise_a_file() {
        for body in [
            "Olá Ana, anexo a fatura.",
            "A fatura vai em anexo.",
            "Segue anexo o contrato.",
            "Segue em anexo o contrato.",
            "O contrato está anexado.",
            "Anexei o contrato.",
        ] {
            assert!(promised("Fatura", body).is_some(), "{body}");
        }
    }

    #[test]
    fn capitals_and_accents_read_the_same_as_plain_letters() {
        assert_eq!(
            sentence("Fatura", "Olá! Segue em ANEXO a fatura de março."),
            Some("Segue em ANEXO a fatura de março.".to_string())
        );
        assert!(promised("Invoice", "SEE ATTACHED").is_some());
    }

    #[test]
    fn a_fastened_thing_promises_nothing() {
        for body in [
            "The bookshelf is attached to the wall.",
            "The house has an attached garage.",
            "The garage stays unattached for now.",
            "Ann is attached to that old car.",
        ] {
            assert_eq!(promised("Bookshelf", body), None, "{body}");
        }
    }

    #[test]
    fn a_link_that_spells_out_a_phrase_promises_nothing() {
        assert_eq!(
            promised(
                "Reading",
                "The rules are at https://example.com/see-attached/anexo.html, have a look."
            ),
            None
        );
    }

    #[test]
    fn a_quoted_reply_is_not_what_the_writer_wrote() {
        let body = "Thanks, that works.\n\nOn Monday, Ann wrote:\n> Please find attached the invoice.\n> Ann";
        assert_eq!(promised("Re: Invoice", body), None);
    }

    #[test]
    fn a_forwarded_message_is_not_what_the_writer_wrote() {
        let body = "Passing this on.\n\n---------- Forwarded message ----------\nFrom: Ann <ann@example.com>\nSubject: Invoice\n\nPlease find attached the invoice.";
        assert_eq!(promised("Fwd: Invoice", body), None);
    }

    #[test]
    fn a_signature_is_not_what_the_writer_wrote() {
        let body =
            "Thanks for the call.\n\n-- \nDana Reyes\nAnexo Studio\nsee-attached.example.com";
        assert_eq!(promised("Our call", body), None);
    }

    #[test]
    fn a_subject_promises_on_its_own() {
        assert_eq!(
            sentence("Segue em anexo o contrato", "Obrigado!"),
            Some("Segue em anexo o contrato".to_string())
        );
    }

    #[test]
    fn a_reply_or_forward_subject_belongs_to_whoever_wrote_it() {
        assert_eq!(promised("Re: Invoice attached is ready", "Thanks!"), None);
        assert_eq!(promised("Fwd: Segue em anexo", "Passing this on."), None);
    }

    #[test]
    fn the_promise_quotes_the_sentence_that_made_it() {
        let body = "Hi Ann.\n\nPlease find attached the invoice for March. Let me know.\n\nDana";
        assert_eq!(
            sentence("March", body),
            Some("Please find attached the invoice for March.".to_string())
        );
    }

    #[test]
    fn a_promised_file_is_told_from_a_promised_picture() {
        let named = promised("Invoice", "Please find the attached file.").expect("a promise");
        assert!(named.names_a_file);
        let shown = promised("Holiday", "See attached, the sea was warm.").expect("a promise");
        assert!(!shown.names_a_file);
        let documento = promised("Contrato", "Segue em anexo o documento.").expect("a promise");
        assert!(documento.names_a_file);
    }

    #[test]
    fn an_empty_message_promises_nothing() {
        assert_eq!(promised("", ""), None);
        assert_eq!(promised("Lunch", "Noon works for me."), None);
    }
}
