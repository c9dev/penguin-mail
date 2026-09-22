//! Named changes to the preferences, and what each one makes the window redo.
//!
//! A change is a value, not a closure, so the same change can come from a
//! switch in Preferences, a keyboard shortcut, or an assistant tool call and
//! land the same way every time. Applying one reports its [`Effects`]: the
//! parts of the window that now show something stale. Nothing here touches
//! GTK, so the rules live under unit tests.

use mailrs_domain::{Category, FlagColor, SmartMailbox};
use mailrs_sync::HiddenAddress;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::assistant::sources::mcp::McpServer;

use super::{
    Choice, ColorScheme, ComposeFormat, Feature, MarkRead, RemoteImages, Settings, TextSize,
    UndoSend, Use, WebSearch,
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
    /// The locale the interface speaks; empty follows the desktop.
    Language(String),
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
    DefaultCategory(Category),
    SuggestFollowUps(bool),
    CheckForUpdates(bool),
    /// The updater asked GitHub at this time, in Unix seconds.
    UpdateChecked(i64),
    /// The release version the updater last put a notification up for.
    UpdateAnnounced(String),
    /// What a new message starts as.
    ComposeFormat(ComposeFormat),
    /// Ask before a message that promises a file goes without one.
    CheckAttachments(bool),
    /// Open the composer with Sign on.
    SignByDefault(bool),
    /// Turn Encrypt on whenever gpg holds a key for every recipient.
    EncryptWhenPossible(bool),
    /// Read one account's Google contacts, or stop and forget them.
    AccountContacts {
        email: String,
        on: bool,
    },
    /// Folds the old one switch for every account into the per-account
    /// list: all of `emails` when it was on.
    AllContacts(Vec<String>),
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
    /// Keeps a Hide My Email address, replacing the one with the same
    /// address.
    SaveHiddenAddress(HiddenAddress),
    /// Forgets the Hide My Email address with this address.
    ForgetHiddenAddress(String),
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
    /// Records that an account has been offered to GNOME Online Accounts,
    /// whichever way the person answered.
    OfferedToGnome(String),
    Ai(AiChange),
    /// Open the assistant's thinking and tool rows as they appear.
    AssistantDetailsExpanded(bool),
    /// Runs this outside tool, `source/tool`, from now on without asking.
    AllowTool(String),
    /// Asks before this outside tool again.
    ForbidTool(String),
    /// Adds an MCP server, or replaces the one named `was`. A server that
    /// changes its name loses the tools answered Always Allow under the
    /// old one, since the keys carry the name.
    SaveMcpServer {
        was: Option<String>,
        server: McpServer,
    },
    /// Removes an MCP server with the tools of it answered Always Allow.
    RemoveMcpServer(String),
    /// Offers an MCP server's tools to the assistant, or stops offering them.
    EnableMcpServer {
        name: String,
        on: bool,
    },
    /// Offers one skill to the assistant, or stops offering it.
    SkillEnabled {
        id: String,
        on: bool,
    },
    /// Lets one skill's commands reach the network, or cuts them off.
    SkillNetwork {
        id: String,
        on: bool,
    },
}

/// A change to the AI settings. API keys live in the keyring and never come
/// through here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiChange {
    /// Which model one feature uses.
    Use {
        feature: Feature,
        choice: Use,
    },
    BaseUrl(String),
    ClaudeCommand(String),
    ConfirmActions(bool),
    /// A server the app found on this machine: its address and first model.
    LocalServer {
        base_url: String,
        model: String,
    },
    /// How the assistant searches the web.
    WebSearch(WebSearch),
    /// The SearXNG server a local model searches with.
    SearxngUrl(String),
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
            Change::Language(code) => settings.language = code,
            Change::Notifications(on) => settings.notifications = on,
            Change::NotificationPreviews(on) => settings.notification_previews = on,
            Change::NotifyVipsOnly(on) => settings.notify_vips_only = on,
            Change::NotificationButton { button, show } => {
                settings.show_notification_button(button, show)
            }
            Change::UndoSend(delay) => settings.undo_send = delay,
            Change::DefaultAccount(email) => settings.default_account = email,
            Change::InboxCategories(on) => settings.inbox_categories = on,
            Change::DefaultCategory(category) => settings.default_category = category,
            Change::SuggestFollowUps(on) => settings.suggest_follow_ups = on,
            Change::CheckForUpdates(on) => settings.check_for_updates = on,
            Change::UpdateChecked(at) => settings.last_update_check = Some(at),
            Change::UpdateAnnounced(version) => settings.announced_update = Some(version),
            Change::ComposeFormat(format) => settings.compose_format = format,
            Change::CheckAttachments(on) => settings.check_attachments = on,
            Change::SignByDefault(on) => settings.sign_by_default = on,
            Change::EncryptWhenPossible(on) => settings.encrypt_when_possible = on,
            Change::AccountContacts { email, on } => {
                let email = email.to_lowercase();
                settings.contact_accounts.retain(|e| *e != email);
                if on {
                    settings.contact_accounts.push(email);
                }
            }
            Change::AllContacts(emails) => {
                if std::mem::take(&mut settings.contacts) {
                    for email in emails {
                        let email = email.to_lowercase();
                        if !settings.contact_accounts.contains(&email) {
                            settings.contact_accounts.push(email);
                        }
                    }
                }
            }
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
            Change::SaveHiddenAddress(hidden) => {
                match settings
                    .hidden_addresses
                    .iter_mut()
                    .find(|h| h.address == hidden.address)
                {
                    Some(slot) => *slot = hidden,
                    None => settings.hidden_addresses.push(hidden),
                }
            }
            Change::ForgetHiddenAddress(address) => {
                settings.hidden_addresses.retain(|h| h.address != address)
            }
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
            Change::OfferedToGnome(email) => {
                let email = email.to_lowercase();
                if !settings.offered_to_gnome.contains(&email) {
                    settings.offered_to_gnome.push(email);
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
            Change::AssistantDetailsExpanded(on) => settings.assistant_details_expanded = on,
            Change::AllowTool(key) => {
                if !settings.assistant_allowed_tools.contains(&key) {
                    settings.assistant_allowed_tools.push(key);
                }
            }
            Change::ForbidTool(key) => settings.assistant_allowed_tools.retain(|k| *k != key),
            Change::SaveMcpServer { was, server } => {
                let replaced = was.as_deref().unwrap_or(&server.name).to_string();
                if replaced != server.name {
                    forget_mcp_tools(settings, &replaced);
                }
                match settings.mcp_servers.iter_mut().find(|s| s.name == replaced) {
                    Some(slot) => *slot = server,
                    None => settings.mcp_servers.push(server),
                }
            }
            Change::RemoveMcpServer(name) => {
                settings.mcp_servers.retain(|s| s.name != name);
                forget_mcp_tools(settings, &name);
            }
            Change::EnableMcpServer { name, on } => {
                if let Some(server) = settings.mcp_servers.iter_mut().find(|s| s.name == name) {
                    server.enabled = on;
                }
            }
            Change::SkillEnabled { id, on } => {
                let mut skill = settings.skill(&id);
                skill.enabled = on;
                set_skill(settings, id, skill);
            }
            Change::SkillNetwork { id, on } => {
                let mut skill = settings.skill(&id);
                skill.allow_network = on;
                set_skill(settings, id, skill);
            }
        }
    }
}

/// Drops the Always Allow answers for one MCP server's tools.
fn forget_mcp_tools(settings: &mut Settings, server: &str) {
    let prefix = format!("mcp:{server}/");
    settings
        .assistant_allowed_tools
        .retain(|key| !key.starts_with(&prefix));
}

impl AiChange {
    fn apply_to(self, ai: &mut super::AiSettings) {
        match self {
            AiChange::Use { feature, choice } => ai.set_use(feature, choice),
            AiChange::BaseUrl(url) => ai.base_url = url,
            AiChange::ClaudeCommand(command) => ai.claude_command = command,
            AiChange::ConfirmActions(on) => ai.confirm_actions = on,
            AiChange::LocalServer { base_url, model } => {
                ai.base_url = base_url;
                ai.local_model = model;
            }
            AiChange::WebSearch(choice) => ai.web_search = choice,
            AiChange::SearxngUrl(url) => ai.searxng_url = url.trim().trim_end_matches('/').into(),
        }
    }
}

/// Stores one skill's switches. A skill with both off keeps no entry, so
/// the file lists only the skills somebody turned something on for.
fn set_skill(settings: &mut Settings, id: String, skill: super::SkillSettings) {
    if skill == super::SkillSettings::default() {
        settings.assistant_skills.remove(&id);
    } else {
        settings.assistant_skills.insert(id, skill);
    }
}

/// Declares [`Setting`] from a table of every field of [`Settings`]: the
/// ones the assistant may set by name, each beside the [`Change`] variant of
/// the same name that takes its one value, and the ones held back. The field
/// is the key in `settings.toml` and the name the tool takes. The table has
/// to name every field once, so a new preference does not compile until
/// someone puts it on one side.
macro_rules! settable {
    (
        may_set { $($setting:ident => $field:ident,)* }
        held_back { $($held:ident,)* }
    ) => {
        /// A preference the assistant may read and change by name. The
        /// assistant's own settings are left out on purpose, and so is
        /// anything it would need a shape more complicated than one JSON
        /// value to set.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Setting {
            $($setting,)*
        }

        impl Setting {
            /// In the order the assistant sees them.
            pub const ALL: [Setting; [$(stringify!($setting)),*].len()] =
                [$(Setting::$setting),*];

            /// The key in `settings.toml`, which is the name the tool takes too.
            pub fn name(self) -> &'static str {
                match self {
                    $(Setting::$setting => stringify!($field),)*
                }
            }

            /// What this setting is now, in the JSON the tool reports.
            pub fn value(self, settings: &Settings) -> Value {
                match self {
                    $(Setting::$setting => json!(settings.$field),)*
                }
            }

            /// Reads the tool's JSON into a change, or says what is wrong
            /// with it.
            pub fn change(self, value: &Value) -> Result<Change, String> {
                fn read<T: DeserializeOwned>(value: &Value) -> Result<T, String> {
                    serde_json::from_value(value.clone()).map_err(|err| err.to_string())
                }
                Ok(match self {
                    $(Setting::$setting => Change::$setting(read(value)?),)*
                })
            }
        }

        // Never called. Building a `Settings` names every field, so a new
        // one fails to compile, as a missing field, until it is listed.
        const _: fn(Settings) -> Settings = |settings| Settings {
            $($field: settings.$field,)*
            $($held: settings.$held,)*
        };
    };
}

settable! {
    may_set {
        Threading => threading,
        MarkRead => mark_read,
        RemoteImages => remote_images,
        TextSize => text_size,
        ColorScheme => color_scheme,
        Notifications => notifications,
        NotificationPreviews => notification_previews,
        NotifyVipsOnly => notify_vips_only,
        UndoSend => undo_send,
        DefaultAccount => default_account,
        ComposeFormat => compose_format,
    }
    held_back {
        account_colors,
        account_names,
        account_order,
        ai,
        // The updater's own record of what it announced.
        announced_update,
        // How the assistant's pane lays out its own turns belongs with the
        // rest of its settings, on the AI page.
        assistant_allowed_tools,
        assistant_details_expanded,
        // Skills and whether their scripts reach the network are for the
        // person to turn on, never the model.
        assistant_skills,
        // The composer reads this as a message goes out, so the assistant
        // has no business turning the warning off.
        check_attachments,
        // Whether the app asks GitHub for new releases is the person's
        // call, made in Preferences.
        check_for_updates,
        // Reading contacts asks Google for access of its own, so it stays a
        // choice the person makes in Preferences.
        contact_accounts,
        contacts,
        // How the inbox is arranged, and which slice of it opens first, is
        // the person's own view of their mail. It sits beside
        // inbox_categories for the same reason.
        default_category,
        // Whether mail goes out signed or encrypted is the person's to
        // decide, not something the assistant flips.
        encrypt_when_possible,
        flag_color,
        hidden_addresses,
        inbox_categories,
        // A new language only arrives with a restart, and the assistant can
        // neither restart the app nor ask for one, so it would change a
        // preference with nothing to show.
        language,
        last_sender,
        last_update_check,
        // Each server runs commands or reaches a service of the person's
        // choosing, so only Preferences adds one.
        mcp_servers,
        // Which buttons a notification carries is a list, and a setting the
        // assistant changes by name holds one value.
        notification_buttons,
        // Whether an account has been offered to GNOME is the card's own
        // memory of asking, not a preference.
        offered_to_gnome,
        send_as,
        sign_by_default,
        signatures,
        smart_mailboxes,
        spell_languages,
        spell_words,
        suggest_follow_ups,
        vips,
    }
}

impl Setting {
    pub fn named(name: &str) -> Option<Setting> {
        Setting::ALL.into_iter().find(|s| s.name() == name)
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
    /// The assistant's connection or its model, which the assistant pane
    /// shows in its title.
    Assistant,
    /// How large the conversation's text is.
    TextSize,
    /// Whether contacts supply names and photos, which rows and the open
    /// conversation show.
    Contacts,
    /// Light or dark.
    Theme,
    /// The interface's language, which only a restart can change: GTK and
    /// gettext both read the locale as the process starts. The window says
    /// so and offers a restart rather than translating half of itself.
    Language,
}

impl Effect {
    /// In the order the window applies them: accounts first, because the
    /// rows and the smart mailbox on screen read what it sets.
    pub const ALL: [Effect; 12] = [
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
        Effect::Language,
    ];
}

/// The effects of one change: what the window redraws, each at most once
/// and in [`Effect::ALL`] order, and what the app does to the address books
/// on this computer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Effects {
    shown: Vec<Effect>,
    address_books: AddressBooks,
}

/// What a change to the contact switches asks of the address books kept on
/// this computer. It follows from the settings alone, so Preferences, the
/// startup fold of the old switch, and any later caller of
/// [`Change::AccountContacts`] read and forget the same way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddressBooks {
    /// An account started reading its contacts: read the address books now,
    /// and ask Google for access where an account lacks it.
    pub read: bool,
    /// The accounts, by lower-case address, that stopped reading contacts.
    /// Their stored contacts and photos go.
    pub forget: Vec<String>,
}

impl AddressBooks {
    fn between(before: &Settings, after: &Settings) -> AddressBooks {
        let started = after
            .contact_accounts
            .iter()
            .any(|email| !before.contact_accounts.contains(email));
        // The fold turns the old switch off as it turns each account on, and
        // it read every account's book even when the list already held them.
        let folded = before.contacts && !after.contacts;
        AddressBooks {
            read: started || folded,
            forget: before
                .contact_accounts
                .iter()
                .filter(|email| !after.contact_accounts.contains(email))
                .cloned()
                .collect(),
        }
    }
}

impl Effects {
    /// Nothing on screen has to change.
    pub fn is_empty(&self) -> bool {
        self.shown.is_empty()
    }

    pub fn has(&self, effect: Effect) -> bool {
        self.shown.contains(&effect)
    }

    pub fn iter(&self) -> impl Iterator<Item = Effect> + '_ {
        self.shown.iter().copied()
    }

    pub fn address_books(&self) -> &AddressBooks {
        &self.address_books
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
            language,
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
            mcp_servers,
            ai,
            assistant_details_expanded,
            assistant_allowed_tools,
            assistant_skills,
            hidden_addresses,
            inbox_categories,
            default_category,
            suggest_follow_ups,
            spell_languages,
            spell_words,
            last_sender,
            send_as,
            compose_format,
            check_attachments,
            sign_by_default,
            encrypt_when_possible,
            contacts,
            contact_accounts,
            offered_to_gnome,
            check_for_updates,
            last_update_check,
            announced_update,
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
            check_attachments,
            sign_by_default,
            encrypt_when_possible,
            // The event card reads this as it goes up, and it changes
            // nothing that is already on screen.
            offered_to_gnome,
            // The pane reads this as it adds a row, and rows already in the
            // chat stay as the reader left them.
            assistant_details_expanded,
            // The toolbox reads these when a turn starts.
            assistant_allowed_tools,
            // So do these, and the AI page stops a server it turns off.
            mcp_servers,
            // A chat reads the skills as it starts, so a switch changes the
            // next chat rather than the one on screen.
            assistant_skills,
            // The updater reads these when its timer fires.
            check_for_updates,
            last_update_check,
            announced_update,
        );
        let smart_changed = *smart_mailboxes != before.smart_mailboxes;
        let colors_changed = *account_colors != before.account_colors;
        let vips_changed = *vips != before.vips;
        // Walking Effect::ALL keeps the order the window relies on.
        let shown = Effect::ALL
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
                // The bar itself, and which of its tabs the window opens
                // on: both are the category strip redrawing.
                Effect::Categories => {
                    *inbox_categories != before.inbox_categories
                        || *default_category != before.default_category
                }
                // The pane shows the connection and its model. The rest of
                // `ai` is read as a turn starts or a feature runs.
                Effect::Assistant => {
                    (ai.provider, ai.model_on(ai.provider))
                        != (before.ai.provider, before.ai.model_on(before.ai.provider))
                }
                Effect::TextSize => *text_size != before.text_size,
                Effect::Contacts => {
                    *contacts != before.contacts || *contact_accounts != before.contact_accounts
                }
                Effect::Theme => *color_scheme != before.color_scheme,
                Effect::Language => *language != before.language,
            })
            .collect();
        Effects {
            shown,
            address_books: AddressBooks::between(before, after),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::AiProvider;
    use mailrs_domain::smart::{Condition, Field};

    fn effects(change: Change) -> Effects {
        change.apply(&mut Settings::default())
    }

    #[test]
    fn web_search_picks_an_engine_and_keeps_its_address_tidy() {
        let mut settings = Settings::default();
        let changed = Change::Ai(AiChange::WebSearch(WebSearch::Searxng)).apply(&mut settings);
        // The next turn reads the engine, and the pane has nothing to redraw.
        assert!(changed.is_empty());
        Change::Ai(AiChange::SearxngUrl(" http://searx.lan:8080/ ".into())).apply(&mut settings);
        assert_eq!(settings.ai.web_search, WebSearch::Searxng);
        assert_eq!(settings.ai.searxng_url, "http://searx.lan:8080");
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
        let anthropic = Change::Ai(AiChange::Use {
            feature: Feature::Assistant,
            choice: Use::Model {
                connection: AiProvider::Anthropic,
                model: "claude-opus-5".into(),
            },
        });
        assert!(effects(anthropic).has(Effect::Assistant));
    }

    #[test]
    fn each_feature_takes_a_model_of_its_own_or_follows_the_assistant() {
        let mut settings = Settings::default();
        let choose = |feature, connection, model: &str| {
            Change::Ai(AiChange::Use {
                feature,
                choice: Use::Model {
                    connection,
                    model: model.into(),
                },
            })
        };
        let effects = choose(Feature::Assistant, AiProvider::Local, "qwen").apply(&mut settings);
        assert!(effects.has(Effect::Assistant));
        // The assistant's choice lands where older versions look for it.
        assert_eq!(settings.ai.provider, AiProvider::Local);
        assert_eq!(settings.ai.local_model, "qwen");
        assert!(settings.ai.uses.is_empty());

        let translation = choose(
            Feature::Translation,
            AiProvider::Anthropic,
            "claude-haiku-4-5",
        )
        .apply(&mut settings);
        assert!(
            translation.is_empty(),
            "the pane shows the assistant's model only"
        );
        assert_eq!(
            settings.ai.resolved(Feature::Translation),
            (AiProvider::Anthropic, "claude-haiku-4-5".to_string())
        );
        assert_eq!(
            settings.ai.resolved(Feature::Assistant),
            (AiProvider::Local, "qwen".to_string())
        );

        let back = Change::Ai(AiChange::Use {
            feature: Feature::Translation,
            choice: Use::SameAsAssistant,
        });
        back.apply(&mut settings);
        assert!(settings.ai.uses.is_empty(), "following leaves no entry");
        assert_eq!(
            settings.ai.resolved(Feature::Translation),
            (AiProvider::Local, "qwen".to_string())
        );

        // The assistant has nobody to follow, so asking it to changes nothing.
        let before = settings.clone();
        let effects = Change::Ai(AiChange::Use {
            feature: Feature::Assistant,
            choice: Use::SameAsAssistant,
        })
        .apply(&mut settings);
        assert!(effects.is_empty());
        assert_eq!(settings, before);
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
            Change::AssistantDetailsExpanded(true),
            Change::UpdateChecked(1_700_000_000),
            Change::UpdateAnnounced("0.2.0".into()),
            Change::SkillEnabled {
                id: "claude-code/pdf".into(),
                on: true,
            },
            Change::SkillNetwork {
                id: "claude-code/pdf".into(),
                on: true,
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
    fn hidden_addresses_are_kept_replaced_and_forgotten_quietly() {
        let hidden = |address: &str, active: bool| HiddenAddress {
            account: "dana@example.com".into(),
            address: address.into(),
            note: String::new(),
            created: 1,
            active,
            label_filter: Some("f1".into()),
            trash_filter: None,
        };
        let kite = "dana+kite.fern482@example.com";
        let mut settings = Settings::default();
        let saved = Change::SaveHiddenAddress(hidden(kite, true)).apply(&mut settings);
        assert_eq!(saved, Effects::default(), "nothing on screen lists them");
        Change::SaveHiddenAddress(hidden("dana+moss.olive017@example.com", true))
            .apply(&mut settings);
        // Saving an address again replaces it rather than adding a second.
        Change::SaveHiddenAddress(hidden(kite, false)).apply(&mut settings);
        assert_eq!(settings.hidden_addresses.len(), 2);
        assert!(!settings.hidden_addresses[0].active);
        Change::ForgetHiddenAddress(kite.into()).apply(&mut settings);
        assert_eq!(settings.hidden_addresses.len(), 1);
        assert_ne!(settings.hidden_addresses[0].address, kite);
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
        after.ai.provider = AiProvider::Local;
        after.text_size = TextSize::Small;
        after.contact_accounts = vec!["ann@example.com".into()];
        after.color_scheme = ColorScheme::Dark;
        after.language = "pt_PT".into();
        let effects = Effects::between(&before, &after);
        assert_eq!(effects.iter().collect::<Vec<_>>(), Effect::ALL);
    }

    #[test]
    fn contacts_turn_on_and_off_one_account_at_a_time() {
        let mut settings = Settings::default();
        let on = |email: &str, on| Change::AccountContacts {
            email: email.into(),
            on,
        };
        assert!(
            on("Ann@Example.com", true)
                .apply(&mut settings)
                .has(Effect::Contacts)
        );
        on("bo@example.com", true).apply(&mut settings);
        on("ann@example.com", true).apply(&mut settings);
        assert_eq!(
            settings.contact_accounts,
            ["bo@example.com", "ann@example.com"]
        );
        assert!(settings.reads_contacts("ANN@example.com"));
        on("ann@example.com", false).apply(&mut settings);
        assert!(!settings.reads_contacts("ann@example.com"));
        assert!(settings.reads_contacts("bo@example.com"));
    }

    #[test]
    fn switching_contacts_reads_or_forgets_that_accounts_book() {
        let mut settings = Settings::default();
        let on = |email: &str, on| Change::AccountContacts {
            email: email.into(),
            on,
        };
        let started = on("Ann@Example.com", true).apply(&mut settings);
        assert_eq!(
            started.address_books(),
            &AddressBooks {
                read: true,
                forget: vec![],
            }
        );
        on("bo@example.com", true).apply(&mut settings);
        let stopped = on("ANN@example.com", false).apply(&mut settings);
        assert_eq!(
            stopped.address_books(),
            &AddressBooks {
                read: false,
                forget: vec!["ann@example.com".into()],
            }
        );
        // A switch that is already where it goes asks nothing of the books.
        assert_eq!(
            on("bo@example.com", true).apply(&mut settings),
            Effects::default()
        );
        // Nothing but the contact switches touches them.
        assert_eq!(
            Change::Threading(false)
                .apply(&mut settings)
                .address_books(),
            &AddressBooks::default()
        );
    }

    #[test]
    fn a_skill_keeps_an_entry_only_while_a_switch_is_on() {
        let mut settings = Settings::default();
        let id = "penguin-mail/receipts".to_string();
        assert_eq!(settings.skill(&id), super::super::SkillSettings::default());
        Change::SkillEnabled {
            id: id.clone(),
            on: true,
        }
        .apply_to(&mut settings);
        Change::SkillNetwork {
            id: id.clone(),
            on: true,
        }
        .apply_to(&mut settings);
        let skill = settings.skill(&id);
        assert!(skill.enabled && skill.allow_network);
        Change::SkillEnabled {
            id: id.clone(),
            on: false,
        }
        .apply_to(&mut settings);
        assert!(settings.skill(&id).allow_network);
        Change::SkillNetwork {
            id: id.clone(),
            on: false,
        }
        .apply_to(&mut settings);
        assert!(settings.assistant_skills.is_empty());
    }

    #[test]
    fn the_old_switch_for_every_account_folds_in_once() {
        let emails = || vec!["ann@example.com".to_string(), "bo@example.com".to_string()];
        let mut settings = Settings {
            contacts: true,
            ..Settings::default()
        };
        let folded = Change::AllContacts(emails()).apply(&mut settings);
        assert!(!settings.contacts);
        assert_eq!(settings.contact_accounts, emails());
        assert!(folded.address_books().read, "the fold reads every book");
        // Off afterwards stays off: the fold does not run twice.
        Change::AccountContacts {
            email: "bo@example.com".into(),
            on: false,
        }
        .apply(&mut settings);
        assert!(
            Change::AllContacts(emails())
                .apply(&mut settings)
                .address_books()
                .forget
                .is_empty()
        );
        assert!(!settings.reads_contacts("bo@example.com"));
    }

    #[test]
    fn the_fold_reads_the_books_even_when_the_list_holds_every_account() {
        let mut settings = Settings {
            contacts: true,
            contact_accounts: vec!["ann@example.com".into()],
            ..Settings::default()
        };
        let folded = Change::AllContacts(vec!["ann@example.com".into()]).apply(&mut settings);
        assert!(folded.address_books().read);
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
            Change::Ai(AiChange::Use {
                feature: Feature::Assistant,
                choice: Use::Model {
                    connection: AiProvider::Local,
                    model: "qwen".into(),
                },
            }),
            Change::StepTextSize(1),
            Change::AccountContacts {
                email: "ann@example.com".into(),
                on: true,
            },
            Change::ColorScheme(ColorScheme::Light),
            Change::Language("pt_PT".into()),
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
    fn settable_names_are_keys_of_the_settings_file() {
        // The names come from the field names, so this catches a serde
        // rename that would make the tool's name and the file's key differ.
        let file = serde_json::to_value(Settings::default()).expect("settings serialise");
        let keys = file.as_object().expect("an object");
        for setting in Setting::ALL {
            assert!(keys.contains_key(setting.name()), "{}", setting.name());
        }
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
