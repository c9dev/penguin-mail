//! The page that asks how a Google account signs in: Add Account's page
//! after the Google tile.

use std::rc::Rc;

use adw::prelude::*;

use super::add_account::{banded, boxed_list, heading, label};
use crate::route_choice::{self, Route, RouteRow};
use mailrs_domain::translate::gettext;

/// The page's tag in the dialog's navigation view.
pub const TAG: &str = "routes";

pub struct RoutePage {
    pub page: adw::NavigationPage,
    list: gtk::ListBox,
    footer: gtk::Label,
}

impl RoutePage {
    pub fn new() -> RoutePage {
        let banded = banded(TAG, &route_choice::title());
        let title = heading(&route_choice::title());
        title.set_xalign(0.5);
        title.set_justify(gtk::Justification::Center);
        title.add_css_class("post-route-title");
        let list = boxed_list();
        list.add_css_class("post-routes");
        let footer = gtk::Label::builder()
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .justify(gtk::Justification::Center)
            .xalign(0.5)
            .visible(false)
            .css_classes(["post-route-foot"])
            .build();
        banded.body.append(&title);
        banded.body.append(&list);
        banded.body.append(&footer);
        // A page pushed on top of a focused button leaves the focus nowhere
        // in the view, so the first row takes it as the page shows.
        let first = list.downgrade();
        banded.page.connect_shown(move |_| {
            if let Some(row) = first.upgrade().and_then(|list| list.row_at_index(0)) {
                row.grab_focus();
            }
        });
        RoutePage {
            page: banded.page,
            list,
            footer,
        }
    }

    /// The first row, which takes the focus when the dialog opens here.
    pub fn first_row(&self) -> Option<gtk::ListBoxRow> {
        self.list.row_at_index(0)
    }

    /// Shows `rows`, with `footer` under them, and calls `picked` with the
    /// route of the row the person activates. A second call replaces the
    /// rows and the answer.
    pub fn show(
        &self,
        rows: &[RouteRow],
        footer: Option<String>,
        picked: impl Fn(Route) + 'static,
    ) {
        let picked: Rc<dyn Fn(Route)> = Rc::new(picked);
        self.list.remove_all();
        for shown in rows {
            let row = route_row(shown);
            let (route, picked) = (shown.route, Rc::clone(&picked));
            row.connect_activated(move |_| picked(route));
            self.list.append(&row);
        }
        self.footer.set_label(footer.as_deref().unwrap_or_default());
        self.footer.set_visible(footer.is_some());
    }
}

/// The page's tag in the dialog's navigation view.
pub const CONSENT_TAG: &str = "consent";

/// The page that offers a Gmail account its calendar and contacts after
/// its mail works.
pub struct ConsentPage {
    pub page: adw::NavigationPage,
    #[cfg_attr(not(test), expect(dead_code, reason = "only the checks read it"))]
    pub title: gtk::Label,
    #[cfg_attr(not(test), expect(dead_code, reason = "only the checks read it"))]
    pub body: gtk::Label,
    pub note: gtk::Label,
    pub continue_button: gtk::Button,
    pub not_now: gtk::Button,
}

impl ConsentPage {
    /// Shows the note about Google's warning screen only to an account
    /// whose consent goes through the built-in client. It stays hidden
    /// until the account is known.
    pub fn fit_note(&self, built_in: bool) {
        self.note.set_visible(built_in);
    }

    pub fn new() -> ConsentPage {
        let banded = banded(
            CONSENT_TAG,
            &gettext("Add your Google Calendar and contacts?"),
        );
        let centered = |label: &gtk::Label| {
            label.set_xalign(0.5);
            label.set_justify(gtk::Justification::Center);
        };
        // Translators: Google is a name; keep it as it is.
        let title = heading(&gettext("Add your Google Calendar and contacts?"));
        centered(&title);
        title.add_css_class("post-route-title");
        let body = label(
            // Translators: Google is a name; keep it as it is.
            &gettext(
                "Penguin Mail asks Google for your calendar, your contacts and the files it adds to Drive. Google will show its own screen. Your mail is already set up.",
            ),
            &["post-lede", "post-consent-text"],
        );
        centered(&body);
        let note = label(
            // Translators: Google and Penguin Mail are names; keep them as they are. The two
            // button names are the ones Google's own page shows.
            &gettext(
                "Google may say \"Google hasn't verified this app\". Click Advanced, then Go to Penguin Mail (unsafe).",
            ),
            &["post-lede", "post-consent-text", "post-consent-note"],
        );
        centered(&note);
        let not_now = gtk::Button::builder()
            .label(gettext("Not Now"))
            .css_classes(["post-consent-button"])
            .build();
        let continue_button = gtk::Button::builder()
            // Translators: Google is a name; keep it as it is.
            .label(gettext("Continue to Google"))
            .css_classes(["suggested-action", "post-consent-button"])
            .build();
        let buttons = gtk::Box::builder()
            .spacing(10)
            .halign(gtk::Align::Center)
            .margin_top(20)
            .build();
        buttons.append(&not_now);
        buttons.append(&continue_button);
        banded.body.append(&title);
        banded.body.append(&body);
        banded.body.append(&note);
        banded.body.append(&buttons);
        // A page pushed on top of a focused button leaves the focus nowhere.
        let first = continue_button.downgrade();
        banded.page.connect_shown(move |_| {
            if let Some(button) = first.upgrade() {
                button.grab_focus();
            }
        });
        ConsentPage {
            page: banded.page,
            title,
            body,
            note,
            continue_button,
            not_now,
        }
    }
}

/// One row: its words, and a chevron after them. Text from the row is
/// plain, never markup.
fn route_row(shown: &RouteRow) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .title(&shown.title)
        .subtitle(&shown.subtitle)
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    if shown.recommended {
        let tag = gtk::Label::builder()
            .label(gettext("Recommended"))
            .halign(gtk::Align::Start)
            .css_classes(["tag", "accent"])
            .build();
        match title_box(row.upcast_ref()) {
            Some(words) => words.prepend(&tag),
            // If libadwaita lays its row out another way, the tag goes
            // before the chevron instead of being lost.
            None => row.add_suffix(&tag),
        }
    }
    row
}

/// The box that holds a row's title and subtitle, which the tag sits
/// above.
fn title_box(row: &gtk::Widget) -> Option<gtk::Box> {
    fn find(widget: &gtk::Widget) -> Option<gtk::Widget> {
        let mut child = widget.first_child();
        while let Some(w) = child {
            if w.is::<gtk::Label>() && w.has_css_class("title") {
                return Some(w);
            }
            if let Some(found) = find(&w) {
                return Some(found);
            }
            child = w.next_sibling();
        }
        None
    }
    find(row)?.parent()?.downcast::<gtk::Box>().ok()
}

#[cfg(test)]
pub(crate) mod checks {
    use super::*;
    use crate::route_choice::{Goa, Platform, Ready, Route, footer, rows};
    use std::cell::RefCell;
    use std::rc::Rc;

    pub fn run() {
        the_rows_are_named_and_reachable_by_keyboard();
        a_mac_page_shows_the_tag_and_the_footer();
        an_address_with_an_ampersand_stays_plain();
        showing_again_replaces_the_rows_and_the_answer();
        the_consent_page_names_both_buttons();
        the_consent_page_shows_its_note();
    }

    fn everything() -> Ready {
        Ready {
            goa: Goa::Holds,
            saved_project: false,
        }
    }

    fn row_at(page: &RoutePage, i: i32) -> adw::ActionRow {
        page.list
            .row_at_index(i)
            .and_downcast::<adw::ActionRow>()
            .expect("an action row")
    }

    fn tags_in(widget: &gtk::Widget) -> Vec<String> {
        let mut found = Vec::new();
        let mut child = widget.first_child();
        while let Some(w) = child {
            if let Some(label) = w.downcast_ref::<gtk::Label>()
                && label.has_css_class("tag")
            {
                found.push(label.label().to_string());
            }
            found.extend(tags_in(&w));
            child = w.next_sibling();
        }
        found
    }

    fn the_rows_are_named_and_reachable_by_keyboard() {
        let page = RoutePage::new();
        let shown = rows(Platform::Linux, everything());
        let picked = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&picked);
        page.show(&shown, Some(footer()), move |route| {
            sink.borrow_mut().push(route)
        });
        assert!(page.list.row_at_index(2).is_none());
        for (i, wanted) in shown.iter().enumerate() {
            let row = row_at(&page, i as i32);
            assert_eq!(row.title(), wanted.title);
            assert!(row.is_activatable());
            assert!(row.is_focusable());
        }
        assert!(page.footer.get_visible());
        ActionRowExt::activate(&row_at(&page, 1));
        assert_eq!(*picked.borrow(), [Route::Goa]);
    }

    fn a_mac_page_shows_the_tag_and_the_footer() {
        let page = RoutePage::new();
        let ready = Ready {
            goa: Goa::Absent,
            ..everything()
        };
        let shown = rows(Platform::MacOs, ready);
        page.show(&shown, Some(footer()), |_| {});
        assert_eq!(tags_in(row_at(&page, 0).upcast_ref()), ["Recommended"]);
        assert!(tags_in(row_at(&page, 1).upcast_ref()).is_empty());
        let words = title_box(row_at(&page, 0).upcast_ref()).expect("the title box");
        let above = words.first_child().and_downcast::<gtk::Label>();
        assert_eq!(
            above.map(|l| l.label().to_string()).as_deref(),
            Some("Recommended")
        );
        assert!(page.footer.get_visible());
        assert_eq!(
            page.footer.label(),
            "Want every Gmail feature? Your own Google project gives you all of them, free."
        );
    }

    fn an_address_with_an_ampersand_stays_plain() {
        let page = RoutePage::new();
        let mut shown = rows(Platform::MacOs, everything());
        shown[0].title = "R&D <b>Team</b>".to_string();
        shown[0].subtitle = "a & b".to_string();
        page.show(&shown, None, |_| {});
        let row = row_at(&page, 0);
        assert!(!row.uses_markup());
        assert_eq!(row.title(), "R&D <b>Team</b>");
        assert_eq!(row.subtitle().as_deref(), Some("a & b"));
    }

    fn showing_again_replaces_the_rows_and_the_answer() {
        let page = RoutePage::new();
        let all = rows(Platform::Linux, everything());
        let first = Rc::new(RefCell::new(0));
        let counter = Rc::clone(&first);
        page.show(&all, None, move |_| *counter.borrow_mut() += 1);
        let second = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&second);
        page.show(&all[..1], None, move |route| sink.borrow_mut().push(route));
        assert!(page.list.row_at_index(1).is_none());
        assert!(!page.footer.get_visible());
        ActionRowExt::activate(&row_at(&page, 0));
        assert_eq!(*first.borrow(), 0);
        assert_eq!(*second.borrow(), [Route::OwnProject]);
    }

    fn the_consent_page_names_both_buttons() {
        let page = ConsentPage::new();
        assert_eq!(
            page.continue_button.label().as_deref(),
            Some("Continue to Google")
        );
        assert_eq!(page.not_now.label().as_deref(), Some("Not Now"));
        assert_eq!(page.title.label(), "Add your Google Calendar and contacts?");
        assert_eq!(
            page.body.label(),
            "Penguin Mail asks Google for your calendar, your contacts and the files it adds to Drive. Google will show its own screen. Your mail is already set up."
        );
    }

    fn the_consent_page_shows_its_note() {
        let page = ConsentPage::new();
        assert!(page.note.get_visible());
        page.fit_note(false);
        assert!(!page.note.get_visible());
        assert_eq!(
            page.note.label(),
            "Google may say \"Google hasn't verified this app\". Click Advanced, then Go to Penguin Mail (unsafe)."
        );
    }
}
