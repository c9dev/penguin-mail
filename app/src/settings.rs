//! App preferences, kept in `~/.config/mailrs/settings.toml`. Sync options
//! stay in `config.toml`, which the command-line tool reads too.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Group replies into conversations instead of listing each message.
    pub threading: bool,
    pub mark_read: MarkRead,
    pub remote_images: RemoteImages,
    pub text_size: TextSize,
    pub color_scheme: ColorScheme,
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
    pub smart_mailboxes: Vec<crate::smart::SmartMailbox>,
    /// Account addresses in sidebar order; accounts not listed follow.
    pub account_order: Vec<String>,
    /// A colour from the palette per account address.
    pub account_colors: BTreeMap<String, usize>,
    /// A name shown instead of the address in the sidebar.
    pub account_names: BTreeMap<String, String>,
    /// Plus addresses made with Hide My Email, oldest first.
    pub hidden_addresses: Vec<crate::hide_my_email::HiddenAddress>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            threading: true,
            mark_read: MarkRead::Immediately,
            remote_images: RemoteImages::Ask,
            text_size: TextSize::Normal,
            color_scheme: ColorScheme::System,
            notifications: true,
            notification_previews: true,
            default_account: None,
            signatures: BTreeMap::new(),
            undo_send: UndoSend::Ten,
            flag_color: mailrs_domain::FlagColor::Red,
            vips: BTreeMap::new(),
            notify_vips_only: false,
            smart_mailboxes: Vec::new(),
            account_order: Vec::new(),
            account_colors: BTreeMap::new(),
            account_names: BTreeMap::new(),
            hidden_addresses: Vec::new(),
        }
    }
}

/// A preference with a fixed set of choices, shown as a combo row.
pub trait Choice: Sized + Copy + PartialEq + 'static {
    const ALL: &'static [Self];
    fn label(self) -> &'static str;

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
    fn label(self) -> &'static str {
        match self {
            MarkRead::Immediately => "When opened",
            MarkRead::AfterDelay => "After 2 seconds",
            MarkRead::Manually => "Only when I choose",
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
    fn label(self) -> &'static str {
        match self {
            RemoteImages::Ask => "Ask each time",
            RemoteImages::Always => "Always load",
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
    fn label(self) -> &'static str {
        match self {
            TextSize::Small => "Small",
            TextSize::Normal => "Default",
            TextSize::Large => "Large",
            TextSize::Larger => "Larger",
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
    fn label(self) -> &'static str {
        match self {
            UndoSend::Off => "Off",
            UndoSend::Five => "5 seconds",
            UndoSend::Ten => "10 seconds",
            UndoSend::Twenty => "20 seconds",
            UndoSend::Thirty => "30 seconds",
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
    fn label(self) -> &'static str {
        match self {
            ColorScheme::System => "Follow system",
            ColorScheme::Light => "Light",
            ColorScheme::Dark => "Dark",
        }
    }
}

/// How often each account checks Gmail, in seconds.
pub const POLL_CHOICES: [(i64, &str); 4] = [
    (30, "Every 30 seconds"),
    (60, "Every minute"),
    (300, "Every 5 minutes"),
    (900, "Every 15 minutes"),
];

/// How many days of mail stay on this computer.
pub const WINDOW_CHOICES: [(i64, &str); 4] = [
    (14, "2 weeks"),
    (30, "30 days"),
    (90, "90 days"),
    (365, "1 year"),
];

/// Body cache limit in megabytes.
pub const CACHE_CHOICES: [(i64, &str); 3] = [(256, "256 MB"), (1024, "1 GB"), (4096, "4 GB")];

/// The index of the choice closest to `value`.
pub fn nearest<T: Copy + Into<i64>>(choices: &[(T, &str)], value: T) -> u32 {
    let value: i64 = value.into();
    choices
        .iter()
        .enumerate()
        .min_by_key(|(_, (c, _))| ((*c).into() - value).abs())
        .map_or(0, |(i, _)| i as u32)
}

impl Settings {
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
        assert_eq!(nearest(&POLL_CHOICES, 45), 0);
        assert_eq!(nearest(&WINDOW_CHOICES, 100), 2);
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
    fn blank_signatures_are_removed() {
        let mut settings = Settings::default();
        settings.set_signature("a@example.com", "Ann");
        settings.set_signature("a@example.com", "   ");
        assert!(settings.signatures.is_empty());
    }
}
