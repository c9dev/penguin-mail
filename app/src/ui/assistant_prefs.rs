//! Preferences → Assistant: which model the assistant uses, and whether it
//! asks before acting.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_ai::{Model, ModelList, ProviderConfig};

use crate::app::App;
use crate::assistant::{self, ANTHROPIC_KEY, LOCAL_KEY};
use crate::settings::{AiChange, AiProvider, Change, Choice};

/// Models a picker shows before it grows a search box.
const SEARCH_FROM: usize = 8;

pub fn page(app: &Rc<App>, dialog: &adw::PreferencesDialog) -> adw::PreferencesPage {
    let ai = app.settings().ai;
    let page = adw::PreferencesPage::builder()
        .title("Assistant")
        .name("assistant")
        .icon_name("penguin-mail-sparkle-symbolic")
        .build();

    let model = adw::PreferencesGroup::builder()
        .title("Model")
        .description("Local models keep your mail on this computer. With Anthropic or a Claude subscription, the mail the assistant reads goes to Anthropic.")
        .build();
    let labels: Vec<&str> = AiProvider::ALL.iter().map(|p| p.label()).collect();
    let provider = adw::ComboRow::builder()
        .title("Provider")
        .model(&gtk::StringList::new(&labels))
        .selected(ai.provider.index())
        .build();
    model.add(&provider);

    // A local or OpenAI-compatible server.
    let base_url = adw::EntryRow::builder()
        .title("Server Address")
        .text(&ai.base_url)
        .show_apply_button(true)
        .build();
    let local_key = adw::PasswordEntryRow::builder()
        .title("API Key (Optional)")
        .show_apply_button(true)
        .build();
    let local_model = model_row(
        app,
        &ai.local_model,
        Picker {
            provider: AiProvider::Local,
            default_label: None,
            change: AiChange::LocalModel,
        },
    );
    // Anthropic.
    let anthropic_key = adw::PasswordEntryRow::builder()
        .title("Anthropic API Key")
        .show_apply_button(true)
        .build();
    let anthropic_model = model_row(
        app,
        &ai.anthropic_model,
        Picker {
            provider: AiProvider::Anthropic,
            default_label: None,
            change: AiChange::AnthropicModel,
        },
    );
    // Claude Code.
    let found = assistant::find_claude();
    let claude = adw::ActionRow::builder()
        .title("Claude Code")
        .subtitle(match &found {
            Some(path) => format!("Uses your Claude subscription through {}", path.display()),
            None => "Not found. Install Claude Code and sign in by running claude once.".into(),
        })
        .build();
    let claude_model = model_row(
        app,
        &ai.claude_model,
        Picker {
            provider: AiProvider::ClaudeCode,
            default_label: Some("Claude Code's own default"),
            change: AiChange::ClaudeModel,
        },
    );
    for row in [
        base_url.upcast_ref::<gtk::Widget>(),
        local_key.upcast_ref(),
        local_model.upcast_ref(),
        anthropic_key.upcast_ref(),
        anthropic_model.upcast_ref(),
        claude.upcast_ref(),
        claude_model.upcast_ref(),
    ] {
        model.add(row);
    }
    let test = gtk::Button::builder()
        .label("Test")
        .valign(gtk::Align::Center)
        .build();
    let test_row = adw::ActionRow::builder()
        .title("Test the Connection")
        .build();
    test_row.add_suffix(&test);
    model.add(&test_row);
    page.add(&model);

    let show_rows = {
        let (base_url, local_key, local_model) =
            (base_url.clone(), local_key.clone(), local_model.clone());
        let (anthropic_key, anthropic_model) = (anthropic_key.clone(), anthropic_model.clone());
        let (claude, claude_model, test_row) =
            (claude.clone(), claude_model.clone(), test_row.clone());
        move |chosen: AiProvider| {
            let local = chosen == AiProvider::Local;
            let anthropic = chosen == AiProvider::Anthropic;
            let code = chosen == AiProvider::ClaudeCode;
            base_url.set_visible(local);
            local_key.set_visible(local);
            local_model.set_visible(local);
            anthropic_key.set_visible(anthropic);
            anthropic_model.set_visible(anthropic);
            claude.set_visible(code);
            claude_model.set_visible(code);
            test_row.set_visible(chosen != AiProvider::Off);
        }
    };
    show_rows(ai.provider);
    let weak = Rc::downgrade(app);
    provider.connect_selected_notify(move |row| {
        let chosen = AiProvider::from_index(row.selected());
        show_rows(chosen);
        if let Some(app) = weak.upgrade() {
            app.change_settings(Change::Ai(AiChange::Provider(chosen)));
        }
    });
    let weak = Rc::downgrade(app);
    base_url.connect_apply(move |row| {
        let url = row.text().trim().trim_end_matches('/').to_string();
        if let Some(app) = weak.upgrade() {
            app.change_settings(Change::Ai(AiChange::BaseUrl(url)));
        }
    });
    let toasts = dialog.clone();
    let save = move |name: &'static str, row: &adw::PasswordEntryRow| {
        let toasts = toasts.clone();
        row.connect_apply(move |row| {
            let key = row.text();
            assistant::save_key(name, &key);
            toasts.add_toast(adw::Toast::new(if key.trim().is_empty() {
                "Removed the key"
            } else {
                "Saved in the keyring"
            }));
            row.set_text("");
        });
    };
    save(LOCAL_KEY, &local_key);
    save(ANTHROPIC_KEY, &anthropic_key);
    if assistant::load_key(ANTHROPIC_KEY).is_some() {
        anthropic_key.set_title("Anthropic API Key (Saved)");
    }
    let (weak, toasts) = (Rc::downgrade(app), dialog.clone());
    test.connect_clicked(move |button| {
        let Some(app) = weak.upgrade() else { return };
        let config = match assistant::provider_config(&app.settings().ai) {
            Ok(config) => config,
            Err(problem) => return toasts.add_toast(adw::Toast::new(&problem)),
        };
        button.set_sensitive(false);
        let (button, toasts) = (button.clone(), toasts.clone());
        glib::spawn_future_local(async move {
            let result = app
                .core
                .call(async move { mailrs_ai::test(&config).await })
                .await;
            button.set_sensitive(true);
            toasts.add_toast(adw::Toast::new(&match result {
                Ok(answer) => answer,
                Err(err) => format!("No answer: {err}"),
            }));
        });
    });

    page.add(&detected_group(app, &provider, &base_url, &local_model));

    let safety = adw::PreferencesGroup::builder().title("Safety").build();
    let confirm = adw::SwitchRow::builder()
        .title("Ask Before Acting")
        .subtitle("Approve each message the assistant sends and each change to Gmail settings, such as automatic replies and rules")
        .active(ai.confirm_actions)
        .build();
    let weak = Rc::downgrade(app);
    confirm.connect_active_notify(move |row| {
        let on = row.is_active();
        if let Some(app) = weak.upgrade() {
            app.change_settings(Change::Ai(AiChange::ConfirmActions(on)));
        }
    });
    safety.add(&confirm);
    page.add(&safety);
    page
}

/// What one provider's model picker lists and what it saves.
#[derive(Clone, Copy)]
struct Picker {
    provider: AiProvider,
    /// The first entry, which empties the field. Claude Code picks its own
    /// model then; the other providers need a name.
    default_label: Option<&'static str>,
    change: fn(String) -> AiChange,
}

/// The Model row every provider gets: a field you can type into, and a
/// picker listing what the provider can run.
fn model_row(app: &Rc<App>, current: &str, picker: Picker) -> adw::EntryRow {
    let row = adw::EntryRow::builder()
        .title("Model")
        .text(current)
        .show_apply_button(true)
        .build();
    let weak = Rc::downgrade(app);
    row.connect_apply(move |row| {
        let model = row.text().trim().to_string();
        if let Some(app) = weak.upgrade() {
            app.change_settings(Change::Ai((picker.change)(model)));
        }
    });
    let pick = gtk::MenuButton::builder()
        .icon_name("pan-down-symbolic")
        .tooltip_text("Models You Can Use")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    let (weak, entry) = (Rc::downgrade(app), row.clone());
    pick.set_create_popup_func(move |button| {
        let Some(app) = weak.upgrade() else { return };
        let popover = gtk::Popover::builder().build();
        button.set_popover(Some(&popover));
        fill_popover(&app, &popover, &entry, picker);
    });
    row.add_suffix(&pick);
    row
}

/// Asks the provider what it offers and shows the answer in the popover.
fn fill_popover(app: &Rc<App>, popover: &gtk::Popover, entry: &adw::EntryRow, picker: Picker) {
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search models")
        .visible(false)
        .build();
    // Hidden until it has rows, so a failed list leaves no empty frame.
    let list = gtk::ListBox::builder()
        .css_classes(["boxed-list"])
        .selection_mode(gtk::SelectionMode::None)
        .visible(false)
        .build();
    let note = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .max_width_chars(36)
        .css_classes(["dim-label", "caption"])
        .visible(false)
        .build();
    let waiting = gtk::Label::builder()
        .label("Asking for the model list…")
        .css_classes(["dim-label"])
        .margin_top(8)
        .margin_bottom(8)
        .build();
    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(8)
        .margin_end(8)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&list)
        .propagate_natural_height(true)
        .max_content_height(360)
        .min_content_width(300)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .visible(false)
        .build();
    box_.append(&search);
    box_.append(&waiting);
    box_.append(&scroller);
    box_.append(&note);
    popover.set_child(Some(&box_));

    let query: Rc<RefCell<String>> = Rc::default();
    list.set_filter_func({
        let query = Rc::clone(&query);
        move |row| matches_query(row, &query.borrow())
    });
    search.connect_search_changed({
        let (query, list) = (Rc::clone(&query), list.clone());
        move |search| {
            *query.borrow_mut() = search.text().to_lowercase();
            list.invalidate_filter();
        }
    });

    let config = listing_config(app, picker.provider);
    let (app, entry, popover) = (Rc::clone(app), entry.clone(), popover.clone());
    glib::spawn_future_local(async move {
        let config = match config {
            Ok(config) => config,
            Err(problem) => {
                waiting.set_visible(false);
                note.set_label(&problem);
                note.set_visible(true);
                return;
            }
        };
        let found = app
            .core
            .call(async move { mailrs_ai::list_models(&config).await })
            .await;
        waiting.set_visible(false);
        let listed = match found {
            Ok(listed) => listed,
            Err(err) => {
                note.set_label(&sentence(&err));
                note.set_visible(true);
                return;
            }
        };
        show_models(&listed, &list, &search, &note, &entry, &popover, picker);
        scroller.set_visible(list.first_child().is_some());
    });
}

/// Fills the popover's list with the models, marking the one in use.
fn show_models(
    listed: &ModelList,
    list: &gtk::ListBox,
    search: &gtk::SearchEntry,
    note: &gtk::Label,
    entry: &adw::EntryRow,
    popover: &gtk::Popover,
    picker: Picker,
) {
    let chosen = entry.text().trim().to_string();
    let mut models: Vec<Model> = Vec::new();
    if let Some(label) = picker.default_label {
        models.push(Model::named(String::new(), label));
    }
    models.extend(listed.models.iter().cloned());
    for model in &models {
        let row = model_item(model, model.id == chosen);
        let (entry, popover, id) = (entry.clone(), popover.clone(), model.id.clone());
        row.connect_activated(move |_| {
            entry.set_text(&id);
            entry.emit_by_name::<()>("apply", &[]);
            popover.popdown();
        });
        list.append(&row);
    }
    list.set_visible(!models.is_empty());
    search.set_visible(models.len() > SEARCH_FROM);
    let mut notes: Vec<String> = Vec::new();
    if listed.models.is_empty() {
        notes.push("This provider lists no models. Type a name in the field instead.".into());
    }
    if let Some(from_provider) = &listed.note {
        notes.push(from_provider.clone());
    }
    if !notes.is_empty() {
        note.set_label(&notes.join(" "));
        note.set_visible(true);
    }
}

/// One model in the picker: its name, its id underneath, and a tick when the
/// field already holds it.
fn model_item(model: &Model, chosen: bool) -> adw::ActionRow {
    let title = if model.name.is_empty() {
        &model.id
    } else {
        &model.name
    };
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(title))
        .activatable(true)
        .build();
    if !model.id.is_empty() && model.id != *title {
        row.set_subtitle(&glib::markup_escape_text(&model.id));
    }
    if model.alias {
        row.add_suffix(
            &gtk::Label::builder()
                .label("alias")
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    if chosen {
        row.add_suffix(&gtk::Image::from_icon_name("object-select-symbolic"));
    }
    row
}

/// True when every word of the search text is in the row's name or id.
fn matches_query(row: &gtk::ListBoxRow, query: &str) -> bool {
    let Some(row) = row.downcast_ref::<adw::ActionRow>() else {
        return true;
    };
    let text = format!("{} {}", row.title(), row.subtitle().unwrap_or_default()).to_lowercase();
    query.split_whitespace().all(|word| text.contains(word))
}

/// The provider to ask for a model list: the saved settings, with the
/// provider the picker belongs to. Listing needs no model name, so a
/// placeholder stands in for an empty one.
fn listing_config(app: &App, provider: AiProvider) -> Result<ProviderConfig, String> {
    let mut ai = app.settings().ai;
    ai.provider = provider;
    if ai.local_model.trim().is_empty() {
        ai.local_model = "list".into();
    }
    assistant::provider_config(&ai)
}

/// An error as a sentence, since the errors start in lower case.
fn sentence(err: &anyhow::Error) -> String {
    let text = err.to_string();
    let mut chars = text.chars();
    let start: String = match chars.next() {
        Some(first) => first.to_uppercase().collect(),
        None => return text,
    };
    format!("{start}{}.", chars.as_str().trim_end_matches('.'))
}

/// Servers and Claude Code found on this computer, each with a Use button.
fn detected_group(
    app: &Rc<App>,
    provider: &adw::ComboRow,
    base_url: &adw::EntryRow,
    local_model: &adw::EntryRow,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Found on This Computer")
        .build();
    let looking = adw::ActionRow::builder()
        .title("Looking for LM Studio, Ollama, Unsloth, and Claude Code…")
        .build();
    group.add(&looking);
    let (app, group_ref) = (Rc::clone(app), group.clone());
    let (provider, base_url, local_model) =
        (provider.clone(), base_url.clone(), local_model.clone());
    glib::spawn_future_local(async move {
        let found = app
            .core
            .call(async { Ok::<_, anyhow::Error>(mailrs_ai::detect().await) })
            .await
            .unwrap_or_default();
        group_ref.remove(&looking);
        if found.is_empty() {
            group_ref.add(
                &adw::ActionRow::builder()
                    .title("Nothing found")
                    .subtitle("Start LM Studio's server or Ollama, or install Claude Code, then open Preferences again.")
                    .build(),
            );
            return;
        }
        for item in found {
            let subtitle = match item.models.len() {
                0 => String::new(),
                1 => item.models[0].clone(),
                n => format!("{} and {} more", item.models[0], n - 1),
            };
            let row = adw::ActionRow::builder()
                .title(&item.label)
                .subtitle(&subtitle)
                .build();
            let use_it = gtk::Button::builder()
                .label("Use")
                .valign(gtk::Align::Center)
                .build();
            let (app, provider, base_url, local_model) = (
                Rc::clone(&app),
                provider.clone(),
                base_url.clone(),
                local_model.clone(),
            );
            use_it.connect_clicked(move |_| {
                let config = item.config.clone();
                let first = item.models.first().cloned().unwrap_or_default();
                match config {
                    ProviderConfig::OpenAiCompatible { base_url: url, .. } => {
                        base_url.set_text(&url);
                        local_model.set_text(&first);
                        app.change_settings(Change::Ai(AiChange::LocalServer {
                            base_url: url,
                            model: first,
                        }));
                        provider.set_selected(AiProvider::Local.index());
                    }
                    ProviderConfig::Anthropic { api_key, .. } => {
                        assistant::save_key(ANTHROPIC_KEY, &api_key);
                        provider.set_selected(AiProvider::Anthropic.index());
                    }
                    ProviderConfig::ClaudeCode { command, .. } => {
                        app.change_settings(Change::Ai(AiChange::ClaudeCommand(
                            command.display().to_string(),
                        )));
                        provider.set_selected(AiProvider::ClaudeCode.index());
                    }
                }
            });
            row.add_suffix(&use_it);
            group_ref.add(&row);
        }
    });
    group
}
