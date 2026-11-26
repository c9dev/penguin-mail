//! Model selection and tool sources for the current assistant pane.

use super::AssistantPane;
use crate::settings::{AiChange, AiProvider, Change, Choice, Feature, Use};
use adw::prelude::*;
use futures::FutureExt;
use gtk::glib;
use mailrs_domain::translate::gettext;
use std::rc::Rc;
use std::time::Instant;

pub(super) fn icon_button(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(label)
        .css_classes(["flat", "circular"])
        .build();
    crate::ui::name(&button, label);
    button
}

pub(super) fn model_label(provider: AiProvider, model: &str) -> String {
    if provider == AiProvider::ClaudeCode && model.is_empty() {
        return "Claude".to_string();
    }
    let slug = model
        .strip_prefix("claude-")
        .or_else(|| (provider == AiProvider::ClaudeCode).then_some(model));
    let Some(slug) = slug else {
        return model.to_string();
    };
    let parts: Vec<_> = slug.split('-').collect();
    let mut label = String::from("Claude");
    let mut previous_number = false;
    for (index, part) in parts.iter().enumerate() {
        if index == parts.len() - 1
            && part.len() == 8
            && part.starts_with("20")
            && part.chars().all(|c| c.is_ascii_digit())
        {
            break;
        }
        let number = !part.is_empty() && part.chars().all(|c| c.is_ascii_digit());
        label.push(if number && previous_number { '.' } else { ' ' });
        let mut letters = part.chars();
        if let Some(first) = letters.next() {
            label.extend(first.to_uppercase());
        }
        label.extend(letters);
        previous_number = number;
    }
    label
}

pub(super) fn install(
    pane: &Rc<AssistantPane>,
    header: &adw::HeaderBar,
    setup: Rc<dyn Fn()>,
    change: Rc<dyn Fn(Change)>,
) {
    let model_popover = gtk::Popover::new();
    model_popover.add_css_class("assistant-model-popover");
    pane.model.set_popover(Some(&model_popover));
    let weak = Rc::downgrade(pane);
    let setup_models = Rc::clone(&setup);
    model_popover.connect_show(move |popover| {
        if let Some(pane) = weak.upgrade() {
            models(&pane, popover, Rc::clone(&setup_models), Rc::clone(&change));
        }
    });

    let tools = gtk::MenuButton::builder()
        .icon_name("penguin-mail-preferences-system-symbolic")
        .tooltip_text(gettext("Tools"))
        .css_classes(["flat", "circular"])
        .build();
    crate::ui::name(&tools, &gettext("Tools"));
    header.pack_start(&tools);
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(8)
        .margin_end(8)
        .build();
    let popover = gtk::Popover::builder().child(&content).build();
    tools.set_popover(Some(&popover));
    for (is_mcp, label, description) in [
        (
            true,
            gettext("MCP Servers"),
            gettext("Servers you added. The assistant asks before each call."),
        ),
        (
            false,
            gettext("Skills"),
            gettext("Instruction folders. Each stays off until you turn it on."),
        ),
    ] {
        let words = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(3)
            .hexpand(true)
            .build();
        words.append(
            &gtk::Label::builder()
                .label(&label)
                .xalign(0.0)
                .css_classes(["heading"])
                .build(),
        );
        words.append(
            &gtk::Label::builder()
                .label(&description)
                .xalign(0.0)
                .wrap(true)
                .max_width_chars(30)
                .css_classes(["caption", "dim-label"])
                .build(),
        );
        let switch = gtk::Switch::builder()
            .active(true)
            .valign(gtk::Align::Center)
            .build();
        crate::ui::name(&switch, &label);
        crate::ui::describe(&switch, &label, &description);
        let row = gtk::Box::builder().spacing(12).build();
        row.append(&words);
        row.append(&switch);
        content.append(&row);
        let weak = Rc::downgrade(pane);
        switch.connect_active_notify(move |switch| {
            if let Some(pane) = weak.upgrade() {
                let enabled = if is_mcp { &pane.mcp } else { &pane.skills };
                enabled.set(switch.is_active());
            }
        });
    }
    content.append(
        &gtk::Label::builder()
            .label(gettext("Changes apply to your next question."))
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(30)
            .css_classes(["caption", "dim-label"])
            .build(),
    );
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let manage = gtk::Button::builder()
        .label(gettext("Manage in Preferences"))
        .css_classes(["flat"])
        .build();
    content.append(&manage);
    let popup = popover.downgrade();
    manage.connect_clicked(move |_| {
        if let Some(popup) = popup.upgrade() {
            popup.popdown();
        }
        setup();
    });
}

fn models(
    pane: &Rc<AssistantPane>,
    popover: &gtk::Popover,
    setup: Rc<dyn Fn()>,
    change: Rc<dyn Fn(Change)>,
) {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(8)
        .margin_end(8)
        .build();
    let search = gtk::SearchEntry::builder()
        .placeholder_text(gettext("Search models"))
        .build();
    crate::ui::name(&search, &gettext("Search models"));
    content.append(&search);
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["assistant-model-list"])
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(
            pane.page
                .root()
                .and_downcast::<gtk::Window>()
                .map_or(360, |w| (w.height() - 180).clamp(120, 360)),
        )
        .min_content_width(240)
        .max_content_width(320)
        .propagate_natural_width(true)
        .build();
    content.append(&scroller);
    let manage = gtk::Button::builder()
        .label(gettext("Manage in Preferences"))
        .css_classes(["flat"])
        .build();
    content.append(&manage);
    popover.set_child(Some(&content));
    let popup = popover.downgrade();
    manage.connect_clicked(move |_| {
        if let Some(popup) = popup.upgrade() {
            popup.popdown();
        }
        setup();
    });
    let ai = (pane.settings)().ai;
    for provider in [ai.provider].into_iter().filter(|p| *p != AiProvider::Off) {
        let section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .build();
        section.append(
            &gtk::Label::builder()
                .label(provider.label())
                .xalign(0.0)
                .css_classes(["caption-heading"])
                .margin_top(6)
                .build(),
        );
        let waiting = gtk::Label::builder()
            .label(gettext("Asking for the model list…"))
            .wrap(true)
            .max_width_chars(32)
            .xalign(0.0)
            .css_classes(["caption", "dim-label"])
            .build();
        section.append(&waiting);
        list.append(&section);
        let row = section.parent().and_downcast::<gtk::ListBoxRow>();
        if let Some(row) = &row {
            row.set_activatable(false);
            row.set_selectable(false);
        }
        let mut configured = ai.clone();
        configured.set_use(
            Feature::Assistant,
            Use::Model {
                connection: provider,
                model: "list".into(),
            },
        );
        let config = crate::assistant::model_for(&configured, Feature::Assistant);
        let core = Rc::clone(&pane.core);
        let found = match config {
            Ok(config) => {
                let query = config.clone();
                pane.model_catalog
                    .borrow_mut()
                    .request(config, Instant::now(), move || {
                        async move {
                            core.call(async move { mailrs_ai::list_models(&query).await })
                                .await
                                .map_err(|e| e.to_string())
                        }
                        .boxed_local()
                    })
            }
            Err(problem) => async move { Err(problem) }.boxed_local().shared(),
        };
        let weak = Rc::downgrade(pane);
        let popup = popover.downgrade();
        let change = Rc::clone(&change);
        let selected = ai.clone();
        let search = search.downgrade();
        glib::spawn_future_local(async move {
            let found = found.await;
            if section.root().is_none() {
                return;
            }
            match found {
                Err(problem) => waiting.set_label(&problem),
                Ok(mut catalog) => {
                    waiting.set_visible(false);
                    // A custom or pinned model remains selectable even if
                    // the provider no longer includes it in its catalog.
                    let current = selected.model_on(provider);
                    if !current.is_empty() && !catalog.models.iter().any(|m| m.id == current) {
                        catalog.models.insert(0, mailrs_ai::Model::new(current));
                    }
                    if provider == AiProvider::ClaudeCode {
                        catalog.models.insert(
                            0,
                            mailrs_ai::Model::named("", gettext("Claude Code's own default")),
                        );
                    }
                    for model in catalog.models {
                        let label = if model.name.is_empty() {
                            model_label(provider, &model.id)
                        } else {
                            model.name
                        };
                        let button = gtk::Button::builder()
                            .css_classes(["flat", "model-choice"])
                            .build();
                        let line = gtk::Box::builder().spacing(6).build();
                        let checked = gtk::Image::from_icon_name("penguin-mail-object-select-symbolic");
                        checked.set_accessible_role(gtk::AccessibleRole::Presentation);
                        checked.set_opacity(
                            if selected.provider == provider && current == model.id {
                                1.0
                            } else {
                                0.0
                            },
                        );
                        line.append(&checked);
                        line.append(
                            &gtk::Label::builder()
                                .label(&label)
                                .xalign(0.0)
                                .wrap(true)
                                .wrap_mode(gtk::pango::WrapMode::WordChar)
                                .max_width_chars(30)
                                .build(),
                        );
                        button.set_child(Some(&line));
                        crate::ui::name(&button, &format!("{label}, {}", provider.label()));
                        if selected.provider == provider && current == model.id {
                            button.add_css_class("model-current");
                            if let Some(pane) = weak.upgrade() {
                                let ai = (pane.settings)().ai;
                                if ai.provider == provider && ai.model_on(provider) == model.id {
                                    *pane.known_model.borrow_mut() =
                                        Some((provider, model.id.clone(), label.clone()));
                                    pane.refresh();
                                }
                            }
                            button.update_property(&[gtk::accessible::Property::Description(
                                &gettext("Current model"),
                            )]);
                        }
                        section.append(&button);
                        if let Some(search) = search.upgrade() {
                            let terms =
                                format!("{label} {} {}", model.id, provider.label()).to_lowercase();
                            button.set_visible(terms.contains(&search.text().to_lowercase()));
                            let shown = button.downgrade();
                            search.connect_search_changed(move |search| {
                                if let Some(button) = shown.upgrade() {
                                    button
                                        .set_visible(terms.contains(&search.text().to_lowercase()));
                                }
                            });
                        }
                        let chosen_label = label.clone();
                        let (weak, popup, change) =
                            (weak.clone(), popup.clone(), Rc::clone(&change));
                        button.connect_clicked(move |_| {
                            let Some(pane) = weak.upgrade() else { return };
                            if pane.running.get() {
                                return;
                            }
                            let ai = (pane.settings)().ai;
                            if ai.provider != provider {
                                return;
                            }
                            if let Some(popup) = popup.upgrade() {
                                popup.popdown();
                            }
                            if ai.model_on(provider) == model.id {
                                return;
                            }
                            *pane.known_model.borrow_mut() =
                                Some((provider, model.id.clone(), chosen_label.clone()));
                            change(Change::Ai(AiChange::Use {
                                feature: Feature::Assistant,
                                choice: Use::Model {
                                    connection: provider,
                                    model: model.id.clone(),
                                },
                            }));
                            pane.refresh();
                        });
                    }
                    if let Some(note) = catalog.note {
                        waiting.set_label(&note);
                        waiting.set_visible(true);
                    }
                    if section.last_child().as_ref() == Some(waiting.upcast_ref()) {
                        waiting.set_label(&gettext("No models found."));
                        waiting.set_visible(true);
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_captions_show_names_and_versions_instead_of_api_slugs() {
        assert_eq!(
            model_label(AiProvider::Anthropic, "claude-haiku-5-5"),
            "Claude Haiku 5.5"
        );
        assert_eq!(
            model_label(AiProvider::Anthropic, "claude-sonnet-4-5-20250929"),
            "Claude Sonnet 4.5"
        );
        assert_eq!(
            model_label(AiProvider::ClaudeCode, "sonnet"),
            "Claude Sonnet"
        );
        assert_eq!(
            model_label(AiProvider::Local, "my-custom-model"),
            "my-custom-model"
        );
    }
}
