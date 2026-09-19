//! The StatusNotifierItem shown by Ubuntu's AppIndicator extension.

use crate::APP_ID;

pub enum TrayCommand {
    Toggle,
    Open,
    Compose,
    Check,
    Quit,
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
        match self.unread {
            0 => "No unread mail".into(),
            1 => "1 unread message".into(),
            n => format!("{n} unread messages"),
        }
    }
}

impl ksni::Tray for MailTray {
    fn id(&self) -> String {
        APP_ID.into()
    }

    fn title(&self) -> String {
        format!("mailrs: {}", self.summary())
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
            title: format!("mailrs: {}", self.summary()),
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
                    label: if *n > 0 {
                        format!("{email}   {n}")
                    } else {
                        email.clone()
                    },
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
        items.push(item("Open mailrs", || TrayCommand::Open));
        items.push(item("New Message", || TrayCommand::Compose));
        items.push(item("Check for Mail", || TrayCommand::Check));
        items.push(MenuItem::Separator);
        items.push(item("Quit", || TrayCommand::Quit));
        items
    }
}
