//! A loose message in a window of its own: the window Ctrl+O opens for a
//! conversation, with the mailbox actions left out, since an `.eml` file
//! or a message attached to another is in no mailbox for them to act on.
//! What stays is reading, Reply and Forward, and the file itself: View
//! Source and Export read the bytes the window holds.

use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::translate::{fill, gettext};

use super::MainWindow;
use super::detached::Detached;
use crate::open_thread::{Loose, OpenThread, Origin};
use crate::ui::conversation::ConversationView;

/// The `win.*` actions a loose window installs: everything in
/// [`VIEW_ACTIONS`](super::shortcuts::VIEW_ACTIONS) and
/// [`MESSAGE_ACTIONS`](super::shortcuts::MESSAGE_ACTIONS) that reads the
/// message or answers it. The rest are not installed, so their keys do
/// nothing here rather than act on a mailbox.
const LOOSE_ACTIONS: &[&str] = &[
    "reply",
    "reply-all",
    "forward",
    "find",
    "print",
    "view-source",
    "export",
    "always-load-images",
    "message-reply",
    "message-reply-all",
    "message-forward",
    "message-export",
    "message-copy-address",
];

/// The actions among [`LOOSE_ACTIONS`] that send something out, turned
/// off when the message was opened with no account to send from. The
/// toolbar hides its buttons for the same reason; this covers the menu
/// items and the keys.
const ANSWERING: &[&str] = &[
    "reply",
    "reply-all",
    "forward",
    "message-reply",
    "message-reply-all",
    "message-forward",
];

impl MainWindow {
    /// Opens `loose` in a window of its own, the way `open_in_window`
    /// opens a conversation.
    pub(crate) fn open_loose(self: &Rc<Self>, loose: Loose) {
        let holder: Rc<std::cell::RefCell<Weak<ConversationView>>> = Rc::default();
        let (win, slot) = (Rc::downgrade(self), Rc::clone(&holder));
        let view = ConversationView::new(move |action| {
            let (Some(win), Some(view)) = (win.upgrade(), slot.borrow().upgrade()) else {
                return;
            };
            win.act(&view, action);
        });
        *holder.borrow_mut() = Rc::downgrade(&view);
        view.set_loose();
        if let Some(filter) = self.app.upgrade().and_then(|app| app.filter()) {
            view.set_filter(filter);
        }
        view.set_zoom(self.settings_with(|s| s.text_size.zoom()));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&view.page));
        let window = adw::Window::builder()
            .title(gettext("Message"))
            .default_width(820)
            .default_height(760)
            .content(&toasts)
            .build();
        super::track_dark_class(&window);
        let actions = self.install_loose_actions(&window, &view, loose.has_account);
        // Listed with the other separate windows, so `views` and
        // `mailbox_of` answer for this one too. The mailbox is a stand-in:
        // a loose message was opened from none, and no action that reads
        // it is installed here.
        self.detached.borrow_mut().push(Detached {
            view: Rc::downgrade(&view),
            mailbox: self.shown(),
            account_id: loose.account_id,
            actions,
            toasts,
        });
        // Counted among the open windows: with no account at all, closing
        // the main window would otherwise quit the process under this one.
        if let Some(app) = self.app.upgrade() {
            app.window_opened();
        }
        let (keep, app, host) = (Rc::clone(&view), self.app.clone(), Rc::downgrade(self));
        window.connect_destroy(move |_| {
            keep.stop_rendering();
            if let Some(app) = app.upgrade() {
                app.separate_window_closed(host.upgrade(), &keep);
            }
        });
        window.present();
        let run = self.thread_run(&view);
        let titled = window.downgrade();
        glib::spawn_future_local(async move {
            run.open_loose(loose).await;
            if let Some((title, subtitle)) = view.read(loose_title) {
                if let Some(window) = titled.upgrade() {
                    window.set_title(Some(&title));
                }
                view.set_origin_title(&title, &subtitle);
            }
        });
    }

    fn install_loose_actions(
        self: &Rc<Self>,
        window: &adw::Window,
        view: &Rc<ConversationView>,
        answerable: bool,
    ) -> gio::SimpleActionGroup {
        let group = gio::SimpleActionGroup::new();
        self.install_actions_named(&group, view, Some(LOOSE_ACTIONS));
        if !answerable {
            for name in ANSWERING {
                if let Some(action) = group
                    .lookup_action(name)
                    .and_downcast::<gio::SimpleAction>()
                {
                    action.set_enabled(false);
                }
            }
        }
        window.insert_action_group("win", Some(&group));
        window.add_controller(super::shortcuts::conversation_chords());
        group
    }
}

/// Which account a message opened from `open` acts as, and whether that
/// account exists: a loose message keeps what it was opened with, so a
/// message nested in a file opened with no account has none either.
pub(super) fn account_of(open: &OpenThread) -> (mailrs_domain::AccountId, bool) {
    (
        open.account_id,
        open.loose.as_ref().is_none_or(|loose| loose.has_account),
    )
}

/// The window's title and subtitle: the subject, or "Message" when it
/// has none, over where the message came from.
fn loose_title(open: &OpenThread) -> (String, String) {
    let title = match open.subject.trim() {
        "" => gettext("Message"),
        subject => subject.to_string(),
    };
    let subtitle = match open.loose.as_ref().map(|l| &l.origin) {
        Some(Origin::File(path)) => path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        Some(Origin::Attachment { subject }) => {
            fill(&gettext("Attached to {subject}"), &[("subject", subject)])
        }
        None => String::new(),
    };
    (title, subtitle)
}

/// A loose message made of `raw`, acting as `account_id` when
/// `has_account`, out of the message `subject` it was attached to.
pub(super) fn attached(
    raw: Vec<u8>,
    account_id: mailrs_domain::AccountId,
    has_account: bool,
    subject: String,
) -> Loose {
    Loose {
        raw: Arc::new(raw),
        account_id,
        has_account,
        origin: Origin::Attachment { subject },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::open_thread::loose::{fixture_raw, parse};
    use crate::open_thread::{Loose, OpenThread, Origin};

    #[test]
    fn the_title_is_the_subject_over_where_the_message_came_from() {
        let mut thread = OpenThread::loose(
            &Loose {
                raw: Arc::new(fixture_raw()),
                account_id: 1,
                has_account: true,
                origin: Origin::File("/home/ann/kites.eml".into()),
            },
            parse(&fixture_raw()),
            vec![],
        );
        assert_eq!(
            loose_title(&thread),
            ("Kite plans".to_string(), "kites.eml".to_string())
        );
        thread.loose.as_mut().unwrap().origin = Origin::Attachment {
            subject: "Fwd: plans".into(),
        };
        assert_eq!(loose_title(&thread).1, "Attached to Fwd: plans");
        thread.subject.clear();
        assert_eq!(loose_title(&thread).0, "Message");
    }

    #[test]
    fn a_nested_message_acts_as_the_account_of_the_message_around_it() {
        let stored = OpenThread::new(
            &mailrs_domain::Target::thread(3, "t1"),
            String::new(),
            vec![],
            Default::default(),
            vec![],
        );
        assert_eq!(account_of(&stored), (3, true));
        let no_account = OpenThread::loose(
            &Loose {
                raw: Arc::new(fixture_raw()),
                account_id: 0,
                has_account: false,
                origin: Origin::File("/tmp/kites.eml".into()),
            },
            parse(&fixture_raw()),
            vec![],
        );
        assert_eq!(account_of(&no_account), (0, false));
    }

    #[test]
    fn every_loose_action_is_one_the_action_tables_know() {
        use crate::ui::window::shortcuts::{MESSAGE_ACTIONS, VIEW_ACTIONS};
        for name in LOOSE_ACTIONS {
            assert!(
                VIEW_ACTIONS.iter().any(|(n, _)| n == name)
                    || MESSAGE_ACTIONS.iter().any(|(n, _)| n == name),
                "{name} is not an action"
            );
        }
        for gone in [
            "archive",
            "trash",
            "junk",
            "toggle-star",
            "toggle-read",
            "unsubscribe",
            "block-sender",
            "message-archive",
            "message-trash",
            "message-flag",
            "message-label",
        ] {
            assert!(!LOOSE_ACTIONS.contains(&gone), "{gone} acts on a mailbox");
        }
    }
}
