//! Preferences → Assistant: which model the assistant uses, and whether it
//! asks before acting.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_ai::ProviderConfig;

use crate::app::App;
use crate::assistant::{self, ANTHROPIC_KEY, LOCAL_KEY};
use crate::settings::{AiChange, AiProvider, Change, Choice};

/// Claude Code's model aliases, with what the menu shows.
const CLAUDE_MODELS: [(&str, &str); 4] = [
    ("", "Default"),
    ("opus", "Opus"),
    ("sonnet", "Sonnet"),
    ("haiku", "Haiku"),
];

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
    let local_model = model_row(app, dialog, "Model", &ai.local_model, |s| {
        s.ai.provider = AiProvider::Local;
    });
    // Anthropic.
    let anthropic_key = adw::PasswordEntryRow::builder()
        .title("Anthropic API Key")
        .show_apply_button(true)
        .build();
    let anthropic_model = model_row(app, dialog, "Model", &ai.anthropic_model, |s| {
        s.ai.provider = AiProvider::Anthropic;
    });
    // Claude Code.
    let found = assistant::find_claude();
    let claude = adw::ActionRow::builder()
        .title("Claude Code")
        .subtitle(match &found {
            Some(path) => format!("Uses your Claude subscription through {}", path.display()),
            None => "Not found. Install Claude Code and sign in by running claude once.".into(),
        })
        .build();
    let claude_names: Vec<&str> = CLAUDE_MODELS.iter().map(|(_, n)| *n).collect();
    let claude_model = adw::ComboRow::builder()
        .title("Model")
        .model(&gtk::StringList::new(&claude_names))
        .selected(
            CLAUDE_MODELS
                .iter()
                .position(|(alias, _)| *alias == ai.claude_model)
                .unwrap_or(0) as u32,
        )
        .build();
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
    let weak = Rc::downgrade(app);
    local_model.connect_apply(move |row| {
        let model = row.text().trim().to_string();
        if let Some(app) = weak.upgrade() {
            app.change_settings(Change::Ai(AiChange::LocalModel(model)));
        }
    });
    let weak = Rc::downgrade(app);
    anthropic_model.connect_apply(move |row| {
        let model = row.text().trim().to_string();
        if let Some(app) = weak.upgrade() {
            app.change_settings(Change::Ai(AiChange::AnthropicModel(model)));
        }
    });
    let weak = Rc::downgrade(app);
    claude_model.connect_selected_notify(move |row| {
        let alias = CLAUDE_MODELS
            .get(row.selected() as usize)
            .map(|(a, _)| a.to_string())
            .unwrap_or_default();
        if let Some(app) = weak.upgrade() {
            app.change_settings(Change::Ai(AiChange::ClaudeModel(alias)));
        }
    });
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

/// A model name row with a menu of the models the server offers.
fn model_row(
    app: &Rc<App>,
    dialog: &adw::PreferencesDialog,
    title: &str,
    current: &str,
    choose_provider: fn(&mut crate::settings::Settings),
) -> adw::EntryRow {
    let row = adw::EntryRow::builder()
        .title(title)
        .text(current)
        .show_apply_button(true)
        .build();
    let pick = gtk::MenuButton::builder()
        .icon_name("pan-down-symbolic")
        .tooltip_text("Models on the Server")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    let (weak, entry, toasts) = (Rc::downgrade(app), row.clone(), dialog.clone());
    pick.set_create_popup_func(move |button| {
        let list = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar"])
            .selection_mode(gtk::SelectionMode::None)
            .build();
        let waiting = gtk::Label::builder()
            .label("Asking the server…")
            .margin_top(10)
            .margin_bottom(10)
            .margin_start(10)
            .margin_end(10)
            .build();
        list.append(&waiting);
        let popover = gtk::Popover::builder()
            .child(
                &gtk::ScrolledWindow::builder()
                    .child(&list)
                    .propagate_natural_height(true)
                    .max_content_height(320)
                    .min_content_width(260)
                    .hscrollbar_policy(gtk::PolicyType::Never)
                    .build(),
            )
            .build();
        button.set_popover(Some(&popover));
        let Some(app) = weak.upgrade() else { return };
        let mut settings = app.settings();
        choose_provider(&mut settings);
        // The model is what the menu picks; any name will do for listing.
        if settings.ai.local_model.is_empty() {
            settings.ai.local_model = "list".into();
        }
        let config: Result<ProviderConfig, String> = assistant::provider_config(&settings.ai);
        let (entry, toasts, popover, list) =
            (entry.clone(), toasts.clone(), popover.clone(), list.clone());
        glib::spawn_future_local(async move {
            let config = match config {
                Ok(config) => config,
                Err(problem) => {
                    waiting.set_label(&problem);
                    return;
                }
            };
            let models = app
                .core
                .call(async move { mailrs_ai::list_models(&config).await })
                .await;
            list.remove(&waiting);
            match models {
                Ok(models) if !models.is_empty() => {
                    for name in models {
                        let item = gtk::Button::builder()
                            .label(&name)
                            .css_classes(["flat"])
                            .build();
                        let (entry, popover) = (entry.clone(), popover.clone());
                        item.connect_clicked(move |_| {
                            entry.set_text(&name);
                            entry.emit_by_name::<()>("apply", &[]);
                            popover.popdown();
                        });
                        list.append(&item);
                    }
                }
                Ok(_) => list.append(&gtk::Label::new(Some("The server lists no models."))),
                Err(err) => {
                    popover.popdown();
                    toasts.add_toast(adw::Toast::new(&format!("Could not list models: {err}")));
                }
            }
        });
    });
    row.add_suffix(&pick);
    row
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
