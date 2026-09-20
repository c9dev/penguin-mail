//! The StatusNotifierItem shown by Ubuntu's AppIndicator extension.

use crate::APP_ID;
use mailrs_domain::translate::{fill, fill_plural, gettext};

pub enum TrayCommand {
    Toggle,
    Open,
    Compose,
    Check,
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

pub struct MailTray {
    pub unread: i64,
    /// Each account's address and unread INBOX count.
    pub accounts: Vec<(String, i64)>,
    pub commands: async_channel::Sender<TrayCommand>,
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
                .filter(|(_, n)| *n > 0)
                .map(|(email, n)| format!("{email}: {n}"))
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
        let mut items: Vec<MenuItem<Self>> = self
            .accounts
            .iter()
            .map(|(email, n)| {
                StandardItem {
                    label: account_line(email, *n),
                    enabled: false,
                    ..Default::default()
                }
                .into()
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
        items.push(MenuItem::Separator);
        items.push(item(&gettext("Quit"), || TrayCommand::Quit));
        items
    }
}

#[cfg(test)]
mod tests {
    use super::{account_line, summary};

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
}
