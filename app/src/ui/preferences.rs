//! The Preferences dialog. Changes save as they happen; sync options apply
//! when the dialog closes, so the engine restarts once.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::Account;
use mailrs_sync::config::SyncConfig;

use crate::app::App;
use crate::autostart;
use crate::settings::{
    CACHE_CHOICES, Change, Choice, POLL_CHOICES, Settings, WINDOW_CHOICES, nearest,
};

/// Shows Preferences. With `signature_of`, opens on that account's signature.
pub fn present(
    app: &Rc<App>,
    accounts: &[Account],
    parent: &impl IsA<gtk::Widget>,
    signature_of: Option<&str>,
) -> adw::PreferencesDialog {
    let settings = app.settings();
    let dialog = adw::PreferencesDialog::builder()
        .search_enabled(true)
        .build();
    dialog.add(&general_page(app, &settings));
    let writing = writing_page(app, &settings, accounts, signature_of, &dialog);
    dialog.add(&writing);
    if signature_of.is_some() {
        dialog.set_visible_page(&writing);
    }
    let pending = Rc::new(RefCell::new(app.core.sync_config().unwrap_or_default()));
    dialog.add(&sync_page(app, &pending));
    dialog.add(&super::assistant_prefs::page(app, &dialog));
    let weak = Rc::downgrade(app);
    dialog.connect_closed(move |_| {
        let Some(app) = weak.upgrade() else { return };
        if let Err(err) = app.core.update_sync(pending.borrow().clone()) {
            tracing::warn!(error = %err, "could not apply the sync settings");
        }
    });
    dialog.present(Some(parent));
    dialog
}

/// Shows Preferences on the page with this name, such as "assistant".
pub fn present_page(
    app: &Rc<App>,
    accounts: &[Account],
    parent: &impl IsA<gtk::Widget>,
    page: &str,
) {
    present(app, accounts, parent, None).set_visible_page_name(page);
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
        Change::Threading,
    ));
    reading.add(&switch(
        app,
        "Group Inbox into Categories",
        Some("Sort the inbox into Primary, Updates, Promotions, and Social, as Gmail does"),
        settings.inbox_categories,
        Change::InboxCategories,
    ));
    reading.add(&switch(
        app,
        "Suggest Follow-Ups",
        Some("List mail you sent that has had no reply for three days"),
        settings.suggest_follow_ups,
        Change::SuggestFollowUps,
    ));
    reading.add(&combo(
        app,
        "Mark as Read",
        None,
        settings.mark_read,
        Change::MarkRead,
    ));
    reading.add(&combo(
        app,
        "Remote Images",
        Some("Loading them can tell senders when you read their mail"),
        settings.remote_images,
        Change::RemoteImages,
    ));
    reading.add(&combo(
        app,
        "Text Size",
        None,
        settings.text_size,
        Change::TextSize,
    ));
    page.add(&reading);

    let contacts = adw::PreferencesGroup::builder()
        .title("Contacts")
        .description(
            "Penguin Mail can read the contacts of each Google account: names, email \
             addresses, photos, organizations, and phone numbers. It uses them to suggest \
             recipients, to show faces beside mail, and to fill the card behind a sender's \
             name. What it reads stays on this computer, and turning this off deletes it.",
        )
        .build();
    contacts.add(&switch_with(
        app,
        "Use Google Contacts",
        Some("Google asks your permission the first time"),
        settings.contacts,
        |app, on| app.set_contacts(on),
    ));
    page.add(&contacts);

    let appearance = adw::PreferencesGroup::builder().title("Appearance").build();
    appearance.add(&combo(
        app,
        "Style",
        None,
        settings.color_scheme,
        Change::ColorScheme,
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
        Change::Notifications,
    );
    let previews = switch(
        app,
        "Show Sender and Subject",
        Some("Turn off to see only how much mail arrived"),
        settings.notification_previews,
        Change::NotificationPreviews,
    );
    enabled
        .bind_property("active", &previews, "sensitive")
        .sync_create()
        .build();
    let vips_only = switch(
        app,
        "Only for VIPs",
        Some("Stay quiet about mail from everyone else"),
        settings.notify_vips_only,
        Change::NotifyVipsOnly,
    );
    enabled
        .bind_property("active", &vips_only, "sensitive")
        .sync_create()
        .build();
    notifications.add(&enabled);
    notifications.add(&vips_only);
    notifications.add(&previews);
    page.add(&notifications);
    page
}

fn writing_page(
    app: &Rc<App>,
    settings: &Settings,
    accounts: &[Account],
    signature_of: Option<&str>,
    dialog: &adw::PreferencesDialog,
) -> adw::PreferencesPage {
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
        app.change_settings(Change::DefaultAccount(Some(email)));
    });
    sending.add(&from);
    sending.add(&combo(
        app,
        "New Messages Start As",
        Some("Rich text styles the words themselves; Markdown shows its marks"),
        settings.compose_format,
        Change::ComposeFormat,
    ));
    sending.add(&combo(
        app,
        "Undo Send",
        Some("How long you can take a message back after sending it"),
        settings.undo_send,
        Change::UndoSend,
    ));
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
            .expanded(signature_of.is_some_and(|e| e.eq_ignore_ascii_case(&account.email)))
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
        let import = gtk::Button::builder()
            .label("Import from Gmail")
            .halign(gtk::Align::End)
            .margin_top(6)
            .margin_bottom(6)
            .margin_end(6)
            .css_classes(["flat"])
            .build();
        row.add_row(&import);
        let (weak, account_id, target, toasts) = (
            Rc::downgrade(app),
            account.id,
            view.buffer(),
            dialog.clone(),
        );
        import.connect_clicked(move |button| {
            let Some(app) = weak.upgrade() else { return };
            let Some(sync) = app.core.account(account_id) else {
                toasts.add_toast(adw::Toast::new("This account is not syncing yet"));
                return;
            };
            button.set_sensitive(false);
            let (button, target, toasts) = (button.clone(), target.clone(), toasts.clone());
            glib::spawn_future_local(async move {
                match app
                    .core
                    .call(async move { sync.gmail_signature().await })
                    .await
                {
                    Ok(Some(signature)) => {
                        target.set_text(&signature);
                        toasts.add_toast(adw::Toast::new("Imported the signature from Gmail"));
                    }
                    Ok(None) => {
                        toasts.add_toast(adw::Toast::new("Gmail has no signature for this account"))
                    }
                    Err(err) => {
                        toasts.add_toast(adw::Toast::new(&format!("Could not import: {err}")))
                    }
                }
                button.set_sensitive(true);
            });
        });
        let (weak, email, subtitle) = (Rc::downgrade(app), account.email.clone(), row.clone());
        view.buffer().connect_changed(move |buffer| {
            let Some(app) = weak.upgrade() else { return };
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();
            subtitle.set_subtitle(&preview(&text));
            let email = email.clone();
            app.change_settings(Change::Signature { email, text });
        });
        signatures.add(&row);
    }
    page.add(&signatures);
    page.add(&spelling_group(app, settings, accounts));
    page
}

/// Which dictionaries are installed, and which one each account writes in.
///
/// With none installed the group says so rather than leaving the composer
/// quietly unchecked, because a missing dictionary is a package away and the
/// writer is the only one who can install it.
fn spelling_group(
    app: &Rc<App>,
    settings: &Settings,
    accounts: &[Account],
) -> adw::PreferencesGroup {
    let installed = app.installed_dictionaries();
    let group = adw::PreferencesGroup::builder()
        .title("Spelling")
        .description(if installed.is_empty() {
            "No dictionaries are installed, so Penguin Mail is not checking \
             spelling. Install a Hunspell dictionary, such as hunspell-en-us \
             or hunspell-pt-pt, and reopen the composer."
                .to_string()
        } else {
            format!("Dictionaries found: {}.", installed.join(", "))
        })
        .build();
    if installed.is_empty() {
        return group;
    }
    // Following the desktop's language is the first choice, then one
    // dictionary per row, then both English and Portuguese together for
    // anyone who writes in two languages.
    let mut choices: Vec<(String, Vec<String>)> = vec![(
        format!(
            "Follow the System Language ({})",
            crate::ui::composer::spell::locale_language()
        ),
        Vec::new(),
    )];
    choices.extend(
        installed
            .iter()
            .map(|language| (language.clone(), vec![language.clone()])),
    );
    if installed.len() > 1 {
        choices.push((installed.join(" and "), installed.clone()));
    }
    for account in accounts {
        let current = settings
            .spell_languages
            .get(&account.email.to_lowercase())
            .cloned()
            .unwrap_or_default();
        let labels: Vec<&str> = choices.iter().map(|(label, _)| label.as_str()).collect();
        let selected = choices
            .iter()
            .position(|(_, languages)| *languages == current)
            .unwrap_or(0);
        let row = adw::ComboRow::builder()
            .title("Check Spelling In")
            .subtitle(&account.email)
            .model(&gtk::StringList::new(&labels))
            .selected(selected as u32)
            .build();
        let (weak, email, choices) = (Rc::downgrade(app), account.email.clone(), choices.clone());
        row.connect_selected_notify(move |row| {
            let (Some(app), Some((_, languages))) =
                (weak.upgrade(), choices.get(row.selected() as usize))
            else {
                return;
            };
            app.change_settings(Change::SpellLanguages {
                account: email.clone(),
                languages: languages.clone(),
            });
        });
        group.add(&row);
    }
    group
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
        .subtitle("Penguin Mail keeps syncing with no window open")
        .build();
    match autostart::path() {
        Some(path) if !app.core.demo => {
            login.set_active(autostart::is_enabled(&path));
            login.connect_active_notify(move |row| {
                let exe = std::env::current_exe().unwrap_or_else(|_| "penguin-mail".into());
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
    change: impl Fn(bool) -> Change + 'static,
) -> adw::SwitchRow {
    switch_with(app, title, subtitle, active, move |app, on| {
        app.change_settings(change(on));
    })
}

/// A switch whose change the app has to do more about than save it.
fn switch_with(
    app: &Rc<App>,
    title: &str,
    subtitle: Option<&str>,
    active: bool,
    flip: impl Fn(&Rc<App>, bool) + 'static,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(title)
        .active(active)
        .build();
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }
    let weak = Rc::downgrade(app);
    row.connect_active_notify(move |row| {
        if let Some(app) = weak.upgrade() {
            flip(&app, row.is_active());
        }
    });
    row
}

fn combo<T: Choice>(
    app: &Rc<App>,
    title: &str,
    subtitle: Option<&str>,
    current: T,
    change: impl Fn(T) -> Change + 'static,
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
    let weak = Rc::downgrade(app);
    row.connect_selected_notify(move |row| {
        if let Some(app) = weak.upgrade() {
            app.change_settings(change(T::from_index(row.selected())));
        }
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
