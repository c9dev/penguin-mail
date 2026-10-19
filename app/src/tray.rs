//! The StatusNotifierItem shown by Ubuntu's AppIndicator extension.

use crate::APP_ID;
use mailrs_domain::AccountId;
use mailrs_domain::translate::{fill, fill_plural, gettext};

pub enum TrayCommand {
    Toggle,
    Open,
    /// Show the window on this account's inbox.
    OpenInbox(AccountId),
    Compose,
    Check,
    CheckForUpdates,
    InstallUpdate,
    WhatsNew,
    Quit,
}

/// What the tray says it is holding. A screen reader reads the title, so
/// the count is a sentence rather than a number beside a word.
fn summary(unread: i64) -> String {
    match unread {
        ..=0 => gettext("No unread mail"),
        n => fill_plural(
            "{count} unread message",
            "{count} unread messages",
            n as usize,
            &[("count", &n.to_string())],
        ),
    }
}

/// One account's line in the tray menu. The count sat two spaces after
/// the address, which reads out as a bare number.
fn account_line(email: &str, unread: i64) -> String {
    match unread {
        ..=0 => email.to_string(),
        n => fill_plural(
            "{account}, {count} unread message",
            "{account}, {count} unread messages",
            n as usize,
            &[("account", email), ("count", &n.to_string())],
        ),
    }
}

/// One account's unread INBOX count, with what the tray needs to name it
/// and to open it.
#[derive(Clone, Debug, PartialEq)]
pub struct AccountUnread {
    pub id: AccountId,
    pub email: String,
    pub unread: i64,
}

/// A line above the menu's separator.
#[derive(Debug, PartialEq)]
enum AccountLine {
    /// An account with unread mail; choosing it opens that inbox.
    Unread { id: AccountId, label: String },
    /// Nothing is unread anywhere.
    NoUnread(String),
}

/// The lines above the separator: the accounts with unread mail in the
/// order given, or one line saying there is none. No accounts, no lines.
fn account_lines(accounts: &[AccountUnread]) -> Vec<AccountLine> {
    if accounts.is_empty() {
        return Vec::new();
    }
    let lines: Vec<AccountLine> = accounts
        .iter()
        .filter(|a| a.unread > 0)
        .map(|a| AccountLine::Unread {
            id: a.id,
            label: account_line(&a.email, a.unread),
        })
        .collect();
    if lines.is_empty() {
        vec![AccountLine::NoUnread(summary(0))]
    } else {
        lines
    }
}

pub struct MailTray {
    pub unread: i64,
    /// Each account's id, address and unread INBOX count.
    pub accounts: Vec<AccountUnread>,
    pub commands: async_channel::Sender<TrayCommand>,
    /// This copy can update itself, so the menu offers a check.
    pub can_update: bool,
    /// A newer release waiting to be installed.
    pub update: Option<String>,
    /// Where the installed icons are when the host may not search there;
    /// see `installed_icons`.
    pub icon_theme_path: String,
}

/// The icon folder of the install this program runs from, for a host
/// that does not search it, or nothing. A snap's icons are not on the
/// host's search path, and without them the host draws a blank.
pub fn installed_icons() -> String {
    crate::exe::path()
        .ok()
        .and_then(|exe| icons_beside(&exe))
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// share/icons beside the bin folder holding `exe`, when the tray's
/// glyph is installed there.
fn icons_beside(exe: &std::path::Path) -> Option<std::path::PathBuf> {
    let icons = exe.parent()?.parent()?.join("share/icons");
    let glyph = format!("hicolor/symbolic/apps/{APP_ID}-unread-symbolic.svg");
    icons.join(glyph).is_file().then_some(icons)
}

/// The text beside the icon for `unread`: the count, held to four
/// characters, and nothing when nothing is unread.
fn label(unread: i64) -> String {
    match unread {
        ..=0 => String::new(),
        1..=999 => unread.to_string(),
        _ => "999+".into(),
    }
}

impl MailTray {
    fn send(&self, command: TrayCommand) {
        let _ = self.commands.try_send(command);
    }

    fn summary(&self) -> String {
        summary(self.unread)
    }
}

impl ksni::Tray for MailTray {
    fn id(&self) -> String {
        APP_ID.into()
    }

    fn title(&self) -> String {
        fill(
            &gettext("Penguin Mail: {summary}"),
            &[("summary", &self.summary())],
        )
    }

    fn icon_theme_path(&self) -> String {
        self.icon_theme_path.clone()
    }

    /// A complete color icon keeps the unread dot orange on hosts that
    /// recolor symbolic icons or omit overlays. The read icon follows the
    /// panel's foreground color.
    fn icon_name(&self) -> String {
        match self.unread {
            ..=0 => format!("{APP_ID}-symbolic"),
            _ => format!("{APP_ID}-unread"),
        }
    }

    /// A host that shows the label reads it out as the item's name unless
    /// it has this, and a bare number says little.
    fn icon_accessible_desc(&self) -> String {
        self.title()
    }

    /// GNOME's AppIndicator extension and Ayatana panels draw this beside
    /// the icon at the panel's font size.
    fn x_ayatana_label(&self) -> String {
        label(self.unread)
    }

    fn x_ayatana_label_guide(&self) -> String {
        label(self.unread)
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: fill(
                &gettext("Penguin Mail: {summary}"),
                &[("summary", &self.summary())],
            ),
            description: self
                .accounts
                .iter()
                .filter(|a| a.unread > 0)
                .map(|a| format!("{}: {}", a.email, a.unread))
                .collect::<Vec<_>>()
                .join("\n"),
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayCommand::Toggle);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};
        let mut items: Vec<MenuItem<Self>> = account_lines(&self.accounts)
            .into_iter()
            .map(|line| match line {
                AccountLine::Unread { id, label } => StandardItem {
                    label,
                    activate: Box::new(move |tray: &mut Self| {
                        tray.send(TrayCommand::OpenInbox(id));
                    }),
                    ..Default::default()
                }
                .into(),
                AccountLine::NoUnread(label) => StandardItem {
                    label,
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            })
            .collect();
        if !items.is_empty() {
            items.push(MenuItem::Separator);
        }
        let item = |label: &str, command: fn() -> TrayCommand| -> MenuItem<Self> {
            StandardItem {
                label: label.into(),
                activate: Box::new(move |tray: &mut Self| tray.send(command())),
                ..Default::default()
            }
            .into()
        };
        items.push(item(&gettext("Open Penguin Mail"), || TrayCommand::Open));
        items.push(item(&gettext("New Message"), || TrayCommand::Compose));
        items.push(item(&gettext("Check for Mail"), || TrayCommand::Check));
        if let Some(version) = &self.update {
            items.push(MenuItem::Separator);
            let install = fill(
                &gettext("Install Update {version}"),
                &[("version", version)],
            );
            let notes = fill(&gettext("What's New in {version}"), &[("version", version)]);
            items.push(item(&install, || TrayCommand::InstallUpdate));
            items.push(item(&notes, || TrayCommand::WhatsNew));
        } else if self.can_update {
            items.push(item(&gettext("Check for Updates"), || {
                TrayCommand::CheckForUpdates
            }));
        }
        items.push(MenuItem::Separator);
        items.push(item(&gettext("Quit"), || TrayCommand::Quit));
        items
    }
}

#[cfg(test)]
mod tests {
    use super::{AccountId, AccountLine, AccountUnread, account_line, account_lines, summary};

    fn tray(unread: i64) -> super::MailTray {
        let (commands, _received) = async_channel::unbounded();
        super::MailTray {
            unread,
            accounts: Vec::new(),
            commands,
            can_update: false,
            update: None,
            icon_theme_path: String::new(),
        }
    }

    #[test]
    fn with_nothing_unread_the_tray_shows_the_plain_envelope_and_no_label() {
        use ksni::Tray;
        let tray = tray(0);
        assert_eq!(tray.icon_name(), format!("{}-symbolic", crate::APP_ID));
        assert_eq!(tray.overlay_icon_name(), "");
        assert_eq!(tray.x_ayatana_label(), "");
        assert_eq!(tray.x_ayatana_label_guide(), "");
        assert_eq!(tray.tool_tip().title, "Penguin Mail: No unread mail");
        assert_eq!(tray.icon_accessible_desc(), "Penguin Mail: No unread mail");
        assert!(tray.icon_pixmap().is_empty());
    }

    #[test]
    fn one_unread_message_puts_a_dot_on_the_envelope_and_1_beside_it() {
        use ksni::Tray;
        let tray = tray(1);
        assert_eq!(
            tray.icon_name(),
            format!("{}-unread", crate::APP_ID)
        );
        assert_eq!(tray.overlay_icon_name(), "");
        assert_eq!(tray.x_ayatana_label(), "1");
        assert_eq!(tray.x_ayatana_label_guide(), "1");
        assert_eq!(tray.tool_tip().title, "Penguin Mail: 1 unread message");
        assert_eq!(
            tray.icon_accessible_desc(),
            "Penguin Mail: 1 unread message"
        );
        assert!(tray.icon_pixmap().is_empty());
    }

    #[test]
    fn a_large_count_is_written_out_in_full() {
        use ksni::Tray;
        let tray = tray(120);
        assert_eq!(
            tray.icon_name(),
            format!("{}-unread", crate::APP_ID)
        );
        assert_eq!(tray.x_ayatana_label(), "120");
        assert_eq!(tray.tool_tip().title, "Penguin Mail: 120 unread messages");
    }

    /// Reading everything takes the count off the panel: the label goes
    /// empty and the icon and tooltip are the ones with nothing unread.
    #[test]
    fn going_from_3_unread_to_none_clears_the_label_the_dot_and_the_count() {
        use ksni::Tray;
        let mut tray = tray(3);
        tray.accounts = vec![unread(1, "a@example.com", 3)];
        assert_eq!(tray.x_ayatana_label(), "3");
        assert_eq!(tray.tool_tip().description, "a@example.com: 3");
        assert_eq!(tray.overlay_icon_name(), "");
        tray.unread = 0;
        tray.accounts = vec![unread(1, "a@example.com", 0)];
        assert_eq!(tray.x_ayatana_label(), "");
        assert_eq!(tray.x_ayatana_label_guide(), "");
        assert_eq!(tray.icon_name(), format!("{}-symbolic", crate::APP_ID));
        assert_eq!(tray.overlay_icon_name(), "");
        assert_eq!(tray.tool_tip().title, "Penguin Mail: No unread mail");
        assert_eq!(tray.tool_tip().description, "");
        assert_eq!(tray.icon_accessible_desc(), "Penguin Mail: No unread mail");
    }

    /// Past 999 the label stops growing; the tooltip keeps the number.
    #[test]
    fn past_999_the_label_says_999_plus() {
        use ksni::Tray;
        let tray = tray(4321);
        assert_eq!(tray.x_ayatana_label(), "999+");
        assert_eq!(tray.tool_tip().title, "Penguin Mail: 4321 unread messages");
    }

    /// All three glyphs ship with the app, under the names the tray sends.
    #[test]
    fn the_tray_glyphs_are_in_the_icon_folder() {
        let apps =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/icons/scalable/apps");
        for name in [
            "io.github.c9dev.PenguinMail-symbolic",
            "io.github.c9dev.PenguinMail-unread-symbolic",
            "io.github.c9dev.PenguinMail-unread",
        ] {
            assert!(apps.join(format!("{name}.svg")).is_file(), "{name}");
        }
    }

    /// An install's icons sit in share/icons beside the bin folder that
    /// holds the program. A snap's are not on the host's search path, so
    /// the tray names the folder; a build run from target/ has none.
    #[test]
    fn the_icon_folder_is_found_beside_the_program() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("bin/penguin-mail");
        assert_eq!(super::icons_beside(&exe), None);
        let glyph = root
            .path()
            .join("share/icons/hicolor/symbolic/apps")
            .join(format!("{}-unread-symbolic.svg", crate::APP_ID));
        std::fs::create_dir_all(glyph.parent().unwrap()).unwrap();
        std::fs::write(&glyph, "<svg/>").unwrap();
        assert_eq!(
            super::icons_beside(&exe),
            Some(root.path().join("share/icons"))
        );
    }

    #[test]
    fn the_tray_counts_unread_mail_in_a_sentence() {
        assert_eq!(summary(0), "No unread mail");
        assert_eq!(summary(1), "1 unread message");
        assert_eq!(summary(7), "7 unread messages");
    }

    #[test]
    fn an_account_line_says_what_its_number_counts() {
        assert_eq!(account_line("ann@example.com", 0), "ann@example.com");
        assert_eq!(
            account_line("ann@example.com", 1),
            "ann@example.com, 1 unread message"
        );
        assert_eq!(
            account_line("ann@example.com", 3),
            "ann@example.com, 3 unread messages"
        );
    }

    fn unread(id: AccountId, email: &str, unread: i64) -> AccountUnread {
        AccountUnread {
            id,
            email: email.into(),
            unread,
        }
    }

    #[test]
    fn the_menu_lists_only_accounts_with_unread_mail_in_their_order() {
        let lines = account_lines(&[
            unread(1, "a@example.com", 0),
            unread(2, "b@example.com", 4),
            unread(3, "c@example.com", 0),
            unread(4, "d@example.com", 1),
        ]);
        assert_eq!(
            lines,
            [
                AccountLine::Unread {
                    id: 2,
                    label: "b@example.com, 4 unread messages".into()
                },
                AccountLine::Unread {
                    id: 4,
                    label: "d@example.com, 1 unread message".into()
                },
            ]
        );
    }

    #[test]
    fn the_menu_says_so_when_nothing_is_unread() {
        let lines = account_lines(&[unread(1, "a@example.com", 0), unread(2, "b@example.com", 0)]);
        assert_eq!(lines, [AccountLine::NoUnread("No unread mail".into())]);
    }

    #[test]
    fn the_menu_lists_nothing_without_accounts() {
        assert_eq!(account_lines(&[]), []);
    }

    #[test]
    fn choosing_an_account_line_asks_to_open_that_inbox() {
        use ksni::Tray;
        use ksni::menu::MenuItem;
        let (commands, received) = async_channel::unbounded();
        let mut tray = super::MailTray {
            unread: 4,
            accounts: vec![unread(1, "a@example.com", 0), unread(2, "b@example.com", 4)],
            commands,
            can_update: false,
            update: None,
            icon_theme_path: String::new(),
        };
        let menu = tray.menu();
        let MenuItem::Standard(line) = &menu[0] else {
            panic!("the first item is an account line");
        };
        assert_eq!(line.label, "b@example.com, 4 unread messages");
        (line.activate)(&mut tray);
        assert!(matches!(
            received.try_recv(),
            Ok(super::TrayCommand::OpenInbox(2))
        ));
        assert!(matches!(menu[1], MenuItem::Separator));
    }
}
