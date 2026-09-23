//! What the words on an unsubscribe page mean, in the two languages the
//! owner's mail comes in.
//!
//! These lists stay out of the po files, and a translator must not be
//! sent them. `gettext` turns what Penguin Mail says into the language
//! the person reads; these match what a sender's page says, which is the
//! sender's language and no business of the interface's. A person
//! reading the app in English gets Portuguese newsletters, so the app
//! carries every language it can read at once and a new one is another
//! table here.

/// One language's words.
pub struct Words {
    /// A button or link that reads as leaving a list.
    pub leave: &'static [&'static str],
    /// A button that reads as going ahead with what the form asks.
    pub confirm: &'static [&'static str],
    /// A checkbox or radio that reads as every list at once, rather than
    /// one topic among several.
    pub all: &'static [&'static str],
    /// Text that reads as the address being off the list already.
    pub done: &'static [&'static str],
    /// The same said in one word, "Unsubscribed". It counts only as a
    /// title or a line of its own, since inside a sentence it is as
    /// likely to ask what to unsubscribe from.
    pub done_alone: &'static [&'static str],
    /// A checkbox or radio that gives a reason for leaving, on a form
    /// that asks why before it lets go.
    pub reason: &'static [&'static str],
    /// The plainest of those reasons: the person no longer wants the
    /// mail. It is the one box such a form gets ticked.
    pub unwanted: &'static [&'static str],
    /// A reason with nothing specific in it, which often opens a box
    /// asking what the reason is.
    pub other: &'static [&'static str],
    /// A box that reports the sender for spam or fraud rather than
    /// giving a reason. The app never files a report on its own.
    pub report: &'static [&'static str],
    /// A field label that asks for an email address.
    pub email_label: &'static [&'static str],
}

pub const EN: Words = Words {
    leave: &[
        "unsubscribe",
        "unsubscribe me",
        "opt out",
        "manage preferences",
        "manage your preferences",
        "email preferences",
        "manage subscriptions",
        "remove me",
        "stop receiving",
        "leave this list",
    ],
    confirm: &[
        "confirm",
        "submit",
        "yes",
        "save",
        "apply",
        "update preferences",
    ],
    all: &["all", "every", "everything"],
    done: &[
        "you have been unsubscribed",
        "you've been unsubscribed",
        "you are unsubscribed",
        "you're unsubscribed",
        "successfully unsubscribed",
        "unsubscribe successful",
        "you have been removed",
        "you've been removed",
        "you were removed",
        "removed from the list",
        "removed from our list",
        "removed you from",
        "email address has been removed",
        "your email has been removed",
        "your address has been removed",
        "no longer subscribed",
        "you will no longer receive",
        "you will not receive",
        "you won't receive",
        "your subscription has been cancelled",
        "your subscription has been canceled",
    ],
    reason: &[
        "do not want to receive",
        "don't want to receive",
        "no longer want",
        "no longer interested",
        "too many emails",
        "too often",
        "not what i subscribed to",
        "never signed up",
        "not relevant",
        "other",
    ],
    unwanted: &[
        "do not want to receive",
        "don't want to receive",
        "no longer want",
        "no longer interested",
    ],
    other: &["other", "other reason"],
    report: &["spam", "fraud", "fraudulent", "phishing", "abuse"],
    done_alone: &["unsubscribed", "opted out"],
    email_label: &["email", "e mail"],
};

pub const PT: Words = Words {
    leave: &[
        "cancelar subscrição",
        "cancelar subscrições",
        "cancelar a subscrição",
        "cancelar todas as subscrições",
        "anular subscrição",
        "anular subscrições",
        "anular todas as subscrições",
        "deixar de receber",
        "gerir preferências",
        "preferências de email",
        "remover me",
    ],
    confirm: &["confirmar", "sim", "guardar", "submeter", "aplicar"],
    all: &["todos", "todas", "tudo"],
    done: &[
        "subscrição cancelada",
        "subscrição anulada",
        "a sua subscrição foi cancelada",
        "cancelámos a sua subscrição",
        "foi removido",
        "foi removida",
        "deixou de receber",
        "deixará de receber",
        "já não vai receber",
        "não vai receber",
    ],
    reason: &[
        "não quero receber",
        "já não tenho interesse",
        "demasiados emails",
        "não me inscrevi",
        "não é relevante",
        "outro",
        "outra razão",
        "outro motivo",
    ],
    unwanted: &["não quero receber", "já não tenho interesse"],
    other: &["outro", "outra razão", "outro motivo"],
    report: &["spam", "fraude", "fraudulento", "fraudulenta", "denunciar"],
    done_alone: &["cancelado", "cancelada"],
    email_label: &[
        "endereço de email",
        "endereço de correio",
        "correio eletrónico",
    ],
};

const LANGUAGES: [&Words; 2] = [&EN, &PT];

/// Words a page adds to a sentence without changing what it says, as in
/// "You have been successfully removed" or "You are now unsubscribed".
/// The page's text is also read with them left out, so one phrase covers
/// every such way of saying it. The phrases themselves keep them: "successfully
/// unsubscribed" without its first word would be "unsubscribed", which a
/// page asking what to unsubscribe from says too.
const FILLER: [&str; 2] = ["successfully", "now"];

/// Whether `text` holds one of `list` as a whole word or phrase, either as
/// the page wrote it or with the filler words left out. Both sides are
/// folded first, so "Opt-Out" matches "opt out" and "Cancelar Subscrição"
/// matches "cancelar subscricao", while "unsubscribed" does not match
/// "unsubscribe".
pub fn reads_as(text: &str, list: &[&str]) -> bool {
    let folded = fold(text);
    let as_written = format!(" {folded} ");
    let without_filler = format!(" {} ", without(&folded));
    list.iter().any(|entry| {
        let entry = format!(" {} ", fold(entry).trim());
        as_written.contains(&entry) || without_filler.contains(&entry)
    })
}

/// Folded `text` with the filler words left out.
fn without(folded: &str) -> String {
    folded
        .split_whitespace()
        .filter(|word| !FILLER.contains(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A button or link that reads as leaving a list.
pub fn leaves(text: &str) -> bool {
    LANGUAGES.iter().any(|words| reads_as(text, words.leave))
}

/// A button worth pressing: one that reads as leaving, or as agreeing to
/// what the form asks.
pub fn presses(text: &str) -> bool {
    LANGUAGES
        .iter()
        .any(|words| reads_as(text, words.leave) || reads_as(text, words.confirm))
}

/// A checkbox or radio that reads as leaving every list at once,
/// "Unsubscribe from all emails". On a preferences page "all" alone can
/// mean the opposite, as in "Send me all emails", so the label has to
/// read as leaving too.
pub fn means_all(text: &str) -> bool {
    leaves(text) && LANGUAGES.iter().any(|words| reads_as(text, words.all))
}

/// A box that gives a reason for leaving, or reports the sender.
pub fn gives_a_reason(text: &str) -> bool {
    LANGUAGES
        .iter()
        .any(|words| reads_as(text, words.reason) || reads_as(text, words.report))
}

/// A reason that says the person no longer wants the mail.
pub fn unwanted(text: &str) -> bool {
    LANGUAGES.iter().any(|words| reads_as(text, words.unwanted))
}

/// A reason that names nothing, "Other".
pub fn other(text: &str) -> bool {
    LANGUAGES.iter().any(|words| reads_as(text, words.other))
}

/// A box that reports the sender for spam or fraud.
pub fn reports(text: &str) -> bool {
    LANGUAGES.iter().any(|words| reads_as(text, words.report))
}

/// Text that reads as the address being off the list already.
pub fn already_off(text: &str) -> bool {
    LANGUAGES.iter().any(|words| reads_as(text, words.done))
}

/// Whether `text` says nothing but that the address is off the list, as
/// a title or a heading does: "Unsubscribed!", or "Successfully
/// unsubscribed".
pub fn off_alone(text: &str) -> bool {
    let said = without(&fold(text));
    LANGUAGES.iter().any(|words| {
        words
            .done_alone
            .iter()
            .any(|entry| fold(entry) == said)
    })
}

/// Whether two labels say the same once folded, so "Unsubscribed" on a
/// line of the page is known to be the name of a radio beside it.
pub fn same(one: &str, other: &str) -> bool {
    fold(one) == fold(other)
}

/// A field label that asks for an email address.
pub fn asks_for_email(text: &str) -> bool {
    LANGUAGES
        .iter()
        .any(|words| reads_as(text, words.email_label))
}

/// The text in lower case, with the accents of Portuguese off, every run
/// of punctuation turned into one space, and the ends trimmed. A page
/// that writes "Cancelar subscricao" without the cedilla says the same
/// thing as one that spells it.
fn fold(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    let mut space = true;
    for letter in text.chars().flat_map(char::to_lowercase) {
        let plain = match letter {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            other => other,
        };
        if plain.is_alphanumeric() {
            folded.push(plain);
            space = false;
        } else if !space {
            folded.push(' ');
            space = true;
        }
    }
    if folded.ends_with(' ') {
        folded.pop();
    }
    folded
}
