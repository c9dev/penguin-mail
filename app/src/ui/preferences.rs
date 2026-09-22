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
use crate::language;
use crate::settings::{
    Change, Choice, Settings, cache_choices, nearest, poll_choices, window_choices,
};
use crate::ui::window::Notice;
use mailrs_domain::translate::{fill, fill_plural, gettext};

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
    dialog.add(&super::contacts_prefs::page(app, &settings, accounts));
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
        .title(gettext("General"))
        .icon_name("emblem-system-symbolic")
        .build();

    let reading = adw::PreferencesGroup::builder()
        .title(gettext("Reading"))
        .build();
    reading.add(&switch(
        app,
        &gettext("Group Messages into Conversations"),
        Some(&gettext(
            "Show a thread's replies together instead of one row per message",
        )),
        settings.threading,
        Change::Threading,
    ));
    reading.add(&switch(
        app,
        &gettext("Group Inbox into Categories"),
        Some(&gettext(
            "Sort the inbox into Primary, Updates, Promotions, and Social, as Gmail does",
        )),
        settings.inbox_categories,
        Change::InboxCategories,
    ));
    reading.add(&combo(
        app,
        &gettext("Open the Inbox On"),
        Some(&gettext("Which category the window starts on")),
        settings.default_category,
        Change::DefaultCategory,
    ));
    reading.add(&switch(
        app,
        &gettext("Suggest Follow-Ups"),
        Some(&gettext(
            "List mail you sent that has had no reply for three days",
        )),
        settings.suggest_follow_ups,
        Change::SuggestFollowUps,
    ));
    reading.add(&combo(
        app,
        &gettext("Mark as Read"),
        None,
        settings.mark_read,
        Change::MarkRead,
    ));
    reading.add(&combo(
        app,
        &gettext("Remote Images"),
        Some(&gettext(
            "Loading them can tell senders when you read their mail",
        )),
        settings.remote_images,
        Change::RemoteImages,
    ));
    reading.add(&allowed_image_senders(app));
    reading.add(&combo(
        app,
        &gettext("Text Size"),
        None,
        settings.text_size,
        Change::TextSize,
    ));
    page.add(&reading);

    let appearance = adw::PreferencesGroup::builder()
        .title(gettext("Appearance"))
        .build();
    appearance.add(&combo(
        app,
        &gettext("Style"),
        None,
        settings.color_scheme,
        Change::ColorScheme,
    ));
    appearance.add(&language_row(app, settings));
    page.add(&appearance);

    let notifications = adw::PreferencesGroup::builder()
        .title(gettext("Notifications"))
        .build();
    let enabled = switch(
        app,
        &gettext("Notify About New Mail"),
        None,
        settings.notifications,
        Change::Notifications,
    );
    let previews = switch(
        app,
        &gettext("Show Sender and Subject"),
        Some(&gettext("Turn off to see only how much mail arrived")),
        settings.notification_previews,
        Change::NotificationPreviews,
    );
    enabled
        .bind_property("active", &previews, "sensitive")
        .sync_create()
        .build();
    let vips_only = switch(
        app,
        &gettext("Only for VIPs"),
        Some(&gettext("Stay quiet about mail from everyone else")),
        settings.notify_vips_only,
        Change::NotifyVipsOnly,
    );
    enabled
        .bind_property("active", &vips_only, "sensitive")
        .sync_create()
        .build();
    let actions = adw::ExpanderRow::builder()
        .title(gettext("Buttons"))
        .subtitle(gettext(
            "What a notification offers besides opening the conversation",
        ))
        .build();
    for button in crate::notify::Button::ALL {
        actions.add_row(&switch(
            app,
            &button.label(),
            None,
            settings.notification_buttons.contains(&button),
            move |show| Change::NotificationButton { button, show },
        ));
    }
    enabled
        .bind_property("active", &actions, "sensitive")
        .sync_create()
        .build();
    notifications.add(&enabled);
    notifications.add(&vips_only);
    notifications.add(&previews);
    notifications.add(&actions);
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
        .title(gettext("Writing"))
        .icon_name("document-edit-symbolic")
        .build();
    if accounts.is_empty() {
        let empty = adw::PreferencesGroup::builder()
            .title(gettext("No Accounts Yet"))
            .description(gettext(
                "Add an account to choose a sender and write signatures.",
            ))
            .build();
        page.add(&empty);
        page.add(&super::templates::group(app));
        return page;
    }

    let sending = adw::PreferencesGroup::builder()
        .title(gettext("New Messages"))
        .build();
    let emails: Vec<String> = accounts.iter().map(|a| a.email.clone()).collect();
    let labels: Vec<&str> = emails.iter().map(String::as_str).collect();
    let current = settings
        .default_account
        .as_ref()
        .and_then(|d| emails.iter().position(|e| e.eq_ignore_ascii_case(d)))
        .unwrap_or(0);
    let from = adw::ComboRow::builder()
        .title(gettext("Send New Messages From"))
        .subtitle(gettext(
            "Replies always come from the account that received the message",
        ))
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
        &gettext("New Messages Start As"),
        Some(&gettext(
            "Rich text styles the words themselves; Markdown shows its marks",
        )),
        settings.compose_format,
        Change::ComposeFormat,
    ));
    sending.add(&combo(
        app,
        &gettext("Undo Send"),
        Some(&gettext(
            "How long you can take a message back after sending it",
        )),
        settings.undo_send,
        Change::UndoSend,
    ));
    sending.add(&switch(
        app,
        &gettext("Check for Missing Attachments"),
        Some(&gettext(
            "Ask before sending a message that promises a file and carries none",
        )),
        settings.check_attachments,
        Change::CheckAttachments,
    ));
    page.add(&sending);

    let signatures = adw::PreferencesGroup::builder()
        .title(gettext("Signatures"))
        .description(gettext(
            "Added below new messages and above quoted text in replies. Markdown works.",
        ))
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
        super::name(
            &view,
            &fill(
                &gettext("Signature for {account}"),
                &[("account", &account.email)],
            ),
        );
        let frame = gtk::ScrolledWindow::builder()
            .child(&view)
            .min_content_height(96)
            .max_content_height(220)
            .propagate_natural_height(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        row.add_row(&frame);
        let import = gtk::Button::builder()
            .label(gettext("Import from Gmail"))
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
                toasts.add_toast(adw::Toast::new(&gettext("This account is not syncing yet")));
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
                        toasts.add_toast(adw::Toast::new(&gettext(
                            "Imported the signature from Gmail",
                        )));
                    }
                    Ok(None) => toasts.add_toast(adw::Toast::new(&gettext(
                        "Gmail has no signature for this account",
                    ))),
                    Err(err) => {
                        let said = fill(
                            &gettext("Could not import: {reason}"),
                            &[("reason", &err.to_string())],
                        );
                        toasts.add_toast(adw::Toast::new(&said));
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
    page.add(&super::templates::group(app));
    page.add(&spelling_group(app, settings, accounts));
    if let Some(protection) = protection_group(app, settings, accounts) {
        page.add(&protection);
    }
    page
}

/// What this computer signs and encrypts with, which of the writer's
/// addresses it holds something for, and what to do about it without being
/// asked every time.
///
/// One group covers both standards, because the writer chooses to sign or
/// to encrypt rather than choosing between OpenPGP and S/MIME. With
/// neither gpg nor gpgsm on the computer there is no group at all: a switch
/// that could do nothing is worse than no switch, and installing GnuPG is
/// the only thing that would change the answer.
fn protection_group(
    app: &Rc<App>,
    settings: &Settings,
    accounts: &[Account],
) -> Option<adw::PreferencesGroup> {
    if !app.core.has_gpg() && !app.core.has_gpgsm() {
        return None;
    }
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Signing and Encryption"))
        .description(gettext(
            "Penguin Mail signs and encrypts through GnuPG, which holds your keys and \
             asks for your passphrase itself.",
        ))
        .build();
    let keys = adw::ActionRow::builder()
        .title(gettext("Your OpenPGP Keys"))
        .subtitle(gettext("Asking gpg…"))
        .visible(app.core.has_gpg())
        .build();
    let certificates = adw::ActionRow::builder()
        .title(gettext("Your S/MIME Certificates"))
        .subtitle(gettext("Asking gpgsm…"))
        .visible(app.core.has_gpgsm())
        .build();
    group.add(&keys);
    group.add(&certificates);
    group.add(&switch(
        app,
        &gettext("Sign My Messages by Default"),
        Some(&gettext("New messages open with Sign turned on")),
        settings.sign_by_default,
        Change::SignByDefault,
    ));
    group.add(&switch(
        app,
        &gettext("Encrypt When I Can"),
        Some(&gettext(
            "Turn Encrypt on as soon as every recipient has a key or a certificate",
        )),
        settings.encrypt_when_possible,
        Change::EncryptWhenPossible,
    ));
    // Every answer here means running a program, so the group goes up
    // saying so and fills itself in.
    let mut addresses: Vec<String> = accounts.iter().map(|a| a.email.clone()).collect();
    for alias in settings.send_as.values().flatten() {
        addresses.push(alias.email.clone());
    }
    addresses.sort();
    addresses.dedup();
    let (app, filling) = (Rc::clone(app), group.clone());
    glib::spawn_future_local(async move {
        let mut programs = Vec::new();
        if app.core.has_gpg() {
            let version = app.core.gpg(|pgp| Ok(crate::pgp::version(pgp))).await;
            programs.push(named("gpg", version.ok().flatten()));
            let wanted = addresses.clone();
            match app.core.gpg(move |pgp| pgp.keys_for(&wanted)).await {
                Ok(held) => keys.set_subtitle(&crate::pgp::own_keys(&held)),
                Err(err) => keys.set_subtitle(&fill(
                    &gettext("gpg could not be asked: {reason}"),
                    &[("reason", &err.to_string())],
                )),
            }
        }
        if app.core.has_gpgsm() {
            let version = app
                .core
                .gpgsm(|smime| Ok(crate::smime::version(smime)))
                .await;
            programs.push(named("gpgsm", version.ok().flatten()));
            let wanted = addresses.clone();
            match app
                .core
                .gpgsm(move |smime| smime.signing_certificates(&wanted))
                .await
            {
                Ok(held) => certificates.set_subtitle(&crate::smime::own_certificates(&held)),
                Err(err) => certificates.set_subtitle(&fill(
                    &gettext("gpgsm could not be asked: {reason}"),
                    &[("reason", &err.to_string())],
                )),
            }
        }
        let named =
            crate::protection::joined(&programs.iter().map(String::as_str).collect::<Vec<_>>());
        filling.set_description(Some(&fill(
            &gettext(
                "Penguin Mail signs and encrypts through {programs}, which holds your \
                 keys and asks for your passphrase itself.",
            ),
            &[("programs", &named)],
        )));
    });
    Some(group)
}

/// One program with the version it reported, for the line naming what the
/// signing runs through. A program that would not say leaves the number
/// out rather than guessing at one.
fn named(program: &str, version: Option<String>) -> String {
    match version {
        Some(version) => format!("{program} {version}"),
        None => program.to_string(),
    }
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
        .title(gettext("Spelling"))
        .description(if installed.is_empty() {
            gettext(
                "No dictionaries are installed, so Penguin Mail is not checking \
                 spelling. Install a Hunspell dictionary, such as hunspell-en-us \
                 or hunspell-pt-pt, and reopen the composer.",
            )
        } else {
            fill(
                &gettext("Dictionaries found: {languages}."),
                &[("languages", &installed.join(", "))],
            )
        })
        .build();
    if installed.is_empty() {
        return group;
    }
    // Following the desktop's language is the first choice, then one
    // dictionary per row, then both English and Portuguese together for
    // anyone who writes in two languages.
    let mut choices: Vec<(String, Vec<String>)> = vec![(
        fill(
            &gettext("Follow the System Language ({language})"),
            &[("language", &crate::ui::composer::spell::locale_language())],
        ),
        Vec::new(),
    )];
    choices.extend(
        installed
            .iter()
            .map(|language| (language.clone(), vec![language.clone()])),
    );
    if installed.len() > 1 {
        let both =
            crate::protection::joined(&installed.iter().map(String::as_str).collect::<Vec<_>>());
        choices.push((both, installed.clone()));
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
            .title(gettext("Check Spelling In"))
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
        .title(gettext("Sync"))
        .icon_name("mail-send-receive-symbolic")
        .build();
    let current = pending.borrow().clone();

    let checking = adw::PreferencesGroup::builder()
        .title(gettext("Checking"))
        .build();
    let poll = current.poll_seconds.map_or(30, |s| s as i64);
    let polls = poll_choices();
    checking.add(&sync_combo(
        &polls,
        &gettext("Check for New Mail"),
        None,
        nearest(&polls, poll),
        pending,
        |c, v| c.poll_seconds = Some(v as u64),
    ));
    page.add(&checking);

    let storage = adw::PreferencesGroup::builder()
        .title(gettext("Storage"))
        .build();
    let windows = window_choices();
    storage.add(&sync_combo(
        &windows,
        &gettext("Keep Mail on This Computer For"),
        Some(&gettext(
            "Everything in your inbox stays too, and older mail remains searchable",
        )),
        nearest(&windows, current.window_days.unwrap_or(30)),
        pending,
        |c, v| c.window_days = Some(v),
    ));
    let caches = cache_choices();
    storage.add(&sync_combo(
        &caches,
        &gettext("Message Cache"),
        Some(&gettext(
            "Bodies of mail you have read, kept for opening offline",
        )),
        nearest(&caches, current.body_cache_mb.unwrap_or(1024)),
        pending,
        |c, v| c.body_cache_mb = Some(v),
    ));
    page.add(&storage);

    let startup = adw::PreferencesGroup::builder()
        .title(gettext("Startup"))
        .build();
    let login = adw::SwitchRow::builder()
        .title(gettext("Start in the Tray at Login"))
        .subtitle(gettext("Penguin Mail keeps syncing with no window open"))
        .build();
    match autostart::path() {
        Some(path) if !app.core.demo => {
            login.set_active(autostart::is_enabled(&path));
            login.connect_active_notify(move |row| {
                let exe = crate::exe::path().unwrap_or_else(|_| "penguin-mail".into());
                if let Err(err) = autostart::set_enabled(&path, &exe, row.is_active()) {
                    tracing::warn!(error = %err, "could not change the login item");
                }
            });
        }
        _ => {
            login.set_sensitive(false);
            login.set_subtitle(&gettext("Not available in demo mode"));
        }
    }
    startup.add(&login);
    if app.can_update() {
        startup.add(&switch(
            app,
            &gettext("Check for Updates"),
            Some(&gettext("Look for a new release once a day")),
            app.settings().check_for_updates,
            Change::CheckForUpdates,
        ));
    }
    page.add(&startup);
    page
}

/// The senders whose images load without asking, each with a way off the
/// list. The rows fill in once the store answers, so opening Preferences
/// never waits on it.
fn allowed_image_senders(app: &Rc<App>) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder()
        .title(gettext("Senders Who May Load Images"))
        .subtitle(gettext("Nobody yet"))
        .build();
    let (app, shown) = (Rc::clone(app), row.clone());
    glib::spawn_future_local(async move {
        let Ok(list) = app.core.read(mailrs_store::image_senders::list).await else {
            return;
        };
        shown.set_subtitle(&match list.len() {
            0 => gettext("Nobody yet"),
            count => fill_plural(
                "{count} sender",
                "{count} senders",
                count,
                &[("count", &count.to_string())],
            ),
        });
        for entry in list {
            let item = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&entry.sender))
                .subtitle(if entry.whole_domain {
                    gettext("Anyone at this domain")
                } else {
                    gettext("This address")
                })
                .build();
            let remove = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text(gettext("Stop Loading Images from This Sender"))
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            super::name(
                &remove,
                &fill(
                    &gettext("Stop loading images from {sender}"),
                    &[("sender", &entry.sender)],
                ),
            );
            let (app, sender, listed, removed) = (
                Rc::clone(&app),
                entry.sender.clone(),
                shown.clone(),
                item.clone(),
            );
            remove.connect_clicked(move |_| {
                let (app, sender) = (Rc::clone(&app), sender.clone());
                let (listed, removed) = (listed.clone(), removed.clone());
                glib::spawn_future_local(async move {
                    let gone = sender.clone();
                    if app
                        .core
                        .write(move |c| mailrs_store::image_senders::forget(c, &gone))
                        .await
                        .is_ok()
                    {
                        listed.remove(&removed);
                        // The window keeps its own copy of the list.
                        app.tell_window(Notice::ImageSendersChanged);
                    }
                });
            });
            item.add_suffix(&remove);
            shown.add_row(&item);
        }
    });
    row
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

/// The language the interface speaks. Follow System comes first and is
/// what a fresh copy does; the rows under it are the translations this
/// computer has, so one that is not installed is never offered.
fn language_row(app: &Rc<App>, settings: &Settings) -> adw::ComboRow {
    let languages = Rc::new(language::choices());
    let mut labels = vec![gettext("Follow System")];
    labels.extend(languages.iter().map(|language| language.name.clone()));
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let row = adw::ComboRow::builder()
        .title(gettext("Language"))
        .subtitle(gettext("Penguin Mail shows a new language after a restart"))
        .model(&gtk::StringList::new(&labels))
        .selected(language::row_of(&languages, &settings.language))
        .build();
    let weak = Rc::downgrade(app);
    row.connect_selected_notify(move |row| {
        if let Some(app) = weak.upgrade() {
            let code = language::code_at(&languages, row.selected());
            app.change_settings(Change::Language(code));
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
    let labels: Vec<String> = T::ALL.iter().map(|c| c.label()).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
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
    choices: &[(T, String)],
    title: &str,
    subtitle: Option<&str>,
    selected: u32,
    pending: &Rc<RefCell<SyncConfig>>,
    set: impl Fn(&mut SyncConfig, T) + 'static,
) -> adw::ComboRow {
    let labels: Vec<&str> = choices.iter().map(|(_, label)| label.as_str()).collect();
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(&labels))
        .selected(selected)
        .build();
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }
    let pending = Rc::clone(pending);
    let values: Vec<T> = choices.iter().map(|(value, _)| *value).collect();
    row.connect_selected_notify(move |row| {
        if let Some(value) = values.get(row.selected() as usize) {
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
        .map_or_else(|| gettext("No signature"), |l| l.trim().to_string())
}
