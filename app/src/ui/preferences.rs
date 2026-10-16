//! The Preferences dialog. Changes save as they happen; sync options apply
//! when the dialog closes, so the engine restarts once.

mod accounts;
pub(crate) mod dialog;
pub(crate) mod space;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::calendar::hours::WorkingHours;
use mailrs_domain::calendar::week::WeekStart;
use mailrs_domain::translate::date_locale;
use mailrs_domain::{Account, AccountId};
use mailrs_sync::config::SyncConfig;
use mailrs_sync::{Offers, Withheld};

use crate::app::App;
use crate::autostart;
use crate::language;
use crate::offered::missing_lines;
use crate::settings::{
    Change, Choice, Settings, cache_choices, nearest, nearest_window, poll_choices, window_choices,
    window_note,
};
use crate::ui::window::Notice;
use mailrs_domain::translate::{fill, fill_plural, gettext, with_reason};

#[derive(Clone, Copy)]
pub(crate) enum Focus<'a> {
    Signature(&'a str),
    Storage(&'a str),
}

impl Focus<'_> {
    fn account(&self) -> &str {
        match self {
            Self::Signature(email) | Self::Storage(email) => email,
        }
    }
}

/// Shows Preferences for `accounts`, each with what its server `offers`
/// and what its own consent `withheld`. `grant` runs the consent flow
/// again for one account's Grant Access button. `focus` opens the account
/// whose signature or storage the person asked to see.
pub fn present(
    app: &Rc<App>,
    accounts: &[Account],
    offers: impl Fn(AccountId) -> Offers,
    withheld: impl Fn(AccountId) -> Withheld,
    grant: impl Fn(AccountId) + Clone + 'static,
    parent: &impl IsA<gtk::Widget>,
    focus: Option<Focus<'_>>,
) -> Rc<dialog::Dialog> {
    let settings = app.settings();
    let offered: Vec<(Account, Offers)> = accounts
        .iter()
        .map(|account| (account.clone(), offers(account.id)))
        .collect();
    // A calendar account belongs on the Calendar page alone: the other
    // pages that list accounts are about mail and contacts, which it has
    // neither of.
    let with_mail: Vec<(Account, Offers)> = offered
        .iter()
        .filter(|(account, _)| account.provider.serves_mail())
        .cloned()
        .collect();
    let mail_accounts = mailrs_domain::mail_accounts(accounts);
    let dialog = dialog::Dialog::new();
    // The phone layout. A dialog applies one breakpoint at a time, so
    // every page puts its narrow setters on this one.
    let narrow = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 450sp").expect("valid breakpoint"),
    );
    let accounts_page = accounts_page(
        app,
        &with_mail,
        &withheld,
        grant.clone(),
        focus,
        &dialog.toasts,
        &narrow,
    );
    dialog.add(&accounts_page);
    dialog.add(&reading_page(app, &settings));
    dialog.add(&writing_page(
        app,
        &settings,
        &mail_accounts,
        &dialog.toasts,
    ));
    dialog.add(&notifications_page(app, &settings));
    dialog.add(&super::contacts_prefs::calendar_page(
        app,
        &offered,
        withheld,
        grant,
        &calendar_rows(app, &settings),
    ));
    dialog.add(&appearance_page(app, &settings));
    let pending = Rc::new(RefCell::new(app.core.sync_config()));
    dialog.add(&sync_page(app, &pending, &narrow));
    dialog.add(&super::assistant_prefs::page(app, &dialog.toasts));
    if focus.is_some() {
        dialog.set_visible_page(&accounts_page);
    }
    dialog.adapt(&narrow);
    dialog.window.add_breakpoint(narrow);
    let weak = Rc::downgrade(app);
    dialog.window.connect_closed(move |_| {
        let Some(app) = weak.upgrade() else { return };
        if let Err(err) = app.core.update_sync(pending.borrow().clone()) {
            tracing::warn!(error = %err, "could not apply the sync settings");
        }
    });
    dialog.present(parent);
    dialog
}

/// Shows Preferences on the page with this name, such as "assistant".
pub fn present_page(
    app: &Rc<App>,
    accounts: &[Account],
    offers: impl Fn(AccountId) -> Offers,
    withheld: impl Fn(AccountId) -> Withheld,
    grant: impl Fn(AccountId) + Clone + 'static,
    parent: &impl IsA<gtk::Widget>,
    page: &str,
) -> Rc<dialog::Dialog> {
    let dialog = present(app, accounts, offers, withheld, grant, parent, None);
    dialog.set_visible_page_name(page);
    dialog
}

fn reading_page(app: &Rc<App>, settings: &Settings) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(gettext("Reading"))
        .name("reading")
        .icon_name("view-paged-symbolic")
        .build();
    let reading = adw::PreferencesGroup::builder()
        .title(gettext("Inbox"))
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
    page.add(&reading);
    let images = adw::PreferencesGroup::builder()
        .title(gettext("Images and Receipts"))
        .build();
    images.add(&combo(
        app,
        &gettext("Remote Images"),
        Some(&gettext(
            "Loading them can tell senders when you read their mail",
        )),
        settings.remote_images,
        Change::RemoteImages,
    ));
    images.add(&allowed_image_senders(app));
    images.add(&combo(
        app,
        &gettext("Read Receipts"),
        Some(&gettext(
            "When a sender asks to know you read their message",
        )),
        settings.read_receipts,
        Change::ReadReceipts,
    ));
    page.add(&images);
    let translation = adw::PreferencesGroup::builder()
        .title(gettext("Translation"))
        .build();
    translation.add(&never_translate_row(app, settings));
    page.add(&translation);
    page
}

fn appearance_page(app: &Rc<App>, settings: &Settings) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(gettext("Appearance"))
        .name("appearance")
        .icon_name("preferences-desktop-display-symbolic")
        .build();
    let appearance = adw::PreferencesGroup::new();
    appearance.add(&combo(
        app,
        &gettext("Style"),
        None,
        settings.color_scheme,
        Change::ColorScheme,
    ));
    appearance.add(&combo(
        app,
        &gettext("Text Size"),
        None,
        settings.text_size,
        Change::TextSize,
    ));
    appearance.add(&language_row(app, settings));
    page.add(&appearance);
    page
}

fn notifications_page(app: &Rc<App>, settings: &Settings) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(gettext("Notifications"))
        .name("notifications")
        .icon_name("preferences-system-notifications-symbolic")
        .build();
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
    let actions = crate::ui::disclosure::row(
        adw::ExpanderRow::builder()
            .title(gettext("Buttons"))
            .subtitle(gettext(
                "What a notification offers besides opening the conversation",
            ))
            .build(),
    );
    actions.add_css_class("preference-explanation");
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

/// Event reminders stay independent of the new-mail notification switch.
fn calendar_rows(app: &Rc<App>, settings: &Settings) -> Vec<gtk::Widget> {
    vec![
        switch(
            app,
            &gettext("Event Reminders"),
            Some(&gettext(
                "A notification before each event, at the times the event or its calendar sets",
            )),
            settings.event_reminders,
            Change::EventReminders,
        )
        .upcast(),
        week_start_row(app, settings.week_start).upcast(),
        working_hours_row(app, settings.working_hours).upcast(),
    ]
}

fn accounts_page(
    app: &Rc<App>,
    accounts: &[(Account, Offers)],
    withheld: &impl Fn(AccountId) -> Withheld,
    grant: impl Fn(AccountId) + Clone + 'static,
    focus: Option<Focus<'_>>,
    dialog: &adw::ToastOverlay,
    narrow: &adw::Breakpoint,
) -> adw::PreferencesPage {
    let settings = app.settings();
    let page = adw::PreferencesPage::builder()
        .title(gettext("Accounts"))
        .name("accounts")
        .icon_name("avatar-default-symbolic")
        .build();
    let defaults = accounts::Defaults::new(app);
    page.add(&defaults.group);
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Accounts"))
        .description(gettext("Signature, addresses you send from, servers, contacts, and the space the server reports."))
        .build();
    if accounts.is_empty() {
        group.add(
            &adw::ActionRow::builder()
                .title(gettext("No Accounts Yet"))
                .subtitle(gettext(
                    "Add an account to choose a sender and write signatures.",
                ))
                .build(),
        );
    }
    let order = accounts::Order::new(app, &group, defaults);
    let storage = app.storage();
    let mut space_rows = Vec::new();
    let missing = missing_lines(accounts, |id, missing| app.core.missed(id, missing));
    for (index, (account, offers)) in accounts.iter().enumerate() {
        let expanded = focus.map_or(index == 0, |focus| {
            focus.account().eq_ignore_ascii_case(&account.email)
        });
        let row = crate::ui::disclosure::row(
            adw::ExpanderRow::builder()
                .use_markup(false)
                .title(&account.email)
                .subtitle(preview(settings.signature(&account.email)))
                .expanded(expanded)
                .build(),
        );
        row.add_row(&signature_row(
            app,
            &settings,
            account,
            matches!(focus, Some(Focus::Signature(_))) && expanded,
            dialog,
            &row,
        ));
        row.add_row(&super::sender_prefs::account_row(app, account));
        super::contacts_prefs::account_rows(
            app,
            &settings,
            account,
            *offers,
            withheld(account.id),
            grant.clone(),
            &row,
        );
        let unavailable = if account.provider == mailrs_domain::Provider::Microsoft {
            gettext("Mailbox storage is not available through this connection.")
        } else {
            gettext("The server has not reported its storage usage.")
        };
        let (space_row, update) = space::Row::new(
            &gettext("Space on the Server"),
            storage.get(&account.id).copied(),
            &unavailable,
            narrow,
        );
        row.add_row(&space_row);
        space_rows.push((account.id, update));
        if let Some((_, reasons)) = missing.iter().find(|(email, _)| email == &account.email) {
            let unavailable = adw::ActionRow::builder()
                .use_markup(false)
                .title(gettext("Not Available"))
                .subtitle(reasons.join("\n"))
                .build();
            unavailable
                .upcast_ref::<gtk::Widget>()
                .reset_relation(gtk::AccessibleRelation::LabelledBy);
            super::name(
                &unavailable,
                &crate::offered::missing_name(&account.email, reasons),
            );
            row.add_row(&unavailable);
        }
        row.add_css_class("account-settings");
        group.add(&row);
        order.add(&row, &account.email);
        if expanded && focus.is_some() {
            let row = row.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(row) = row.upgrade() {
                    row.child_focus(gtk::DirectionType::TabForward);
                }
            });
        }
    }
    order.refresh();
    let (weak_app, weak_group) = (Rc::downgrade(app), group.downgrade());
    let mut previous = storage;
    glib::timeout_add_seconds_local(1, move || {
        let (Some(app), Some(_group)) = (weak_app.upgrade(), weak_group.upgrade()) else {
            return glib::ControlFlow::Break;
        };
        let found = app.storage();
        if found != previous {
            for (id, row) in &space_rows {
                row.update(found.get(id).copied());
            }
            previous = found;
        }
        glib::ControlFlow::Continue
    });
    page.add(&group);
    page
}

fn signature_row(
    app: &Rc<App>,
    settings: &Settings,
    account: &Account,
    expanded: bool,
    dialog: &adw::ToastOverlay,
    account_row: &adw::ExpanderRow,
) -> adw::ExpanderRow {
    let text = settings.signature(&account.email).to_string();
    // The subtitle is the signature's first line, which may hold "&"
    // or a Markdown link in angle brackets.
    let row = crate::ui::disclosure::row(
        adw::ExpanderRow::builder()
            .use_markup(false)
            .title(gettext("Signature"))
            .subtitle(preview(&text))
            .expanded(expanded)
            .build(),
    );
    let view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(10)
        .bottom_margin(10)
        .left_margin(12)
        .right_margin(12)
        .accepts_tab(false)
        .build();
    let help =
        gettext("Added below new messages and above quoted text in replies. Markdown works.");
    dialog::explain(&row, &help);
    dialog::explain(&view, &help);
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
    if account.provider == mailrs_domain::Provider::Gmail {
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
            dialog.downgrade(),
        );
        import.connect_clicked(move |button| {
            let (Some(app), Some(toasts)) = (weak.upgrade(), toasts.upgrade()) else {
                return;
            };
            if app.core.account(account_id).is_none() {
                toasts.add_toast(crate::ui::toast(&gettext(
                    "This account is not syncing yet",
                )));
                return;
            }
            let settings = app.core.gmail_settings();
            button.set_sensitive(false);
            let (button, target, toasts) = (button.clone(), target.clone(), toasts.clone());
            glib::spawn_future_local(async move {
                match app
                    .core
                    .call(async move { settings.signature(account_id).await })
                    .await
                {
                    Ok(Some(signature)) => {
                        target.set_text(&signature);
                        toasts.add_toast(crate::ui::toast(&gettext(
                            "Imported the signature from Gmail",
                        )));
                    }
                    Ok(None) => toasts.add_toast(crate::ui::toast(&gettext(
                        "Gmail has no signature for this account",
                    ))),
                    Err(err) => {
                        let said = with_reason(&gettext("Could not import: {reason}"), &err, &[]);
                        toasts.add_toast(crate::ui::toast(&said));
                    }
                }
                button.set_sensitive(true);
            });
        });
    }
    let (weak, email, subtitle) = (Rc::downgrade(app), account.email.clone(), row.downgrade());
    let account_row = account_row.downgrade();
    view.buffer().connect_changed(move |buffer| {
        let Some(app) = weak.upgrade() else { return };
        let text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string();
        if let Some(subtitle) = subtitle.upgrade() {
            subtitle.set_subtitle(&preview(&text));
        }
        if let Some(account) = account_row.upgrade() {
            account.set_subtitle(&preview(&text));
        }
        let email = email.clone();
        app.change_settings(Change::Signature { email, text });
    });
    row
}

fn writing_page(
    app: &Rc<App>,
    settings: &Settings,
    accounts: &[Account],
    dialog: &adw::ToastOverlay,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .name("writing")
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

    if let Some(protection) = protection_group(app, settings, accounts, dialog) {
        page.add(&protection);
    }
    page.add(&spelling_group(app, settings, accounts));
    page.add(&super::templates::group(app));
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
    dialog: &adw::ToastOverlay,
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
    // saying so and fills itself in, and again after an import.
    let mut addresses: Vec<String> = accounts.iter().map(|a| a.email.clone()).collect();
    for alias in settings.send_as.values().flatten() {
        addresses.push(alias.email.clone());
    }
    addresses.sort();
    addresses.dedup();
    let rows = Rows {
        group: group.clone(),
        keys: keys.clone(),
        certificates: certificates.clone(),
        addresses: Rc::new(addresses),
    };
    // The buttons sit in these rows, so the refill holds them weakly
    // rather than keeping them alive in a cycle.
    let refill = {
        let (weak, group, keys, certificates, addresses) = (
            Rc::downgrade(app),
            group.downgrade(),
            keys.downgrade(),
            certificates.downgrade(),
            Rc::clone(&rows.addresses),
        );
        move || {
            let (Some(app), Some(group), Some(keys), Some(certificates)) = (
                weak.upgrade(),
                group.upgrade(),
                keys.upgrade(),
                certificates.upgrade(),
            ) else {
                return;
            };
            let rows = Rows {
                group,
                keys,
                certificates,
                addresses: Rc::clone(&addresses),
            };
            glib::spawn_future_local(fill_protection(app, rows));
        }
    };
    keys.add_suffix(&super::key_import::button(
        app,
        super::key_import::Kind::Pgp,
        dialog,
        refill.clone(),
    ));
    certificates.add_suffix(&super::key_import::button(
        app,
        super::key_import::Kind::Smime,
        dialog,
        refill,
    ));
    glib::spawn_future_local(fill_protection(Rc::clone(app), rows));
    Some(group)
}

/// The rows of the signing group that say what gpg and gpgsm hold.
struct Rows {
    group: adw::PreferencesGroup,
    keys: adw::ActionRow,
    certificates: adw::ActionRow,
    /// Every address the person sends from.
    addresses: Rc<Vec<String>>,
}

/// Asks gpg and gpgsm what they hold for the addresses the person sends
/// from, and which versions they are, and writes the answers into `rows`.
async fn fill_protection(app: Rc<App>, rows: Rows) {
    let Rows {
        group: filling,
        keys,
        certificates,
        addresses,
    } = rows;
    let mut programs = Vec::new();
    if app.core.has_gpg() {
        let version = app.core.gpg(|pgp| Ok(crate::pgp::version(pgp))).await;
        programs.push(named("gpg", version.ok().flatten()));
        let wanted = addresses.as_ref().clone();
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
        let wanted = addresses.as_ref().clone();
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
    let named = crate::protection::joined(&programs.iter().map(String::as_str).collect::<Vec<_>>());
    // The verb agrees with the programs: "gpg, which holds" but "gpg and
    // gpgsm, which hold".
    filling.set_description(Some(&fill_plural(
        "Penguin Mail signs and encrypts through {programs}, which holds your \
         keys and asks for your passphrase itself.",
        "Penguin Mail signs and encrypts through {programs}, which hold your \
         keys and ask for your passphrase themselves.",
        programs.len(),
        &[("programs", &named)],
    )));
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
    if installed.is_empty() {
        return adw::PreferencesGroup::builder()
            .title(gettext("Spelling"))
            .description(gettext(
                "No dictionaries are installed, so Penguin Mail is not checking \
                 spelling. Install a Hunspell dictionary, such as hunspell-en-us \
                 or hunspell-pt-pt, and reopen the composer.",
            ))
            .build();
    }
    // Each row is an account and its value the language, so the group
    // says what the rows choose.
    let names = crate::language_names::IsoNames::load();
    let named: Vec<String> = installed.iter().map(|code| names.name(code)).collect();
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Spelling"))
        .description(fill(
            &gettext("The language each account checks spelling in. Dictionaries installed: {languages}."),
            &[(
                "languages",
                &crate::protection::joined(&named.iter().map(String::as_str).collect::<Vec<_>>()),
            )],
        ))
        .build();
    // Following the desktop's language is the first choice, then one
    // dictionary per row, then every one together for anyone who writes
    // in several languages.
    let mut choices: Vec<(String, Vec<String>)> = vec![(
        fill(
            &gettext("System: {language}"),
            &[(
                "language",
                &names.name(&crate::ui::composer::spell::locale_language()),
            )],
        ),
        Vec::new(),
    )];
    choices.extend(
        installed
            .iter()
            .zip(&named)
            .map(|(code, name)| (name.clone(), vec![code.clone()])),
    );
    if installed.len() > 1 {
        choices.push((gettext("All Installed Languages"), installed.clone()));
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
            .use_markup(false)
            .title(&account.email)
            .model(&gtk::StringList::new(&labels))
            .selected(selected as u32)
            .build();
        super::combo_value::widen_value(&row);
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

fn sync_page(
    app: &Rc<App>,
    pending: &Rc<RefCell<SyncConfig>>,
    narrow: &adw::Breakpoint,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(gettext("Sync"))
        // Calls that open Sync use this name.
        .name("sync")
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
    let days = current
        .window_days
        .unwrap_or(mailrs_sync::DEFAULT_WINDOW_DAYS);
    let window = sync_combo(
        &windows,
        &gettext("Keep Mail on This Computer For"),
        Some(&window_note(days)),
        nearest_window(&windows, days),
        pending,
        |c, v| c.window_days = Some(v),
    );
    // The line under the row says what the choice on show costs, except
    // on a phone, where the value needs the room.
    let notes_shown = Rc::new(Cell::new(true));
    let picked: Vec<i64> = windows.iter().map(|(days, _)| *days).collect();
    let shown = Rc::clone(&notes_shown);
    window.connect_selected_notify(move |row| {
        if !shown.get() {
            return;
        }
        if let Some(days) = picked.get(row.selected() as usize) {
            row.set_subtitle(&window_note(*days));
        }
    });
    storage.add(&window);
    let caches = cache_choices();
    let cache_note = gettext("Bodies of mail you have read, kept for opening offline");
    let cache = sync_combo(
        &caches,
        &gettext("Message Cache"),
        Some(&cache_note),
        nearest(&caches, current.body_cache_mb.unwrap_or(1024)),
        pending,
        |c, v| c.body_cache_mb = Some(v),
    );
    storage.add(&cache);
    page.add(&storage);
    // On a phone the two lines under these rows took the room their values
    // need, and Message Cache read "1 …". They go below 450 sp.
    {
        let (shown, rows) = (Rc::clone(&notes_shown), (window.clone(), cache.clone()));
        narrow.connect_apply(move |_| {
            shown.set(false);
            rows.0.set_subtitle("");
            rows.1.set_subtitle("");
        });
        let (shown, rows) = (Rc::clone(&notes_shown), (window.clone(), cache.clone()));
        let windows = window_choices();
        narrow.connect_unapply(move |_| {
            shown.set(true);
            if let Some((days, _)) = windows.get(rows.0.selected() as usize) {
                rows.0.set_subtitle(&window_note(*days));
            }
            rows.1.set_subtitle(&cache_note);
        });
    }

    let startup = adw::PreferencesGroup::builder()
        .title(gettext("Startup"))
        .build();
    let login = adw::SwitchRow::builder()
        .title(gettext("Start in the Tray at Login"))
        .subtitle(gettext("Penguin Mail keeps syncing with no window open"))
        .build();
    match autostart::path() {
        Some(path) if !app.core.demo => {
            login.add_css_class("preference-explanation");
            login.set_active(autostart::is_enabled(&path));
            login.connect_active_notify(move |row| {
                if let Err(err) = autostart::apply(&path, row.is_active()) {
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
    } else if let Some(updater) = crate::packaging::BUILT_FOR.updated_by() {
        startup.add(
            &adw::ActionRow::builder()
                .title(gettext("Updates"))
                .subtitle(updater.line())
                .build(),
        );
    }
    page.add(&startup);
    page
}

/// A size on a server, as `human_size` writes it but keeping the tenth
/// from 10 GB up: on a nearly full account "14.3 GB of 15 GB" says how
/// much room is left, where "14 GB" hides it. A whole number drops it.
fn space_size(bytes: u64) -> String {
    const UNITS: [&str; 3] = ["KB", "MB", "GB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let tenths = (value * 10.0).round();
    if value >= 10.0 && tenths % 10.0 == 0.0 {
        format!("{} {}", tenths / 10.0, UNITS[unit])
    } else {
        format!("{:.1} {}", tenths / 10.0, UNITS[unit])
    }
}

/// "2.1 GB of 15 GB".
fn amount_line(storage: mailrs_sync::Storage) -> String {
    fill(
        &gettext("{used} of {limit}"),
        &[
            ("used", &space_size(storage.used)),
            ("limit", &space_size(storage.limit)),
        ],
    )
}

/// The row's second line: the amount, and how full once past the warning.
fn state_line(storage: mailrs_sync::Storage) -> String {
    let amount = amount_line(storage);
    if storage.full() {
        fill(&gettext("{amount} · Full"), &[("amount", &amount)])
    } else if storage.past_warning() {
        fill(&gettext("{amount} · Nearly full"), &[("amount", &amount)])
    } else {
        amount
    }
}

/// The senders whose images load without asking, each with a way off the
/// list. The rows fill in once the store answers, so opening Preferences
/// never waits on it.
fn allowed_image_senders(app: &Rc<App>) -> adw::ExpanderRow {
    let row = crate::ui::disclosure::row(
        adw::ExpanderRow::builder()
            .title(gettext("Senders Who May Load Images"))
            .subtitle(gettext("Nobody yet"))
            .build(),
    );
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

/// The seven weekdays in `Settings::working_hours.days`' own storage
/// order, Monday first. Preferences shows the week starting on Monday
/// whatever the desktop's own locale does: this is a settings row, not
/// the calendar's own grid, and the storage order it edits is fixed.
const WEEK_MONDAY_FIRST: [chrono::Weekday; 7] = [
    chrono::Weekday::Mon,
    chrono::Weekday::Tue,
    chrono::Weekday::Wed,
    chrono::Weekday::Thu,
    chrono::Weekday::Fri,
    chrono::Weekday::Sat,
    chrono::Weekday::Sun,
];

/// `day` in `pattern`'s words: `Weekday` carries no date of its own, so
/// this pairs it with a known Monday, the way
/// `ui::calendar::sidebar::weekday_initials` names the mini month's
/// weekday row.
fn weekday_words(day: chrono::Weekday, pattern: &str) -> String {
    let monday = chrono::NaiveDate::from_ymd_opt(2024, 1, 1).expect("2024-01-01 is a Monday");
    (monday + chrono::Days::new(u64::from(day.num_days_from_monday())))
        .format_localized(pattern, date_locale())
        .to_string()
}

/// "09:00–18:00, Monday to Friday": [`WorkingHours`]'s hours and days in
/// words, for the row's subtitle.
fn working_hours_words(hours: &WorkingHours) -> String {
    let time = fill(
        &gettext("{start}–{end}"),
        &[
            ("start", &crate::clock_format::time_text(hours.start_time())),
            ("end", &crate::clock_format::time_text(hours.end_time())),
        ],
    );
    let days = match hours.days {
        [true, true, true, true, true, false, false] => gettext("Monday to Friday"),
        [true, true, true, true, true, true, true] => gettext("Every day of the week"),
        [false, false, false, false, false, false, false] => gettext("No days"),
        _ => WEEK_MONDAY_FIRST
            .iter()
            .zip(hours.days)
            .filter(|(_, worked)| *worked)
            .map(|(&day, _)| weekday_words(day, &gettext("%a")))
            .collect::<Vec<_>>()
            .join(&gettext(", ")),
    };
    fill(
        &gettext("{time}, {days}"),
        &[("time", &time), ("days", &days)],
    )
}

/// A drop-down of the day's 24 whole hours, in the clock
/// [`crate::clock_format::current`] names, `current`'s own hour selected.
/// A drop-down of whole hours, `hours` in order, on `current`. Hour 24 is
/// midnight at the day's end and reads as midnight does.
fn hour_dropdown(hours: &[u32], current: u32) -> gtk::DropDown {
    let labels: Vec<String> = hours
        .iter()
        .map(|hour| {
            crate::clock_format::time_text(
                chrono::NaiveTime::from_hms_opt(hour % 24, 0, 0).unwrap_or_default(),
            )
        })
        .collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let drop = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&refs))
        .valign(gtk::Align::Center)
        .build();
    let at = hours.iter().position(|h| *h == current).unwrap_or(0);
    drop.set_selected(u32::try_from(at).unwrap_or(0));
    drop
}

/// The hours the working day may start at.
fn start_hours() -> Vec<u32> {
    (0..24).collect()
}

/// The hours the working day may end at: an hour after midnight at the
/// earliest, and midnight at the day's end at the latest.
fn end_hours() -> Vec<u32> {
    (1..=24).collect()
}

/// `hours` starting at `hour`. An end at or before it moves to an hour
/// after it, so the working day never runs backwards, which would shade
/// the whole day.
fn starting_at(mut hours: WorkingHours, hour: u32) -> WorkingHours {
    let start = u16::try_from(hour.min(23) * 60).unwrap_or(0);
    hours.start_minutes = start;
    if hours.end_minutes <= start {
        hours.end_minutes = start + 60;
    }
    hours
}

/// `hours` ending at `hour`. A start at or after it moves to an hour
/// before it.
fn ending_at(mut hours: WorkingHours, hour: u32) -> WorkingHours {
    let end = u16::try_from(hour.clamp(1, 24) * 60).unwrap_or(24 * 60);
    hours.end_minutes = end;
    if hours.start_minutes >= end {
        hours.start_minutes = end - 60;
    }
    hours
}

/// The "Week Starts On" row: Automatic follows the locale's own first
/// weekday. An explicit accessible name, since the closed row's own
/// text does not always reach a screen reader on a plain title
/// (`libadwaita-dialog-traps`, "Two choices read better as a toggle
/// group").
fn week_start_row(app: &Rc<App>, week_start: WeekStart) -> adw::ComboRow {
    let title = gettext("Week Starts On");
    let row = combo(
        app,
        &title,
        Some(&gettext(
            "Automatic follows the locale's own first day of the week",
        )),
        week_start,
        Change::WeekStart,
    );
    crate::ui::name(&row, &title);
    row
}

/// The "Working Hours" row: an expander with the current hours and days
/// in words, and an "Hours" and a "Days" row under it to change them.
fn working_hours_row(app: &Rc<App>, hours: WorkingHours) -> adw::ExpanderRow {
    let row = crate::ui::disclosure::row(
        adw::ExpanderRow::builder()
            .title(gettext("Working Hours"))
            .subtitle(working_hours_words(&hours))
            .build(),
    );

    let stored = Rc::new(Cell::new(hours));
    let weak_row = row.downgrade();
    let commit = {
        let app = Rc::clone(app);
        let stored = Rc::clone(&stored);
        move || {
            let hours = stored.get();
            if let Some(row) = weak_row.upgrade() {
                row.set_subtitle(&working_hours_words(&hours));
            }
            app.change_settings(Change::WorkingHours(hours));
        }
    };

    let times = adw::ActionRow::builder().title(gettext("Hours")).build();
    let time_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .valign(gtk::Align::Center)
        .build();
    let (starts, ends) = (start_hours(), end_hours());
    let start = hour_dropdown(&starts, u32::from(hours.start_minutes / 60));
    crate::ui::name(&start, &gettext("Starts"));
    let end = hour_dropdown(&ends, u32::from(hours.end_minutes / 60));
    crate::ui::name(&end, &gettext("Ends"));
    // Each drop-down moves the other when the day would run backwards.
    // The other's own handler then finds the order right and changes
    // nothing more.
    {
        let stored = Rc::clone(&stored);
        let commit = commit.clone();
        let (end, ends) = (end.downgrade(), ends.clone());
        start.connect_selected_notify(move |drop| {
            let hours = starting_at(
                stored.get(),
                starts.get(drop.selected() as usize).copied().unwrap_or(0),
            );
            stored.set(hours);
            if let (Some(end), Some(at)) = (
                end.upgrade(),
                ends.iter()
                    .position(|h| *h == u32::from(hours.end_minutes / 60)),
            ) {
                end.set_selected(u32::try_from(at).unwrap_or(0));
            }
            commit();
        });
    }
    {
        let stored = Rc::clone(&stored);
        let commit = commit.clone();
        let start = start.downgrade();
        end.connect_selected_notify(move |drop| {
            let hours = ending_at(
                stored.get(),
                ends.get(drop.selected() as usize).copied().unwrap_or(24),
            );
            stored.set(hours);
            if let Some(start) = start.upgrade() {
                start.set_selected(u32::from(hours.start_minutes / 60));
            }
            commit();
        });
    }
    time_box.append(&start);
    time_box.append(&gtk::Label::new(Some(&gettext("–"))));
    time_box.append(&end);
    times.add_suffix(&time_box);
    row.add_row(&times);

    let days_row = adw::ActionRow::builder().title(gettext("Days")).build();
    let days_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .css_classes(["linked", "day-toggles"])
        .homogeneous(true)
        .valign(gtk::Align::Center)
        .build();
    for (index, day) in WEEK_MONDAY_FIRST.into_iter().enumerate() {
        let initial = weekday_words(day, &gettext("%a"))
            .chars()
            .next()
            .map(|c| c.to_string())
            .unwrap_or_default();
        let button = gtk::ToggleButton::builder()
            .label(&initial)
            .active(hours.days[index])
            .valign(gtk::Align::Center)
            .build();
        crate::ui::name(&button, &weekday_words(day, &gettext("%A")));
        let stored = Rc::clone(&stored);
        let commit = commit.clone();
        button.connect_toggled(move |button| {
            let mut hours = stored.get();
            hours.days[index] = button.is_active();
            stored.set(hours);
            commit();
        });
        days_box.append(&button);
    }
    days_row.add_suffix(&days_box);
    row.add_row(&days_row);

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
        row.add_css_class("preference-explanation");
        row.set_tooltip_text(Some(subtitle));
    }
    let weak = Rc::downgrade(app);
    row.connect_active_notify(move |row| {
        if let Some(app) = weak.upgrade() {
            flip(&app, row.is_active());
        }
    });
    row
}

/// The languages whose messages get no offer to translate them, a switch
/// each, in the order their names sort in.
fn never_translate_row(app: &Rc<App>, settings: &Settings) -> adw::ExpanderRow {
    let row = crate::ui::disclosure::row(
        adw::ExpanderRow::builder()
            .title(gettext("Never Translate"))
            .subtitle(gettext(
                "Messages in these languages get no offer to translate them",
            ))
            .build(),
    );
    row.add_css_class("preference-explanation");
    row.set_tooltip_text(Some(row.subtitle().as_str()));
    let mut languages: Vec<_> = crate::translation::named_languages().collect();
    languages.sort_by_key(|language| language.name());
    for language in languages {
        let code = language.code;
        row.add_row(&switch(
            app,
            &language.name(),
            None,
            settings.never_translate.iter().any(|never| never == code),
            move |never| Change::NeverTranslate {
                language: code.to_string(),
                never,
            },
        ));
    }
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
    row.add_css_class("preference-explanation");
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
        row.add_css_class("preference-explanation");
        row.set_tooltip_text(Some(subtitle));
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
        row.add_css_class("preference-explanation");
        row.set_tooltip_text(Some(subtitle));
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

#[cfg(test)]
mod tests {
    use mailrs_domain::calendar::hours::WorkingHours;

    use super::{amount_line, end_hours, ending_at, starting_at, state_line};

    const GIB: u64 = 1 << 30;

    fn space(used: u64, limit: u64) -> mailrs_sync::Storage {
        mailrs_sync::Storage::new(used, limit, None).unwrap()
    }

    #[test]
    fn the_amount_reads_used_of_limit() {
        assert_eq!(
            amount_line(space(2_254_857_830, 16_106_127_360)),
            "2.1 GB of 15 GB"
        );
    }

    #[test]
    fn the_amount_keeps_a_tenth_above_ten_gigabytes() {
        assert_eq!(
            amount_line(space(143 * GIB / 10, 15 * GIB)),
            "14.3 GB of 15 GB"
        );
    }

    #[test]
    fn the_amount_keeps_a_tenth_below_ten_gigabytes() {
        assert_eq!(amount_line(space(5 * GIB, 5 * GIB)), "5.0 GB of 5.0 GB");
    }

    #[test]
    fn a_row_with_room_says_only_the_amount() {
        assert_eq!(
            state_line(space(21 * GIB / 10, 30 * GIB)),
            "2.1 GB of 30 GB"
        );
    }

    #[test]
    fn a_row_past_the_warning_says_nearly_full() {
        assert_eq!(
            state_line(space(143 * GIB / 10, 15 * GIB)),
            "14.3 GB of 15 GB · Nearly full"
        );
    }

    #[test]
    fn a_full_row_says_full() {
        assert_eq!(
            state_line(space(5 * GIB, 5 * GIB)),
            "5.0 GB of 5.0 GB · Full"
        );
    }

    fn nine_to_six() -> WorkingHours {
        WorkingHours::default()
    }

    #[test]
    fn a_start_past_the_end_moves_the_end_an_hour_after_it() {
        let hours = starting_at(nine_to_six(), 19);
        assert_eq!((hours.start_minutes, hours.end_minutes), (19 * 60, 20 * 60));
    }

    #[test]
    fn a_start_at_the_last_hour_ends_the_day_at_midnight() {
        let hours = starting_at(nine_to_six(), 23);
        assert_eq!((hours.start_minutes, hours.end_minutes), (23 * 60, 24 * 60));
    }

    #[test]
    fn an_end_before_the_start_moves_the_start_an_hour_before_it() {
        let hours = ending_at(nine_to_six(), 8);
        assert_eq!((hours.start_minutes, hours.end_minutes), (7 * 60, 8 * 60));
    }

    #[test]
    fn a_change_that_keeps_the_order_moves_nothing_else() {
        let hours = ending_at(starting_at(nine_to_six(), 8), 17);
        assert_eq!((hours.start_minutes, hours.end_minutes), (8 * 60, 17 * 60));
    }

    #[test]
    fn the_day_can_end_at_midnight_and_not_at_its_first_hour() {
        assert_eq!(end_hours(), (1..=24).collect::<Vec<u32>>());
    }
}
