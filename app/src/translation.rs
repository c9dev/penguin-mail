//! Reading a message in the language the interface is in.
//!
//! Three things happen here, none of them touching GTK. The words of a
//! message say which language it is in, and [`read_language`] compares
//! that with the language the interface speaks. [`Prose`] cuts the words
//! out of the body and puts them back where they came from, so the markup
//! a sender wrote is the markup the reader sees: a bank statement stays a
//! table and only its words travel. And [`destination`] says where those
//! words go, in the words Preferences uses, because with Anthropic or a
//! Claude subscription they leave this computer.
//!
//! The model never writes markup. Every piece it sends back goes into the
//! page as text, escaped, and the rebuilt body is cleaned again by
//! [`crate::sanitize`] before the WebView sees it.

use std::sync::Arc;

use mailrs_ai::{AgentEvent, Conversation, NoTools, ProviderConfig};
use mailrs_domain::MessageBody;

use crate::assistant;
use crate::render::escape;
use crate::settings::{AiSettings, Feature};
use mailrs_domain::translate::{fill, gettext};

/// How much of a message is read to tell its language. A paragraph settles
/// it, and a newsletter's thousandth word says nothing the first hundred
/// did not.
const SAMPLE_CHARS: usize = 1_200;

/// How many words a message needs before counting them means anything.
const ENOUGH_WORDS: usize = 12;

/// How many words it needs before the absence of the interface's own words
/// is worth anything on its own.
const ENOUGH_FOR_ABSENCE: usize = 25;

/// How many of a language's words have to be there before it is named, as
/// a percentage of the words in the sample.
const NAMED_PERCENT: usize = 10;

/// The fewest words of one language that will name it. A short note in
/// French has three or four of them and no more.
const NAMED_WORDS: usize = 4;

/// Below this percentage, the interface's own words are missing rather
/// than thin on the ground, and the message is in something else.
const ABSENT_PERCENT: usize = 5;

/// How much prose goes into one request. A long newsletter would cost
/// real tokens; past this many characters the rest of the message keeps
/// the words it arrived in, and the card says so.
pub const MAX_CHARS: usize = 10_000;

/// A language the app can tell apart, or at least name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language {
    /// The language part of a locale, such as `pt`.
    pub code: &'static str,
    /// What the model is asked to translate into. It goes into a prompt
    /// and never on screen, so it stays in English.
    pub english: &'static str,
    /// The writing the language is in. Two languages in different scripts
    /// are told apart by the letters alone.
    script: Script,
    /// The commonest words that carry no meaning of their own, which is
    /// what a language gives itself away by. Empty for a language known
    /// only by its script.
    words: &'static [&'static str],
}

impl Language {
    /// The language's name, in the language the interface is in.
    pub fn name(&self) -> String {
        match self.code {
            "pt" => gettext("Portuguese"),
            "es" => gettext("Spanish"),
            "fr" => gettext("French"),
            "de" => gettext("German"),
            "it" => gettext("Italian"),
            "nl" => gettext("Dutch"),
            "el" => gettext("Greek"),
            "he" => gettext("Hebrew"),
            "ja" => gettext("Japanese"),
            "ko" => gettext("Korean"),
            "zh" => gettext("Chinese"),
            _ => gettext("English"),
        }
    }
}

pub const ENGLISH: Language = Language {
    code: "en",
    english: "English",
    script: Script::Latin,
    words: &[
        "a", "an", "and", "are", "as", "at", "be", "been", "but", "by", "can", "do", "for", "from",
        "has", "have", "here", "i", "if", "in", "is", "it", "me", "not", "of", "on", "or", "our",
        "please", "should", "thanks", "that", "the", "their", "there", "they", "this", "to", "was",
        "we", "were", "what", "will", "with", "would", "you", "your",
    ],
};

/// The only Portuguese the app is translated into is the European one, so
/// that is what the model is asked for. Asked for Portuguese alone it
/// writes Brazilian as often as not.
pub const PORTUGUESE: Language = Language {
    code: "pt",
    english: "European Portuguese",
    script: Script::Latin,
    words: &[
        "ao", "aos", "as", "com", "como", "da", "das", "de", "do", "dos", "em", "está", "este",
        "foi", "isso", "já", "mais", "mas", "meu", "muito", "na", "não", "nas", "no", "nos",
        "obrigado", "os", "ou", "para", "pela", "pelo", "por", "pode", "qual", "quando", "que",
        "são", "se", "sem", "ser", "seu", "sua", "também", "tem", "um", "uma", "você", "é",
    ],
};

pub const SPANISH: Language = Language {
    code: "es",
    english: "Spanish",
    script: Script::Latin,
    words: &[
        "al", "como", "con", "cuando", "de", "del", "el", "ella", "ellos", "en", "es", "esta",
        "este", "gracias", "hay", "la", "las", "los", "más", "muy", "no", "nos", "o", "para",
        "pero", "por", "puede", "que", "se", "si", "sin", "son", "su", "sus", "también", "tiene",
        "un", "una", "usted", "y", "ya",
    ],
};

pub const FRENCH: Language = Language {
    code: "fr",
    english: "French",
    script: Script::Latin,
    words: &[
        "au", "aux", "avec", "bonjour", "ce", "cette", "dans", "de", "des", "du", "elle", "en",
        "est", "et", "il", "je", "la", "le", "les", "mais", "merci", "ne", "nous", "ou", "par",
        "pas", "plus", "pour", "que", "qui", "sa", "sont", "sur", "un", "une", "vos", "votre",
        "vous",
    ],
};

pub const GERMAN: Language = Language {
    code: "de",
    english: "German",
    script: Script::Latin,
    words: &[
        "aber", "als", "auch", "auf", "bei", "bitte", "danke", "das", "dem", "den", "der", "die",
        "ein", "eine", "einen", "für", "haben", "ich", "ihre", "ist", "mit", "nach", "nicht",
        "oder", "sehr", "sich", "sie", "sind", "über", "und", "uns", "von", "werden", "wir",
        "wird", "zu",
    ],
};

pub const ITALIAN: Language = Language {
    code: "it",
    english: "Italian",
    script: Script::Latin,
    words: &[
        "alla", "anche", "che", "ci", "come", "con", "da", "del", "della", "di", "e", "gli",
        "grazie", "i", "il", "in", "la", "le", "lo", "ma", "non", "per", "più", "questa", "questo",
        "si", "sono", "su", "tutti", "un", "una", "è",
    ],
};

pub const DUTCH: Language = Language {
    code: "nl",
    english: "Dutch",
    script: Script::Latin,
    words: &[
        "aan", "als", "bij", "dank", "dat", "de", "die", "een", "en", "er", "het", "hij", "ik",
        "is", "je", "maar", "met", "naar", "niet", "of", "ons", "ook", "op", "over", "te", "uw",
        "van", "voor", "we", "wij", "wordt", "worden", "zeer", "zijn",
    ],
};

/// The languages whose words the app counts. A language outside this list
/// is named by its script or not at all.
const KNOWN: [Language; 7] = [ENGLISH, PORTUGUESE, SPANISH, FRENCH, GERMAN, ITALIAN, DUTCH];

/// Languages a script names on its own. Cyrillic and Arabic are not among
/// them, because either one covers languages the app would get wrong.
const BY_SCRIPT: [Language; 5] = [
    Language {
        code: "el",
        english: "Greek",
        script: Script::Greek,
        words: &[],
    },
    Language {
        code: "he",
        english: "Hebrew",
        script: Script::Hebrew,
        words: &[],
    },
    Language {
        code: "ja",
        english: "Japanese",
        script: Script::Kana,
        words: &[],
    },
    Language {
        code: "ko",
        english: "Korean",
        script: Script::Hangul,
        words: &[],
    },
    Language {
        code: "zh",
        english: "Chinese",
        script: Script::Han,
        words: &[],
    },
];

/// The writing a letter belongs to. Enough of it to tell a message in
/// another alphabet from one in this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    Latin,
    Greek,
    Cyrillic,
    Hebrew,
    Arabic,
    Han,
    Kana,
    Hangul,
    Other,
}

/// What the words of a message say about the language it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    /// It reads as the language the interface is in.
    Same,
    /// Another language, named when enough of its words are there to say
    /// which one.
    Other(Option<Language>),
    /// Too little to tell. The app stays quiet rather than guess.
    Unsure,
}

/// The language `text` is in, as far as counting words can say, against
/// the language the interface is in.
///
/// A different script settles it on its own. Otherwise the commonest
/// words decide. Another language is offered when it holds enough of the
/// sample and half as many again as the interface's own language holds.
/// Twice was too strict: Portuguese uses "a" and "me" all the time, and
/// both count as English. A sample long enough to show the interface's
/// words and showing almost none of them is in something else, even when
/// nothing here can name it.
///
/// Offering and naming are two questions. Portuguese and Spanish share
/// half their small words, as German and Dutch share some of theirs, so a
/// message can be plainly foreign and still close between two languages.
/// The name goes on the card only when the words the leader has and the
/// next foreign language lacks outnumber the reverse more than two to
/// one. Otherwise the card says "another language".
pub fn read_language(text: &str, interface: Language) -> Reading {
    let sample: String = text.chars().take(SAMPLE_CHARS).collect();
    if let Some(script) = dominant_script(&sample)
        && script != interface.script
    {
        return Reading::Other(BY_SCRIPT.iter().copied().find(|l| l.script == script));
    }
    if interface.words.is_empty() {
        return Reading::Unsure;
    }
    let words = words(&sample);
    let total = words.len();
    let count = Count::of(&words, interface);
    if total < ENOUGH_WORDS {
        return short_note(&count, total);
    }
    let offered = count.best_hits >= NAMED_WORDS
        && count.best_hits * 100 >= total * NAMED_PERCENT
        && count.best_hits * 2 >= count.mine * 3;
    if offered {
        return Reading::Other(count.named());
    }
    if total >= ENOUGH_FOR_ABSENCE && count.mine * 100 < total * ABSENT_PERCENT {
        return Reading::Other(None);
    }
    match count.mine * 100 >= total * ABSENT_PERCENT {
        true => Reading::Same,
        false => Reading::Unsure,
    }
}

/// A note too short for the percentages above, such as "Ok, obrigado!".
///
/// Short notes are most of what lands in an inbox, and a card over each
/// of them would bury the mail. So one gets a card only on firm evidence:
/// none of the interface's words, and a third of its words belonging to a
/// single language that beats every other. On that evidence the language
/// can be named as well.
fn short_note(count: &Count, total: usize) -> Reading {
    let firm = count.mine == 0
        && count.best_hits > 0
        && count.best_hits * 3 >= total
        && count.best_hits > count.runner_up;
    match firm {
        true => Reading::Other(Some(count.best)),
        false => Reading::Unsure,
    }
}

/// How the known languages' words scored in one sample.
struct Count {
    /// The interface's own language's hits.
    mine: usize,
    /// The foreign language with the most hits, and how many it has.
    best: Language,
    best_hits: usize,
    /// The most hits another foreign language has. The interface's own
    /// language is left out: whether to offer settles that contest.
    runner_up: usize,
    /// Hits on words the leader has and the runner-up lacks, and the other
    /// way round. "de" and "que" count for Portuguese and Spanish alike,
    /// so only these say which of the two a message is in.
    lead: usize,
    trail: usize,
}

impl Count {
    fn of(words: &[String], interface: Language) -> Count {
        let counted: Vec<(Language, usize)> = KNOWN
            .iter()
            .map(|language| (*language, hits(words, language)))
            .collect();
        let mine = counted
            .iter()
            .find(|(language, _)| language.code == interface.code)
            .map_or(0, |(_, hits)| *hits);
        // A tie goes to the language listed first in `KNOWN`. It names
        // nobody either way, since neither side then has a lead.
        let first_most = |most: (Language, usize), next: (Language, usize)| match next.1 > most.1 {
            true => next,
            false => most,
        };
        let (best, best_hits) = counted
            .iter()
            .copied()
            .filter(|(language, _)| language.code != interface.code)
            .fold((interface, 0), first_most);
        let (runner, runner_up) = counted
            .iter()
            .copied()
            .filter(|(language, _)| language.code != best.code && language.code != interface.code)
            .fold((interface, 0), first_most);
        let only = |ours: &Language, theirs: &Language| {
            words
                .iter()
                .filter(|word| {
                    ours.words.contains(&word.as_str()) && !theirs.words.contains(&word.as_str())
                })
                .count()
        };
        // With no foreign language behind it, every hit the leader has is
        // its own.
        let (lead, trail) = match runner_up {
            0 => (best_hits, 0),
            _ => (only(&best, &runner), only(&runner, &best)),
        };
        Count {
            mine,
            best,
            best_hits,
            runner_up,
            lead,
            trail,
        }
    }

    /// The leader, when the words only it has number more than twice the
    /// runner-up's own. A tie on those words, or a lead of two to one, is
    /// a message that mixes the two.
    fn named(&self) -> Option<Language> {
        (self.lead > self.trail * 2).then_some(self.best)
    }
}

/// The language of the message on screen, `own`, helped by `writer`: what
/// the same sender wrote in the thread's other messages.
///
/// A note too short to read on its own, with none of the interface's words
/// in it, takes the language its writer used in the longer messages
/// around it. Someone who wrote three paragraphs in Portuguese and then
/// "Combinado, até lá" has not switched language.
pub fn read_message(own: &str, writer: &str, interface: Language) -> Reading {
    let alone = read_language(own, interface);
    if alone != Reading::Unsure {
        return alone;
    }
    let sample: String = own.chars().take(SAMPLE_CHARS).collect();
    let words = words(&sample);
    if words.is_empty() || words.len() >= ENOUGH_WORDS || hits(&words, &interface) > 0 {
        return alone;
    }
    match read_language(writer, interface) {
        Reading::Other(from) => Reading::Other(from),
        _ => Reading::Unsure,
    }
}

/// The language the interface is in: the Language preference when it names
/// one, and otherwise the desktop's locale. Either only counts while a
/// catalogue for it is installed, since gettext shows English without one.
/// `None` is a language whose words this module cannot count, and the app
/// then offers nothing.
pub fn interface_language(chosen: &str, locale: &str, installed: &[String]) -> Option<Language> {
    let asked = match chosen.is_empty() {
        true => locale,
        false => chosen,
    };
    let base = match installed.iter().any(|code| base_of(code) == base_of(asked)) {
        true => base_of(asked),
        false => "en",
    };
    KNOWN.iter().copied().find(|language| language.code == base)
}

/// The language part of a locale tag: `pt` from `pt_PT.UTF-8`.
fn base_of(tag: &str) -> &str {
    tag.split(['_', '-', '.', '@']).next().unwrap_or(tag)
}

/// The words of `text`, lower-cased. A run of letters is a word and
/// everything else is a gap, so an address or a price is no word at all.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphabetic() && c != '\'')
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn hits(words: &[String], language: &Language) -> usize {
    words
        .iter()
        .filter(|word| language.words.contains(&word.as_str()))
        .count()
}

/// Every script, so one array counts them all.
const SCRIPTS: [Script; 9] = [
    Script::Latin,
    Script::Greek,
    Script::Cyrillic,
    Script::Hebrew,
    Script::Arabic,
    Script::Han,
    Script::Kana,
    Script::Hangul,
    Script::Other,
];

/// The script more than half the letters belong to, or `None` when none
/// has that many. Kana carry Japanese on their own, since a Japanese
/// sentence holds more Han characters than kana and would otherwise read
/// as Chinese.
fn dominant_script(text: &str) -> Option<Script> {
    let mut counts = [0usize; SCRIPTS.len()];
    let mut total = 0usize;
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        total += 1;
        let script = script_of(c);
        if let Some(at) = SCRIPTS.iter().position(|s| *s == script) {
            counts[at] += 1;
        }
    }
    if total == 0 {
        return None;
    }
    let count = |script: Script| {
        SCRIPTS
            .iter()
            .position(|s| *s == script)
            .map_or(0, |at| counts[at])
    };
    if count(Script::Kana) > 0 && (count(Script::Kana) + count(Script::Han)) * 2 > total {
        return Some(Script::Kana);
    }
    let (at, most) = counts
        .iter()
        .enumerate()
        .max_by_key(|(_, n)| **n)
        .unwrap_or((0, &0));
    (most * 2 > total).then(|| SCRIPTS[at])
}

fn script_of(c: char) -> Script {
    match c as u32 {
        0x0041..=0x024F | 0x1E00..=0x1EFF => Script::Latin,
        0x0370..=0x03FF | 0x1F00..=0x1FFF => Script::Greek,
        0x0400..=0x052F => Script::Cyrillic,
        0x0590..=0x05FF => Script::Hebrew,
        0x0600..=0x06FF | 0x0750..=0x077F => Script::Arabic,
        0x3040..=0x30FF => Script::Kana,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF => Script::Han,
        0xAC00..=0xD7AF | 0x1100..=0x11FF => Script::Hangul,
        _ => Script::Other,
    }
}

/// A message body, as the page draws it.
pub enum Body<'a> {
    /// HTML that has already been through [`crate::sanitize`].
    Html(&'a str),
    Text(&'a str),
}

/// The prose of one message and the markup around it, in the order they
/// appear. Translating rebuilds the body from the same parts, so the
/// markup that comes out is the markup that went in.
pub struct Prose {
    parts: Vec<Part>,
    html: bool,
}

enum Part {
    /// A tag, a line ending, whitespace, a price: kept as it is.
    Kept(String),
    Words {
        text: String,
        /// Inside a quote of somebody else's message, which says nothing
        /// about the language this writer wrote in.
        quoted: bool,
    },
}

impl Prose {
    /// Cuts the prose out of a body.
    pub fn read(body: Body) -> Prose {
        match body {
            Body::Html(html) => Prose {
                parts: read_html(html),
                html: true,
            },
            Body::Text(text) => Prose {
                parts: read_text(text),
                html: false,
            },
        }
    }

    /// The pieces to translate, in the order they are read in.
    pub fn pieces(&self) -> Vec<&str> {
        self.parts
            .iter()
            .filter_map(|part| match part {
                Part::Words { text, .. } => Some(text.as_str()),
                Part::Kept(_) => None,
            })
            .collect()
    }

    /// The words to read the language off: this writer's own, without the
    /// message they quoted. A message that is nothing but a quote gives
    /// up the quote rather than nothing.
    pub fn sample(&self) -> String {
        let own = self.joined(false);
        match own.chars().any(char::is_alphabetic) {
            true => own,
            false => self.joined(true),
        }
    }

    fn joined(&self, quotes: bool) -> String {
        let mut out = String::new();
        for part in &self.parts {
            let Part::Words { text, quoted } = part else {
                continue;
            };
            if *quoted && !quotes {
                continue;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(text);
            if out.chars().count() >= SAMPLE_CHARS {
                break;
            }
        }
        out
    }

    /// The body again, with `translated[i]` in place of piece `i`. A piece
    /// the model left out keeps its own words. Nothing the model wrote
    /// lands anywhere but inside a piece, and in HTML it is escaped on the
    /// way in, so a translation cannot become markup.
    pub fn rebuild(&self, translated: &[Option<String>]) -> String {
        let mut out = String::with_capacity(self.parts.len() * 16);
        let mut piece = 0;
        for part in &self.parts {
            match part {
                Part::Kept(text) => out.push_str(text),
                Part::Words { text, .. } => {
                    let said = translated
                        .get(piece)
                        .and_then(Option::as_deref)
                        .unwrap_or(text);
                    match self.html {
                        true => out.push_str(&escape(said)),
                        false => out.push_str(said),
                    }
                    piece += 1;
                }
            }
        }
        out
    }
}

/// How many of the pieces fit in one request, counting from the first.
/// The rest of the message stays in the language it arrived in. A first
/// piece longer than the whole budget goes on its own, since stopping
/// before it would translate nothing at all.
pub fn fits(pieces: &[&str]) -> usize {
    let mut room = MAX_CHARS;
    for (at, piece) in pieces.iter().enumerate() {
        let Some(left) = room.checked_sub(piece.chars().count()) else {
            return at.max(1);
        };
        room = left;
    }
    pieces.len()
}

/// Tags whose text is not prose. `<style>` survives cleaning, because a
/// message's own stylesheet is what lays its tables out.
const NOT_PROSE: [&str; 3] = ["style", "script", "title"];

fn read_html(html: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut rest = html;
    let mut quoted = 0usize;
    let mut skipping = 0usize;
    while let Some(open) = rest.find('<') {
        push_text(&mut parts, &rest[..open], quoted > 0, skipping > 0);
        rest = &rest[open..];
        let end = rest.find('>').map_or(rest.len(), |at| at + 1);
        let (name, closing) = tag_name(&rest[..end]);
        if name == "blockquote" {
            match closing {
                true => quoted = quoted.saturating_sub(1),
                false => quoted += 1,
            }
        }
        if NOT_PROSE.contains(&name.as_str()) {
            match closing {
                true => skipping = skipping.saturating_sub(1),
                false => skipping += 1,
            }
        }
        parts.push(Part::Kept(rest[..end].to_string()));
        rest = &rest[end..];
    }
    push_text(&mut parts, rest, quoted > 0, skipping > 0);
    parts
}

/// The tag's name in lower case, and whether it closes one.
fn tag_name(tag: &str) -> (String, bool) {
    let inside = tag.trim_start_matches('<').trim_end_matches('>');
    let closing = inside.starts_with('/');
    let name: String = inside
        .trim_start_matches('/')
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    (name.to_lowercase(), closing)
}

/// Adds one run of text between tags. The whitespace at either end is
/// kept, so the spacing between two inline elements survives a
/// translation that trims what it was given.
fn push_text(parts: &mut Vec<Part>, text: &str, quoted: bool, skipping: bool) {
    if text.is_empty() {
        return;
    }
    if skipping || !text.chars().any(char::is_alphabetic) {
        parts.push(Part::Kept(text.to_string()));
        return;
    }
    let body = text.trim();
    let (before, after) = text.split_once(body).unwrap_or(("", ""));
    if !before.is_empty() {
        parts.push(Part::Kept(before.to_string()));
    }
    parts.push(Part::Words {
        text: unescape(body),
        quoted,
    });
    if !after.is_empty() {
        parts.push(Part::Kept(after.to_string()));
    }
}

/// The five entities the cleaner writes, plus the one for a non-breaking
/// space, which comes back as the character itself and draws the same.
/// Anything else the cleaner leaves alone, and so does this.
fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", "\u{a0}")
        .replace("&amp;", "&")
}

fn read_text(text: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    for line in text.split_inclusive('\n') {
        let (line, ending) = match line.strip_suffix('\n') {
            Some(rest) => (rest, "\n"),
            None => (line, ""),
        };
        let marks: String = line
            .chars()
            .take_while(|c| *c == '>' || c.is_whitespace())
            .collect();
        let said = &line[marks.len()..];
        if !marks.is_empty() {
            parts.push(Part::Kept(marks.clone()));
        }
        if said.chars().any(char::is_alphabetic) {
            let body = said.trim_end();
            parts.push(Part::Words {
                text: body.to_string(),
                quoted: marks.contains('>'),
            });
            parts.push(Part::Kept(format!("{}{ending}", &said[body.len()..])));
        } else {
            parts.push(Part::Kept(format!("{said}{ending}")));
        }
    }
    parts
}

/// What the model is told before the pieces. It never reaches a reader, so
/// it stays in English whatever the interface speaks.
const SYSTEM: &str = "You translate email. You answer with the translation and nothing else: \
no notes, no apologies, no summary.";

/// The request for one message: the pieces, numbered, and how to send
/// them back. The numbering is what puts each piece back where it came
/// from, so the answer is read by number rather than by order.
pub fn prompt(into: Language, pieces: &[&str]) -> String {
    let mut out = format!(
        "Translate each numbered piece of an email into {}.\n\n\
         Answer with one line per piece, in the same order, each line starting with the \
         piece's own number in double square brackets. Keep every piece, even an empty \
         translation. Leave names, addresses, links and numbers as they are. A piece already \
         in {} comes back as it is. Write nothing else.\n\n",
        into.english, into.english
    );
    for (at, piece) in pieces.iter().enumerate() {
        out.push_str(&format!("[[{}]] {}\n", at + 1, flatten(piece)));
    }
    out
}

/// One piece on one line: every run of whitespace becomes one space.
fn flatten(text: &str) -> String {
    text.split_whitespace().collect::<Vec<&str>>().join(" ")
}

/// Reads the model's answer back into one translation per piece. A piece
/// it dropped, numbered twice, or numbered out of range comes back as
/// `None` and keeps the words it arrived in.
pub fn read_reply(reply: &str, count: usize) -> Vec<Option<String>> {
    let mut out: Vec<Option<String>> = vec![None; count];
    let mut at: Option<usize> = None;
    for line in reply.lines() {
        if line.trim_start().starts_with("```") {
            continue;
        }
        match numbered(line) {
            Some((number, said)) => {
                at = number
                    .checked_sub(1)
                    .filter(|index| out.get(*index).is_some_and(Option::is_none));
                if let Some(index) = at {
                    out[index] = Some(said.trim().to_string());
                }
            }
            None => {
                let Some(index) = at else { continue };
                let said = out[index].get_or_insert_with(String::new);
                said.push(' ');
                said.push_str(line.trim());
            }
        }
    }
    for said in out.iter_mut().flatten() {
        *said = flatten(said);
    }
    out
}

/// The `[[7]] ` at the start of a line, with what follows it.
fn numbered(line: &str) -> Option<(usize, &str)> {
    let rest = line.trim_start().strip_prefix("[[")?;
    let (number, rest) = rest.split_once("]]")?;
    Some((number.trim().parse().ok()?, rest))
}

/// One message in the reader's language, kept for as long as the thread
/// stays open. It never reaches the store: it is text the model derived,
/// and tomorrow's model would write it differently.
pub struct Translation {
    /// The language it arrived in, when the words said which.
    pub from: Option<Language>,
    /// The body with its prose translated, beside the one that arrived
    /// rather than in place of it.
    pub body: MessageBody,
    /// The translated HTML after cleaning, which is what the page draws.
    /// `None` for a message with no HTML part.
    pub clean: Option<String>,
    /// Set when the message was too long and only its start was
    /// translated.
    pub cut: bool,
    /// Whether the page shows this rather than what arrived. The window
    /// turns it over when the reader asks for the original back.
    pub shown: bool,
}

/// Where a message's words go to be translated, and the model that reads
/// them. `Err` says why there is nowhere to send them, in the words
/// Preferences uses.
pub fn destination(ai: &AiSettings) -> Result<(ProviderConfig, String), String> {
    let config = assistant::model_for(ai, Feature::Translation)?;
    let said = match &config {
        ProviderConfig::OpenAiCompatible {
            base_url, model, ..
        } => match host_of(base_url).filter(|host| !is_this_computer(host)) {
            Some(host) => fill(
                &gettext("The message goes to {host}, for {model} to read."),
                &[("host", &host), ("model", model)],
            ),
            None => fill(
                &gettext("The message goes to {model} on this computer and no further."),
                &[("model", model)],
            ),
        },
        ProviderConfig::Anthropic { model, .. } => fill(
            &gettext("The message goes to Anthropic, for {model} to read."),
            &[("model", model)],
        ),
        ProviderConfig::ClaudeCode { .. } => {
            gettext("The message goes to Anthropic, for Claude to read through your subscription.")
        }
    };
    Ok((config, said))
}

/// The host part of a server address, without the port.
fn host_of(base_url: &str) -> Option<String> {
    let rest = base_url
        .split_once("://")
        .map_or(base_url, |(_, after)| after);
    let host = rest.split('/').next()?;
    let host = host.rsplit_once(':').map_or(host, |(before, _)| before);
    let host = host.trim_matches(['[', ']']);
    (!host.is_empty()).then(|| host.to_lowercase())
}

/// Whether a server address names this computer. Anything else is named
/// in full, because a box on the network is not this one.
fn is_this_computer(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "0.0.0.0")
}

/// Sends the pieces to the model and reads the translations back.
pub async fn ask(
    config: ProviderConfig,
    into: Language,
    pieces: &[&str],
) -> Result<Vec<Option<String>>, String> {
    let mut chat = Conversation::new(config, SYSTEM.to_string());
    // Nobody watches a translation go by, and a closed channel is a
    // channel the agent loop carries on past.
    let (events, watching) = async_channel::unbounded::<AgentEvent>();
    drop(watching);
    let reply = chat
        .send(prompt(into, pieces), Arc::new(NoTools), events)
        .await
        .map_err(|err| err.to_string())?;
    Ok(read_reply(&reply, pieces.len()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::settings::AiProvider;

    const ENGLISH_NOTE: &str = "Hello Ana, I have attached the invoice for last month and \
         the receipt that goes with it. Let me know if you need anything else before Friday.";
    const PORTUGUESE_NOTE: &str = "Olá Ana, junto envio a factura do mês passado e o recibo \
         que vai com ela. Diga-me se precisa de mais alguma coisa antes de sexta-feira.";

    #[test]
    fn a_message_in_the_interfaces_own_language_is_left_alone() {
        assert_eq!(read_language(ENGLISH_NOTE, ENGLISH), Reading::Same);
        assert_eq!(read_language(PORTUGUESE_NOTE, PORTUGUESE), Reading::Same);
    }

    #[test]
    fn a_message_in_another_language_is_named() {
        assert_eq!(
            read_language(PORTUGUESE_NOTE, ENGLISH),
            Reading::Other(Some(PORTUGUESE))
        );
        assert_eq!(
            read_language(ENGLISH_NOTE, PORTUGUESE),
            Reading::Other(Some(ENGLISH))
        );
    }

    #[test]
    fn spanish_is_not_read_as_portuguese() {
        let spanish = "Hola Ana, le envío la factura del mes pasado y el recibo que va con \
             ella. Dígame si necesita algo más antes del viernes, por favor. Muchas gracias.";
        assert_eq!(
            read_language(spanish, ENGLISH),
            Reading::Other(Some(SPANISH))
        );
        assert_eq!(
            read_language(spanish, PORTUGUESE),
            Reading::Other(Some(SPANISH))
        );
    }

    #[test]
    fn french_german_italian_and_dutch_are_told_apart() {
        let cases = [
            (
                "Bonjour Ana, je vous envoie la facture du mois dernier avec le reçu qui va \
                 avec. Dites-moi si vous avez besoin de quelque chose avant vendredi.",
                FRENCH,
            ),
            (
                "Guten Tag Ana, ich schicke Ihnen die Rechnung für den letzten Monat und die \
                 Quittung dazu. Sagen Sie mir bitte, ob Sie noch etwas brauchen.",
                GERMAN,
            ),
            (
                "Buongiorno Ana, le invio la fattura del mese scorso e la ricevuta che va con \
                 questa. Mi dica se ha bisogno di qualcosa prima di venerdì, grazie.",
                ITALIAN,
            ),
            (
                "Hallo Ana, ik stuur je de factuur van vorige maand en het bonnetje dat erbij \
                 hoort. Laat me weten of je nog iets nodig hebt voor vrijdag.",
                DUTCH,
            ),
        ];
        for (text, language) in cases {
            assert_eq!(
                read_language(text, ENGLISH),
                Reading::Other(Some(language)),
                "{}",
                language.english
            );
        }
    }

    #[test]
    fn a_short_note_in_another_language_is_offered_and_named() {
        let cases = [
            ("Ok, obrigado!", PORTUGUESE),
            ("Vale, muchas gracias.", SPANISH),
            ("Danke, bis morgen!", GERMAN),
            ("Dank je, tot morgen.", DUTCH),
        ];
        for (text, language) in cases {
            assert_eq!(
                read_language(text, ENGLISH),
                Reading::Other(Some(language)),
                "{text}"
            );
        }
        assert_eq!(
            read_language("Thanks, see you tomorrow.", PORTUGUESE),
            Reading::Other(Some(ENGLISH))
        );
    }

    #[test]
    fn a_short_note_with_the_interfaces_own_words_gets_no_card() {
        assert_eq!(
            read_language("Thanks, see you tomorrow.", ENGLISH),
            Reading::Unsure
        );
        assert_eq!(read_language("Ok, obrigado!", PORTUGUESE), Reading::Unsure);
        // "Obrigado" is Portuguese and "from" English: no clear winner.
        assert_eq!(
            read_language("Obrigado. Sent from my phone", ENGLISH),
            Reading::Unsure
        );
    }

    #[test]
    fn a_short_note_with_no_words_to_count_says_nothing() {
        assert_eq!(
            read_language("Combinado, até lá.", ENGLISH),
            Reading::Unsure
        );
        assert_eq!(read_language("Ok", ENGLISH), Reading::Unsure);
        assert_eq!(read_language("", ENGLISH), Reading::Unsure);
    }

    #[test]
    fn a_short_note_takes_the_language_its_writer_used_in_the_thread() {
        assert_eq!(
            read_message("Combinado, até lá.", PORTUGUESE_NOTE, ENGLISH),
            Reading::Other(Some(PORTUGUESE))
        );
        // The writer's other messages never outvote the note's own words.
        assert_eq!(
            read_message("Thanks, see you there.", PORTUGUESE_NOTE, ENGLISH),
            Reading::Unsure
        );
        assert_eq!(
            read_message("Combinado, até lá.", ENGLISH_NOTE, ENGLISH),
            Reading::Unsure
        );
        assert_eq!(
            read_message("Combinado, até lá.", "", ENGLISH),
            Reading::Unsure
        );
        // A picture with no words under it has no language to borrow.
        assert_eq!(read_message("", PORTUGUESE_NOTE, ENGLISH), Reading::Unsure);
    }

    #[test]
    fn portuguese_that_uses_english_looking_words_is_still_offered() {
        let text = "Se puder, diga-me quando chega a encomenda, que não a encontro em casa.";
        assert_eq!(
            read_language(text, ENGLISH),
            Reading::Other(Some(PORTUGUESE))
        );
    }

    #[test]
    fn neighbouring_languages_are_named_when_one_clearly_leads() {
        let cases = [
            (
                "De reunião de sexta ficou para segunda, por causa da viagem do diretor.",
                PORTUGUESE,
            ),
            (
                "La reunión del viernes se pasó al lunes, por el viaje del director.",
                SPANISH,
            ),
            (
                "Wir sind morgen um zehn Uhr im Büro, und die Unterlagen liegen auf dem Tisch.",
                GERMAN,
            ),
            (
                "Wij zijn morgen om tien uur op kantoor, en de papieren liggen op de tafel.",
                DUTCH,
            ),
        ];
        for (text, language) in cases {
            assert_eq!(
                read_language(text, ENGLISH),
                Reading::Other(Some(language)),
                "{text}"
            );
        }
    }

    #[test]
    fn english_words_in_a_german_message_do_not_hide_its_name() {
        let text = "Danke für alles, wir sehen uns morgen und bitte bring the charger, \
             the keys and the map mit.";
        assert_eq!(read_language(text, ENGLISH), Reading::Other(Some(GERMAN)));
    }

    #[test]
    fn a_close_call_between_two_languages_names_neither() {
        let portunhol = "Hola Ana, obrigado por todo, que tengas un buen día. Beijinhos \
             para a família, y hasta pronto.";
        assert_eq!(read_language(portunhol, ENGLISH), Reading::Other(None));
        let between = "Ik bin morgen in het Büro und wir haben de Unterlagen op tafel, bitte.";
        assert_eq!(read_language(between, ENGLISH), Reading::Other(None));
    }

    #[test]
    fn another_alphabet_needs_no_word_list() {
        let greek = "Γεια σας, σας στέλνω το τιμολόγιο του περασμένου μήνα.";
        assert_eq!(
            read_language(greek, ENGLISH),
            Reading::Other(Some(BY_SCRIPT[0]))
        );
        let russian = "Здравствуйте, отправляю вам счёт за прошлый месяц и квитанцию.";
        assert_eq!(read_language(russian, ENGLISH), Reading::Other(None));
    }

    #[test]
    fn a_language_with_no_word_list_still_reads_as_another_one() {
        let polish = "Dzień dobry, przesyłam fakturę za ubiegły miesiąc oraz paragon, który \
             należy do niej. Proszę dać znać, jeśli potrzebne jest coś jeszcze przed \
             piątkiem, z góry dziękuję za pomoc.";
        assert_eq!(read_language(polish, ENGLISH), Reading::Other(None));
    }

    #[test]
    fn the_interface_follows_the_preference_then_the_desktop() {
        let installed = ["en".to_string(), "pt_PT".to_string()];
        assert_eq!(
            interface_language("pt_PT", "en_GB", &installed),
            Some(PORTUGUESE)
        );
        assert_eq!(
            interface_language("", "pt_PT.UTF-8", &installed),
            Some(PORTUGUESE)
        );
        // No catalogue, so gettext shows English and so does the app.
        assert_eq!(interface_language("", "de_DE", &installed), Some(ENGLISH));
        assert_eq!(interface_language("", "C", &installed), Some(ENGLISH));
    }

    #[test]
    fn a_table_keeps_its_shape_and_only_its_words_travel() {
        let html = "<table><tr><td>Saldo final</td><td>1.204,55 €</td></tr>\
                    <tr><td>Data</td><td>12/03</td></tr></table>";
        let prose = Prose::read(Body::Html(html));
        assert_eq!(prose.pieces(), ["Saldo final", "Data"]);
        let built = prose.rebuild(&[Some("Closing balance".into()), Some("Date".into())]);
        assert_eq!(
            built,
            "<table><tr><td>Closing balance</td><td>1.204,55 €</td></tr>\
             <tr><td>Date</td><td>12/03</td></tr></table>"
        );
    }

    #[test]
    fn a_stylesheet_is_not_prose() {
        let html = "<style>td{color:red}</style><p>Olá</p>";
        let prose = Prose::read(Body::Html(html));
        assert_eq!(prose.pieces(), ["Olá"]);
        assert_eq!(
            prose.rebuild(&[Some("Hello".into())]),
            "<style>td{color:red}</style><p>Hello</p>"
        );
    }

    #[test]
    fn the_spacing_between_inline_tags_survives() {
        let html = "<p>Bom <b>dia</b> a todos</p>";
        let prose = Prose::read(Body::Html(html));
        assert_eq!(prose.pieces(), ["Bom", "dia", "a todos"]);
        assert_eq!(
            prose.rebuild(&[None, None, None]),
            "<p>Bom <b>dia</b> a todos</p>"
        );
    }

    #[test]
    fn a_piece_the_model_left_out_keeps_its_own_words() {
        let prose = Prose::read(Body::Html("<p>Um</p><p>Dois</p>"));
        assert_eq!(
            prose.rebuild(&[Some("One".into()), None]),
            "<p>One</p><p>Dois</p>"
        );
    }

    #[test]
    fn nothing_the_model_writes_becomes_markup() {
        let prose = Prose::read(Body::Html("<p>Olá</p>"));
        let built = prose.rebuild(&[Some("<script>steal()</script>".into())]);
        assert_eq!(built, "<p>&lt;script&gt;steal()&lt;/script&gt;</p>");
    }

    #[test]
    fn entities_go_out_as_characters_and_come_back_as_entities() {
        let prose = Prose::read(Body::Html("<p>Ana &amp; Jo&#39;s &lt;lista&gt;</p>"));
        assert_eq!(prose.pieces(), ["Ana & Jo's <lista>"]);
        assert_eq!(
            prose.rebuild(&[None]),
            "<p>Ana &amp; Jo&#39;s &lt;lista&gt;</p>"
        );
    }

    #[test]
    fn plain_text_keeps_its_lines_and_its_quote_marks() {
        let text = "Olá Ana,\n\n> On Tuesday you wrote this\n\nAté já.\n";
        let prose = Prose::read(Body::Text(text));
        assert_eq!(
            prose.pieces(),
            ["Olá Ana,", "On Tuesday you wrote this", "Até já."]
        );
        let built = prose.rebuild(&[
            Some("Hello Ana,".into()),
            Some("On Tuesday you wrote this".into()),
            Some("See you soon.".into()),
        ]);
        assert_eq!(
            built,
            "Hello Ana,\n\n> On Tuesday you wrote this\n\nSee you soon.\n"
        );
    }

    #[test]
    fn the_language_is_read_off_this_writers_own_words() {
        let text = "Bom dia Ana, aqui vai a factura do mês passado e o recibo que vai com \
             ela. Diga-me se precisa de mais alguma coisa.\n\n\
             > Hello Ana, could you please send me the invoice for last month and the \
             > receipt that goes with it? I have not been able to find either of them in \
             > the folder that we share, and the auditors are asking for them.\n";
        let prose = Prose::read(Body::Text(text));
        assert_eq!(
            read_language(&prose.sample(), ENGLISH),
            Reading::Other(Some(PORTUGUESE))
        );
    }

    #[test]
    fn a_long_message_is_translated_as_far_as_the_budget_goes() {
        let long = "palavra ".repeat(MAX_CHARS / 4);
        let pieces = [long.as_str(), "fim"];
        assert_eq!(fits(&pieces), 1);
        assert_eq!(fits(&["curto", "fim"]), 2);
    }

    #[test]
    fn the_prompt_numbers_the_pieces_and_names_the_language() {
        let prompt = prompt(PORTUGUESE, &["Good morning", "See you\nsoon"]);
        assert!(prompt.contains("into European Portuguese"), "{prompt}");
        assert!(prompt.contains("[[1]] Good morning\n"), "{prompt}");
        assert!(prompt.contains("[[2]] See you soon\n"), "{prompt}");
    }

    #[test]
    fn the_answer_is_read_by_number() {
        let reply = "[[1]] Bom dia\n[[2]] Até já";
        assert_eq!(
            read_reply(reply, 2),
            [Some("Bom dia".to_string()), Some("Até já".to_string())]
        );
    }

    #[test]
    fn an_answer_that_wanders_keeps_what_it_got_right() {
        let reply =
            "Sure, here you go:\n```\n[[2]] Até já\ne mais\n[[9]] nowhere\n[[2]] again\n```";
        assert_eq!(
            read_reply(reply, 3),
            [None, Some("Até já e mais".to_string()), None]
        );
    }

    #[test]
    fn a_body_comes_back_as_it_was_when_nothing_is_translated() {
        let html = "<div style=\"color:#333\"><p>Olá&nbsp;Ana</p><ul><li>Um</li>\
                    <li>Dois</li></ul><a href=\"https://exemplo.pt\">Ver a conta</a></div>";
        let prose = Prose::read(Body::Html(html));
        // A non-breaking space comes back as the character rather than
        // as the entity, which draws the same.
        assert_eq!(prose.rebuild(&[]), html.replace("&nbsp;", "\u{a0}"));
    }

    #[test]
    fn what_the_model_answers_is_cleaned_before_it_is_drawn() {
        let prose = Prose::read(Body::Html("<p>Bom dia</p><p>Até já</p>"));
        let pieces = prose.pieces();
        assert!(prompt(ENGLISH, &pieces).contains("[[2]] Até já\n"));
        let said = read_reply(
            "[[1]] Good morning\n[[2]] See you <b>soon</b>",
            pieces.len(),
        );
        let clean = crate::sanitize::sanitize_html(&prose.rebuild(&said), &HashMap::new());
        assert_eq!(
            clean,
            "<p>Good morning</p><p>See you &lt;b&gt;soon&lt;/b&gt;</p>"
        );
    }

    #[test]
    fn with_no_model_the_action_says_so_rather_than_failing() {
        let off = AiSettings::default();
        assert!(matches!(off.provider, AiProvider::Off));
        let problem = destination(&off).expect_err("a provider that is off has nowhere to send");
        assert!(problem.contains("Preferences"), "{problem}");
    }

    #[test]
    fn where_the_words_go_is_said_in_the_words_preferences_uses() {
        let local = AiSettings {
            provider: AiProvider::Local,
            base_url: "http://127.0.0.1:1234/v1".to_string(),
            local_model: "gemma-3".to_string(),
            ..AiSettings::default()
        };
        let (_, said) = destination(&local).expect("a local server is somewhere to send");
        assert_eq!(
            said,
            "The message goes to gemma-3 on this computer and no further."
        );
        let remote = AiSettings {
            base_url: "https://api.openai.com/v1".to_string(),
            ..local
        };
        let (_, said) = destination(&remote).expect("a remote server is somewhere to send");
        assert_eq!(
            said,
            "The message goes to api.openai.com, for gemma-3 to read."
        );
    }
}
