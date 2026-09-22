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
        "no longer subscribed",
        "you will no longer receive",
        "you will not receive",
        "you won't receive",
        "your subscription has been cancelled",
        "your subscription has been canceled",
    ],
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
    email_label: &[
        "endereço de email",
        "endereço de correio",
        "correio eletrónico",
    ],
};

const LANGUAGES: [&Words; 2] = [&EN, &PT];

/// Whether `text` holds one of `list` as a whole word or phrase. Both
/// sides are folded first, so "Opt-Out" matches "opt out" and
/// "Cancelar Subscrição" matches "cancelar subscricao", while
/// "unsubscribed" does not match "unsubscribe".
pub fn reads_as(text: &str, list: &[&str]) -> bool {
    let haystack = format!(" {} ", fold(text));
    list.iter()
        .any(|entry| haystack.contains(&format!(" {} ", fold(entry))))
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

/// A checkbox or radio that reads as every list at once.
pub fn means_all(text: &str) -> bool {
    LANGUAGES.iter().any(|words| reads_as(text, words.all))
}

/// Text that reads as the address being off the list already.
pub fn already_off(text: &str) -> bool {
    LANGUAGES.iter().any(|words| reads_as(text, words.done))
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
