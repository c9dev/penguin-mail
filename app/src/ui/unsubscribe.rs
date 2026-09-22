//! The one dialog that asks before Penguin Mail leaves a mailing list.
//!
//! It takes one line or twenty, so the Unsubscribe button and the
//! assistant ask the same question in the same words. Each line names a
//! list and says what leaving it will do: a request to the sender, a
//! mail from the person's own address, or a button pressed on the
//! sender's page with the address the newsletter was sent to. Reading a
//! page takes up to twenty seconds, so a line that is still being read
//! carries a spinner and fills itself in when the read ends, and
//! Unsubscribe stays insensitive until every line has settled.
//!
//! The words are this module's, translated; [`crate::unsubscribe_page`]
//! keeps its outcomes in plain English for the log and the assistant.
//! The text half is [`line_text`], [`summary`] and [`mask`], which are
//! sentences about plain data and are tested as such.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use futures::future::{Either, select};
use gtk::glib;
use mailrs_domain::translate::{fill, fill_plural, gettext};

use crate::unsubscribe_page::{Outcome, Prepared, Step};

/// One list on the dialog, and what leaving it will do.
pub struct ListLine {
    /// The sender as a person reads it, "Trail Notes".
    pub name: String,
    pub way: Way,
}

/// The way out of one list, as far as it is known.
pub enum Way {
    /// The page is still being read. The line carries a spinner until
    /// one of the others arrives to take its place.
    Reading,
    /// The sender promised RFC 8058, and one request is the whole of it.
    OneClick,
    /// A request goes out as mail from `from`.
    Mail { from: String },
    /// The page was read and decided on.
    Page(Prepared),
}

/// The response that goes ahead. Cancel, Escape and closing the dialog
/// all answer something else, and none of them submits anything.
const GO: &str = "go";

/// Asks about `lines`, filling each of them in as `updates` says what a
/// page turned out to hold. Answers the ticked lines, each with the way
/// it settled on, or nothing when the person said no.
///
/// The caller keeps whatever else it knows about a list in the same
/// order, since the index of a line is what comes back with it.
pub async fn confirm(
    parent: &impl IsA<gtk::Widget>,
    lines: Vec<ListLine>,
    updates: async_channel::Receiver<(usize, Way)>,
) -> Option<Vec<(usize, Way)>> {
    let dialog = adw::AlertDialog::new(Some(&heading(&lines)), body(&lines).as_deref());
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let mut rows = Vec::with_capacity(lines.len());
    let ways = Rc::new(RefCell::new(Vec::with_capacity(lines.len())));
    for line in lines {
        let check = gtk::CheckButton::builder().active(true).build();
        // The row's title is the list's name, and a checkbox beside it
        // carries no words of its own, so a screen reader is told the
        // name here too.
        crate::ui::name(&check, &line.name);
        let spinner = adw::Spinner::builder()
            .visible(matches!(line.way, Way::Reading))
            .build();
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&line.name))
            .subtitle(glib::markup_escape_text(&line_text(&line.way)))
            .activatable_widget(&check)
            .build();
        row.add_prefix(&check);
        row.add_suffix(&spinner);
        list.append(&row);
        rows.push(Line {
            check,
            row,
            spinner,
        });
        ways.borrow_mut().push(line.way);
    }
    dialog.set_extra_child(Some(&list));
    dialog.add_responses(&[
        ("cancel", &gettext("Cancel")),
        (GO, &gettext("Unsubscribe")),
    ]);
    dialog.set_response_appearance(GO, adw::ResponseAppearance::Suggested);
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some(GO));
    dialog.set_response_enabled(GO, !anyone_reading(&ways.borrow()));

    let rows = Rc::new(rows);
    let settling = settle(dialog.clone(), Rc::clone(&rows), Rc::clone(&ways), updates);
    let answering = dialog.choose_future(Some(parent));
    futures::pin_mut!(settling, answering);
    let answer = match select(answering, settling).await {
        // The person answered while a page was still loading. The read
        // goes on until the run that started it drops the browser.
        Either::Left((answer, _)) => answer,
        Either::Right((_, answering)) => answering.await,
    };
    if answer != GO {
        return None;
    }
    let ways = std::mem::take(&mut *ways.borrow_mut());
    Some(
        ways.into_iter()
            .enumerate()
            .filter(|(at, _)| rows[*at].check.is_active())
            .collect(),
    )
}

/// One line's widgets, kept so that a page arriving late can fill the
/// line in.
struct Line {
    check: gtk::CheckButton,
    row: adw::ActionRow,
    spinner: adw::Spinner,
}

/// Fills lines in as their pages arrive, and lets Unsubscribe go
/// sensitive once none is left reading. It ends when whoever is reading
/// the pages has nothing more to send.
async fn settle(
    dialog: adw::AlertDialog,
    rows: Rc<Vec<Line>>,
    ways: Rc<RefCell<Vec<Way>>>,
    updates: async_channel::Receiver<(usize, Way)>,
) {
    while let Ok((at, way)) = updates.recv().await {
        let Some(line) = rows.get(at) else {
            continue;
        };
        line.row
            .set_subtitle(&glib::markup_escape_text(&line_text(&way)));
        line.spinner.set_visible(false);
        let mut ways = ways.borrow_mut();
        if let Some(held) = ways.get_mut(at) {
            *held = way;
        }
        dialog.set_response_enabled(GO, !anyone_reading(&ways));
    }
}

fn anyone_reading(ways: &[Way]) -> bool {
    ways.iter().any(|way| matches!(way, Way::Reading))
}

/// The question at the top. One list is named; several are counted.
fn heading(lines: &[ListLine]) -> String {
    match lines {
        [only] => fill(
            &gettext("Unsubscribe from {sender}?"),
            &[("sender", &only.name)],
        ),
        many => fill_plural(
            "Unsubscribe from {count} list?",
            "Unsubscribe from {count} lists?",
            many.len(),
            &[("count", &many.len().to_string())],
        ),
    }
}

/// The line under the question, for a dialog that will load a page.
/// Loading one is an act the sender can see, which is worth saying
/// before it happens rather than after.
fn body(lines: &[ListLine]) -> Option<String> {
    let page = lines
        .iter()
        .any(|line| matches!(line.way, Way::Reading | Way::Page(_)));
    page.then(|| gettext("Loading a page tells the sender you acted, as opening it yourself does."))
}

/// What one line says will happen, under the list's name.
pub fn line_text(way: &Way) -> String {
    match way {
        Way::Reading => gettext("Reading the page…"),
        Way::OneClick => gettext("ask the sender to take you off the list"),
        Way::Mail { from } => fill(
            &gettext("send a request from {address}"),
            &[("address", &mask(from))],
        ),
        Way::Page(prepared) => page_text(prepared),
    }
}

/// What a page that has been read will have done to it. A page the rules
/// and the model both gave up on is the one the person finishes
/// themselves, in their own browser, as they did before any of this.
fn page_text(prepared: &Prepared) -> String {
    let plan = match &prepared.step {
        Step::AlreadyOff => return gettext("nothing: the page says you are off the list already"),
        Step::Browser(_) => return gettext("open the page in the browser"),
        Step::Submit(plan) => plan,
    };
    let button = &prepared.button;
    let site = site(&prepared.url);
    let address = mask(&prepared.address);
    let values = [
        ("button", button.as_str()),
        ("site", site.as_str()),
        ("address", address.as_str()),
    ];
    match (!plan.fill.is_empty(), !plan.tick.is_empty()) {
        (true, true) => fill(
            &gettext("press “{button}” on {site} with {address}, all emails"),
            &values,
        ),
        (true, false) => fill(
            &gettext("press “{button}” on {site} with {address}"),
            &values,
        ),
        (false, true) => fill(&gettext("press “{button}” on {site}, all emails"), &values),
        (false, false) => fill(&gettext("press “{button}” on {site}"), &values),
    }
}

/// The toast after the ticked lists have run. One list is named, since
/// the person just read its name on the dialog; several are counted,
/// with whatever still wants them counted apart.
pub fn summary(outcomes: &[(String, Outcome)]) -> String {
    if let [(name, only)] = outcomes {
        return match only {
            Outcome::Done => fill(&gettext("Unsubscribed from {sender}"), &[("sender", name)]),
            Outcome::Unclear => gettext("Sent, but the page did not say it worked"),
            Outcome::OpenInBrowser(_) => gettext("The page needs you to finish it"),
            Outcome::Failed(why) => fill(
                &gettext("Could not unsubscribe from {sender}: {reason}"),
                &[("sender", name), ("reason", why)],
            ),
        };
    }
    let done = outcomes
        .iter()
        .filter(|(_, outcome)| *outcome == Outcome::Done)
        .count();
    let left = outcomes.len() - done;
    match (done, left) {
        (_, 0) => fill_plural(
            "Unsubscribed from {count} list",
            "Unsubscribed from {count} lists",
            done,
            &[("count", &done.to_string())],
        ),
        (0, _) => fill_plural(
            "{count} list needs you",
            "{count} lists need you",
            left,
            &[("count", &left.to_string())],
        ),
        _ => fill_plural(
            "Unsubscribed from {done}. {count} needs you",
            "Unsubscribed from {done}. {count} need you",
            left,
            &[("done", &done.to_string()), ("count", &left.to_string())],
        ),
    }
}

/// An address with all but its first letter hidden, "d…@gmail.com". The
/// dialog says which address goes into a page, and the whole of it
/// belongs on screen no more than a password does.
pub fn mask(address: &str) -> String {
    let Some((who, domain)) = address.split_once('@') else {
        return address.to_string();
    };
    let mut letters = who.chars();
    match letters.next() {
        Some(first) if letters.next().is_some() => format!("{first}…@{domain}"),
        _ => address.to_string(),
    }
}

/// The site a page belongs to, "news.shop.com". The scheme, the path and
/// the token an unsubscribe link carries say nothing a person reading a
/// dialog wants.
fn site(url: &str) -> String {
    let rest = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
    let host = match host.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => host,
    };
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// The address a newsletter was sent to: the first of `to_and_cc` the
/// account may send mail as, and the account's own address when none of
/// them is. It is the only text Penguin Mail types into a page, so it
/// has to be one of the person's own addresses rather than whatever the
/// message happens to name.
pub fn sent_to(to_and_cc: &[String], send_as: &[String], account: &str) -> String {
    to_and_cc
        .iter()
        .find(|address| {
            send_as
                .iter()
                .any(|mine| mine.eq_ignore_ascii_case(address))
        })
        .cloned()
        .unwrap_or_else(|| account.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unsubscribe_page::Plan;

    fn prepared(step: Step, button: &str) -> Prepared {
        Prepared {
            url: "https://news.shop.com/u/9f2?t=abc".to_string(),
            address: "dana@gmail.com".to_string(),
            button: button.to_string(),
            step,
        }
    }

    fn plan(fill: bool, tick: bool) -> Step {
        Step::Submit(Plan {
            form: 0,
            fill: match fill {
                true => vec![(1, "dana@gmail.com".to_string())],
                false => Vec::new(),
            },
            tick: match tick {
                true => vec![2],
                false => Vec::new(),
            },
            press: 3,
        })
    }

    #[test]
    fn a_page_line_names_the_button_the_site_and_the_address() {
        assert_eq!(
            line_text(&Way::Page(prepared(plan(true, true), "Unsubscribe"))),
            "press “Unsubscribe” on news.shop.com with d…@gmail.com, all emails"
        );
        assert_eq!(
            line_text(&Way::Page(prepared(plan(true, false), "Confirm"))),
            "press “Confirm” on news.shop.com with d…@gmail.com"
        );
        assert_eq!(
            line_text(&Way::Page(prepared(plan(false, false), "Unsubscribe"))),
            "press “Unsubscribe” on news.shop.com"
        );
    }

    #[test]
    fn a_page_nobody_could_read_says_it_goes_to_the_browser() {
        let url = "https://news.shop.com/u/9f2".to_string();
        assert_eq!(
            line_text(&Way::Page(prepared(Step::Browser(url), ""))),
            "open the page in the browser"
        );
        assert_eq!(
            line_text(&Way::Page(prepared(Step::AlreadyOff, ""))),
            "nothing: the page says you are off the list already"
        );
    }

    #[test]
    fn the_other_two_ways_out_say_what_they_do() {
        assert_eq!(
            line_text(&Way::OneClick),
            "ask the sender to take you off the list"
        );
        assert_eq!(
            line_text(&Way::Mail {
                from: "dana@gmail.com".to_string()
            }),
            "send a request from d…@gmail.com"
        );
        assert_eq!(line_text(&Way::Reading), "Reading the page…");
    }

    #[test]
    fn an_address_keeps_its_first_letter_and_its_domain() {
        assert_eq!(mask("dana@gmail.com"), "d…@gmail.com");
        // Nothing worth hiding, and nothing to hide it behind.
        assert_eq!(mask("d@gmail.com"), "d@gmail.com");
        assert_eq!(mask("not an address"), "not an address");
    }

    #[test]
    fn a_site_is_the_host_without_the_rest_of_the_link() {
        assert_eq!(
            site("https://www.news.shop.com/u/9f2?t=abc"),
            "news.shop.com"
        );
        assert_eq!(site("http://list.example:8080/out"), "list.example");
        assert_eq!(site("https://news.shop.com"), "news.shop.com");
    }

    #[test]
    fn one_list_is_named_and_several_are_counted() {
        let done = |name: &str| (name.to_string(), Outcome::Done);
        assert_eq!(
            summary(&[done("Trail Notes")]),
            "Unsubscribed from Trail Notes"
        );
        assert_eq!(
            summary(&[done("Trail Notes"), done("Shop News")]),
            "Unsubscribed from 2 lists"
        );
    }

    #[test]
    fn whatever_still_wants_the_person_is_counted_apart() {
        let done = ("Trail Notes".to_string(), Outcome::Done);
        let left = (
            "Shop News".to_string(),
            Outcome::OpenInBrowser("https://shop.example/u".to_string()),
        );
        assert_eq!(
            summary(&[
                done.clone(),
                ("Old Forum".to_string(), Outcome::Done),
                left.clone()
            ]),
            "Unsubscribed from 2. 1 needs you"
        );
        assert_eq!(
            summary(&[left.clone(), ("Old Forum".to_string(), Outcome::Unclear)]),
            "2 lists need you"
        );
    }

    #[test]
    fn one_list_that_did_not_work_says_which_and_why() {
        let name = "Trail Notes".to_string();
        assert_eq!(
            summary(&[(
                name.clone(),
                Outcome::Failed("the page took longer than 20 seconds".into())
            )]),
            "Could not unsubscribe from Trail Notes: the page took longer than 20 seconds"
        );
        assert_eq!(
            summary(&[(name, Outcome::Unclear)]),
            "Sent, but the page did not say it worked"
        );
    }

    #[test]
    fn the_address_a_newsletter_came_to_is_one_of_the_persons_own() {
        let mine = [
            "dana@gmail.com".to_string(),
            "dana@studio.example".to_string(),
        ];
        let sent = [
            "newsletter@shop.example".to_string(),
            "DANA@studio.example".to_string(),
        ];
        assert_eq!(
            sent_to(&sent, &mine, "dana@gmail.com"),
            "DANA@studio.example"
        );
        // A list that carries nobody's address in the open falls back to
        // the account it arrived in.
        let hidden = ["undisclosed-recipients:;".to_string()];
        assert_eq!(sent_to(&hidden, &mine, "dana@gmail.com"), "dana@gmail.com");
    }
}
