//! Named changes to the preferences, and what each one makes the window redo.
//!
//! A change is a value, not a closure, so the same change can come from a
//! switch in Preferences, a keyboard shortcut, or an assistant tool call and
//! land the same way every time. Applying one reports its [`Effects`]: the
//! parts of the window that now show something stale. Nothing here touches
//! GTK, so the rules live under unit tests.

use mailrs_domain::{FlagColor, SmartMailbox};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{
    AiProvider, Choice, ColorScheme, ComposeFormat, MarkRead, RemoteImages, Settings, TextSize,
    UndoSend,
};

/// One named change to the preferences.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Threading(bool),
    MarkRead(MarkRead),
    RemoteImages(RemoteImages),
    TextSize(TextSize),
    /// Moves one step up or down the text sizes; `0` goes back to the default.
    StepTextSize(i32),
    ColorScheme(ColorScheme),
    Notifications(bool),
    NotificationPreviews(bool),
    NotifyVipsOnly(bool),
    /// Puts one button on new-mail notifications, or takes it off.
    NotificationButton {
        button: crate::notify::Button,
        show: bool,
    },
    UndoSend(UndoSend),
    /// The address new messages come from; `None` means the first account.
    DefaultAccount(Option<String>),
    InboxCategories(bool),
    SuggestFollowUps(bool),
    /// What a new message starts as.
    ComposeFormat(ComposeFormat),
    /// Read the accounts' Google contacts, or stop and forget them.
    Contacts(bool),
    /// The colour the flag button reaches for next.
    FlagColor(FlagColor),
    /// An account's signature. Blank text removes it.
    Signature {
        email: String,
        text: String,
    },
    /// Adds the address when it is not a VIP yet, removes it when it is.
    ToggleVip {
        email: String,
        name: String,
    },
    /// Adds or removes a VIP, whatever it was before.
    SetVip {
        email: String,
        name: String,
        add: bool,
    },
    /// Saves a smart mailbox, replacing the one that has its id.
    SaveSmartMailbox(Box<SmartMailbox>),
    DeleteSmartMailbox(String),
    /// Moves a smart mailbox one place up (`-1`) or down (`1`).
    MoveSmartMailbox {
        id: String,
        step: isize,
    },
    /// Moves an account one place up or down among `emails`.
    MoveAccount {
        emails: Vec<String>,
        email: String,
        step: isize,
    },
    /// The name the sidebar shows for an account. Blank goes back to the
    /// address.
    AccountName {
        email: String,
        name: String,
    },
    AccountColor {
        email: String,
        index: usize,
    },
    /// Every address an account may send as, as Gmail just reported them.
    SendAsAddresses {
        account: String,
        addresses: Vec<crate::compose::SendAsAddress>,
    },
    /// The send-as address an account just sent from.
    LastSender {
        account: String,
        email: String,
    },
    /// The dictionaries to check an account's mail against. Empty follows
    /// the desktop's locale.
    SpellLanguages {
        account: String,
        languages: Vec<String>,
    },
    /// Keeps a word Add to Dictionary accepted.
    KeepWord(String),
    Ai(AiChange),
}

/// A change to the assistant's settings. API keys live in the keyring and
/// never come through here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiChange {
    Provider(AiProvider),
    BaseUrl(String),
    LocalModel(String),
    AnthropicModel(String),
    ClaudeModel(String),
    ClaudeCommand(String),
    ConfirmActions(bool),
    /// A server the app found on this machine: its address and first model.
    LocalServer {
        base_url: String,
        model: String,
    },
}

impl Change {
    /// Makes the change and reports what the window has to redo.
    pub fn apply(self, settings: &mut Settings) -> Effects {
        let before = settings.clone();
        self.apply_to(settings);
        Effects::between(&before, settings)
    }

    /// Makes the change without working out its effects.
    pub fn apply_to(self, settings: &mut Settings) {
        match self {
            Change::Threading(on) => settings.threading = on,
            Change::MarkRead(when) => settings.mark_read = when,
            Change::RemoteImages(when) => settings.remote_images = when,
            Change::TextSize(size) => settings.text_size = size,
            Change::StepTextSize(step) => {
                settings.text_size = if step == 0 {
                    TextSize::Normal
                } else {
                    let last = TextSize::ALL.len() as i32 - 1;
                    let next = (settings.text_size.index() as i32 + step).clamp(0, last);
                    TextSize::from_index(next as u32)
                };
            }
            Change::ColorScheme(scheme) => settings.color_scheme = scheme,
            Change::Notifications(on) => settings.notifications = on,
            Change::NotificationPreviews(on) => settings.notification_previews = on,
            Change::NotifyVipsOnly(on) => settings.notify_vips_only = on,
            Change::NotificationButton { button, show } => {
                settings.show_notification_button(button, show)
            }
            Change::UndoSend(delay) => settings.undo_send = delay,
            Change::DefaultAccount(email) => settings.default_account = email,
            Change::InboxCategories(on) => settings.inbox_categories = on,
            Change::SuggestFollowUps(on) => settings.suggest_follow_ups = on,
            Change::ComposeFormat(format) => settings.compose_format = format,
            Change::Contacts(on) => settings.contacts = on,
            Change::FlagColor(color) => settings.flag_color = color,
            Change::Signature { email, text } => settings.set_signature(&email, &text),
            Change::ToggleVip { email, name } => {
                settings.toggle_vip(&email, &name);
            }
            Change::SetVip { email, name, add } => {
                // Toggling is the one way in and out of the VIP list, so the
                // assistant and the menu store the same key and the same name
                // for the same address. Taking an address out first lets
                // adding it again give it a new name.
                if settings.is_vip(email.trim()) {
                    settings.toggle_vip(&email, &name);
                }
                if add {
                    settings.toggle_vip(&email, &name);
                }
            }
            Change::SaveSmartMailbox(mailbox) => {
                let mailbox = *mailbox;
                match settings
                    .smart_mailboxes
                    .iter_mut()
                    .find(|m| m.id == mailbox.id)
                {
                    Some(slot) => *slot = mailbox,
                    None => settings.smart_mailboxes.push(mailbox),
                }
            }
            Change::DeleteSmartMailbox(id) => settings.smart_mailboxes.retain(|m| m.id != id),
            Change::MoveSmartMailbox { id, step } => settings.move_smart(&id, step),
            Change::MoveAccount {
                emails,
                email,
                step,
            } => {
                let refs: Vec<&str> = emails.iter().map(String::as_str).collect();
                settings.move_account(&refs, &email, step);
            }
            Change::AccountName { email, name } => {
                if name.trim().is_empty() {
                    settings.account_names.remove(&email);
                } else {
                    settings.account_names.insert(email, name);
                }
            }
            Change::AccountColor { email, index } => {
                settings.account_colors.insert(email, index);
            }
            Change::SendAsAddresses { account, addresses } => {
                settings.send_as.insert(account.to_lowercase(), addresses);
            }
            Change::LastSender { account, email } => {
                settings.last_sender.insert(account.to_lowercase(), email);
            }
            Change::SpellLanguages { account, languages } => {
                let key = account.to_lowercase();
                if languages.is_empty() {
                    settings.spell_languages.remove(&key);
                } else {
                    settings.spell_languages.insert(key, languages);
                }
            }
            Change::KeepWord(word) => {
                let word = word.trim().to_lowercase();
                if !word.is_empty() && !settings.spell_words.contains(&word) {
                    settings.spell_words.push(word);
                    settings.spell_words.sort();
                }
            }
            Change::Ai(change) => change.apply_to(&mut settings.ai),
        }
    }
}

impl AiChange {
    fn apply_to(self, ai: &mut super::AiSettings) {
        match self {
            AiChange::Provider(provider) => ai.provider = provider,
            AiChange::BaseUrl(url) => ai.base_url = url,
            AiChange::LocalModel(model) => ai.local_model = model,
            AiChange::AnthropicModel(model) => ai.anthropic_model = model,
            AiChange::ClaudeModel(alias) => ai.claude_model = alias,
            AiChange::ClaudeCommand(command) => ai.claude_command = command,
            AiChange::ConfirmActions(on) => ai.confirm_actions = on,
            AiChange::LocalServer { base_url, model } => {
                ai.base_url = base_url;
                ai.local_model = model;
            }
        }
    }
}

/// A preference the assistant may read and change by name. The assistant's
/// own settings are left out on purpose, and so is anything it would need a
/// shape more complicated than one JSON value to set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Threading,
    MarkRead,
    RemoteImages,
    TextSize,
    ColorScheme,
    Notifications,
    NotificationPreviews,
    NotifyVipsOnly,
    UndoSend,
    DefaultAccount,
    ComposeFormat,
}

impl Setting {
    /// In the order the assistant sees them.
    pub const ALL: [Setting; 11] = [
        Setting::Threading,
        Setting::MarkRead,
        Setting::RemoteImages,
        Setting::TextSize,
        Setting::ColorScheme,
        Setting::Notifications,
        Setting::NotificationPreviews,
        Setting::NotifyVipsOnly,
        Setting::UndoSend,
        Setting::DefaultAccount,
        Setting::ComposeFormat,
    ];

    /// The key in `settings.toml`, which is the name the tool takes too.
    pub fn name(self) -> &'static str {
        match self {
            Setting::Threading => "threading",
            Setting::MarkRead => "mark_read",
            Setting::RemoteImages => "remote_images",
            Setting::TextSize => "text_size",
            Setting::ColorScheme => "color_scheme",
            Setting::Notifications => "notifications",
            Setting::NotificationPreviews => "notification_previews",
            Setting::NotifyVipsOnly => "notify_vips_only",
            Setting::UndoSend => "undo_send",
            Setting::DefaultAccount => "default_account",
            Setting::ComposeFormat => "compose_format",
        }
    }

    pub fn named(name: &str) -> Option<Setting> {
        Setting::ALL.into_iter().find(|s| s.name() == name)
    }

    /// What this setting is now, in the JSON the tool reports.
    pub fn value(self, settings: &Settings) -> Value {
        match self {
            Setting::Threading => json!(settings.threading),
            Setting::MarkRead => json!(settings.mark_read),
            Setting::RemoteImages => json!(settings.remote_images),
            Setting::TextSize => json!(settings.text_size),
            Setting::ColorScheme => json!(settings.color_scheme),
            Setting::Notifications => json!(settings.notifications),
            Setting::NotificationPreviews => json!(settings.notification_previews),
            Setting::NotifyVipsOnly => json!(settings.notify_vips_only),
            Setting::UndoSend => json!(settings.undo_send),
            Setting::DefaultAccount => json!(settings.default_account),
            Setting::ComposeFormat => json!(settings.compose_format),
        }
    }

    /// Reads the tool's JSON into a change, or says what is wrong with it.
    pub fn change(self, value: &Value) -> Result<Change, String> {
        fn read<T: DeserializeOwned>(value: &Value) -> Result<T, String> {
            serde_json::from_value(value.clone()).map_err(|err| err.to_string())
        }
        Ok(match self {
            Setting::Threading => Change::Threading(read(value)?),
            Setting::MarkRead => Change::MarkRead(read(value)?),
            Setting::RemoteImages => Change::RemoteImages(read(value)?),
            Setting::TextSize => Change::TextSize(read(value)?),
            Setting::ColorScheme => Change::ColorScheme(read(value)?),
            Setting::Notifications => Change::Notifications(read(value)?),
            Setting::NotificationPreviews => Change::NotificationPreviews(read(value)?),
            Setting::NotifyVipsOnly => Change::NotifyVipsOnly(read(value)?),
            Setting::UndoSend => Change::UndoSend(read(value)?),
            Setting::DefaultAccount => Change::DefaultAccount(read(value)?),
            Setting::ComposeFormat => Change::ComposeFormat(read(value)?),
        })
    }
}

/// One part of the window that a settings change leaves stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Effect {
    /// The list groups mail differently, so list the mailbox again.
    ListShape,
    /// The sidebar's accounts: their order, names, colours, smart
    /// mailboxes, and VIPs.
    Accounts,
    /// Rows carry an account's colour, so draw them again.
    RowColors,
    /// A smart mailbox's conditions, which the one on screen holds a copy of.
    SmartMailboxes,
    /// Who counts as a VIP, which the open conversation marks.
    Vips,
    /// Whether Follow Up belongs in the sidebar.
    FollowUps,
    /// Whether the inbox splits into categories.
    Categories,
    /// The assistant's provider or model.
    Assistant,
    /// How large the conversation's text is.
    TextSize,
    /// Whether contacts supply names and photos, which rows and the open
    /// conversation show.
    Contacts,
    /// Light or dark.
    Theme,
}

impl Effect {
    /// In the order the window applies them: accounts first, because the
    /// rows and the smart mailbox on screen read what it sets.
    pub const ALL: [Effect; 11] = [
        Effect::ListShape,
        Effect::Accounts,
        Effect::RowColors,
        Effect::SmartMailboxes,
        Effect::Vips,
        Effect::FollowUps,
        Effect::Categories,
        Effect::Assistant,
        Effect::TextSize,
        Effect::Contacts,
        Effect::Theme,
    ];
}

/// The effects of one change, each at most once, in [`Effect::ALL`] order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Effects(Vec<Effect>);

impl Effects {
    /// Nothing on screen has to change.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn has(&self, effect: Effect) -> bool {
        self.0.contains(&effect)
    }

    pub fn iter(&self) -> impl Iterator<Item = Effect> + '_ {
        self.0.iter().copied()
    }

    /// What changed between two sets of preferences.
    ///
    /// Every field is named below, so a new preference will not compile
    /// until someone says whether the window has to react to it.
    pub fn between(before: &Settings, after: &Settings) -> Effects {
        let Settings {
            threading,
            mark_read,
            remote_images,
            text_size,
            color_scheme,
            notifications,
            notification_previews,
            default_account,
            signatures,
            undo_send,
            flag_color,
            vips,
            notify_vips_only,
            notification_buttons,
            smart_mailboxes,
            account_order,
            account_colors,
            account_names,
            ai,
            hidden_addresses,
            inbox_categories,
            suggest_follow_ups,
            spell_languages,
            spell_words,
            last_sender,
            send_as,
            compose_format,
            contacts,
        } = after;
        // These leave the window as it is. The flag colour, the delay before
        // Send commits, and the rest are read when they are needed, so
        // changing one saves the file and stops there.
        let _quiet = (
            mark_read,
            remote_images,
            notifications,
            notification_previews,
            default_account,
            signatures,
            undo_send,
            flag_color,
            notify_vips_only,
            notification_buttons,
            hidden_addresses,
            // The composer reads these when it opens, so a refreshed alias
            // list or a newly kept word changes nothing already on screen.
            spell_languages,
            spell_words,
            last_sender,
            send_as,
            compose_format,
        );
        let smart_changed = *smart_mailboxes != before.smart_mailboxes;
        let colors_changed = *account_colors != before.account_colors;
        let vips_changed = *vips != before.vips;
        // Walking Effect::ALL keeps the order the window relies on.
        let effects = Effect::ALL
            .into_iter()
            .filter(|effect| match effect {
                Effect::ListShape => *threading != before.threading,
                Effect::Accounts => {
                    smart_changed
                        || colors_changed
                        || vips_changed
                        || *account_order != before.account_order
                        || *account_names != before.account_names
                }
                Effect::RowColors => colors_changed,
                Effect::SmartMailboxes => smart_changed,
                Effect::Vips => vips_changed,
                Effect::FollowUps => *suggest_follow_ups != before.suggest_follow_ups,
                Effect::Categories => *inbox_categories != before.inbox_categories,
                Effect::Assistant => *ai != before.ai,
                Effect::TextSize => *text_size != before.text_size,
                Effect::Contacts => *contacts != before.contacts,
                Effect::Theme => *color_scheme != before.color_scheme,
            })
            .collect();
        Effects(effects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailrs_domain::smart::{Condition, Field};

    fn effects(change: Change) -> Effects {
        change.apply(&mut Settings::default())
    }

    fn effects_from(settings: &Settings, change: Change) -> Effects {
        let mut after = settings.clone();
        change.apply(&mut after)
    }

    fn smart(id: &str, name: &str) -> SmartMailbox {
        SmartMailbox {
            id: id.into(),
            name: name.into(),
            account: None,
            match_all: true,
            conditions: vec![Condition {
                field: Field::From,
                value: "ann".into(),
            }],
        }
    }

    #[test]
    fn a_change_that_alters_nothing_has_no_effects() {
        let settings = Settings::default();
        assert!(effects_from(&settings, Change::Threading(settings.threading)).is_empty());
        assert!(Effects::between(&settings, &settings).is_empty());
    }

    #[test]
    fn reading_and_display_changes_name_their_effects() {
        assert!(effects(Change::Threading(false)).has(Effect::ListShape));
        assert!(effects(Change::InboxCategories(false)).has(Effect::Categories));
        assert!(effects(Change::SuggestFollowUps(false)).has(Effect::FollowUps));
        assert!(effects(Change::TextSize(TextSize::Large)).has(Effect::TextSize));
        assert!(effects(Change::ColorScheme(ColorScheme::Dark)).has(Effect::Theme));
        assert!(
            effects(Change::Ai(AiChange::Provider(AiProvider::Anthropic))).has(Effect::Assistant)
        );
    }

    #[test]
    fn small_preferences_need_no_reload() {
        // The list the module's comment calls quiet, checked one by one.
        let quiet = [
            Change::MarkRead(MarkRead::Manually),
            Change::RemoteImages(RemoteImages::Always),
            Change::Notifications(false),
            Change::NotificationPreviews(false),
            Change::NotifyVipsOnly(true),
            Change::UndoSend(UndoSend::Thirty),
            Change::DefaultAccount(Some("ann@example.com".into())),
            Change::FlagColor(FlagColor::Blue),
            Change::Signature {
                email: "ann@example.com".into(),
                text: "Ann".into(),
            },
        ];
        for change in quiet {
            let named = format!("{change:?}");
            assert!(
                effects(change).is_empty(),
                "{named} should leave the window alone"
            );
        }
    }

    #[test]
    fn text_size_steps_stop_at_the_ends() {
        let mut settings = Settings::default();
        Change::StepTextSize(1).apply_to(&mut settings);
        assert_eq!(settings.text_size, TextSize::Large);
        for _ in 0..5 {
            Change::StepTextSize(1).apply_to(&mut settings);
        }
        assert_eq!(settings.text_size, TextSize::Larger);
        for _ in 0..9 {
            Change::StepTextSize(-1).apply_to(&mut settings);
        }
        assert_eq!(settings.text_size, TextSize::Small);
        assert!(effects_from(&settings, Change::StepTextSize(0)).has(Effect::TextSize));
    }

    #[test]
    fn both_vip_paths_store_the_same_key_and_name() {
        let mut toggled = Settings::default();
        let mut set = Settings::default();
        let effects = Change::ToggleVip {
            email: " Ann@Example.com ".into(),
            name: "  ".into(),
        }
        .apply(&mut toggled);
        Change::SetVip {
            email: " Ann@Example.com ".into(),
            name: "  ".into(),
            add: true,
        }
        .apply(&mut set);
        assert_eq!(toggled.vips, set.vips);
        assert_eq!(toggled.vips["ann@example.com"], "Ann@Example.com");
        assert!(effects.has(Effect::Vips));
        assert!(effects.has(Effect::Accounts), "the sidebar lists VIPs");

        // Adding twice keeps one entry and takes the newer name.
        let again = Change::SetVip {
            email: "ANN@example.com".into(),
            name: "Ann Lee".into(),
            add: true,
        }
        .apply(&mut set);
        assert_eq!(set.vips.len(), 1);
        assert_eq!(set.vips["ann@example.com"], "Ann Lee");
        assert!(again.has(Effect::Vips));
        // Adding the same name again, or removing someone who is not there,
        // changes nothing.
        assert!(
            Change::SetVip {
                email: "ann@example.com".into(),
                name: "Ann Lee".into(),
                add: true,
            }
            .apply(&mut set)
            .is_empty()
        );
        assert!(
            Change::SetVip {
                email: "nobody@example.com".into(),
                name: String::new(),
                add: false,
            }
            .apply(&mut set)
            .is_empty()
        );
        let removed = Change::SetVip {
            email: "Ann@Example.com".into(),
            name: String::new(),
            add: false,
        }
        .apply(&mut set);
        assert!(set.vips.is_empty());
        assert!(removed.has(Effect::Vips));
    }

    #[test]
    fn smart_mailboxes_save_move_and_go() {
        let mut settings = Settings::default();
        let saved =
            Change::SaveSmartMailbox(Box::new(smart("s1", "From Ann"))).apply(&mut settings);
        assert!(saved.has(Effect::Accounts) && saved.has(Effect::SmartMailboxes));
        Change::SaveSmartMailbox(Box::new(smart("s2", "From Bo"))).apply(&mut settings);
        // Saving an id again replaces it instead of adding a second one.
        Change::SaveSmartMailbox(Box::new(smart("s1", "From Ann Lee"))).apply(&mut settings);
        assert_eq!(settings.smart_mailboxes.len(), 2);
        assert_eq!(settings.smart_mailboxes[0].name, "From Ann Lee");
        Change::MoveSmartMailbox {
            id: "s1".into(),
            step: 1,
        }
        .apply(&mut settings);
        assert_eq!(settings.smart_mailboxes[0].id, "s2");
        Change::DeleteSmartMailbox("s2".into()).apply(&mut settings);
        assert_eq!(settings.smart_mailboxes.len(), 1);
    }

    #[test]
    fn account_changes_redraw_the_sidebar_and_the_rows() {
        let mut settings = Settings::default();
        let colored = Change::AccountColor {
            email: "ann@example.com".into(),
            index: 3,
        }
        .apply(&mut settings);
        assert!(colored.has(Effect::Accounts) && colored.has(Effect::RowColors));
        let named = Change::AccountName {
            email: "ann@example.com".into(),
            name: "Ann".into(),
        }
        .apply(&mut settings);
        assert!(named.has(Effect::Accounts) && !named.has(Effect::RowColors));
        Change::AccountName {
            email: "ann@example.com".into(),
            name: "   ".into(),
        }
        .apply(&mut settings);
        assert!(settings.account_names.is_empty());
        Change::MoveAccount {
            emails: vec!["a@x.com".into(), "b@x.com".into()],
            email: "b@x.com".into(),
            step: -1,
        }
        .apply(&mut settings);
        assert_eq!(settings.account_order, ["b@x.com", "a@x.com"]);
    }

    #[test]
    fn effects_come_in_one_order_without_repeats() {
        let mut before = Settings::default();
        Change::SaveSmartMailbox(Box::new(smart("s1", "From Ann"))).apply(&mut before);
        let mut after = before.clone();
        after.threading = !before.threading;
        after.smart_mailboxes[0].name = "Renamed".into();
        after.account_colors.insert("ann@example.com".into(), 2);
        after.vips.insert("bo@example.com".into(), "Bo".into());
        after.suggest_follow_ups = !before.suggest_follow_ups;
        after.inbox_categories = !before.inbox_categories;
        after.ai.local_model = "qwen".into();
        after.text_size = TextSize::Small;
        after.contacts = !before.contacts;
        after.color_scheme = ColorScheme::Dark;
        let effects = Effects::between(&before, &after);
        assert_eq!(effects.iter().collect::<Vec<_>>(), Effect::ALL);
    }

    #[test]
    fn every_effect_has_a_change_that_causes_it() {
        // A window arm nothing can reach is dead code; this catches it.
        let changes = [
            Change::Threading(false),
            Change::AccountName {
                email: "ann@example.com".into(),
                name: "Ann".into(),
            },
            Change::AccountColor {
                email: "ann@example.com".into(),
                index: 1,
            },
            Change::SaveSmartMailbox(Box::new(smart("s1", "From Ann"))),
            Change::ToggleVip {
                email: "ann@example.com".into(),
                name: "Ann".into(),
            },
            Change::SuggestFollowUps(false),
            Change::InboxCategories(false),
            Change::Ai(AiChange::ConfirmActions(false)),
            Change::StepTextSize(1),
            Change::Contacts(true),
            Change::ColorScheme(ColorScheme::Light),
        ];
        let mut seen: Vec<Effect> = Vec::new();
        for change in changes {
            for effect in effects(change).iter() {
                if !seen.contains(&effect) {
                    seen.push(effect);
                }
            }
        }
        seen.sort();
        assert_eq!(seen, Effect::ALL);
    }

    #[test]
    fn settable_names_match_the_settings_file() {
        let file = serde_json::to_value(Settings::default()).expect("settings serialise");
        let keys: Vec<&str> = file
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        let settable: Vec<&str> = Setting::ALL.iter().map(|s| s.name()).collect();
        for name in &settable {
            assert!(keys.contains(name), "{name} is not a field of Settings");
        }
        // The rest are the ones the assistant cannot set by name. A new
        // preference lands here and fails until someone decides which side
        // it belongs on.
        let mut rest: Vec<&str> = keys
            .iter()
            .filter(|k| !settable.contains(k))
            .copied()
            .collect();
        rest.sort_unstable();
        assert_eq!(
            rest,
            [
                "account_colors",
                "account_names",
                "account_order",
                "ai",
                // Reading contacts asks Google for access of its own, so
                // it stays a choice the person makes in Preferences.
                "contacts",
                "flag_color",
                "hidden_addresses",
                "inbox_categories",
                "last_sender",
                // Which buttons a notification carries is a list, and a
                // setting the assistant changes by name holds one value.
                "notification_buttons",
                "send_as",
                "signatures",
                "smart_mailboxes",
                "spell_languages",
                "spell_words",
                "suggest_follow_ups",
                "vips",
            ]
        );
    }

    #[test]
    fn a_name_and_a_json_value_become_a_change() {
        assert_eq!(Setting::named("nonsense"), None);
        let setting = Setting::named("text_size").expect("a known setting");
        assert_eq!(setting.value(&Settings::default()), json!("normal"));
        assert_eq!(
            setting.change(&json!("larger")),
            Ok(Change::TextSize(TextSize::Larger))
        );
        assert!(setting.change(&json!("enormous")).is_err());
        assert!(
            Setting::named("threading")
                .unwrap()
                .change(&json!(7))
                .is_err()
        );
        assert_eq!(
            Setting::named("default_account")
                .unwrap()
                .change(&json!(null)),
            Ok(Change::DefaultAccount(None))
        );
    }

    #[test]
    fn a_name_reads_back_what_it_wrote() {
        let mut settings = Settings::default();
        for setting in Setting::ALL {
            let before = setting.value(&settings);
            setting
                .change(&before)
                .expect("its own value round trips")
                .apply_to(&mut settings);
            assert_eq!(setting.value(&settings), before, "{}", setting.name());
        }
    }
}
