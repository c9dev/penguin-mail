//! App preferences, kept in `~/.config/penguin-mail/settings.toml`. Sync options
//! stay in `config.toml`, which the command-line tool reads too.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mailrs_domain::Category;
use mailrs_domain::translate::{fill_plural, gettext};
use serde::{Deserialize, Serialize};

mod change;

pub use change::{AiChange, Change, Effect, Effects, Setting};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Group replies into conversations instead of listing each message.
    pub threading: bool,
    pub mark_read: MarkRead,
    pub remote_images: RemoteImages,
    pub text_size: TextSize,
    pub color_scheme: ColorScheme,
    /// The locale the interface speaks, such as `pt_PT`. Empty follows the
    /// desktop. Read once at startup, since GTK reads its own locale then
    /// and never again.
    pub language: String,
    pub notifications: bool,
    /// Show sender and subject in notifications, not just a count.
    pub notification_previews: bool,
    /// Address new messages come from; the first account when unset.
    pub default_account: Option<String>,
    /// Markdown signature per account address.
    pub signatures: BTreeMap<String, String>,
    /// How long Undo stays available after Send.
    pub undo_send: UndoSend,
    /// The colour a new flag gets: the last one chosen.
    pub flag_color: mailrs_domain::FlagColor,
    /// Very important people: lower-case address to display name.
    pub vips: BTreeMap<String, String>,
    /// Notify only about mail from VIPs.
    pub notify_vips_only: bool,
    /// The buttons a new-mail notification carries, in the order it shows
    /// them. Empty leaves a notification with nothing but its body to click.
    pub notification_buttons: Vec<crate::notify::Button>,
    pub smart_mailboxes: Vec<mailrs_domain::SmartMailbox>,
    /// Account addresses in sidebar order; accounts not listed follow.
    pub account_order: Vec<String>,
    /// A colour from the palette per account address.
    pub account_colors: BTreeMap<String, usize>,
    /// A name shown instead of the address in the sidebar.
    pub account_names: BTreeMap<String, String>,
    pub ai: AiSettings,
    /// Open the assistant's thinking and tool rows as they appear, rather
    /// than folded to one line each.
    pub assistant_details_expanded: bool,
    /// Plus addresses made with Hide My Email, oldest first.
    pub hidden_addresses: Vec<crate::hide_my_email::HiddenAddress>,
    /// Split inboxes into Primary, Updates, Promotions, and Social, from
    /// Gmail's category labels.
    pub inbox_categories: bool,
    /// The category the window opens on. All shows the whole inbox, which
    /// is what a mail client does when nobody has asked it to hide
    /// anything.
    pub default_category: Category,
    /// Show Follow Up for sent mail nobody has answered.
    pub suggest_follow_ups: bool,
    /// Dictionary languages per account address, such as `["en_US",
    /// "pt_PT"]`. Empty follows the desktop's locale.
    pub spell_languages: BTreeMap<String, Vec<String>>,
    /// Words Add to Dictionary kept, lower case.
    pub spell_words: Vec<String>,
    /// The send-as address each account last sent from, so the composer
    /// opens where the writer left it.
    pub last_sender: BTreeMap<String, String>,
    /// Every address each account may send as, as Gmail last reported them,
    /// keyed by the account's own address. Kept here so the composer opens
    /// without waiting on the network; the app refreshes it in the
    /// background.
    pub send_as: BTreeMap<String, Vec<crate::compose::SendAsAddress>>,
    /// What a new message starts as: styled text, or Markdown source.
    pub compose_format: ComposeFormat,
    /// Ask before sending a message that promises a file and carries none.
    pub check_attachments: bool,
    /// Open the composer with Sign on. It does nothing without a gpg.
    pub sign_by_default: bool,
    /// Turn Encrypt on as soon as gpg holds a key for every recipient.
    pub encrypt_when_possible: bool,
    /// Accounts already offered to GNOME Online Accounts, lower case.
    /// The offer is worth making once: whoever says no to it means no,
    /// and whoever says yes has GNOME asking them the rest.
    pub offered_to_gnome: Vec<String>,
    /// Ask GitHub once a day whether a newer release is out.
    pub check_for_updates: bool,
    /// When the last timed check ran, in seconds since the Unix epoch. The
    /// app restarts itself after its window closes, so without this it
    /// would check on every start.
    pub last_update_check: Option<i64>,
    /// The newest release already announced in a notification, so each one
    /// is announced once.
    pub announced_update: Option<String>,
    /// Before contacts were chosen per account, one switch for all of them.
    /// True folds into `contact_accounts` as every account the first time
    /// the accounts load, and goes back to false.
    pub contacts: bool,
    /// The accounts whose Google contacts Penguin Mail reads, for names,
    /// photos, and recipient suggestions, by lower-case address. Each is off
    /// until the owner turns it on, because each asks Google for more
    /// access.
    pub contact_accounts: Vec<String>,
}

/// Where the assistant's model runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AiProvider {
    Off,
    /// A local or self-hosted server with OpenAI's API: LM Studio, Ollama,
    /// Unsloth, llama.cpp, vLLM, or OpenAI itself.
    Local,
    Anthropic,
    /// The user's Claude subscription, through Claude Code.
    ClaudeCode,
}

impl Choice for AiProvider {
    const ALL: &'static [Self] = &[
        AiProvider::Off,
        AiProvider::Local,
        AiProvider::Anthropic,
        AiProvider::ClaudeCode,
    ];
    fn label(self) -> String {
        match self {
            AiProvider::Off => gettext("Off"),
            AiProvider::Local => gettext("Local or OpenAI-compatible server"),
            AiProvider::Anthropic => gettext("Anthropic API key"),
            AiProvider::ClaudeCode => gettext("Claude subscription (Claude Code)"),
        }
    }
}

/// The assistant's model and how careful it is. API keys live in the
/// keyring, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    pub provider: AiProvider,
    /// The local server's API address, ending in `/v1`.
    pub base_url: String,
    pub local_model: String,
    pub anthropic_model: String,
    /// A Claude Code model alias such as `sonnet`; empty uses its default.
    pub claude_model: String,
    /// The `claude` command; empty finds it automatically.
    pub claude_command: String,
    /// Ask before the assistant sends mail or changes Gmail settings.
    pub confirm_actions: bool,
}

impl Default for AiSettings {
    fn default() -> Self {
        AiSettings {
            provider: AiProvider::Off,
            base_url: "http://localhost:1234/v1".into(),
            local_model: String::new(),
            anthropic_model: "claude-opus-5".into(),
            claude_model: String::new(),
            claude_command: String::new(),
            confirm_actions: true,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            threading: true,
            mark_read: MarkRead::Immediately,
            remote_images: RemoteImages::Ask,
            text_size: TextSize::Normal,
            color_scheme: ColorScheme::System,
            language: String::new(),
            notifications: true,
            notification_previews: true,
            default_account: None,
            signatures: BTreeMap::new(),
            undo_send: UndoSend::Ten,
            flag_color: mailrs_domain::FlagColor::Red,
            vips: BTreeMap::new(),
            notify_vips_only: false,
            notification_buttons: crate::notify::Button::ALL.to_vec(),
            smart_mailboxes: Vec::new(),
            account_order: Vec::new(),
            account_colors: BTreeMap::new(),
            account_names: BTreeMap::new(),
            ai: AiSettings::default(),
            assistant_details_expanded: false,
            hidden_addresses: Vec::new(),
            inbox_categories: true,
            default_category: Category::All,
            suggest_follow_ups: true,
            spell_languages: BTreeMap::new(),
            spell_words: Vec::new(),
            last_sender: BTreeMap::new(),
            send_as: BTreeMap::new(),
            compose_format: ComposeFormat::Rich,
            check_attachments: true,
            sign_by_default: false,
            encrypt_when_possible: false,
            contacts: false,
            contact_accounts: Vec::new(),
            offered_to_gnome: Vec::new(),
            check_for_updates: true,
            last_update_check: None,
            announced_update: None,
        }
    }
}

/// How the composer holds a message while it is being written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComposeFormat {
    /// Bold shows as bold: the formatting bar styles the text itself.
    Rich,
    /// The writer types Markdown and sees its marks.
    Markdown,
}

impl Choice for ComposeFormat {
    const ALL: &'static [Self] = &[ComposeFormat::Rich, ComposeFormat::Markdown];
    fn label(self) -> String {
        match self {
            ComposeFormat::Rich => gettext("Rich text"),
            ComposeFormat::Markdown => gettext("Markdown"),
        }
    }
}

/// The inbox category a fresh window opens on. The name is the one the
/// category bar shows, so it reads the same in both places.
impl Choice for Category {
    const ALL: &'static [Self] = &Category::ALL;
    fn label(self) -> String {
        self.name()
    }
}

/// A preference with a fixed set of choices, shown as a combo row.
pub trait Choice: Sized + Copy + PartialEq + 'static {
    const ALL: &'static [Self];
    fn label(self) -> String;

    fn index(self) -> u32 {
        Self::ALL.iter().position(|c| *c == self).unwrap_or(0) as u32
    }

    fn from_index(index: u32) -> Self {
        Self::ALL
            .get(index as usize)
            .copied()
            .unwrap_or(Self::ALL[0])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MarkRead {
    Immediately,
    AfterDelay,
    Manually,
}

impl Choice for MarkRead {
    const ALL: &'static [Self] = &[
        MarkRead::Immediately,
        MarkRead::AfterDelay,
        MarkRead::Manually,
    ];
    fn label(self) -> String {
        match self {
            MarkRead::Immediately => gettext("When opened"),
            MarkRead::AfterDelay => gettext("After 2 seconds"),
            MarkRead::Manually => gettext("Only when I choose"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteImages {
    Ask,
    Always,
}

impl Choice for RemoteImages {
    const ALL: &'static [Self] = &[RemoteImages::Ask, RemoteImages::Always];
    fn label(self) -> String {
        match self {
            RemoteImages::Ask => gettext("Ask each time"),
            RemoteImages::Always => gettext("Always load"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextSize {
    Small,
    Normal,
    Large,
    Larger,
}

impl TextSize {
    pub fn zoom(self) -> f64 {
        match self {
            TextSize::Small => 0.9,
            TextSize::Normal => 1.0,
            TextSize::Large => 1.15,
            TextSize::Larger => 1.3,
        }
    }
}

impl Choice for TextSize {
    const ALL: &'static [Self] = &[
        TextSize::Small,
        TextSize::Normal,
        TextSize::Large,
        TextSize::Larger,
    ];
    fn label(self) -> String {
        match self {
            TextSize::Small => gettext("Small"),
            TextSize::Normal => gettext("Default"),
            TextSize::Large => gettext("Large"),
            TextSize::Larger => gettext("Larger"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UndoSend {
    Off,
    Five,
    Ten,
    Twenty,
    Thirty,
}

impl UndoSend {
    pub fn seconds(self) -> u32 {
        match self {
            UndoSend::Off => 0,
            UndoSend::Five => 5,
            UndoSend::Ten => 10,
            UndoSend::Twenty => 20,
            UndoSend::Thirty => 30,
        }
    }
}

impl Choice for UndoSend {
    const ALL: &'static [Self] = &[
        UndoSend::Off,
        UndoSend::Five,
        UndoSend::Ten,
        UndoSend::Twenty,
        UndoSend::Thirty,
    ];
    fn label(self) -> String {
        match self {
            UndoSend::Off => gettext("Off"),
            UndoSend::Five => gettext("5 seconds"),
            UndoSend::Ten => gettext("10 seconds"),
            UndoSend::Twenty => gettext("20 seconds"),
            UndoSend::Thirty => gettext("30 seconds"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorScheme {
    System,
    Light,
    Dark,
}

impl Choice for ColorScheme {
    const ALL: &'static [Self] = &[ColorScheme::System, ColorScheme::Light, ColorScheme::Dark];
    fn label(self) -> String {
        match self {
            ColorScheme::System => gettext("Follow system"),
            ColorScheme::Light => gettext("Light"),
            ColorScheme::Dark => gettext("Dark"),
        }
    }
}

/// How often each account checks Gmail, in seconds. These say "every so
/// often" the way an event's repeat rule does, and share its words.
pub fn poll_choices() -> Vec<(i64, String)> {
    let seconds = |count: usize| {
        fill_plural(
            "Every second",
            "Every {count} seconds",
            count,
            &[("count", &count.to_string())],
        )
    };
    let minutes = |count: usize| {
        fill_plural(
            "Every minute",
            "Every {count} minutes",
            count,
            &[("count", &count.to_string())],
        )
    };
    vec![
        (30, seconds(30)),
        (60, minutes(1)),
        (300, minutes(5)),
        (900, minutes(15)),
    ]
}

/// How many days of mail stay on this computer.
pub fn window_choices() -> Vec<(i64, String)> {
    vec![
        (14, gettext("2 weeks")),
        (30, gettext("30 days")),
        (90, gettext("90 days")),
        (365, gettext("1 year")),
    ]
}

/// Body cache limit in megabytes.
pub fn cache_choices() -> Vec<(i64, String)> {
    vec![
        (256, gettext("256 MB")),
        (1024, gettext("1 GB")),
        (4096, gettext("4 GB")),
    ]
}

/// The index of the choice closest to `value`.
pub fn nearest<T: Copy + Into<i64>>(choices: &[(T, String)], value: T) -> u32 {
    let value: i64 = value.into();
    choices
        .iter()
        .enumerate()
        .min_by_key(|(_, (c, _))| ((*c).into() - value).abs())
        .map_or(0, |(i, _)| i as u32)
}

impl Settings {
    /// Whether Penguin Mail reads this account's Google contacts.
    pub fn reads_contacts(&self, email: &str) -> bool {
        let email = email.to_lowercase();
        self.contact_accounts.contains(&email)
    }

    /// Reads the file, falling back to defaults when it is missing or invalid.
    pub fn load(path: &Path) -> Settings {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|err| {
                tracing::warn!(path = %path.display(), error = %err, "ignoring unreadable settings");
                Settings::default()
            }),
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    /// `$MAILRS_SETTINGS`, else next to `config.toml`.
    pub fn default_path() -> PathBuf {
        if let Some(path) = std::env::var_os("MAILRS_SETTINGS") {
            return PathBuf::from(path);
        }
        mailrs_sync::config::config_path()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(std::env::temp_dir)
            .join("settings.toml")
    }

    pub fn signature(&self, email: &str) -> &str {
        self.signatures
            .get(&email.to_lowercase())
            .map_or("", String::as_str)
    }

    /// `emails` in the order the user chose; unlisted ones keep theirs, last.
    pub fn ordered<'a>(&self, emails: &[&'a str]) -> Vec<&'a str> {
        let rank = |email: &str| {
            self.account_order
                .iter()
                .position(|e| e.eq_ignore_ascii_case(email))
                .unwrap_or(usize::MAX)
        };
        let mut sorted = emails.to_vec();
        sorted.sort_by_key(|e| rank(e));
        sorted
    }

    /// Moves `email` one place up (`-1`) or down (`1`) among `emails`.
    pub fn move_account(&mut self, emails: &[&str], email: &str, step: isize) {
        let mut order: Vec<String> = self.ordered(emails).iter().map(|e| e.to_string()).collect();
        let Some(at) = order.iter().position(|e| e.eq_ignore_ascii_case(email)) else {
            return;
        };
        let to = at as isize + step;
        if to < 0 || to as usize >= order.len() {
            return;
        }
        order.swap(at, to as usize);
        self.account_order = order;
    }

    /// Moves smart mailbox `id` one place up or down.
    pub fn move_smart(&mut self, id: &str, step: isize) {
        let Some(at) = self.smart_mailboxes.iter().position(|m| m.id == id) else {
            return;
        };
        let to = at as isize + step;
        if to >= 0 && (to as usize) < self.smart_mailboxes.len() {
            self.smart_mailboxes.swap(at, to as usize);
        }
    }

    pub fn is_vip(&self, email: &str) -> bool {
        self.vips.contains_key(&email.to_lowercase())
    }

    /// Adds or removes a VIP. Returns whether the address is a VIP now.
    pub fn toggle_vip(&mut self, email: &str, name: &str) -> bool {
        let key = email.trim().to_lowercase();
        if self.vips.remove(&key).is_some() {
            return false;
        }
        let name = if name.trim().is_empty() {
            email.trim()
        } else {
            name.trim()
        };
        self.vips.insert(key, name.to_string());
        true
    }

    /// Puts `button` on new-mail notifications, or takes it off. The
    /// buttons stay in `Button::ALL` order however they were turned on.
    pub fn show_notification_button(&mut self, button: crate::notify::Button, show: bool) {
        self.notification_buttons = crate::notify::Button::ALL
            .into_iter()
            .filter(|b| {
                if *b == button {
                    show
                } else {
                    self.notification_buttons.contains(b)
                }
            })
            .collect();
    }

    /// What goes below a message sent from `email`. A signature written here
    /// wins; failing that, the one Gmail keeps for that send-as address.
    pub fn signature_for(&self, account: &str, email: &str) -> &str {
        let written = self.signature(email);
        if !written.is_empty() {
            return written;
        }
        self.send_as
            .get(&account.to_lowercase())
            .into_iter()
            .flatten()
            .find(|a| a.email.eq_ignore_ascii_case(email))
            .map_or("", |a| a.signature.as_str())
    }

    /// Every address `account` may send from, its own address first when
    /// Gmail reported nothing. Gmail's default comes before the rest.
    pub fn senders(&self, account: &str) -> Vec<crate::compose::SendAsAddress> {
        let stored = self.send_as.get(&account.to_lowercase());
        let mut addresses: Vec<crate::compose::SendAsAddress> = stored.cloned().unwrap_or_default();
        if !addresses
            .iter()
            .any(|a| a.email.eq_ignore_ascii_case(account))
        {
            addresses.insert(
                0,
                crate::compose::SendAsAddress {
                    email: account.to_string(),
                    default: addresses.is_empty(),
                    ..Default::default()
                },
            );
        }
        addresses.sort_by_key(|a| !a.default);
        addresses
    }

    pub fn set_signature(&mut self, email: &str, signature: &str) {
        let key = email.to_lowercase();
        if signature.trim().is_empty() {
            self.signatures.remove(&key);
        } else {
            self.signatures
                .insert(key, signature.trim_end().to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    // The category bar itself cannot be tested here: the harness gives
    // every test its own thread, GTK refuses a second init from a
    // different one, and `richbuffer` already spends this binary's one
    // GTK test. So what is tested is the setting the bar is built from.
    #[test]
    fn the_inbox_opens_on_everything_until_somebody_says_otherwise() {
        assert_eq!(Settings::default().default_category, Category::All);
    }

    #[test]
    fn choosing_a_category_survives_the_settings_file() {
        for category in Category::ALL {
            let mut settings = Settings::default();
            Change::DefaultCategory(category).apply(&mut settings);
            let written = serde_json::to_string(&settings).expect("settings serialise");
            let read: Settings = serde_json::from_str(&written).expect("and come back");
            assert_eq!(read.default_category, category, "{category:?}");
        }
    }

    #[test]
    fn the_categories_redraw_when_the_one_they_open_on_changes() {
        let before = Settings::default();
        let mut after = before.clone();
        after.default_category = Category::Promotions;
        assert!(Effects::between(&before, &after).has(Effect::Categories));
    }

    use super::*;

    #[test]
    fn a_missing_file_gives_defaults() {
        let settings = Settings::load(Path::new("/nonexistent/settings.toml"));
        assert_eq!(settings, Settings::default());
        assert!(settings.threading);
    }

    #[test]
    fn settings_round_trip_and_tolerate_missing_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let mut settings = Settings {
            threading: false,
            text_size: TextSize::Large,
            ..Settings::default()
        };
        settings.set_signature("Me@Example.com", "Dana\n\n");
        settings.save(&path).unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded, settings);
        assert_eq!(loaded.signature("me@example.com"), "Dana");
        std::fs::write(&path, "threading = false\nmark_read = \"after-delay\"\n").unwrap();
        let partial = Settings::load(&path);
        assert!(!partial.threading);
        assert_eq!(partial.mark_read, MarkRead::AfterDelay);
        assert!(partial.notifications, "missing keys take their defaults");
    }

    #[test]
    fn a_broken_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "threading = maybe").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
    }

    #[test]
    fn choices_map_to_and_from_rows() {
        assert_eq!(
            TextSize::from_index(TextSize::Larger.index()),
            TextSize::Larger
        );
        assert_eq!(MarkRead::from_index(99), MarkRead::Immediately);
        assert_eq!(nearest(&poll_choices(), 45), 0);
        assert_eq!(nearest(&window_choices(), 100), 2);
    }

    #[test]
    fn notification_buttons_keep_their_order_however_they_come_on() {
        use crate::notify::Button;
        let mut settings = Settings::default();
        for button in Button::ALL {
            settings.show_notification_button(button, false);
        }
        assert!(settings.notification_buttons.is_empty());
        settings.show_notification_button(Button::Reply, true);
        settings.show_notification_button(Button::Archive, true);
        assert_eq!(
            settings.notification_buttons,
            [Button::Archive, Button::Reply]
        );
        settings.show_notification_button(Button::Archive, true);
        assert_eq!(
            settings.notification_buttons.len(),
            2,
            "turning one on twice lists it once"
        );
    }

    #[test]
    fn accounts_move_within_the_chosen_order() {
        let mut settings = Settings::default();
        let emails = ["a@x.com", "b@x.com", "c@x.com"];
        settings.move_account(&emails, "c@x.com", -1);
        assert_eq!(settings.ordered(&emails), ["a@x.com", "c@x.com", "b@x.com"]);
        settings.move_account(&emails, "a@x.com", -1);
        assert_eq!(settings.ordered(&emails), ["a@x.com", "c@x.com", "b@x.com"]);
        // An account added later goes last.
        assert_eq!(
            settings.ordered(&["d@x.com", "b@x.com", "a@x.com", "c@x.com"]),
            ["a@x.com", "c@x.com", "b@x.com", "d@x.com"]
        );
    }

    #[test]
    fn vips_toggle_by_address_in_any_case() {
        let mut settings = Settings::default();
        assert!(settings.toggle_vip("Ann@Example.com", "Ann Lee"));
        assert!(settings.is_vip("ann@example.com"));
        assert_eq!(settings.vips["ann@example.com"], "Ann Lee");
        assert!(!settings.toggle_vip("ANN@example.com", ""));
        assert!(settings.vips.is_empty());
    }

    #[test]
    fn hidden_addresses_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let settings = Settings {
            hidden_addresses: vec![crate::hide_my_email::HiddenAddress {
                account: "dana@gmail.com".into(),
                address: "dana+kite.fern482@gmail.com".into(),
                note: "Bike shop".into(),
                created: 1_758_000_000_000,
                active: false,
                label_filter: Some("f1".into()),
                trash_filter: Some("f2".into()),
            }],
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
    }

    #[test]
    fn hidden_addresses_fill_in_missing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(
            &path,
            "[[hidden_addresses]]\naccount = \"a@x.com\"\naddress = \"a+b.c123@x.com\"\ncreated = 5\n",
        )
        .unwrap();
        let loaded = Settings::load(&path);
        let [hidden] = loaded.hidden_addresses.as_slice() else {
            panic!("one address");
        };
        assert!(hidden.active);
        assert_eq!(hidden.note, "");
        assert_eq!(hidden.label_filter, None);
        assert!(Settings::default().hidden_addresses.is_empty());
    }

    #[test]
    fn a_stored_alias_survives_a_save_and_load() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("settings.toml");
        let mut settings = Settings::default();
        settings.send_as.insert(
            "dana@example.com".into(),
            vec![
                crate::compose::SendAsAddress {
                    email: "dana@example.com".into(),
                    name: Some("Dana".into()),
                    signature: "Dana".into(),
                    default: true,
                },
                crate::compose::SendAsAddress {
                    email: "sales@example.com".into(),
                    name: Some("Sales".into()),
                    signature: "The Sales Desk".into(),
                    default: false,
                },
            ],
        );
        settings
            .last_sender
            .insert("dana@example.com".into(), "sales@example.com".into());
        settings.save(&path).expect("settings save");
        assert_eq!(Settings::load(&path), settings);
    }

    #[test]
    fn an_account_always_offers_its_own_address_with_gmails_first() {
        let settings = Settings::default();
        let senders = settings.senders("dana@example.com");
        assert_eq!(senders.len(), 1);
        assert_eq!(senders[0].email, "dana@example.com");
        assert!(senders[0].default);
    }

    #[test]
    fn a_written_signature_beats_the_one_gmail_keeps() {
        let mut settings = Settings::default();
        settings.send_as.insert(
            "dana@example.com".into(),
            vec![crate::compose::SendAsAddress {
                email: "sales@example.com".into(),
                signature: "The Sales Desk".into(),
                ..Default::default()
            }],
        );
        assert_eq!(
            settings.signature_for("dana@example.com", "SALES@example.com"),
            "The Sales Desk"
        );
        settings.set_signature("sales@example.com", "Dana, Sales");
        assert_eq!(
            settings.signature_for("dana@example.com", "sales@example.com"),
            "Dana, Sales"
        );
    }

    #[test]
    fn blank_signatures_are_removed() {
        let mut settings = Settings::default();
        settings.set_signature("a@example.com", "Ann");
        settings.set_signature("a@example.com", "   ");
        assert!(settings.signatures.is_empty());
    }
}
