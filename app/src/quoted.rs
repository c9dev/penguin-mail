//! Finds the quoted history at the end of a message, which the page folds
//! away behind a small button the way Gmail does.

use std::ops::Range;

/// The part of cleaned HTML that holds the quoted history, when the
/// message ends with one and something the writer wrote comes before it.
/// The range covers whole elements that share one parent, so the page can
/// wrap it in an element of its own.
///
/// It reads what the sanitizer wrote, which quotes every attribute and
/// escapes every `<` in text, so a scan by hand is enough and costs one
/// pass over the body.
pub fn history_in_html(html: &str) -> Option<Range<usize>> {
    unfolded_on_panic(|| html_history(html))
}

fn html_history(html: &str) -> Option<Range<usize>> {
    let tree = Tree::parse(html);
    let first = tree.nodes.iter().find(|n| tree.visible(n))?.start;
    let last = tree.nodes.iter().rev().find(|n| tree.visible(n))?.start;
    (1..tree.nodes.len()).find_map(|at| {
        let kind = tree.opens_history(at)?;
        let node = &tree.nodes[at];
        let parent = &tree.nodes[node.parent];
        if ["p", "table", "tbody", "thead", "tr", "ul", "ol", "dl", "select"]
            .contains(&tree.name(parent))
        {
            return None;
        }
        let from = match kind {
            Kind::Quote { needs_attribution } => {
                let from = tree.attribution(at, needs_attribution);
                if (needs_attribution && from == node.start) || last >= node.end {
                    return None;
                }
                from
            }
            Kind::Header => {
                if last >= parent.inner_end {
                    return None;
                }
                tree.rule_above(at)
            }
        };
        (first < from).then_some(from..parent.inner_end)
    })
}

/// Where the quoted history starts in a plain text body: a trailing run of
/// lines that start with `>`, with the line above it when that one ends
/// in a colon, or a forwarded message's marker line. None when the writer
/// wrote nothing above it.
pub fn history_in_text(text: &str) -> Option<usize> {
    unfolded_on_panic(|| text_history(text))
}

/// Runs a search for quoted history, and folds nothing if it panics.
/// Folding is an extra: a fault in it must leave the message readable,
/// where a panic here once kept a receipt from opening at all, even after
/// a restart, because the window cleans a body again on its own thread
/// when the worker's try fails.
fn unfolded_on_panic<T>(find: impl FnOnce() -> Option<T> + std::panic::UnwindSafe) -> Option<T> {
    std::panic::catch_unwind(find).unwrap_or_else(|_| {
        tracing::warn!("finding a message's quoted history failed; showing it unfolded");
        None
    })
}

fn text_history(text: &str) -> Option<usize> {
    let mut starts = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        starts.push((at, line.trim_end()));
        at += line.len();
    }
    let from = match starts.iter().position(|(_, line)| forward_marker(line)) {
        Some(marker) => marker,
        None => {
            let mut end = starts.len();
            while end > 0 && starts[end - 1].1.trim().is_empty() {
                end -= 1;
            }
            let mut from = end;
            while from > 0 && starts[from - 1].1.trim_start().starts_with('>') {
                from -= 1;
            }
            if from == end {
                return None;
            }
            match from.checked_sub(1) {
                Some(above) if starts[above].1.ends_with(':') => above,
                _ => from,
            }
        }
    };
    starts[..from]
        .iter()
        .any(|(_, line)| !line.trim().is_empty())
        .then(|| starts[from].0)
}

/// Whether a line opens a forwarded message, as Gmail, Penguin Mail,
/// Outlook and Apple Mail write it.
fn forward_marker(text: &str) -> bool {
    let text = text.trim_start();
    [
        "---------- Forwarded message",
        "-------- Original Message",
        "-----Original Message-----",
        "Begin forwarded message:",
    ]
    .iter()
    .any(|marker| text.starts_with(marker))
}

/// How an element opens the history.
enum Kind {
    /// A quote, which must end the message. A bare `<blockquote>` counts
    /// only under a line saying who wrote it; a writer may quote a poem.
    Quote { needs_attribution: bool },
    /// The header of a quoted or forwarded message, which the quoted
    /// body follows as siblings, so what comes after it in the same
    /// parent belongs to the history too.
    Header,
}

/// One element or run of text, by its place in the HTML.
struct Node {
    /// Where its start tag, or its text, starts.
    start: usize,
    /// Where its start tag ends; the same as `start` for text.
    tag_end: usize,
    /// Where its end tag starts, or where it ends when it has none.
    inner_end: usize,
    /// Just past its end tag.
    end: usize,
    parent: usize,
    text: bool,
    /// The first run of text inside it, by index, for telling which
    /// element is the outermost one a header's words open.
    first_text: Option<usize>,
}

/// The elements of cleaned HTML, in document order. Node 0 stands for the
/// whole body.
struct Tree<'a> {
    html: &'a str,
    nodes: Vec<Node>,
    children: Vec<Vec<usize>>,
}

const VOID: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
    "source", "track", "wbr",
];

const INLINE: [&str; 11] = [
    "a", "b", "i", "u", "em", "strong", "span", "font", "small", "big", "code",
];

impl<'a> Tree<'a> {
    fn parse(html: &'a str) -> Tree<'a> {
        let mut tree = Tree {
            html,
            nodes: vec![Node {
                start: 0,
                tag_end: 0,
                inner_end: html.len(),
                end: html.len(),
                parent: 0,
                text: false,
                first_text: None,
            }],
            children: vec![Vec::new()],
        };
        let mut open = vec![0usize];
        let bytes = html.as_bytes();
        let mut at = 0;
        while at < html.len() {
            let parent = *open.last().unwrap_or(&0);
            if bytes[at] != b'<' {
                let end = html[at..].find('<').map_or(html.len(), |i| at + i);
                let index = tree.push(at, at, end, end, parent, true);
                // Every ancestor without words yet starts with these. One
                // that has words already has ancestors that do too.
                let mut up = parent;
                while !tree.blank(index) && tree.nodes[up].first_text.is_none() {
                    tree.nodes[up].first_text = Some(index);
                    if up == 0 {
                        break;
                    }
                    up = tree.nodes[up].parent;
                }
                at = end;
                continue;
            }
            let close = tag_close(html, at);
            if html[at..].starts_with("</") {
                let name = tag_name(&html[at + 2..close]);
                if let Some(depth) = open
                    .iter()
                    .rposition(|&n| n != 0 && tree.name(&tree.nodes[n]) == name)
                {
                    for &n in &open[depth..] {
                        tree.nodes[n].inner_end = at;
                        tree.nodes[n].end = close;
                    }
                    open.truncate(depth);
                }
                at = close;
                continue;
            }
            if html[at..].starts_with("<!") {
                at = close;
                continue;
            }
            let name = tag_name(&html[at + 1..close]).to_ascii_lowercase();
            if VOID.contains(&name.as_str()) {
                tree.push(at, close, close, close, parent, false);
                at = close;
            } else if name == "style" || name == "script" {
                let inner = html[close..]
                    .find("</")
                    .map_or(html.len(), |i| close + i);
                let end = tag_close(html, inner);
                tree.push(at, close, inner, end, parent, false);
                at = end;
            } else {
                let index = tree.push(at, close, html.len(), html.len(), parent, false);
                open.push(index);
                at = close;
            }
        }
        tree
    }

    fn push(
        &mut self,
        start: usize,
        tag_end: usize,
        inner_end: usize,
        end: usize,
        parent: usize,
        text: bool,
    ) -> usize {
        let index = self.nodes.len();
        self.nodes.push(Node {
            start,
            tag_end,
            inner_end,
            end,
            parent,
            text,
            first_text: None,
        });
        self.children.push(Vec::new());
        self.children[parent].push(index);
        index
    }

    /// The lower-case name of an element; empty for text.
    fn name(&self, node: &Node) -> &str {
        if node.text || node.tag_end == 0 {
            return "";
        }
        tag_name(&self.html[node.start + 1..node.tag_end])
    }

    /// The value of one attribute of an element.
    fn attribute(&self, node: &Node, name: &str) -> Option<&str> {
        let tag = &self.html[node.start..node.tag_end];
        let from = tag.find(&format!(" {name}=\""))? + name.len() + 3;
        let len = tag[from..].find('"')?;
        Some(&tag[from..from + len])
    }

    fn has_class(&self, node: &Node, class: &str) -> bool {
        self.attribute(node, "class")
            .is_some_and(|all| all.split_whitespace().any(|c| c == class))
    }

    /// Whether a run of text holds no words.
    fn blank(&self, index: usize) -> bool {
        let node = &self.nodes[index];
        is_blank(&self.html[node.start..node.end])
    }

    /// Whether a node puts something in front of the reader: words or a
    /// picture.
    fn visible(&self, node: &Node) -> bool {
        match node.text {
            true => !is_blank(&self.html[node.start..node.end]),
            false => self.name(node) == "img",
        }
    }

    /// The words of a node, tags left out, when it is short enough to be
    /// the line that says who wrote.
    fn words(&self, index: usize) -> Option<String> {
        let node = &self.nodes[index];
        let raw = &self.html[node.start..node.end];
        if raw.len() > 2000 {
            return None;
        }
        let mut out = String::new();
        let mut rest = raw;
        while let Some(open) = rest.find('<') {
            out.push_str(&rest[..open]);
            rest = &rest[tag_close(rest, open)..];
        }
        out.push_str(rest);
        Some(decode(&out))
    }

    /// Whether the element at `at` opens the history, and how.
    fn opens_history(&self, at: usize) -> Option<Kind> {
        let node = &self.nodes[at];
        if node.text {
            return None;
        }
        let name = self.name(node);
        let quote = |needs_attribution| Some(Kind::Quote { needs_attribution });
        if ["gmail_quote", "gmail_quote_container", "yahoo_quoted"]
            .iter()
            .any(|class| self.has_class(node, class))
        {
            return quote(false);
        }
        if name == "blockquote" {
            return quote(self.attribute(node, "type") != Some("cite"));
        }
        if matches!(
            self.attribute(node, "id"),
            Some("appendonsend" | "divRplyFwdMsg")
        ) || self.has_class(node, "mailrs-forwarded")
        {
            return Some(Kind::Header);
        }
        // The outermost element whose words start with a header's.
        let first = node.first_text?;
        if node.parent != 0 && self.nodes[node.parent].first_text == Some(first) {
            return None;
        }
        let text = &self.nodes[first];
        let words = decode(&self.html[text.start..text.end]);
        let words = words.trim_start();
        if forward_marker(words) {
            return Some(Kind::Header);
        }
        let from = ["From:", "De:"].iter().any(|w| words.starts_with(w));
        // 1200 bytes can end inside a character, such as a receipt's
        // figure space, so the cut moves back to where that one starts.
        let head = &self.html[node.start..self.html.floor_char_boundary(node.end.min(node.start + 1200))];
        let sent = ["Sent:", "Enviado:", "Enviada:", "Date:", "Data:"]
            .iter()
            .any(|w| head.contains(w));
        (from && sent).then_some(Kind::Header)
    }

    /// Where a quote's history starts: at the line above it that says who
    /// wrote, when there is one, or at the quote itself. A line that only
    /// ends in a colon counts unless `strict`, when it has to say "wrote"
    /// in one of the languages [`WROTE`] lists.
    fn attribution(&self, at: usize, strict: bool) -> usize {
        let says = |words: &str| match strict {
            true => says_wrote(words),
            false => ends_with_colon(words),
        };
        let node = &self.nodes[at];
        let siblings = &self.children[node.parent];
        let mut before = siblings[..siblings.iter().position(|&s| s == at).unwrap_or(0)]
            .iter()
            .rev()
            .copied()
            .skip_while(|&s| {
                let sibling = &self.nodes[s];
                (sibling.text && self.blank(s)) || self.name(sibling) == "br"
            })
            .peekable();
        let Some(&above) = before.peek() else {
            return node.start;
        };
        let above_node = &self.nodes[above];
        if !above_node.text && !INLINE.contains(&self.name(above_node)) {
            let says = self.has_class(above_node, "moz-cite-prefix")
                || self.words(above).is_some_and(|w| says(&w));
            return if says { above_node.start } else { node.start };
        }
        // A line of loose text and inline elements, back to a break.
        let mut from = node.start;
        let mut line = String::new();
        for sibling in before {
            let s = &self.nodes[sibling];
            if !s.text && !INLINE.contains(&self.name(s)) {
                break;
            }
            let Some(words) = self.words(sibling) else {
                break;
            };
            line.insert_str(0, &words);
            from = s.start;
        }
        match says(&line) && line.len() < 400 {
            true => from,
            false => node.start,
        }
    }

    /// Where a header's history starts: at the rule Outlook draws above
    /// it, when there is one.
    fn rule_above(&self, at: usize) -> usize {
        let node = &self.nodes[at];
        let siblings = &self.children[node.parent];
        let index = siblings.iter().position(|&s| s == at).unwrap_or(0);
        siblings[..index]
            .iter()
            .rev()
            .find(|&&s| !(self.nodes[s].text && self.blank(s)))
            .filter(|&&s| self.name(&self.nodes[s]) == "hr")
            .map_or(node.start, |&s| self.nodes[s].start)
    }
}

/// The name at the start of a tag's inside, such as `div` in `div class`.
fn tag_name(inside: &str) -> &str {
    let end = inside
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(inside.len());
    &inside[..end]
}

/// Just past the `>` that closes the tag starting at `at`, skipping any
/// `>` inside an attribute value. The sanitizer puts every value in
/// double quotes.
fn tag_close(html: &str, at: usize) -> usize {
    let mut quote = false;
    for (i, c) in html[at..].char_indices() {
        match (quote, c) {
            (false, '"') => quote = true,
            (true, '"') => quote = false,
            (false, '>') => return at + i + 1,
            _ => {}
        }
    }
    html.len()
}

fn is_blank(text: &str) -> bool {
    text.replace("&nbsp;", " ").trim().is_empty()
}

fn ends_with_colon(words: &str) -> bool {
    words.trim_end().ends_with(':')
}

/// How mail clients end the line above a quote, in the languages the
/// owner's mail comes in and their neighbours.
const WROTE: [&str; 9] = [
    "wrote:",
    "escreveu:",
    "escribió:",
    "a écrit :",
    "a écrit:",
    "schrieb:",
    "ha scritto:",
    "schreef:",
    "skrev:",
];

/// Whether a line says someone wrote what follows.
fn says_wrote(words: &str) -> bool {
    let words = words.trim_end().to_lowercase();
    WROTE.iter().any(|tail| words.ends_with(tail))
}

/// The few character references the checks above care about.
fn decode(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sanitize::sanitize_html;

    /// The history the page would fold in `html` after cleaning, and what
    /// stays shown before it.
    fn fold(html: &str) -> Option<(String, String)> {
        let clean = sanitize_html(html, None);
        let range = history_in_html(&clean)?;
        Some((
            clean[..range.start].to_string(),
            clean[range].to_string(),
        ))
    }

    #[test]
    fn gmail_folds_its_quote_with_the_line_that_says_who_wrote() {
        let (shown, hidden) = fold(
            "<div dir=\"ltr\">Sounds good, see you then.</div><br>\
             <div class=\"gmail_quote gmail_quote_container\"><div dir=\"ltr\" class=\"gmail_attr\">\
             On Fri, 25 Sept 2026 at 17:55, Ann Lee &lt;ann@example.com&gt; wrote:<br></div>\
             <blockquote class=\"gmail_quote\" style=\"margin:0px 0px 0px 0.8ex;\
             border-left:1px solid rgb(204,204,204);padding-left:1ex\">\
             <div dir=\"ltr\">Lunch on Monday?</div></blockquote></div>",
        )
        .expect("a fold");
        assert!(shown.contains("Sounds good") && !shown.contains("wrote:"), "{shown}");
        assert!(hidden.starts_with("<div class=\"gmail_quote"), "{hidden}");
        assert!(hidden.contains("Lunch on Monday?"), "{hidden}");
    }

    #[test]
    fn an_old_gmail_quote_takes_the_loose_line_above_it() {
        let (shown, hidden) = fold(
            "<div>Thanks!</div><div class=\"gmail_extra\"><br>On Mon, Ann Lee &lt;<a href=\"mailto:ann@example.com\">\
             ann@example.com</a>&gt; wrote:<br><blockquote class=\"gmail_quote\">Hi</blockquote></div>",
        )
        .expect("a fold");
        assert!(shown.ends_with("<br>"), "{shown}");
        assert!(hidden.starts_with("On Mon, Ann"), "{hidden}");
    }

    #[test]
    fn penguin_mail_folds_its_own_reply() {
        let (shown, hidden) = fold(
            "<div style=\"font-family:sans-serif\"><p style=\"margin:0 0 1em\">Monday works.</p>\
             <p style=\"margin:0 0 1em\">On Monday, 21 September 2026 at 09:00, Ann Lee \
             &lt;ann@example.com&gt; wrote:</p><blockquote style=\"margin:0 0 0 0.8ex;\
             border-left:2px solid #ccc;padding-left:1ex;color:#555\">\
             <p style=\"margin:0 0 1em\">Is Monday good?</p></blockquote></div>",
        )
        .expect("a fold");
        assert!(shown.contains("Monday works."), "{shown}");
        assert!(hidden.starts_with("<p style=\"margin:0 0 1em\">On Monday"), "{hidden}");
        assert!(hidden.ends_with("</blockquote>"), "{hidden}");
    }

    #[test]
    fn outlook_on_the_web_folds_from_its_marker() {
        let (shown, hidden) = fold(
            "<div dir=\"ltr\"><div>Approved.</div><div id=\"appendonsend\"></div>\
             <hr style=\"display:inline-block;width:98%\" tabindex=\"-1\">\
             <div id=\"divRplyFwdMsg\" dir=\"ltr\"><font face=\"Calibri\" style=\"font-size:11pt\" color=\"#000000\">\
             <b>From:</b> Ann Lee &lt;ann@example.com&gt;<br><b>Sent:</b> Friday, September 25, 2026 17:55<br>\
             <b>To:</b> Dana<br><b>Subject:</b> Budget</font><div>&nbsp;</div></div>\
             <div><p>Can you approve the budget?</p></div></div>",
        )
        .expect("a fold");
        assert!(shown.contains("Approved."), "{shown}");
        assert!(hidden.starts_with("<div id=\"appendonsend\">"), "{hidden}");
        assert!(hidden.contains("approve the budget"), "{hidden}");
    }

    #[test]
    fn outlook_on_the_desktop_folds_from_its_from_and_sent_lines() {
        let (shown, hidden) = fold(
            "<div class=\"WordSection1\"><p class=\"MsoNormal\">Approved.</p><p class=\"MsoNormal\">&nbsp;</p>\
             <div style=\"border:none;border-top:solid #E1E1E1 1.0pt;padding:3.0pt 0cm 0cm 0cm\">\
             <p class=\"MsoNormal\"><b>From:</b> Ann Lee<br><b>Sent:</b> Friday, 25 September 2026 17:55<br>\
             <b>To:</b> Dana<br><b>Subject:</b> Budget</p></div>\
             <p class=\"MsoNormal\">Can you approve?</p></div>",
        )
        .expect("a fold");
        assert!(shown.contains("Approved."), "{shown}");
        assert!(hidden.starts_with("<div style=\"border:none"), "{hidden}");
        assert!(hidden.contains("Can you approve?"), "{hidden}");
    }

    #[test]
    fn apple_mail_folds_its_cited_quote() {
        let (shown, hidden) = fold(
            "<div>Yes, Thursday.</div><div><br><blockquote type=\"cite\">\
             <div>On 25 Sep 2026, at 17:55, Ann Lee &lt;ann@example.com&gt; wrote:</div>\
             <br class=\"Apple-interchange-newline\"><div><div>Thursday?</div></div></blockquote></div>",
        )
        .expect("a fold");
        assert!(shown.contains("Yes, Thursday."), "{shown}");
        assert!(hidden.starts_with("<blockquote type=\"cite\">"), "{hidden}");
    }

    #[test]
    fn thunderbird_folds_its_quote_with_the_cite_prefix() {
        let (shown, hidden) = fold(
            "<p>Works for me.</p><div class=\"moz-cite-prefix\">On 25/09/2026 17:55, Ann Lee wrote:<br></div>\
             <blockquote type=\"cite\" cite=\"mid:1@example.com\">Friday?</blockquote>",
        )
        .expect("a fold");
        assert!(shown.contains("Works for me."), "{shown}");
        assert!(hidden.starts_with("<div class=\"moz-cite-prefix\">"), "{hidden}");
    }

    #[test]
    fn a_forward_with_a_note_above_it_folds() {
        let (shown, hidden) = fold(
            "<p>See below.</p><div>---------- Forwarded message ---------<br>From: Ann Lee<br>\
             Subject: Budget</div><p>The numbers for October.</p>",
        )
        .expect("a fold");
        assert!(shown.contains("See below."), "{shown}");
        assert!(hidden.starts_with("<div>---------- Forwarded"), "{hidden}");
        assert!(hidden.contains("October"), "{hidden}");
    }

    #[test]
    fn a_bare_forward_stays_whole() {
        assert_eq!(
            fold(
                "<div dir=\"ltr\"><div class=\"gmail_quote\"><div dir=\"ltr\" class=\"gmail_attr\">\
                 ---------- Forwarded message ---------<br>From: Ann Lee</div><div>Budget</div></div></div>"
            ),
            None
        );
    }

    #[test]
    fn an_answer_between_quotes_stays_whole() {
        assert_eq!(
            fold(
                "<div>On Friday, Ann wrote:</div><blockquote type=\"cite\">Lunch?</blockquote>\
                 <div>Yes.</div><blockquote type=\"cite\">Where?</blockquote><div>The usual place.</div>"
            ),
            None
        );
    }

    /// Whatever goes wrong in a search, the message still opens, unfolded.
    #[test]
    fn a_search_that_panics_folds_nothing() {
        let found: Option<usize> = unfolded_on_panic(|| panic!("a fault in the search"));
        assert_eq!(found, None);
        assert_eq!(unfolded_on_panic(|| Some(3)), Some(3));
    }

    /// A receipt full of figure spaces (U+2007, three bytes each) once
    /// cut the header check in the middle of one and panicked, so the
    /// message never opened, on this computer or after a restart.
    #[test]
    fn a_wide_character_where_the_header_check_stops_does_not_panic() {
        let spaces = "\u{2007}".repeat(600);
        let html = format!("<p>Your receipt</p><div>From: Accounts{spaces}</div>");
        // The header check reads 1200 bytes from the <div>. With these
        // lengths that lands inside a figure space.
        let div = html.find("<div>").expect("a div");
        assert!(!html.is_char_boundary(div + 1200));
        assert_eq!(history_in_html(&html), None);
    }

    #[test]
    fn a_quote_nobody_is_said_to_have_written_stays() {
        assert_eq!(
            fold("<p>As the poem goes:</p><blockquote>The fog comes on little cat feet.</blockquote>"),
            None
        );
    }

    #[test]
    fn plain_text_folds_the_quoted_lines_and_the_line_above_them() {
        let text = "Sounds good.\n\nOn Fri, 25 Sep 2026 at 17:55, Ann wrote:\n> Lunch?\n>\n> Ann\n";
        let at = history_in_text(text).expect("a fold");
        assert!(text[at..].starts_with("On Fri"), "{}", &text[at..]);
    }

    #[test]
    fn plain_text_folds_a_forward_under_a_note() {
        let text = "FYI\n\n---------- Forwarded message ---------\nFrom: Ann\n\nThe numbers";
        let at = history_in_text(text).expect("a fold");
        assert!(text[at..].starts_with("----------"), "{}", &text[at..]);
    }

    #[test]
    fn plain_text_with_answers_between_quotes_stays_whole() {
        assert_eq!(history_in_text("> Lunch?\nYes.\n> Where?\nThe usual place.\n"), None);
    }

    #[test]
    fn plain_text_that_is_all_quote_stays_whole() {
        assert_eq!(history_in_text("On Friday, Ann wrote:\n> Lunch?\n"), None);
    }
}
