//! The find bar over an open conversation.
//!
//! WebKit does the finding, through the `FindController` of the view's
//! WebView. What is kept here is the query, the place among the matches,
//! and the words the count reads: WebKit counts the matches but never
//! says which one it highlighted, so the bar counts the steps itself.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use mailrs_domain::translate::{fill_plural, gettext};
use webkit::prelude::*;

use crate::ui::name;

/// How many matches WebKit may highlight and count. Nothing is capped.
const MATCH_LIMIT: u32 = u32::MAX;

/// Where a search stands: the query, how many matches the page holds,
/// and which one is highlighted.
#[derive(Default)]
struct Place {
    query: String,
    matches: usize,
    /// 1 to `matches`, and 0 until WebKit has found something.
    at: usize,
}

impl Place {
    /// Starts over on `query`, before WebKit has answered.
    fn start(&mut self, query: &str) {
        self.query = query.to_string();
        self.matches = 0;
        self.at = 0;
    }

    /// WebKit highlighted a match. It says nothing about which one, so
    /// this only settles the first: every later one was stepped to.
    fn found(&mut self) {
        self.matches = self.matches.max(1);
        self.at = self.at.max(1);
    }

    /// WebKit counted the matches on the page.
    fn counted(&mut self, matches: usize) {
        self.matches = matches;
        self.at = match matches {
            0 => 0,
            _ => self.at.clamp(1, matches),
        };
    }

    /// WebKit found nothing.
    fn missed(&mut self) {
        self.matches = 0;
        self.at = 0;
    }

    /// Moves to the next match, or the previous one, wrapping at the ends
    /// the way WebKit's own search does.
    fn step(&mut self, forward: bool) {
        if self.matches == 0 {
            return;
        }
        self.at = match forward {
            true => self.at % self.matches + 1,
            false => match self.at {
                0 | 1 => self.matches,
                at => at - 1,
            },
        };
    }

    /// What the bar says beside the entry.
    fn label(&self) -> String {
        if self.query.is_empty() {
            return String::new();
        }
        if self.matches == 0 {
            return gettext("No matches");
        }
        let values = [
            ("position", self.at.to_string()),
            ("total", self.matches.to_string()),
        ];
        let values: Vec<(&str, &str)> = values.iter().map(|(k, v)| (*k, v.as_str())).collect();
        fill_plural(
            "{position} of {total} match",
            "{position} of {total} matches",
            self.matches,
            &values,
        )
    }
}

/// Whether the query asks for the capitals it carries. Every editor reads
/// a lower-case query as either case and a capital as a capital, and
/// nobody has to be told.
fn case_sensitive(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

/// How WebKit should search for `query`.
fn options(query: &str) -> webkit::FindOptions {
    let mut options = webkit::FindOptions::WRAP_AROUND;
    if !case_sensitive(query) {
        options |= webkit::FindOptions::CASE_INSENSITIVE;
    }
    options
}

/// The entry, the count, and the two arrows, over one WebView.
pub struct FindBar {
    pub widget: gtk::SearchBar,
    entry: gtk::SearchEntry,
    count: gtk::Label,
    previous: gtk::Button,
    next: gtk::Button,
    find: webkit::FindController,
    place: RefCell<Place>,
    running: RefCell<Rc<dyn Fn(bool)>>,
}

impl FindBar {
    pub fn new(webview: &webkit::WebView) -> Rc<FindBar> {
        let entry = gtk::SearchEntry::builder()
            .placeholder_text(gettext("Find in the conversation"))
            .hexpand(true)
            .build();
        let count = gtk::Label::builder()
            .css_classes(["dim-label", "numeric"])
            .build();
        let arrow = |icon: &str, tip: String| {
            gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tip)
                .sensitive(false)
                .build()
        };
        let previous = arrow("go-up-symbolic", gettext("Previous Match"));
        let next = arrow("go-down-symbolic", gettext("Next Match"));
        name(&entry, &gettext("Find in the conversation"));
        name(&previous, &gettext("Previous Match"));
        name(&next, &gettext("Next Match"));
        // Nothing else moves between matches, so the arrows say which
        // keys do it as well.
        previous.update_property(&[gtk::accessible::Property::KeyShortcuts("Shift+Ctrl+G")]);
        next.update_property(&[gtk::accessible::Property::KeyShortcuts("Ctrl+G")]);
        let arrows = gtk::Box::builder().css_classes(["linked"]).build();
        arrows.append(&previous);
        arrows.append(&next);
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .build();
        row.append(&entry);
        row.append(&count);
        row.append(&arrows);
        let widget = gtk::SearchBar::builder()
            .child(&adw::Clamp::builder().maximum_size(520).child(&row).build())
            .show_close_button(false)
            .build();
        widget.connect_entry(&entry);

        let find = webview
            .find_controller()
            .expect("a WebView has a find controller");
        let this = Rc::new(FindBar {
            widget,
            entry,
            count,
            previous,
            next,
            find,
            place: RefCell::new(Place::default()),
            running: RefCell::new(Rc::new(|_| {})),
        });

        let weak = Rc::downgrade(&this);
        this.entry.connect_search_changed(move |_| {
            if let Some(bar) = weak.upgrade() {
                bar.search();
            }
        });
        for (button, forward) in [(&this.previous, false), (&this.next, true)] {
            let weak = Rc::downgrade(&this);
            button.connect_clicked(move |_| {
                if let Some(bar) = weak.upgrade() {
                    bar.step(forward);
                }
            });
        }
        // Enter, and the Ctrl+G that GTK binds inside a search entry.
        let weak = Rc::downgrade(&this);
        this.entry.connect_activate(move |_| {
            if let Some(bar) = weak.upgrade() {
                bar.step(true);
            }
        });
        let weak = Rc::downgrade(&this);
        this.entry.connect_next_match(move |_| {
            if let Some(bar) = weak.upgrade() {
                bar.step(true);
            }
        });
        let weak = Rc::downgrade(&this);
        this.entry.connect_previous_match(move |_| {
            if let Some(bar) = weak.upgrade() {
                bar.step(false);
            }
        });
        let weak = Rc::downgrade(&this);
        this.entry.connect_stop_search(move |_| {
            if let Some(bar) = weak.upgrade() {
                bar.close();
            }
        });
        let weak = Rc::downgrade(&this);
        this.find.connect_found_text(move |_, _| {
            if let Some(bar) = weak.upgrade() {
                bar.place.borrow_mut().found();
                bar.update();
            }
        });
        let weak = Rc::downgrade(&this);
        this.find.connect_counted_matches(move |_, matches| {
            if let Some(bar) = weak.upgrade() {
                bar.place.borrow_mut().counted(matches as usize);
                bar.update();
            }
        });
        let weak = Rc::downgrade(&this);
        this.find.connect_failed_to_find_text(move |_| {
            if let Some(bar) = weak.upgrade() {
                bar.place.borrow_mut().missed();
                bar.update();
            }
        });
        this
    }

    /// What to do when a search starts and when it stops. The
    /// conversation opens every message while one runs, since WebKit
    /// finds nothing in a message the stylesheet has hidden.
    pub fn on_running(&self, run: impl Fn(bool) + 'static) {
        *self.running.borrow_mut() = Rc::new(run);
    }

    pub fn is_open(&self) -> bool {
        self.widget.is_search_mode()
    }

    /// Puts the bar up and takes the focus into it. A query left from
    /// last time stays, selected, so typing replaces it.
    pub fn open(&self) {
        let opening = !self.is_open();
        self.widget.set_search_mode(true);
        if opening {
            self.tell_running(true);
        }
        self.entry.grab_focus();
        self.entry.select_region(0, -1);
        if opening && !self.entry.text().is_empty() {
            self.search();
        }
    }

    /// Takes the bar down and the highlight off the page.
    pub fn close(&self) {
        if !self.is_open() {
            return;
        }
        self.widget.set_search_mode(false);
        self.find.search_finish();
        self.place.borrow_mut().start("");
        self.update();
        self.tell_running(false);
    }

    /// Searches again after the page was redrawn under a running search,
    /// which leaves WebKit with nothing highlighted.
    pub fn refresh(&self) {
        if self.is_open() && !self.entry.text().is_empty() {
            self.search();
        }
    }

    /// Counts the matches again after some of the page was replaced under
    /// a running search. Searching again would move the highlight to the
    /// first match and the page with it; counting leaves both alone.
    pub fn recount(&self) {
        let query = self.entry.text().to_string();
        if self.is_open() && !query.is_empty() {
            self.find
                .count_matches(&query, options(&query).bits(), MATCH_LIMIT);
        }
    }

    fn search(&self) {
        let query = self.entry.text().to_string();
        self.place.borrow_mut().start(&query);
        match query.is_empty() {
            true => self.find.search_finish(),
            false => {
                let options = options(&query).bits();
                self.find.search(&query, options, MATCH_LIMIT);
                self.find.count_matches(&query, options, MATCH_LIMIT);
            }
        }
        self.update();
    }

    fn step(&self, forward: bool) {
        if self.entry.text().is_empty() {
            return;
        }
        self.place.borrow_mut().step(forward);
        match forward {
            true => self.find.search_next(),
            false => self.find.search_previous(),
        }
        self.update();
    }

    fn update(&self) {
        let place = self.place.borrow();
        let said = place.label();
        self.count.set_label(&said);
        // The count sits beside the entry as a label of its own, where a
        // reader stepping through matches never passes it. Hanging it off
        // the entry puts it where the focus already is.
        self.entry
            .update_property(&[gtk::accessible::Property::Description(&said)]);
        for arrow in [&self.previous, &self.next] {
            arrow.set_sensitive(place.matches > 0);
        }
    }

    /// The callback leaves the `RefCell` before it runs, since it reaches
    /// back into the conversation that owns this bar.
    fn tell_running(&self, running: bool) {
        let run = Rc::clone(&self.running.borrow());
        run(running);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn searched(query: &str, matches: usize) -> Place {
        let mut place = Place::default();
        place.start(query);
        place.found();
        place.counted(matches);
        place
    }

    #[test]
    fn a_capital_in_the_query_asks_for_that_capital() {
        assert!(!case_sensitive("receipt"));
        assert!(!case_sensitive("€12 rue"));
        assert!(case_sensitive("Receipt"));
        assert!(case_sensitive("iPad"));
        assert!(case_sensitive("Água"));
    }

    #[test]
    fn the_count_says_which_match_of_how_many() {
        assert_eq!(searched("rent", 12).label(), "1 of 12 matches");
        assert_eq!(searched("rent", 1).label(), "1 of 1 match");
        assert_eq!(searched("rent", 0).label(), "No matches");
        assert_eq!(Place::default().label(), "");
    }

    #[test]
    fn the_matches_wrap_around_at_both_ends() {
        let mut place = searched("rent", 3);
        place.step(true);
        place.step(true);
        assert_eq!(place.label(), "3 of 3 matches");
        place.step(true);
        assert_eq!(place.label(), "1 of 3 matches");
        place.step(false);
        assert_eq!(place.label(), "3 of 3 matches");
    }

    #[test]
    fn a_page_with_nothing_to_find_stays_where_it_is() {
        let mut place = Place::default();
        place.start("rent");
        place.missed();
        place.step(true);
        assert_eq!(place.label(), "No matches");
    }

    #[test]
    fn a_shorter_page_keeps_the_place_inside_it() {
        let mut place = searched("rent", 9);
        place.step(true);
        place.step(true);
        place.counted(2);
        assert_eq!(place.label(), "2 of 2 matches");
    }
}
