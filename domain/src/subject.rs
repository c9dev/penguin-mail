//! What a subject says once the reply and forward prefixes and the
//! mailing-list tags are gone, for threading mail a server does not
//! thread. "Re: [team] Fwd: Lunch" and "Lunch" have the same base.

/// The prefixes mail programs put before a subject when they reply or
/// forward, in English and in the languages whose mail the owner and
/// most European senders use. Compared without case.
const PREFIXES: [&str; 12] = [
    "re", "fwd", "fw", "aw", "sv", "vs", "res", "enc", "tr", "wg", "antw", "odp",
];

/// The subject without its prefixes and list tags, with its spaces
/// collapsed, in lower case.
pub fn base(subject: &str) -> String {
    strip(subject).0
}

/// Whether the subject starts with a reply or forward prefix, once list
/// tags are set aside.
pub fn replies(subject: &str) -> bool {
    strip(subject).1
}

fn strip(subject: &str) -> (String, bool) {
    let mut rest = subject.trim();
    let mut prefixed = false;
    loop {
        if let Some(tagged) = rest.strip_prefix('[')
            && let Some((_, after)) = tagged.split_once(']')
        {
            rest = after.trim_start();
            continue;
        }
        let Some((head, after)) = rest.split_once(':') else {
            break;
        };
        // "Re[2]" and "RE(3)" count a reply's depth; the count goes.
        let word = head.trim_end_matches(|c: char| c.is_ascii_digit() || "[]()".contains(c));
        if PREFIXES.contains(&word.trim().to_lowercase().as_str()) {
            rest = after.trim_start();
            prefixed = true;
        } else {
            break;
        }
    }
    let words: Vec<&str> = rest.split_whitespace().collect();
    (words.join(" ").to_lowercase(), prefixed)
}

#[cfg(test)]
mod tests {
    use super::{base, replies};

    #[test]
    fn reply_and_forward_prefixes_come_off_in_any_number() {
        assert_eq!(base("Re: Lunch"), "lunch");
        assert_eq!(base("RE: Fwd: re: Lunch"), "lunch");
        assert_eq!(base("Re[2]: Lunch"), "lunch");
        assert_eq!(base("Res: Almoço"), "almoço");
        assert_eq!(base("Enc: Almoço"), "almoço");
        assert_eq!(base("AW: Mittag"), "mittag");
        assert_eq!(base("Lunch"), "lunch");
    }

    #[test]
    fn list_tags_and_spaces_do_not_count() {
        assert_eq!(base("[team] Re: Lunch  plans"), "lunch plans");
        assert_eq!(base("Re: [team]   Lunch plans "), "lunch plans");
    }

    #[test]
    fn a_colon_inside_the_subject_stays() {
        assert_eq!(base("Agenda: Monday"), "agenda: monday");
        assert_eq!(base("Re: Agenda: Monday"), "agenda: monday");
    }

    #[test]
    fn only_a_prefixed_subject_replies() {
        assert!(replies("Re: Lunch"));
        assert!(replies("[team] Fwd: Lunch"));
        assert!(!replies("Lunch"));
        assert!(!replies("Agenda: Monday"));
        assert!(!replies(""));
    }
}
