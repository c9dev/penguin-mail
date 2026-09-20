//! Preferences → Assistant: which model the assistant uses, and whether it
//! asks before acting.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_ai::{Model, ModelList, ProviderConfig};

use crate::app::App;
use crate::assistant::{self, ANTHROPIC_KEY, LOCAL_KEY};
use crate::settings::{AiChange, AiProvider, AiSettings, Change, Choice};
use mailrs_domain::translate::{fill, fill_plural, gettext};

/// Models a picker shows before it grows a search box.
const SEARCH_FROM: usize = 8;

pub fn page(app: &Rc<App>, dialog: &adw::PreferencesDialog) -> adw::PreferencesPage {
    let ai = app.settings().ai;
    let page = adw::PreferencesPage::builder()
        .title(gettext("Assistant"))
        .name("assistant")
        .icon_name("penguin-mail-sparkle-symbolic")
        .build();

    let model = adw::PreferencesGroup::builder()
        .title(gettext("Model"))
        .description(gettext(
            "Local models keep your mail on this computer. With Anthropic or a Claude \
             subscription, the mail the assistant reads goes to Anthropic.",
        ))
        .build();
    let labels: Vec<String> = AiProvider::ALL.iter().map(|p| p.label()).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let provider = adw::ComboRow::builder()
        .title(gettext("Provider"))
        .model(&gtk::StringList::new(&labels))
        .selected(ai.provider.index())
        .build();
    model.add(&provider);

    // A local or OpenAI-compatible server.
    let base_url = adw::EntryRow::builder()
        .title(gettext("Server Address"))
        .text(&ai.base_url)
        .show_apply_button(true)
        .build();
    let local_key = adw::PasswordEntryRow::builder()
        .title(gettext("API Key (Optional)"))
        .show_apply_button(true)
        .build();
    // Anthropic.
    let anthropic_key = adw::PasswordEntryRow::builder()
        .title(gettext("Anthropic API Key"))
        .show_apply_button(true)
        .build();
    // Every picker and the Test button read the fields through this, so
    // they ask what the dialog shows rather than what was last saved.
    let typed = Typed {
        base_url: base_url.clone(),
        local_key: local_key.clone(),
        anthropic_key: anthropic_key.clone(),
    };
    let local_model = model_row(
        app,
        &ai.local_model,
        Picker {
            provider: AiProvider::Local,
            default_label: None,
            change: AiChange::LocalModel,
        },
        typed.clone(),
    );

    let anthropic_model = model_row(
        app,
        &ai.anthropic_model,
        Picker {
            provider: AiProvider::Anthropic,
            default_label: None,
            change: AiChange::AnthropicModel,
        },
        typed.clone(),
    );
    // Claude Code.
    let found = assistant::find_claude();
    let claude = adw::ActionRow::builder()
        .title(gettext("Claude Code"))
        .subtitle(match &found {
            Some(path) => fill(
                &gettext("Uses your Claude subscription through {path}"),
                &[("path", &path.display().to_string())],
            ),
            None => gettext("Not found. Install Claude Code and sign in by running claude once."),
        })
        .build();
    let claude_model = model_row(
        app,
        &ai.claude_model,
        Picker {
            provider: AiProvider::ClaudeCode,
            default_label: Some(gettext("Claude Code's own default")),
            change: AiChange::ClaudeModel,
        },
        typed.clone(),
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
        .label(gettext("Test"))
        .valign(gtk::Align::Center)
        .build();
    let test_row = adw::ActionRow::builder()
        .title(gettext("Test the Connection"))
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
            toasts.add_toast(adw::Toast::new(&if key.trim().is_empty() {
                gettext("Removed the key")
            } else {
                gettext("Saved in the keyring")
            }));
            row.set_text("");
        });
    };
    save(LOCAL_KEY, &local_key);
    save(ANTHROPIC_KEY, &anthropic_key);
    if assistant::load_key(ANTHROPIC_KEY).is_some() {
        anthropic_key.set_title(&gettext("Anthropic API Key (Saved)"));
    }
    let (weak, toasts) = (Rc::downgrade(app), dialog.clone());
    let testing = typed.clone();
    test.connect_clicked(move |button| {
        let Some(app) = weak.upgrade() else { return };
        // Test what the dialog shows. Testing the saved address while the
        // person looks at a different one is how a working server gets
        // reported as broken.
        let provider = app.settings().ai.provider;
        let config = match testing.config(&app, provider) {
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
                Err(err) => fill(
                    &gettext("No answer: {reason}"),
                    &[("reason", &err.to_string())],
                ),
            }));
        });
    });

    page.add(&detected_group(app, &provider, &base_url, &local_model));

    let safety = adw::PreferencesGroup::builder()
        .title(gettext("Safety"))
        .build();
    let confirm = adw::SwitchRow::builder()
        .title(gettext("Ask Before Acting"))
        .subtitle(gettext(
            "Approve each message the assistant sends and each change to Gmail \
             settings, such as automatic replies and rules",
        ))
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

/// The fields the dialog shows, so the model list and the Test button ask
/// the server the person is looking at rather than the one last saved.
///
/// A Server Address is only saved when the apply button is pressed. Type a
/// new one, open the model list, and it used to ask the old address and
/// report that nothing was there, which is a confusing way to be told to
/// press a button.
#[derive(Clone)]
struct Typed {
    base_url: adw::EntryRow,
    local_key: adw::PasswordEntryRow,
    anthropic_key: adw::PasswordEntryRow,
}

impl Typed {
    /// The provider's settings with whatever is on screen written over
    /// them, ready to talk to.
    fn config(&self, app: &App, provider: AiProvider) -> Result<ProviderConfig, String> {
        self.built(app.settings().ai, provider)
    }

    /// The same, with a model name standing in, for asking a server what
    /// it offers before a model has been chosen.
    fn listing(&self, app: &App, provider: AiProvider) -> Result<ProviderConfig, String> {
        let mut ai = app.settings().ai;
        if ai.local_model.trim().is_empty() {
            ai.local_model = "list".into();
        }
        self.built(ai, provider)
    }

    fn built(&self, mut ai: AiSettings, provider: AiProvider) -> Result<ProviderConfig, String> {
        ai.provider = provider;
        let address = self.base_url.text().trim().to_string();
        if !address.is_empty() {
            ai.base_url = address;
        }
        // A key typed and not applied is still in its field: one that
        // reached the keyring clears it.
        let typed_key = match provider {
            AiProvider::Local => self.local_key.text().trim().to_string(),
            AiProvider::Anthropic => self.anthropic_key.text().trim().to_string(),
            _ => String::new(),
        };
        if provider == AiProvider::Anthropic && !typed_key.is_empty() {
            // `provider_config` refuses for want of a saved key before it
            // could be told about this one, so this answers in its place.
            return Ok(ProviderConfig::Anthropic {
                api_key: typed_key,
                model: ai.anthropic_model.trim().to_string(),
            });
        }
        let mut config = assistant::provider_config(&ai)?;
        if let ProviderConfig::OpenAiCompatible { api_key, .. } = &mut config
            && !typed_key.is_empty()
        {
            *api_key = Some(typed_key);
        }
        Ok(config)
    }
}

/// What one provider's model picker lists and what it saves.
#[derive(Clone)]
struct Picker {
    provider: AiProvider,
    /// The first entry, which empties the field. Claude Code picks its own
    /// model then; the other providers need a name.
    default_label: Option<String>,
    change: fn(String) -> AiChange,
}

/// The Model row every provider gets: a field you can type into, and a
/// picker listing what the provider can run.
fn model_row(app: &Rc<App>, current: &str, picker: Picker, typed: Typed) -> adw::EntryRow {
    let row = adw::EntryRow::builder()
        .title(gettext("Model"))
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
        .tooltip_text(gettext("Models You Can Use"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    let (weak, entry) = (Rc::downgrade(app), row.clone());
    pick.set_create_popup_func(move |button| {
        let Some(app) = weak.upgrade() else { return };
        let popover = gtk::Popover::builder().build();
        button.set_popover(Some(&popover));
        fill_popover(&app, &popover, &entry, picker.clone(), &typed);
    });
    row.add_suffix(&pick);
    row
}

/// Asks the provider what it offers and shows the answer in the popover.
fn fill_popover(
    app: &Rc<App>,
    popover: &gtk::Popover,
    entry: &adw::EntryRow,
    picker: Picker,
    typed: &Typed,
) {
    let search = gtk::SearchEntry::builder()
        .placeholder_text(gettext("Search models"))
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
        .max_width_chars(52)
        .css_classes(["dim-label", "caption"])
        .visible(false)
        .build();
    let waiting = gtk::Label::builder()
        .label(gettext("Asking for the model list…"))
        .css_classes(["dim-label"])
        .margin_top(8)
        .margin_bottom(8)
        .build();
    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        // The popover sizes itself to this box while the list is still
        // hidden, so the width has to be asked for here. Without it the
        // popover settles around the waiting label and every model name
        // wraps onto two lines for the rest of its life.
        .width_request(440)
        .spacing(8)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(8)
        .margin_end(8)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&list)
        .propagate_natural_height(true)
        .max_content_height(420)
        // Wide enough for a dated model id on one line, so the common
        // case does not wrap at all.
        .min_content_width(460)
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

    let config = typed.listing(app, picker.provider);
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
        show_models(
            &listed,
            &list,
            &search,
            &note,
            &entry,
            &popover,
            picker.clone(),
        );
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
        notes.push(gettext(
            "This provider lists no models. Type a name in the field instead.",
        ));
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
        // A model id runs long, and the default single line cuts the end
        // off, which is the half that tells two versions of one model
        // apart. Zero lines lets both wrap instead.
        .title_lines(0)
        .subtitle_lines(0)
        .build();
    if !model.id.is_empty() && model.id != *title {
        row.set_subtitle(&glib::markup_escape_text(&model.id));
    }
    if model.alias {
        row.add_suffix(
            &gtk::Label::builder()
                .label(gettext("alias"))
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
        .title(gettext("Found on This Computer"))
        .build();
    let looking = adw::ActionRow::builder()
        .title(gettext(
            "Looking for LM Studio, Ollama, Unsloth, and Claude Code…",
        ))
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
                    .title(gettext("Nothing found"))
                    .subtitle(gettext(
                        "Start LM Studio's server or Ollama, or install Claude Code, \
                         then open Preferences again.",
                    ))
                    .build(),
            );
            return;
        }
        for item in found {
            let subtitle = match item.models.len() {
                0 => String::new(),
                1 => item.models[0].clone(),
                n => fill_plural(
                    "{model} and {count} more",
                    "{model} and {count} more",
                    n - 1,
                    &[("model", &item.models[0]), ("count", &(n - 1).to_string())],
                ),
            };
            let row = adw::ActionRow::builder()
                .title(&item.label)
                .subtitle(&subtitle)
                .build();
            let use_it = gtk::Button::builder()
                .label(gettext("Use"))
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
