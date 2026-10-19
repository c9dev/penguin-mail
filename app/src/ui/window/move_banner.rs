//! The banner at the top of the mail list that asks Gmail accounts to
//! choose a new way to sign in.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use mailrs_domain::AccountId;

use crate::move_banner::MoveBanner;

/// What a press on the banner's button asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Press {
    pub accounts: Vec<AccountId>,
    pub goa_now: bool,
}

/// Lets every label under `widget` wrap to as many lines as its text needs.
fn show_whole_title(widget: &gtk::Widget) {
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        label.set_ellipsize(gtk::pango::EllipsizeMode::None);
        label.set_lines(-1);
    }
    let mut child = widget.first_child();
    while let Some(w) = child {
        show_whole_title(&w);
        child = w.next_sibling();
    }
}

/// One banner for every account that waits. The widget stays the same as
/// accounts join and leave; only its words and its button follow.
pub struct Bar {
    banner: adw::Banner,
    shown: Rc<RefCell<Option<MoveBanner>>>,
}

impl Bar {
    pub fn new() -> Bar {
        // The title holds an address, which may carry an "&".
        // The button is the banner's one way forward, so it takes the
        // accent, as the mockup draws it.
        let banner = adw::Banner::builder()
            .use_markup(false)
            .button_style(adw::BannerButtonStyle::Suggested)
            .build();
        // The paused words say that nothing was lost; the banner's own
        // label cuts a long title short, which would hide them.
        show_whole_title(banner.upcast_ref());
        Bar {
            banner,
            shown: Rc::default(),
        }
    }

    /// Runs `on_press` when the button is pressed. The window connects it
    /// once it exists, since the handler holds a weak reference to it.
    pub fn connect_press(&self, on_press: impl Fn(Press) + 'static) {
        let current = Rc::clone(&self.shown);
        self.banner.connect_button_clicked(move |_| {
            // The borrow ends before the handler runs: it may show again.
            let press = current.borrow().as_ref().map(|b| Press {
                accounts: b.accounts.clone(),
                goa_now: b.goa_now,
            });
            if let Some(press) = press {
                on_press(press);
            }
        });
    }

    pub fn widget(&self) -> &adw::Banner {
        &self.banner
    }

    /// Shows `wanted`, or hides the banner when nothing waits.
    pub fn show(&self, wanted: Option<MoveBanner>) {
        if let Some(wanted) = &wanted {
            self.banner.set_title(&wanted.title);
            self.banner.set_button_label(Some(&wanted.button));
            if wanted.paused {
                self.banner.add_css_class("move-banner-paused");
            } else {
                self.banner.remove_css_class("move-banner-paused");
            }
        }
        self.banner.set_revealed(wanted.is_some());
        self.shown.replace(wanted);
    }

    /// Turns the button off while a move runs.
    pub fn set_busy(&self, busy: bool) {
        if let Some(button) = self.button() {
            button.set_sensitive(!busy);
        }
    }

    /// The banner's own button: libadwaita gives no way to switch it off.
    fn button(&self) -> Option<gtk::Button> {
        fn find(widget: &gtk::Widget) -> Option<gtk::Button> {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                return Some(button.clone());
            }
            let mut child = widget.first_child();
            while let Some(w) = child {
                if let Some(found) = find(&w) {
                    return Some(found);
                }
                child = w.next_sibling();
            }
            None
        }
        find(self.banner.upcast_ref())
    }
}

#[cfg(test)]
pub(crate) mod checks {
    use super::*;
    use crate::move_banner::{Waiting, banner, grant_banner_wanted, sign_in_banner_wanted};
    use mailrs_domain::{Account, AccountState, Provider};

    pub fn run() {
        the_banner_keeps_one_widget_as_accounts_join();
        the_banner_has_no_close_button();
        a_press_carries_the_accounts_in_order();
        a_busy_banner_has_a_dead_button();
        a_move_account_gets_no_sign_in_or_grant_banner();
        a_paused_banner_is_tinted_and_its_button_filled();
    }

    fn a_paused_banner_is_tinted_and_its_button_filled() {
        let bar = Bar::new();
        assert_eq!(
            bar.widget().button_style(),
            adw::BannerButtonStyle::Suggested
        );
        bar.show(banner(
            &[waiting(1, "a@gmail.com", AccountState::MailPaused)],
            none_held,
        ));
        assert!(bar.widget().has_css_class("move-banner-paused"));
        bar.show(banner(
            &[waiting(1, "a@gmail.com", AccountState::Ok)],
            none_held,
        ));
        assert!(!bar.widget().has_css_class("move-banner-paused"));
    }

    fn waiting(id: i64, email: &str, state: AccountState) -> Waiting {
        Waiting {
            account: Account {
                id,
                email: email.into(),
                state,
                provider: Provider::Gmail,
                provider_name: None,
            },
            paused: state == AccountState::MailPaused,
        }
    }

    fn none_held(_: &str) -> bool {
        false
    }

    fn the_banner_keeps_one_widget_as_accounts_join() {
        let bar = Bar::new();
        let before = bar.widget().clone();
        let one = [waiting(1, "a@gmail.com", AccountState::Ok)];
        bar.show(banner(&one, none_held));
        assert_eq!(
            bar.widget().title(),
            "Choose how a@gmail.com signs in to keep your mail arriving."
        );
        assert!(bar.widget().is_revealed());
        let two = [
            waiting(1, "a@gmail.com", AccountState::Ok),
            waiting(2, "b&c@gmail.com", AccountState::Ok),
        ];
        bar.show(banner(&two, none_held));
        assert_eq!(bar.widget(), &before);
        assert_eq!(
            bar.widget().title(),
            "2 Gmail accounts need a new way to sign in"
        );
        bar.show(None);
        assert!(!bar.widget().is_revealed());
        // An address with an ampersand is shown as typed, not parsed.
        let amp = [waiting(2, "b&c@gmail.com", AccountState::Ok)];
        bar.show(banner(&amp, none_held));
        assert!(bar.widget().title().contains("b&c@gmail.com"));
        assert!(!bar.widget().uses_markup());
    }

    fn names_in(widget: &gtk::Widget, found: &mut Vec<String>) {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            found.push(button.label().map(|l| l.to_string()).unwrap_or_default());
        }
        let mut child = widget.first_child();
        while let Some(w) = child {
            names_in(&w, found);
            child = w.next_sibling();
        }
    }

    fn the_banner_has_no_close_button() {
        let bar = Bar::new();
        let two = [
            waiting(1, "a@gmail.com", AccountState::Ok),
            waiting(2, "b@gmail.com", AccountState::MailPaused),
        ];
        bar.show(banner(&two, none_held));
        let mut labels = Vec::new();
        names_in(bar.widget().upcast_ref(), &mut labels);
        assert_eq!(labels, ["Choose…"]);
    }

    fn a_press_carries_the_accounts_in_order() {
        let pressed: Rc<RefCell<Vec<Press>>> = Rc::default();
        let sink = Rc::clone(&pressed);
        let bar = Bar::new();
        bar.connect_press(move |press| sink.borrow_mut().push(press));
        let all = [
            waiting(3, "c@gmail.com", AccountState::Ok),
            waiting(1, "a@gmail.com", AccountState::Ok),
        ];
        bar.show(banner(&all, none_held));
        bar.widget().emit_by_name::<()>("button-clicked", &[]);
        assert_eq!(
            *pressed.borrow(),
            [Press {
                accounts: vec![3, 1],
                goa_now: false
            }]
        );
        // A fake GNOME Online Accounts that holds the address: the press
        // asks for the one-click move. The GOA plan wires what it does.
        let one = [waiting(5, "dana@gmail.com", AccountState::Ok)];
        bar.show(banner(&one, |email| email == "dana@gmail.com"));
        assert_eq!(
            bar.widget().button_label().as_deref(),
            Some("Use GNOME Online Accounts")
        );
        bar.widget().emit_by_name::<()>("button-clicked", &[]);
        assert_eq!(
            pressed.borrow().last(),
            Some(&Press {
                accounts: vec![5],
                goa_now: true
            })
        );
    }

    fn a_busy_banner_has_a_dead_button() {
        let bar = Bar::new();
        bar.set_busy(true);
        assert!(!bar.button().expect("a button").is_sensitive());
        bar.set_busy(false);
        assert!(bar.button().expect("a button").is_sensitive());
    }

    fn a_move_account_gets_no_sign_in_or_grant_banner() {
        let again = waiting(7, "g@gmail.com", AccountState::NeedsReauth);
        let ids = [7];
        assert!(!sign_in_banner_wanted(&again.account, &ids));
        assert!(!grant_banner_wanted(&ids, again.account.id));
        let other = waiting(8, "h@gmail.com", AccountState::NeedsReauth);
        assert!(sign_in_banner_wanted(&other.account, &ids));
        assert!(grant_banner_wanted(&ids, other.account.id));
    }
}
