//! Preferences → AI: the connections a model can run on, which model each
//! AI feature uses, and whether the assistant asks before acting.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_ai::{Model, ModelList, ProviderConfig};

use crate::app::App;
use crate::assistant::{self, ANTHROPIC_KEY, LOCAL_KEY};
use crate::settings::{AiChange, AiProvider, AiSettings, Change, Choice, Feature, Use};
use mailrs_domain::translate::{fill, fill_plural, gettext};

/// Models a picker shows before it grows a search box.
const SEARCH_FROM: usize = 8;

pub fn page(app: &Rc<App>, dialog: &adw::PreferencesDialog) -> adw::PreferencesPage {
    let ai = app.settings().ai;
    // Other code opens this page by the name it had when it was called
    // Assistant, so the name stays.
    let page = adw::PreferencesPage::builder()
        .title(gettext("AI"))
        .name("assistant")
        .icon_name("penguin-mail-sparkle-symbolic")
        .build();

    let connections = adw::PreferencesGroup::builder()
        .title(gettext("Connections"))
        .description(gettext(
            "A local server is LM Studio, Ollama, or any server with OpenAI's API. Local \
             models keep your mail on this computer. With Anthropic or a Claude \
             subscription, what a feature reads goes to Anthropic.",
        ))
        .build();

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
    // Every picker and Test button reads the fields through this, so they
    // ask what the dialog shows rather than what was last saved.
    let typed = Typed {
        base_url: base_url.clone(),
        local_key: local_key.clone(),
        anthropic_key: anthropic_key.clone(),
    };

    let local = connection_row(AiProvider::Local, &ai.base_url);
    local.add_row(&base_url);
    local.add_row(&local_key);
    local.add_row(&test_row(app, dialog, AiProvider::Local, &typed));
    let anthropic = connection_row(AiProvider::Anthropic, "");
    anthropic.add_row(&anthropic_key);
    anthropic.add_row(&test_row(app, dialog, AiProvider::Anthropic, &typed));
    let found = assistant::find_claude();
    let claude = connection_row(
        AiProvider::ClaudeCode,
        &match &found {
            Some(path) => fill(
                &gettext("Uses your Claude subscription through {path}"),
                &[("path", &path.display().to_string())],
            ),
            None => gettext("Not found. Install Claude Code and sign in by running claude once."),
        },
    );
    claude.add_row(&test_row(app, dialog, AiProvider::ClaudeCode, &typed));
    for row in [&local, &anthropic, &claude] {
        connections.add(row);
    }
    page.add(&connections);

    let base_url_shown = local.clone();
    let weak = Rc::downgrade(app);
    base_url.connect_apply(move |row| {
        let url = row.text().trim().trim_end_matches('/').to_string();
        base_url_shown.set_subtitle(&glib::markup_escape_text(&url));
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
    let key_state = |saved: bool| {
        if saved {
            gettext("Key saved in the keyring")
        } else {
            gettext("No key saved")
        }
    };
    let saved = assistant::load_key(ANTHROPIC_KEY).is_some();
    anthropic.set_subtitle(&key_state(saved));
    // Connected before `save`, which empties the field, so this still sees
    // what was typed.
    let anthropic_shown = anthropic.clone();
    anthropic_key.connect_apply(move |row| {
        anthropic_shown.set_subtitle(&key_state(!row.text().trim().is_empty()));
    });
    save(LOCAL_KEY, &local_key);
    save(ANTHROPIC_KEY, &anthropic_key);
    if saved {
        anthropic_key.set_title(&gettext("Anthropic API Key (Saved)"));
    }

    let used_for = adw::PreferencesGroup::builder()
        .title(gettext("Used For"))
        .description(gettext(
            "Each feature sends what it reads to the model chosen for it.",
        ))
        .build();
    let rows: Vec<FeatureRow> = Feature::ALL
        .into_iter()
        .map(|feature| feature_row(app, &ai, feature, &typed))
        .collect();
    for row in &rows {
        used_for.add(&row.expander);
    }
    page.add(&used_for);
    page.add(&crate::ui::assistant_web_prefs::group(app));
    page.add(&crate::ui::assistant_mcp_prefs::group(app, dialog));

    let assistant_row = rows
        .into_iter()
        .find(|row| row.feature == Feature::Assistant)
        .expect("Feature::ALL holds the assistant");
    page.add(&detected_group(app, assistant_row, &base_url, &local));

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

    let details = adw::PreferencesGroup::builder()
        .title(gettext("Conversation"))
        .build();
    let expanded = adw::SwitchRow::builder()
        .title(gettext("Show Details Expanded"))
        .subtitle(gettext(
            "Open the model's thinking and each tool it runs as they appear",
        ))
        .active(app.settings().assistant_details_expanded)
        .build();
    let weak = Rc::downgrade(app);
    expanded.connect_active_notify(move |row| {
        if let Some(app) = weak.upgrade() {
            app.change_settings(Change::AssistantDetailsExpanded(row.is_active()));
        }
    });
    details.add(&expanded);
    page.add(&details);
    page.add(&always_allowed(app));
    page
}

/// The outside tools answered Always Allow, each with a way to go back to
/// being asked. Hidden while there are none.
fn always_allowed(app: &Rc<App>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Always Allowed"))
        .description(gettext(
            "Tools from outside sources that run without asking. Remove one to be asked \
             again next time.",
        ))
        .build();
    let keys = app.settings().assistant_allowed_tools;
    group.set_visible(!keys.is_empty());
    for key in keys {
        let (source, tool) = key.split_once('/').unwrap_or(("", key.as_str()));
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(tool))
            .subtitle(glib::markup_escape_text(source))
            .build();
        let remove = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .tooltip_text(gettext("Ask Again"))
            .build();
        crate::ui::name(
            &remove,
            &mailrs_domain::translate::fill(&gettext("Ask again before {tool}"), &[("tool", tool)]),
        );
        let weak = Rc::downgrade(app);
        let (row_ref, group_ref) = (row.clone(), group.clone());
        remove.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.change_settings(Change::ForbidTool(key.clone()));
            }
            group_ref.remove(&row_ref);
        });
        row.add_suffix(&remove);
        group.add(&row);
    }
    group
}

/// One connection under Connections, holding the rows that set it up.
fn connection_row(connection: AiProvider, subtitle: &str) -> adw::ExpanderRow {
    adw::ExpanderRow::builder()
        .title(connection.label())
        .subtitle(glib::markup_escape_text(subtitle))
        .build()
}

/// The Test row inside a connection. It asks the model the assistant would
/// use there, or the first feature that runs on it.
fn test_row(
    app: &Rc<App>,
    dialog: &adw::PreferencesDialog,
    connection: AiProvider,
    typed: &Typed,
) -> adw::ActionRow {
    let test = gtk::Button::builder()
        .label(gettext("Test"))
        .valign(gtk::Align::Center)
        .build();
    let row = adw::ActionRow::builder()
        .title(gettext("Test the Connection"))
        .build();
    row.add_suffix(&test);
    let (weak, toasts, typed) = (Rc::downgrade(app), dialog.clone(), typed.clone());
    test.connect_clicked(move |button| {
        let Some(app) = weak.upgrade() else { return };
        // Test what the dialog shows. Testing the saved address while the
        // person looks at a different one is how a working server gets
        // reported as broken.
        let ai = app.settings().ai;
        let config = match typed.config(ai.clone(), connection, &model_to_test(&ai, connection)) {
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
    row
}

/// The model a connection's Test button asks: the one a feature already
/// runs there, else the assistant's model for it.
fn model_to_test(ai: &AiSettings, connection: AiProvider) -> String {
    Feature::ALL
        .into_iter()
        .map(|feature| ai.resolved(feature))
        .find(|(on, model)| *on == connection && !model.trim().is_empty())
        .map(|(_, model)| model)
        .unwrap_or_else(|| ai.model_on(connection).to_string())
}

/// One feature under Used For: where it runs and on which model.
#[derive(Clone)]
struct FeatureRow {
    feature: Feature,
    expander: adw::ExpanderRow,
    connection: adw::ComboRow,
    model: adw::EntryRow,
}

impl FeatureRow {
    /// Moves the feature to a connection, as if picked in its drop-down.
    fn choose(&self, app: &App, connection: AiProvider) {
        let at = self.feature.choices().iter().position(
            |choice| matches!(choice, Use::Model { connection: c, .. } if *c == connection),
        );
        let Some(at) = at else { return };
        if self.connection.selected() == at as u32 {
            // The drop-down does not tell anyone when it is set to what it
            // already shows, and the connection's model may have changed
            // under it.
            if let Use::Model { model, .. } = app.settings().ai.use_for(self.feature) {
                self.model.set_text(&model);
            }
        } else {
            self.connection.set_selected(at as u32);
        }
    }
}

fn feature_row(app: &Rc<App>, ai: &AiSettings, feature: Feature, typed: &Typed) -> FeatureRow {
    let choices = feature.choices();
    let current = ai.use_for(feature);
    let labels: Vec<String> = choices.iter().map(Use::label).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let expander = adw::ExpanderRow::builder()
        .title(feature.label())
        .subtitle(feature.description())
        .expanded(true)
        .build();
    let connection = adw::ComboRow::builder()
        .title(gettext("Connection"))
        .model(&gtk::StringList::new(&labels))
        .selected(
            choices
                .iter()
                .position(|choice| choice.same_choice(&current))
                .unwrap_or(0) as u32,
        )
        .build();
    let (on, model) = match &current {
        Use::Model { connection, model } => (*connection, model.as_str()),
        Use::SameAsAssistant => (AiProvider::Off, ""),
    };
    let picker = Picker {
        feature,
        connection: Rc::new(Cell::new(on)),
    };
    let model_row = model_row(app, model, picker.clone(), typed.clone());
    model_row.set_visible(on != AiProvider::Off);
    let model_shown = model_row.clone();
    expander.add_row(&connection);
    expander.add_row(&model_row);

    let weak = Rc::downgrade(app);
    connection.connect_selected_notify(move |row| {
        let Some(app) = weak.upgrade() else { return };
        let Some(choice) = choices.get(row.selected() as usize).cloned() else {
            return;
        };
        let ai = app.settings().ai;
        let choice = match choice {
            Use::SameAsAssistant => Use::SameAsAssistant,
            Use::Model { connection, .. } => {
                // Staying on a connection keeps the model already chosen
                // there. Moving to another one starts from the assistant's
                // model on it, which is a model that connection has.
                let model = match ai.use_for(feature) {
                    Use::Model {
                        connection: was,
                        model,
                    } if was == connection => model,
                    _ => ai.model_on(connection).to_string(),
                };
                Use::Model { connection, model }
            }
        };
        let on = match &choice {
            Use::Model { connection, model } => {
                model_row.set_text(model);
                *connection
            }
            Use::SameAsAssistant => AiProvider::Off,
        };
        picker.connection.set(on);
        model_row.set_visible(on != AiProvider::Off);
        app.change_settings(Change::Ai(AiChange::Use { feature, choice }));
    });
    FeatureRow {
        feature,
        expander,
        connection,
        model: model_shown,
    }
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
    /// A connection with whatever is on screen written over the saved
    /// settings, ready to talk to `model`. It goes through
    /// `assistant::model_for` like every feature does, as the assistant on
    /// a copy of the settings, so testing a connection builds the same
    /// thing using it would.
    fn config(
        &self,
        mut ai: AiSettings,
        connection: AiProvider,
        model: &str,
    ) -> Result<ProviderConfig, String> {
        ai.set_use(
            Feature::Assistant,
            Use::Model {
                connection,
                model: model.to_string(),
            },
        );
        let address = self.base_url.text().trim().to_string();
        if !address.is_empty() {
            ai.base_url = address;
        }
        // A key typed and not applied is still in its field: one that
        // reached the keyring clears it.
        let typed_key = match connection {
            AiProvider::Local => self.local_key.text().trim().to_string(),
            AiProvider::Anthropic => self.anthropic_key.text().trim().to_string(),
            _ => String::new(),
        };
        if connection == AiProvider::Anthropic && !typed_key.is_empty() {
            // `model_for` refuses for want of a saved key before it could
            // be told about this one, so this answers in its place.
            return Ok(ProviderConfig::Anthropic {
                api_key: typed_key,
                model: model.trim().to_string(),
            });
        }
        let mut config = assistant::model_for(&ai, Feature::Assistant)?;
        if let ProviderConfig::OpenAiCompatible { api_key, .. } = &mut config
            && !typed_key.is_empty()
        {
            *api_key = Some(typed_key);
        }
        Ok(config)
    }

    /// The same, with a model name standing in, for asking a server what
    /// it offers before a model has been chosen.
    fn listing(&self, app: &App, connection: AiProvider) -> Result<ProviderConfig, String> {
        self.config(app.settings().ai, connection, "list")
    }
}

/// Which feature a model picker belongs to, and the connection its
/// drop-down shows now.
#[derive(Clone)]
struct Picker {
    feature: Feature,
    connection: Rc<Cell<AiProvider>>,
}

impl Picker {
    /// The first entry, which empties the field. Claude Code picks its own
    /// model then; the other connections need a name.
    fn default_label(&self) -> Option<String> {
        (self.connection.get() == AiProvider::ClaudeCode)
            .then(|| gettext("Claude Code's own default"))
    }
}

/// The Model row every feature gets: a field you can type into, and a
/// picker listing what the connection can run.
fn model_row(app: &Rc<App>, current: &str, picker: Picker, typed: Typed) -> adw::EntryRow {
    let row = adw::EntryRow::builder()
        .title(gettext("Model"))
        .text(current)
        .show_apply_button(true)
        .build();
    let (weak, saving) = (Rc::downgrade(app), picker.clone());
    row.connect_apply(move |row| {
        let choice = Use::Model {
            connection: saving.connection.get(),
            model: row.text().trim().to_string(),
        };
        if let Some(app) = weak.upgrade() {
            app.change_settings(Change::Ai(AiChange::Use {
                feature: saving.feature,
                choice,
            }));
        }
    });
    let pick = gtk::MenuButton::builder()
        .icon_name("pan-down-symbolic")
        .tooltip_text(gettext("Models You Can Use"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    crate::ui::name(&pick, &gettext("Models You Can Use"));
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

    let config = typed.listing(app, picker.connection.get());
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
    if let Some(label) = picker.default_label() {
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

/// Servers and Claude Code found on this computer, each with a Use button
/// that sets up the connection and puts the assistant on it.
fn detected_group(
    app: &Rc<App>,
    assistant_row: FeatureRow,
    base_url: &adw::EntryRow,
    local: &adw::ExpanderRow,
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
    let (base_url, local) = (base_url.clone(), local.clone());
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
            crate::ui::name(
                &use_it,
                &fill(&gettext("Use {provider}"), &[("provider", &item.label)]),
            );
            let (app, assistant_row, base_url, local) = (
                Rc::clone(&app),
                assistant_row.clone(),
                base_url.clone(),
                local.clone(),
            );
            use_it.connect_clicked(move |_| {
                let config = item.config.clone();
                let first = item.models.first().cloned().unwrap_or_default();
                // The connection is saved first, so moving the assistant
                // onto it picks up the model just found.
                let connection = match config {
                    ProviderConfig::OpenAiCompatible { base_url: url, .. } => {
                        base_url.set_text(&url);
                        local.set_subtitle(&glib::markup_escape_text(&url));
                        app.change_settings(Change::Ai(AiChange::LocalServer {
                            base_url: url,
                            model: first,
                        }));
                        AiProvider::Local
                    }
                    ProviderConfig::Anthropic { api_key, .. } => {
                        assistant::save_key(ANTHROPIC_KEY, &api_key);
                        AiProvider::Anthropic
                    }
                    ProviderConfig::ClaudeCode { command, .. } => {
                        app.change_settings(Change::Ai(AiChange::ClaudeCommand(
                            command.display().to_string(),
                        )));
                        AiProvider::ClaudeCode
                    }
                };
                assistant_row.choose(&app, connection);
            });
            row.add_suffix(&use_it);
            group_ref.add(&row);
        }
    });
    group
}
