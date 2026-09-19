//! The Preferences dialog. Changes save as they happen; sync options apply
//! when the dialog closes, so the engine restarts once.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use mailrs_domain::Account;
use mailrs_sync::config::SyncConfig;

use crate::app::App;
use crate::autostart;
use crate::settings::{
    CACHE_CHOICES, Choice, ColorScheme, MarkRead, POLL_CHOICES, RemoteImages, Settings, TextSize,
    WINDOW_CHOICES, nearest,
};

pub fn present(app: &Rc<App>, accounts: &[Account], parent: &impl IsA<gtk::Widget>) {
    let settings = app.settings();
    let dialog = adw::PreferencesDialog::builder()
        .search_enabled(true)
        .build();
    dialog.add(&general_page(app, &settings));
    dialog.add(&writing_page(app, &settings, accounts));
    let pending = Rc::new(RefCell::new(app.core.sync_config().unwrap_or_default()));
    dialog.add(&sync_page(app, &pending));
    let weak = Rc::downgrade(app);
    dialog.connect_closed(move |_| {
        let Some(app) = weak.upgrade() else { return };
        if let Err(err) = app.core.update_sync(pending.borrow().clone()) {
            tracing::warn!(error = %err, "could not apply the sync settings");
        }
    });
    dialog.present(Some(parent));
}

fn general_page(app: &Rc<App>, settings: &Settings) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("emblem-system-symbolic")
        .build();

    let reading = adw::PreferencesGroup::builder().title("Reading").build();
    reading.add(&switch(
        app,
        "Group Messages into Conversations",
        Some("Show a thread's replies together instead of one row per message"),
        settings.threading,
        |s, v| s.threading = v,
    ));
    reading.add(&combo(
        app,
        "Mark as Read",
        None,
        settings.mark_read,
        |s, v: MarkRead| s.mark_read = v,
    ));
    reading.add(&combo(
        app,
        "Remote Images",
        Some("Loading them can tell senders when you read their mail"),
        settings.remote_images,
        |s, v: RemoteImages| s.remote_images = v,
    ));
    reading.add(&combo(
        app,
        "Text Size",
        None,
        settings.text_size,
        |s, v: TextSize| s.text_size = v,
    ));
    page.add(&reading);

    let appearance = adw::PreferencesGroup::builder().title("Appearance").build();
    appearance.add(&combo(
        app,
        "Style",
        None,
        settings.color_scheme,
        |s, v: ColorScheme| s.color_scheme = v,
    ));
    page.add(&appearance);

    let notifications = adw::PreferencesGroup::builder()
        .title("Notifications")
        .build();
    let enabled = switch(
        app,
        "Notify About New Mail",
        None,
        settings.notifications,
        |s, v| s.notifications = v,
    );
    let previews = switch(
        app,
        "Show Sender and Subject",
        Some("Turn off to see only how much mail arrived"),
        settings.notification_previews,
        |s, v| s.notification_previews = v,
    );
    enabled
        .bind_property("active", &previews, "sensitive")
        .sync_create()
        .build();
    notifications.add(&enabled);
    notifications.add(&previews);
    page.add(&notifications);
    page
}

fn writing_page(app: &Rc<App>, settings: &Settings, accounts: &[Account]) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Writing")
        .icon_name("document-edit-symbolic")
        .build();
    if accounts.is_empty() {
        let empty = adw::PreferencesGroup::builder()
            .title("No Accounts Yet")
            .description("Add an account to choose a sender and write signatures.")
            .build();
        page.add(&empty);
        return page;
    }

    let sending = adw::PreferencesGroup::builder()
        .title("New Messages")
        .build();
    let emails: Vec<String> = accounts.iter().map(|a| a.email.clone()).collect();
    let labels: Vec<&str> = emails.iter().map(String::as_str).collect();
    let current = settings
        .default_account
        .as_ref()
        .and_then(|d| emails.iter().position(|e| e.eq_ignore_ascii_case(d)))
        .unwrap_or(0);
    let from = adw::ComboRow::builder()
        .title("Send New Messages From")
        .subtitle("Replies always come from the account that received the message")
        .model(&gtk::StringList::new(&labels))
        .selected(current as u32)
        .build();
    let weak = Rc::downgrade(app);
    from.connect_selected_notify(move |row| {
        let (Some(app), Some(email)) =
            (weak.upgrade(), emails.get(row.selected() as usize).cloned())
        else {
            return;
        };
        app.update_settings(move |s| s.default_account = Some(email));
    });
    sending.add(&from);
    page.add(&sending);

    let signatures = adw::PreferencesGroup::builder()
        .title("Signatures")
        .description("Added below new messages and above quoted text in replies. Markdown works.")
        .build();
    for account in accounts {
        let text = settings.signature(&account.email).to_string();
        let row = adw::ExpanderRow::builder()
            .title(&account.email)
            .subtitle(preview(&text))
            .build();
        let view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(10)
            .bottom_margin(10)
            .left_margin(12)
            .right_margin(12)
            .accepts_tab(false)
            .build();
        view.buffer().set_text(&text);
        let frame = gtk::ScrolledWindow::builder()
            .child(&view)
            .min_content_height(96)
            .max_content_height(220)
            .propagate_natural_height(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        row.add_row(&frame);
        let (weak, email, subtitle) = (Rc::downgrade(app), account.email.clone(), row.clone());
        view.buffer().connect_changed(move |buffer| {
            let Some(app) = weak.upgrade() else { return };
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();
            subtitle.set_subtitle(&preview(&text));
            let email = email.clone();
            app.update_settings(move |s| s.set_signature(&email, &text));
        });
        signatures.add(&row);
    }
    page.add(&signatures);
    page
}

fn sync_page(app: &Rc<App>, pending: &Rc<RefCell<SyncConfig>>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Sync")
        .icon_name("mail-send-receive-symbolic")
        .build();
    let current = pending.borrow().clone();

    let checking = adw::PreferencesGroup::builder().title("Checking").build();
    let poll = current.poll_seconds.map_or(30, |s| s as i64);
    checking.add(&sync_combo(
        &POLL_CHOICES,
        "Check for New Mail",
        None,
        nearest(&POLL_CHOICES, poll),
        pending,
        |c, v| c.poll_seconds = Some(v as u64),
    ));
    page.add(&checking);

    let storage = adw::PreferencesGroup::builder().title("Storage").build();
    storage.add(&sync_combo(
        &WINDOW_CHOICES,
        "Keep Mail on This Computer For",
        Some("Everything in your inbox stays too, and older mail remains searchable"),
        nearest(&WINDOW_CHOICES, current.window_days.unwrap_or(30)),
        pending,
        |c, v| c.window_days = Some(v),
    ));
    storage.add(&sync_combo(
        &CACHE_CHOICES,
        "Message Cache",
        Some("Bodies of mail you have read, kept for opening offline"),
        nearest(&CACHE_CHOICES, current.body_cache_mb.unwrap_or(1024)),
        pending,
        |c, v| c.body_cache_mb = Some(v),
    ));
    page.add(&storage);

    let startup = adw::PreferencesGroup::builder().title("Startup").build();
    let login = adw::SwitchRow::builder()
        .title("Start in the Tray at Login")
        .subtitle("mailrs keeps syncing with no window open")
        .build();
    match autostart::path() {
        Some(path) if !app.core.demo => {
            login.set_active(autostart::is_enabled(&path));
            login.connect_active_notify(move |row| {
                let exe = std::env::current_exe().unwrap_or_else(|_| "mailrs".into());
                if let Err(err) = autostart::set_enabled(&path, &exe, row.is_active()) {
                    tracing::warn!(error = %err, "could not change the login item");
                }
            });
        }
        _ => {
            login.set_sensitive(false);
            login.set_subtitle("Not available in demo mode");
        }
    }
    startup.add(&login);
    page.add(&startup);
    page
}

fn switch(
    app: &Rc<App>,
    title: &str,
    subtitle: Option<&str>,
    active: bool,
    set: impl Fn(&mut Settings, bool) + 'static,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(title)
        .active(active)
        .build();
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }
    let (weak, set) = (Rc::downgrade(app), Rc::new(set));
    row.connect_active_notify(move |row| {
        let (Some(app), value, set) = (weak.upgrade(), row.is_active(), Rc::clone(&set)) else {
            return;
        };
        app.update_settings(move |s| set(s, value));
    });
    row
}

fn combo<T: Choice>(
    app: &Rc<App>,
    title: &str,
    subtitle: Option<&str>,
    current: T,
    set: impl Fn(&mut Settings, T) + 'static,
) -> adw::ComboRow {
    let labels: Vec<&str> = T::ALL.iter().map(|c| c.label()).collect();
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(&labels))
        .selected(current.index())
        .build();
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }
    let (weak, set) = (Rc::downgrade(app), Rc::new(set));
    row.connect_selected_notify(move |row| {
        let (Some(app), value, set) = (
            weak.upgrade(),
            T::from_index(row.selected()),
            Rc::clone(&set),
        ) else {
            return;
        };
        app.update_settings(move |s| set(s, value));
    });
    row
}

fn sync_combo<T: Copy + 'static>(
    choices: &'static [(T, &'static str)],
    title: &str,
    subtitle: Option<&str>,
    selected: u32,
    pending: &Rc<RefCell<SyncConfig>>,
    set: impl Fn(&mut SyncConfig, T) + 'static,
) -> adw::ComboRow {
    let labels: Vec<&str> = choices.iter().map(|(_, label)| *label).collect();
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(&labels))
        .selected(selected)
        .build();
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }
    let pending = Rc::clone(pending);
    row.connect_selected_notify(move |row| {
        if let Some((value, _)) = choices.get(row.selected() as usize) {
            set(&mut pending.borrow_mut(), *value);
        }
    });
    row
}

/// The first line of a signature, for the row's subtitle.
fn preview(signature: &str) -> String {
    signature
        .lines()
        .find(|l| !l.trim().is_empty())
        .map_or_else(|| "No signature".to_string(), |l| l.trim().to_string())
}
