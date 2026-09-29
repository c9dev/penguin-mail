//! The StatusNotifierItem shown by Ubuntu's AppIndicator extension.

use crate::APP_ID;
use mailrs_domain::AccountId;
use mailrs_domain::translate::{fill, fill_plural, gettext};

/// How long the tray waits after a change before it counts again. Sync
/// reports each batch of changed threads on its own, and a new account's
/// first sync sends hundreds; one count covers everything in the window.
pub const RECOUNT_AFTER: std::time::Duration = std::time::Duration::from_millis(500);

/// Lets a burst of requests through as one. The first request in a quiet
/// spell claims the next run, and the rest ride along with it until it
/// starts.
#[derive(Default)]
pub struct Burst(std::cell::Cell<bool>);

impl Burst {
    /// Whether this request should schedule the run.
    pub fn claim(&self) -> bool {
        !self.0.replace(true)
    }

    /// The run is starting, so a request from here on needs one of its own.
    pub fn start(&self) {
        self.0.set(false);
    }
}

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

    fn icon_name(&self) -> String {
        if self.unread > 0 {
            "mail-unread-symbolic"
        } else {
            "mail-read-symbolic"
        }
        .into()
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
    use super::{AccountId, AccountLine, AccountUnread, Burst, account_line, account_lines, summary};

    #[test]
    fn a_burst_of_changes_counts_once() {
        let burst = Burst::default();
        assert!(burst.claim());
        assert!((0..300).all(|_| !burst.claim()));
        burst.start();
        assert!(
            burst.claim(),
            "a change after the count starts counts again"
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
