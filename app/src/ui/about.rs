//! The About window: which Penguin Mail this is, who made it, where to read
//! about it, and a button that checks for a newer version and installs it.
//! libadwaita's own About dialog takes no widgets of ours, so this one draws
//! its main page the same way and adds the button.

use std::rc::Rc;

use adw::prelude::*;
use mailrs_domain::translate::{fill, gettext};

use crate::update::State;

const REPOSITORY: &str = "https://github.com/c9dev/penguin-mail";

pub struct About {
    pub dialog: adw::Dialog,
    /// Holds the update button and the line under it. Hidden when this copy
    /// never updates: the demo and a cargo build.
    updates: gtk::Box,
    button: gtk::Button,
    label: gtk::Label,
    spinner: adw::Spinner,
    status: gtk::Label,
}

impl About {
    pub fn new(can_update: bool) -> Rc<About> {
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();

        let icon = gtk::Image::builder()
            .icon_name(crate::APP_ID)
            .pixel_size(128)
            .margin_bottom(12)
            .build();
        icon.add_css_class("icon-dropshadow");
        icon.set_accessible_role(gtk::AccessibleRole::Presentation);
        content.append(&icon);

        let name = gtk::Label::new(Some(&gettext("Penguin Mail")));
        name.add_css_class("title-1");
        name.set_wrap(true);
        content.append(&name);

        // A link, so the name opens the studio's site as the rows below
        // open theirs.
        let developer = gtk::Label::builder()
            .label(r#"<a href="https://pivotd.com">Pivotd</a>"#)
            .use_markup(true)
            .build();
        content.append(&developer);

        let version = gtk::Label::new(Some(&fill(
            &gettext("Version {version}"),
            &[("version", env!("CARGO_PKG_VERSION"))],
        )));
        version.add_css_class("dim-label");
        version.set_margin_top(6);
        content.append(&version);
        // A copy that dnf or a store updates has no update button, so it
        // says who updates it.
        if let Some(updater) = crate::packaging::BUILT_FOR.updated_by() {
            let updates = gtk::Label::new(Some(&updater.line()));
            updates.add_css_class("dim-label");
            updates.add_css_class("caption");
            content.append(&updates);
        }

        let comments = gtk::Label::builder()
            .label(gettext(
                "A fast, private Gmail client for the GNOME desktop. Mail stays on your \
                 computer and your own Google Cloud project.",
            ))
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(40)
            .margin_top(12)
            .build();
        content.append(&comments);

        let label = gtk::Label::new(Some(&gettext("Check for Updates")));
        let spinner = adw::Spinner::builder().visible(false).build();
        let inside = gtk::Box::builder()
            .spacing(8)
            .halign(gtk::Align::Center)
            .build();
        inside.append(&spinner);
        inside.append(&label);
        let button = gtk::Button::builder()
            .child(&inside)
            .halign(gtk::Align::Center)
            .build();
        button.add_css_class("pill");
        let status = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .visible(false)
            .build();
        status.add_css_class("dim-label");
        status.add_css_class("caption");
        let updates = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(18)
            .visible(can_update)
            .build();
        updates.append(&button);
        updates.append(&status);
        content.append(&updates);

        let links = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .margin_top(24)
            .build();
        links.add_css_class("boxed-list");
        let link = |title: String, subtitle: Option<&str>, uri: String| {
            let row = adw::ActionRow::builder()
                .title(title)
                .activatable(true)
                .build();
            if let Some(subtitle) = subtitle {
                row.set_subtitle(subtitle);
            }
            row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
            row.connect_activated(move |row| {
                let window = row.root().and_downcast::<gtk::Window>();
                gtk::UriLauncher::new(&uri).launch(
                    window.as_ref(),
                    None::<&gtk::gio::Cancellable>,
                    |_| {},
                );
            });
            links.append(&row);
        };
        link(
            gettext("What's New"),
            None,
            format!("{REPOSITORY}/blob/main/CHANGELOG.md"),
        );
        link(gettext("Website"), None, REPOSITORY.into());
        link(
            gettext("Report a Problem"),
            None,
            format!("{REPOSITORY}/issues/new/choose"),
        );
        link(
            gettext("License"),
            Some("GPL-3.0-or-later"),
            format!("{REPOSITORY}/blob/main/LICENSE"),
        );
        content.append(&links);

        let header = adw::HeaderBar::builder().show_title(false).build();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .child(&content)
                .build(),
        ));
        let dialog = adw::Dialog::builder()
            .title(gettext("About Penguin Mail"))
            .content_width(380)
            .child(&view)
            .build();

        Rc::new(About {
            dialog,
            updates,
            button,
            label,
            spinner,
            status,
        })
    }

    /// Puts the update button and the line under it in step with where an
    /// update stands. The button runs an app action, as the banner's does.
    pub fn show_update(&self, state: &State) {
        if !self.updates.is_visible() {
            return;
        }
        let version = |v: &crate::update::version::Version| v.to_string();
        let (label, action, busy, suggested, status) = match state {
            State::Idle => (
                gettext("Check for Updates"),
                Some("app.check-for-updates"),
                false,
                false,
                None,
            ),
            State::Checking => (gettext("Checking…"), None, true, false, None),
            State::Current => (
                gettext("Check for Updates"),
                Some("app.check-for-updates"),
                false,
                false,
                Some(gettext("Penguin Mail is up to date")),
            ),
            State::Unreachable => (
                gettext("Check for Updates"),
                Some("app.check-for-updates"),
                false,
                false,
                Some(gettext(
                    "Could not reach GitHub. Check your connection and try again.",
                )),
            ),
            State::Available(release) => (
                fill(
                    &gettext("Install {version}"),
                    &[("version", &version(&release.version))],
                ),
                Some("app.install-update"),
                false,
                true,
                Some(fill(
                    &gettext("Version {version} is available"),
                    &[("version", &version(&release.version))],
                )),
            ),
            State::Installing(v) => (
                gettext("Installing…"),
                None,
                true,
                false,
                Some(fill(
                    &gettext("Installing version {version}"),
                    &[("version", &version(v))],
                )),
            ),
            State::Installed(v) => (
                gettext("Restart to Update"),
                Some("app.restart-for-update"),
                false,
                true,
                Some(fill(
                    &gettext("Version {version} is installed"),
                    &[("version", &version(v))],
                )),
            ),
            State::Failed { version: v, .. } => (
                gettext("Show Log"),
                Some("app.update-log"),
                false,
                false,
                Some(fill(
                    &gettext("The update to {version} failed"),
                    &[("version", &version(v))],
                )),
            ),
        };
        self.label.set_label(&label);
        // The label sits inside a box, which a screen reader does not read
        // as the button's name.
        crate::ui::name(&self.button, &label);
        self.button.set_action_name(action);
        // A button with no action is insensitive, which is what a check or
        // an install in progress wants.
        self.button.set_sensitive(action.is_some());
        self.spinner.set_visible(busy);
        if suggested {
            self.button.add_css_class("suggested-action");
        } else {
            self.button.remove_css_class("suggested-action");
        }
        match status {
            Some(text) => {
                self.status.set_label(&text);
                self.status.set_visible(true);
            }
            None => self.status.set_visible(false),
        }
    }
}
