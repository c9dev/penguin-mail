//! The band above the message list for an account whose server is
//! nearly full. It shows in All Inboxes and in that account's own
//! mailboxes, nowhere else. Review Storage opens Preferences on the Accounts
//! page, where each account shows its space on the server, and takes the
//! band down until the account's level changes: one dismissed while
//! nearly full comes back once the account is full, and the other way
//! round. That dismissal is kept in the settings, so a restart does not
//! bring it back. The close button hides it until the next launch.

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;
use mailrs_domain::AccountId;
use mailrs_domain::translate::{fill, gettext};
use mailrs_sync::Storage;

use super::MainWindow;
use crate::settings::Change;
use crate::ui::{Mailbox, Standard};

/// What the banner says about `address`, or `None` while it has room.
pub(super) fn banner_line(address: &str, storage: Storage) -> Option<String> {
    if !storage.past_warning() {
        return None;
    }
    Some(if storage.full() {
        fill(
            &gettext("{address} is full and may refuse new mail"),
            &[("address", address)],
        )
    } else {
        fill(
            &gettext("{address} is {percent}% full"),
            &[
                ("address", address),
                ("percent", &storage.percent().to_string()),
            ],
        )
    })
}

/// Whether a banner the person took down stays down. `when_full` says
/// whether the account was full then. Any other level is new news: full
/// after nearly full, nearly full again after full, or room again.
pub(super) fn stays_dismissed(when_full: bool, storage: Storage) -> bool {
    storage.past_warning() && storage.full() == when_full
}

/// Whether the band for `account` shows over `mailbox`: All Inboxes and
/// the account's own mailboxes, but not a search, which is about what
/// was asked for.
pub(super) fn shows_in(mailbox: &Mailbox, account: AccountId) -> bool {
    match mailbox {
        Mailbox::Unified(Standard::Inbox) => true,
        Mailbox::Search { .. } => false,
        _ => mailbox.account() == Some(account),
    }
}

/// The band's words with a break allowed before the address's "@", for a
/// list too narrow for the whole address. Only for show: the label is not
/// selectable, so the hidden character never reaches a clipboard.
fn breakable(line: &str) -> String {
    line.replacen('@', "\u{200b}@", 1)
}

/// Whether the account was full when its band last changed, and whether
/// the person closed the band at that level. Closing lasts until the next
/// launch or the next level, as Review Storage's dismissal does: a band
/// closed while nearly full comes back once the account is full, and the
/// other way round.
#[derive(Default)]
pub(super) struct Level {
    full: Cell<bool>,
    closed: Cell<bool>,
}

impl Level {
    pub(super) fn set_full(&self, full: bool) {
        if self.full.replace(full) != full {
            self.closed.set(false);
        }
    }

    pub(super) fn close(&self) {
        self.closed.set(true);
    }

    pub(super) fn closed(&self) -> bool {
        self.closed.get()
    }
}

/// One account's band: a tint of the accent, its words, Review Storage
/// and a close button, as the Follow Up banner has.
pub(super) struct StorageBand {
    revealer: gtk::Revealer,
    title: gtk::Label,
    /// The account's level, and whether the band was closed at it.
    level: Level,
}

impl StorageBand {
    fn new(line: &str) -> (StorageBand, gtk::Button, gtk::Button) {
        // The title holds the address, which may carry an "&", so it is
        // plain text.
        let title = gtk::Label::builder()
            .label(breakable(line))
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .build();
        let review = gtk::Button::builder()
            .label(gettext("Review Storage"))
            .valign(gtk::Align::Center)
            .build();
        let close = gtk::Button::builder()
            .icon_name("penguin-mail-window-close-symbolic")
            .tooltip_text(gettext("Hide Until Next Launch"))
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        // The Follow Up banner's close has the tooltip's words for its
        // name, so this one says what it hides.
        crate::ui::name(&close, &gettext("Hide Storage Warning Until Next Launch"));
        let content = gtk::Box::builder()
            .spacing(10)
            .css_classes(["follow-up-banner", "storage-band"])
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        // The words and Review Storage share a line where the list is wide
        // enough, and Review Storage moves under the words where it is not,
        // so the address never breaks mid-word.
        let words = adw::WrapBox::builder()
            .child_spacing(10)
            .line_spacing(6)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        words.append(&title);
        words.append(&review);
        content.append(&gtk::Image::from_icon_name("penguin-mail-drive-harddisk-symbolic"));
        content.append(&words);
        content.append(&close);
        let revealer = gtk::Revealer::builder()
            .child(&content)
            .reveal_child(false)
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .build();
        let band = StorageBand {
            revealer,
            title,
            level: Level::default(),
        };
        (band, review, close)
    }
}

impl MainWindow {
    /// Puts up a band for each account past its warning that the person
    /// has not taken down, beside the Grant Access banners, and takes down
    /// the rest. A band already up keeps its widget and changes its words.
    pub(super) fn rebuild_storage_banners(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let storage = app.storage();
        let dismissed = app.settings().storage_dismissed;
        let mut lapsed = Vec::new();
        let mut wanted: Vec<(AccountId, String, bool, String)> = Vec::new();
        for account in app.accounts() {
            let Some(space) = storage.get(&account.id).copied() else {
                continue;
            };
            match dismissed.get(&account.email.to_lowercase()) {
                Some(&full) if stays_dismissed(full, space) => continue,
                Some(_) => lapsed.push(account.email.clone()),
                None => {}
            }
            if let Some(line) = banner_line(&account.email, space) {
                wanted.push((account.id, account.email.clone(), space.full(), line));
            }
        }
        for account in lapsed {
            app.change_settings(Change::StorageDismissed {
                account,
                full: None,
            });
        }
        let ids: HashSet<AccountId> = wanted.iter().map(|(id, _, _, _)| *id).collect();
        {
            let mut widgets = self.storage_banner_widgets.borrow_mut();
            widgets.retain(|id, band| {
                let keep = ids.contains(id);
                if !keep {
                    self.grant_banners.remove(&band.revealer);
                }
                keep
            });
            for (id, email, full, line) in wanted {
                if let Some(band) = widgets.get(&id) {
                    band.title.set_label(&breakable(&line));
                    band.level.set_full(full);
                    continue;
                }
                let (band, review, close) = StorageBand::new(&line);
                band.level.set_full(full);
                let weak = Rc::downgrade(self);
                // Runs after this rebuild returned and dropped its borrows, so
                // the borrows below meet no other (gtk-rs-refcell-reentrancy).
                review.connect_clicked(move |_| {
                    let Some(window) = weak.upgrade() else {
                        return;
                    };
                    let full = window
                        .storage_banner_widgets
                        .borrow()
                        .get(&id)
                        .is_some_and(|band| band.level.full.get());
                    if let Some(app) = window.app.upgrade() {
                        app.change_settings(Change::StorageDismissed {
                            account: email.clone(),
                            full: Some(full),
                        });
                    }
                    window.rebuild_storage_banners();
                    window.show_preferences_focus(Some(crate::ui::preferences::Focus::Storage(
                        &email,
                    )));
                });
                let weak = Rc::downgrade(self);
                close.connect_clicked(move |_| {
                    let Some(window) = weak.upgrade() else {
                        return;
                    };
                    if let Some(band) = window.storage_banner_widgets.borrow().get(&id) {
                        band.level.close();
                    }
                    window.place_storage_banners();
                });
                self.grant_banners.append(&band.revealer);
                widgets.insert(id, band);
            }
        }
        self.place_storage_banners();
    }

    /// Shows each band over the mailboxes it belongs to, and hides it
    /// everywhere else.
    pub(super) fn place_storage_banners(&self) {
        let mailbox = self.shown();
        for (id, band) in self.storage_banner_widgets.borrow().iter() {
            band.revealer
                .set_reveal_child(!band.level.closed() && shows_in(&mailbox, *id));
        }
    }
}

#[cfg(test)]
mod tests {
    use mailrs_sync::Storage;

    use super::{banner_line, shows_in, stays_dismissed};
    use crate::ui::{Mailbox, Standard};

    #[test]
    fn a_closed_band_comes_back_when_the_account_fills_up() {
        let level = super::Level::default();
        level.set_full(false);
        level.close();
        level.set_full(false);
        assert!(level.closed(), "the same level keeps it closed");
        level.set_full(true);
        assert!(!level.closed(), "full is new news");
    }

    #[test]
    fn the_band_may_break_before_the_at_and_nowhere_inside_the_address() {
        assert_eq!(
            super::breakable("dana@home.example is full and may refuse new mail"),
            "dana\u{200b}@home.example is full and may refuse new mail"
        );
    }

    #[test]
    fn the_band_shows_in_all_inboxes_and_the_accounts_own_mailboxes() {
        assert!(shows_in(&Mailbox::Unified(Standard::Inbox), 3));
        assert!(shows_in(
            &Mailbox::Standard {
                account_id: 3,
                which: Standard::Sent
            },
            3
        ));
        assert!(!shows_in(
            &Mailbox::Standard {
                account_id: 4,
                which: Standard::Inbox
            },
            3
        ));
    }

    #[test]
    fn the_band_stays_out_of_mailboxes_that_are_not_the_accounts() {
        assert!(!shows_in(&Mailbox::Outbox, 3));
        assert!(!shows_in(&Mailbox::Scheduled, 3));
        assert!(!shows_in(&Mailbox::Unified(Standard::Sent), 3));
        assert!(!shows_in(
            &Mailbox::Search {
                query: "kites".into(),
                account_id: Some(3)
            },
            3
        ));
    }

    fn used(used: u64) -> Storage {
        Storage::new(used, 100, None).unwrap()
    }

    #[test]
    fn the_banner_waits_for_the_warning() {
        assert_eq!(banner_line("dana@fastmail.example", used(50)), None);
    }

    #[test]
    fn the_banner_says_how_full_past_the_warning() {
        assert_eq!(
            banner_line("dana.reyes@work.example", used(95)).as_deref(),
            Some("dana.reyes@work.example is 95% full")
        );
    }

    #[test]
    fn the_banner_takes_the_servers_own_warning_mark() {
        let storage = Storage::new(85, 100, Some(80)).unwrap();
        assert_eq!(
            banner_line("dana@fastmail.example", storage).as_deref(),
            Some("dana@fastmail.example is 85% full")
        );
    }

    #[test]
    fn the_banner_says_a_full_account_may_refuse_mail() {
        assert_eq!(
            banner_line("dana@home.example", used(100)).as_deref(),
            Some("dana@home.example is full and may refuse new mail")
        );
    }

    #[test]
    fn a_banner_dismissed_while_nearly_full_comes_back_once_full() {
        assert!(stays_dismissed(false, used(97)));
        assert!(!stays_dismissed(false, used(100)));
    }

    #[test]
    fn a_banner_dismissed_while_full_stays_down() {
        assert!(stays_dismissed(true, used(100)));
    }

    #[test]
    fn a_banner_dismissed_while_full_comes_back_at_the_next_level() {
        assert!(
            !stays_dismissed(true, used(97)),
            "full to nearly full is a new level"
        );
        assert!(
            !stays_dismissed(false, used(50)),
            "room again ends the dismissal"
        );
    }
}
