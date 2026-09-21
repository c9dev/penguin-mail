//! Preferences → AI → Web Search: whether the assistant may search the web,
//! and which engine a local model searches with.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::app::App;
use crate::assistant::sources::web::{self, SearchEngine};
use crate::assistant::{self, BRAVE_KEY};
use crate::settings::{AiChange, Change, Choice, WebSearch};
use mailrs_domain::translate::{fill, gettext};

pub fn group(app: &Rc<App>) -> adw::PreferencesGroup {
    let ai = app.settings().ai;
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Web Search"))
        .description(gettext(
            "Lets the assistant search the web and read pages. Claude searches with \
             Anthropic's own tools. A local model searches with the engine chosen here, \
             which receives every query.",
        ))
        .build();

    let labels: Vec<String> = WebSearch::ALL.iter().map(|c| c.label()).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let choice = adw::ComboRow::builder()
        .title(gettext("Search With"))
        .model(&gtk::StringList::new(&labels))
        .selected(ai.web_search.index())
        .build();

    let saved = assistant::load_key(BRAVE_KEY).is_some();
    let brave_key = adw::PasswordEntryRow::builder()
        .title(brave_title(saved))
        .show_apply_button(true)
        .build();
    brave_key.connect_apply(|row| {
        let key = row.text();
        assistant::save_key(BRAVE_KEY, &key);
        row.set_title(&brave_title(!key.trim().is_empty()));
        row.set_text("");
    });

    let searxng = adw::EntryRow::builder()
        .title(gettext("SearXNG Server Address"))
        .text(&ai.searxng_url)
        .show_apply_button(true)
        .build();
    let weak = Rc::downgrade(app);
    searxng.connect_apply(move |row| {
        if let Some(app) = weak.upgrade() {
            app.change_settings(Change::Ai(AiChange::SearxngUrl(row.text().to_string())));
        }
    });

    let test = gtk::Button::builder()
        .label(gettext("Test"))
        .valign(gtk::Align::Center)
        .build();
    let test_row = adw::ActionRow::builder()
        .title(gettext("Test the Search"))
        .subtitle(gettext("Runs one search and shows the first result"))
        .build();
    test_row.add_suffix(&test);
    let weak = Rc::downgrade(app);
    let (shown, typed) = (test_row.clone(), searxng.clone());
    test.connect_clicked(move |button| {
        let Some(app) = weak.upgrade() else { return };
        // Test the address the dialog shows, applied or not.
        let mut ai = app.settings().ai;
        ai.searxng_url = typed.text().to_string();
        let engine = match SearchEngine::chosen(&ai, assistant::load_key(BRAVE_KEY)) {
            Ok(engine) => engine,
            Err(problem) => return shown.set_subtitle(&glib::markup_escape_text(&problem)),
        };
        button.set_sensitive(false);
        shown.set_subtitle(&gettext("Searching…"));
        let (button, shown) = (button.clone(), shown.clone());
        glib::spawn_future_local(async move {
            let result = app
                .core
                .call(async move { web::test(engine).await.map_err(anyhow::Error::msg) })
                .await;
            button.set_sensitive(true);
            let said = match result {
                Ok(title) => fill(&gettext("First result: {title}"), &[("title", &title)]),
                Err(problem) => fill(
                    &gettext("No answer: {reason}"),
                    &[("reason", &problem.to_string())],
                ),
            };
            shown.set_subtitle(&glib::markup_escape_text(&said));
        });
    });

    // Each engine shows only the rows that set it up.
    let show = {
        let (brave_key, searxng, test_row) = (brave_key.clone(), searxng.clone(), test_row.clone());
        move |on: WebSearch| {
            brave_key.set_visible(on == WebSearch::Brave);
            searxng.set_visible(on == WebSearch::Searxng);
            test_row.set_visible(matches!(on, WebSearch::Brave | WebSearch::Searxng));
        }
    };
    show(ai.web_search);
    let weak = Rc::downgrade(app);
    choice.connect_selected_notify(move |row| {
        let on = WebSearch::from_index(row.selected());
        show(on);
        if let Some(app) = weak.upgrade() {
            app.change_settings(Change::Ai(AiChange::WebSearch(on)));
        }
    });

    group.add(&choice);
    group.add(&brave_key);
    group.add(&searxng);
    group.add(&test_row);
    group
}

fn brave_title(saved: bool) -> String {
    match saved {
        true => gettext("Brave Search API Key (Saved)"),
        false => gettext("Brave Search API Key"),
    }
}
