//! Preferences → AI → MCP Servers: the servers whose tools the assistant
//! may use, each with how it connects, how it stands, and a switch.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use crate::app::App;
use crate::assistant;
use crate::assistant::sources::mcp::{
    self, McpServer, McpTransport, Status, join_command_line, registry, split_command_line,
};
use crate::settings::Change;
use mailrs_domain::translate::{fill, fill_plural, gettext};

/// How often the rows read each server's status again while the page is
/// open, since a server starts in the background when a turn needs it.
const REFRESH: Duration = Duration::from_secs(2);

/// The group, with an Add Server button and a row per saved server.
pub fn group(app: &Rc<App>, dialog: &adw::PreferencesDialog) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("MCP Servers"))
        .description(gettext(
            "Tools from servers you add. The assistant asks before each call, unless \
             you choose Always Allow. A server gets what the assistant sends it.",
        ))
        .build();
    let add = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .tooltip_text(gettext("Add Server"))
        .build();
    crate::ui::name(&add, &gettext("Add Server"));
    group.set_header_suffix(Some(&add));

    let list = Rc::new(List {
        app: Rc::downgrade(app),
        dialog: dialog.clone(),
        group: group.clone(),
        rows: RefCell::new(Vec::new()),
    });
    list.fill();
    // Rows follow each server as it starts or fails. The timer holds the
    // list weakly and stops once the page is gone.
    let weak = Rc::downgrade(&list);
    let alive = group.downgrade();
    glib::timeout_add_local(REFRESH, move || match (weak.upgrade(), alive.upgrade()) {
        (Some(list), Some(_)) => {
            list.show_status();
            glib::ControlFlow::Continue
        }
        _ => glib::ControlFlow::Break,
    });
    // The button keeps the list alive for as long as the page shows it;
    // GTK drops the handler, and the list with it, when the page goes.
    add.connect_clicked(move |_| list.edit(None));
    group
}

struct List {
    app: std::rc::Weak<App>,
    dialog: adw::PreferencesDialog,
    group: adw::PreferencesGroup,
    rows: RefCell<Vec<(McpServer, adw::ActionRow)>>,
}

impl List {
    /// Builds a row per saved server, replacing any rows there were.
    fn fill(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        for (_, row) in self.rows.take() {
            self.group.remove(&row);
        }
        let servers = app.settings().mcp_servers;
        let rows = servers
            .into_iter()
            .map(|server| {
                let row = self.row(&server);
                self.group.add(&row);
                (server, row)
            })
            .collect();
        *self.rows.borrow_mut() = rows;
        self.show_status();
    }

    fn row(self: &Rc<Self>, server: &McpServer) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&server.name))
            .subtitle_lines(3)
            .build();
        let switch = gtk::Switch::builder()
            .active(server.enabled)
            .valign(gtk::Align::Center)
            .build();
        crate::ui::name(
            &switch,
            &fill(&gettext("Use {server}"), &[("server", &server.name)]),
        );
        let edit = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .tooltip_text(gettext("Edit"))
            .build();
        crate::ui::name(
            &edit,
            &fill(&gettext("Edit {server}"), &[("server", &server.name)]),
        );
        let remove = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .tooltip_text(gettext("Remove"))
            .build();
        crate::ui::name(
            &remove,
            &fill(&gettext("Remove {server}"), &[("server", &server.name)]),
        );
        row.add_suffix(&edit);
        row.add_suffix(&remove);
        row.add_suffix(&switch);

        let (weak, name) = (Rc::downgrade(self), server.name.clone());
        switch.connect_active_notify(move |switch| {
            if let Some(list) = weak.upgrade() {
                list.change(Change::EnableMcpServer {
                    name: name.clone(),
                    on: switch.is_active(),
                });
                list.show_status();
            }
        });
        let (weak, saved) = (Rc::downgrade(self), server.clone());
        edit.connect_clicked(move |_| {
            if let Some(list) = weak.upgrade() {
                list.edit(Some(saved.clone()));
            }
        });
        let (weak, saved) = (Rc::downgrade(self), server.clone());
        remove.connect_clicked(move |_| {
            if let Some(list) = weak.upgrade() {
                assistant::save_key(&saved.token_key(), "");
                list.change(Change::RemoveMcpServer(saved.name.clone()));
                list.dialog.add_toast(adw::Toast::new(&fill(
                    &gettext("Removed {server}"),
                    &[("server", &saved.name)],
                )));
                list.fill();
            }
        });
        row
    }

    /// Makes a change and stops whatever server it turned off or changed.
    fn change(&self, change: Change) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        app.change_settings(change);
        registry().reconcile(&app.settings().mcp_servers);
    }

    fn show_status(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let servers = app.settings().mcp_servers;
        for (server, row) in self.rows.borrow().iter() {
            let enabled = servers
                .iter()
                .find(|s| s.name == server.name)
                .is_some_and(|s| s.enabled);
            let status = if enabled {
                status_text(&registry().status(&server.name))
            } else {
                gettext("Off")
            };
            let subtitle = format!("{}\n{status}", mcp::summary(server));
            row.set_subtitle(&glib::markup_escape_text(&subtitle));
        }
    }

    /// Opens the Add Server dialog, or the Edit one for `saved`.
    fn edit(self: &Rc<Self>, saved: Option<McpServer>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let weak = Rc::downgrade(self);
        editor(&app, &self.dialog, saved, move |was, server, token| {
            let Some(list) = weak.upgrade() else { return };
            let key = server.token_key();
            if let Some(old) = was.as_ref().filter(|old| old.name != server.name) {
                assistant::save_key(&old.token_key(), "");
            }
            match (&server.transport, token) {
                (McpTransport::Http { .. }, Some(token)) => assistant::save_key(&key, &token),
                _ => assistant::save_key(&key, ""),
            }
            let name = server.name.clone();
            list.change(Change::SaveMcpServer {
                was: was.map(|old| old.name),
                server,
            });
            // The token may be new even where the settings are not, so the
            // next turn starts the server afresh either way.
            registry().restart(&name);
            list.fill();
        });
    }
}

/// What a row says about a running, failed or waiting server.
fn status_text(status: &Status) -> String {
    match status {
        Status::Waiting => gettext("Starts when the assistant needs it"),
        Status::Starting => gettext("Starting…"),
        Status::Connected { tools } => fill_plural(
            "Connected, {count} tool",
            "Connected, {count} tools",
            *tools,
            &[("count", &tools.to_string())],
        ),
        Status::Failed(reason) => fill(
            &gettext("Could not connect: {reason}"),
            &[("reason", reason)],
        ),
    }
}

/// The Add Server and Edit dialog. `on_save` gets the server as it was, the
/// server as saved, and the token typed for a URL.
fn editor(
    app: &Rc<App>,
    parent: &adw::PreferencesDialog,
    saved: Option<McpServer>,
    on_save: impl Fn(Option<McpServer>, McpServer, Option<String>) + 'static,
) {
    let editing = saved.is_some();
    let group = adw::PreferencesGroup::builder()
        .description(gettext(
            "A command runs on this computer and talks over its input and output, as in \
             npx -y @modelcontextprotocol/server-filesystem ~/Documents. Put NAME=value \
             in front for the environment. A URL reaches a server over HTTP.",
        ))
        .build();
    let name = adw::EntryRow::builder().title(gettext("Name")).build();
    let kinds = gtk::StringList::new(&[&gettext("Command"), &gettext("URL")]);
    let kind = adw::ComboRow::builder()
        .title(gettext("Connect With"))
        .model(&kinds)
        .build();
    let command = adw::EntryRow::builder()
        .title(gettext("Command Line"))
        .build();
    let url = adw::EntryRow::builder().title(gettext("URL")).build();
    let token = adw::PasswordEntryRow::builder()
        .title(gettext("Bearer Token (Optional)"))
        .build();
    let result = adw::ActionRow::builder()
        .title(gettext("Test the Server"))
        .subtitle_lines(4)
        .build();
    let test = gtk::Button::builder()
        .label(gettext("Test"))
        .valign(gtk::Align::Center)
        .build();
    result.add_suffix(&test);
    for row in [
        name.upcast_ref::<gtk::Widget>(),
        kind.upcast_ref(),
        command.upcast_ref(),
        url.upcast_ref(),
        token.upcast_ref(),
        result.upcast_ref(),
    ] {
        group.add(row);
    }

    if let Some(server) = &saved {
        name.set_text(&server.name);
        match &server.transport {
            McpTransport::Stdio {
                command: c,
                args,
                env,
            } => {
                command.set_text(&join_command_line(env, c, args));
            }
            McpTransport::Http { url: u } => {
                kind.set_selected(1);
                url.set_text(u);
                // The keyring can keep the GTK thread waiting, so the
                // saved token arrives when it arrives.
                let (key, token) = (server.token_key(), token.clone());
                let app = Rc::clone(app);
                glib::spawn_future_local(async move {
                    let found = app
                        .core
                        .call(async move {
                            tokio::task::spawn_blocking(move || assistant::read_key(&key))
                                .await
                                .map_err(anyhow::Error::from)
                        })
                        .await;
                    if let Ok(Some(saved)) = found
                        && token.text().is_empty()
                    {
                        token.set_text(&saved);
                    }
                });
            }
        }
    }
    let show_kind = {
        let (kind, command, url, token) =
            (kind.clone(), command.clone(), url.clone(), token.clone());
        move || {
            let by_url = kind.selected() == 1;
            command.set_visible(!by_url);
            url.set_visible(by_url);
            token.set_visible(by_url);
        }
    };
    show_kind();
    kind.connect_selected_notify(move |_| show_kind());

    let page = adw::PreferencesPage::new();
    page.add(&group);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&page));
    let save = gtk::Button::builder()
        .label(if editing {
            gettext("Save")
        } else {
            gettext("Add")
        })
        .css_classes(["suggested-action"])
        .build();
    let cancel = gtk::Button::with_label(&gettext("Cancel"));
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&save);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toasts));
    let dialog = adw::Dialog::builder()
        .title(if editing {
            gettext("Edit Server")
        } else {
            gettext("Add Server")
        })
        .content_width(560)
        .child(&toolbar)
        .build();

    // Reads the dialog into a server, or says what is missing.
    let read = {
        let (app, name, kind, command, url, token) = (
            Rc::downgrade(app),
            name.clone(),
            kind.clone(),
            command.clone(),
            url.clone(),
            token.clone(),
        );
        let saved = saved.clone();
        move || -> Result<(McpServer, Option<String>), String> {
            let typed = name.text().trim().to_string();
            if !mcp::valid_name(&typed) {
                return Err(gettext(
                    "Give the server a short name of letters, digits, - and _",
                ));
            }
            let taken = app.upgrade().is_some_and(|app| {
                app.settings().mcp_servers.iter().any(|s| {
                    s.name == typed && saved.as_ref().is_none_or(|saved| saved.name != typed)
                })
            });
            if taken {
                return Err(gettext("Another server has that name"));
            }
            let transport = if kind.selected() == 1 {
                let url = url.text().trim().to_string();
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return Err(gettext("Enter a URL that starts with http:// or https://"));
                }
                McpTransport::Http { url }
            } else {
                let line = split_command_line(&command.text()).map_err(|problem| {
                    fill(
                        &gettext("The command line does not read: {problem}"),
                        &[("problem", &problem)],
                    )
                })?;
                McpTransport::Stdio {
                    command: line.command,
                    args: line.args,
                    env: line.env,
                }
            };
            let token = Some(token.text().trim().to_string()).filter(|t| !t.is_empty());
            let server = McpServer {
                name: typed,
                enabled: saved.as_ref().is_none_or(|s| s.enabled),
                transport,
            };
            Ok((server, token))
        }
    };
    let read = Rc::new(read);

    let closer = dialog.clone();
    cancel.connect_clicked(move |_| {
        closer.close();
    });

    let (reader, weak, shown) = (Rc::clone(&read), Rc::downgrade(app), toasts.clone());
    test.connect_clicked(move |button| {
        let Some(app) = weak.upgrade() else { return };
        let (server, token) = match reader() {
            Ok(read) => read,
            Err(problem) => return shown.add_toast(adw::Toast::new(&problem)),
        };
        button.set_sensitive(false);
        result.set_subtitle(&gettext("Connecting…"));
        let (button, result) = (button.clone(), result.clone());
        glib::spawn_future_local(async move {
            let status = app
                .core
                .call(async move { Ok::<_, anyhow::Error>(registry().test(server, token).await) })
                .await
                .unwrap_or_else(|err| Status::Failed(err.to_string()));
            button.set_sensitive(true);
            result.set_subtitle(&glib::markup_escape_text(&status_text(&status)));
        });
    });

    let closer = dialog.clone();
    save.connect_clicked(move |_| match read() {
        Ok((server, token)) => {
            closer.close();
            on_save(saved.clone(), server, token);
        }
        Err(problem) => toasts.add_toast(adw::Toast::new(&problem)),
    });
    dialog.present(Some(parent));
}
