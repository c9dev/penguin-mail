//! Preferences → AI → Skills: the skills found on this computer, a switch
//! for each, and for a skill with scripts whether they may reach the
//! internet.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::app::App;
use crate::assistant::sources::skills::{self, Skill, sandbox};
use crate::settings::Change;
use mailrs_domain::translate::{fill, gettext};

pub fn group(app: &Rc<App>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Skills"))
        .description(gettext(
            "Instructions the assistant follows for one kind of task, found in Penguin \
             Mail's skills folder and in Claude Code's. Each skill is off until you turn it \
             on. Scripts in a skill run in a sandbox that cannot reach your mail, keys or \
             home folder, and the assistant asks before each command.",
        ))
        .build();
    let open = gtk::Button::builder()
        .label(gettext("Open Folder"))
        .valign(gtk::Align::Center)
        .tooltip_text(gettext("Open Penguin Mail's skills folder"))
        .build();
    open.connect_clicked(open_folder);
    group.set_header_suffix(Some(&open));

    let found = skills::discover(&skills::roots());
    if found.iter().any(|skill| skill.has_scripts)
        && let Err(reason) = sandbox::check()
    {
        let row = adw::ActionRow::builder()
            .title(gettext("Scripts Cannot Run"))
            .subtitle(glib::markup_escape_text(&reason))
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
        group.add(&row);
    }
    if found.is_empty() {
        group.add(
            &adw::ActionRow::builder()
                .title(gettext("No Skills Found"))
                .subtitle(glib::markup_escape_text(&fill(
                    &gettext("Put a folder holding a SKILL.md file in {folder}."),
                    &[("folder", &skills::own_folder().to_string_lossy())],
                )))
                .build(),
        );
    }
    for skill in found {
        group.add(&skill_row(app, &skill));
    }
    group
}

/// Where the skill came from, then what it does.
fn subtitle(skill: &Skill) -> String {
    glib::markup_escape_text(&fill(
        &gettext("{origin}: {description}"),
        &[
            ("origin", &skill.origin.label()),
            ("description", &skill.description),
        ],
    ))
    .to_string()
}

fn skill_row(app: &Rc<App>, skill: &Skill) -> gtk::Widget {
    let settings = app.settings().skill(&skill.id);
    let title = glib::markup_escape_text(&skill.name);
    if !skill.has_scripts {
        let row = adw::SwitchRow::builder()
            .title(title)
            .subtitle(subtitle(skill))
            .subtitle_lines(3)
            .active(settings.enabled)
            .build();
        let (weak, id) = (Rc::downgrade(app), skill.id.clone());
        row.connect_active_notify(move |row| {
            if let Some(app) = weak.upgrade() {
                app.change_settings(Change::SkillEnabled {
                    id: id.clone(),
                    on: row.is_active(),
                });
            }
        });
        return row.upcast();
    }
    // A skill with scripts opens to its second switch.
    let row = adw::ExpanderRow::builder()
        .title(title)
        .subtitle(subtitle(skill))
        .subtitle_lines(3)
        .build();
    let on = gtk::Switch::builder()
        .active(settings.enabled)
        .valign(gtk::Align::Center)
        .build();
    crate::ui::name(
        &on,
        &fill(&gettext("Use the skill {skill}"), &[("skill", &skill.name)]),
    );
    row.add_suffix(&on);
    let (weak, id) = (Rc::downgrade(app), skill.id.clone());
    on.connect_active_notify(move |on| {
        if let Some(app) = weak.upgrade() {
            app.change_settings(Change::SkillEnabled {
                id: id.clone(),
                on: on.is_active(),
            });
        }
    });
    let network = adw::SwitchRow::builder()
        .title(gettext("Allow Network"))
        .subtitle(gettext(
            "Let this skill's scripts reach the internet. Without it they can only use \
             what is on this computer.",
        ))
        .active(settings.allow_network)
        .build();
    let (weak, id) = (Rc::downgrade(app), skill.id.clone());
    network.connect_active_notify(move |row| {
        if let Some(app) = weak.upgrade() {
            app.change_settings(Change::SkillNetwork {
                id: id.clone(),
                on: row.is_active(),
            });
        }
    });
    row.add_row(&network);
    row.upcast()
}

/// Creates Penguin Mail's skills folder when it is missing, readable by
/// this user alone like the rest of its folders, and shows it in Files.
fn open_folder(button: &gtk::Button) {
    use std::os::unix::fs::DirBuilderExt;
    let folder = skills::own_folder();
    if let Err(err) = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder)
    {
        tracing::warn!(folder = %folder.display(), error = %err, "could not make the skills folder");
        return;
    }
    let window = button.root().and_downcast::<gtk::Window>();
    gtk::FileLauncher::new(Some(&gio::File::for_path(&folder))).launch(
        window.as_ref(),
        gio::Cancellable::NONE,
        |_| {},
    );
}
