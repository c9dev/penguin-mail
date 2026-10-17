//! GIO's Cocoa backend uses NSUserNotification, which exposes only its
//! first button and activates the app. UserNotifications keeps mail
//! actions in the background and lets macOS draw its native action menu.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use block2::{DynBlock, RcBlock};
use gtk::prelude::*;
use gtk::{gio, glib};
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{
    NSArray, NSBundle, NSDictionary, NSError, NSObject, NSObjectProtocol, NSSet, NSString,
    ns_string,
};
use objc2_user_notifications::*;

use super::{Action, Actions};
use crate::notify;

pub(super) struct Desktop {
    native: Option<Native>,
}

struct Native {
    center: Retained<UNUserNotificationCenter>,
    // The notification center holds its delegate weakly.
    _delegate: Retained<Delegate>,
    posting: async_channel::Sender<Post>,
    pending: RefCell<Pending>,
}

struct Post {
    request: Retained<UNNotificationRequest>,
    wanted: Rc<Cell<bool>>,
}

impl Desktop {
    pub fn new(app: &gio::Application, demo: bool) -> Self {
        // Calling currentNotificationCenter outside an app bundle raises
        // an Objective-C exception. The demo must not ask for permission.
        if demo || NSBundle::mainBundle().bundleIdentifier().is_none() {
            return Self { native: None };
        }
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let (clicked, clicks) = async_channel::unbounded::<(Actions, String)>();
        let allocated = Delegate::alloc().set_ivars(clicked);
        let delegate: Retained<Delegate> = unsafe { msg_send![super(allocated), init] };
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        center.setNotificationCategories(&NSSet::from_retained_slice(&categories()));
        let weak = app.downgrade();
        glib::spawn_future_local(async move {
            while let Ok((actions, key)) = clicks.recv().await {
                let Some(app) = weak.upgrade() else { break };
                match actions.chosen(&key) {
                    Some(Some(action)) => {
                        let (name, target) = action.destination(&actions.target);
                        // activate_action does not activate the application.
                        // Open and Reply present their own window as usual.
                        app.activate_action(name, Some(&target.to_variant()));
                    }
                    Some(None) => app.activate(),
                    None => {}
                }
            }
        });
        let (posting, posts) = async_channel::unbounded::<Post>();
        let sending = center.clone();
        glib::spawn_future_local(async move {
            while let Ok(post) = posts.recv().await {
                if !post.wanted.get() || !authorized(&sending).await || !post.wanted.get() {
                    continue;
                }
                let (done, answer) = async_channel::bounded(1);
                let completion = RcBlock::new(move |error: *mut NSError| {
                    report(error);
                    let _ = done.try_send(());
                });
                sending
                    .addNotificationRequest_withCompletionHandler(&post.request, Some(&completion));
                let _ = answer.recv().await;
                // Withdrawal may race permission or delivery. Serialize
                // posting so a replaced reminder cannot be removed here.
                if !post.wanted.get() {
                    withdraw(&sending, &post.request.identifier().to_string());
                }
            }
        });
        Self {
            native: Some(Native {
                center,
                _delegate: delegate,
                posting,
                pending: RefCell::default(),
            }),
        }
    }

    pub fn post(&self, id: &str, title: &str, body: &str, actions: Actions) {
        let Some(native) = &self.native else { return };
        let category_id =
            serde_json::to_string(&actions.buttons).expect("notification buttons serialize");
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        content.setCategoryIdentifier(&NSString::from_str(&category_id));
        let payload = NSString::from_str(
            &serde_json::to_string(&actions).expect("notification actions serialize"),
        );
        let info =
            NSDictionary::<NSString, NSString>::from_slices(&[ns_string!("actions")], &[&payload]);
        // SAFETY: The dictionary contains only strings, which are valid
        // property-list values and stay owned by the copied content.
        unsafe { content.setUserInfo(info.cast_unchecked()) };
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(id),
            &content,
            None,
        );
        let wanted = native.pending.borrow_mut().add(id);
        let _ = native.posting.try_send(Post { request, wanted });
    }

    pub fn withdraw(&self, id: &str) {
        let Some(native) = &self.native else { return };
        native.pending.borrow_mut().cancel(id);
        withdraw(&native.center, id);
    }
}

/// Flags live only until delivery completes, but withdrawal can cancel
/// one while permission or the notification service is answering.
#[derive(Default)]
struct Pending(HashMap<String, Weak<Cell<bool>>>);

impl Pending {
    fn add(&mut self, id: &str) -> Rc<Cell<bool>> {
        self.0.retain(|_, flag| flag.strong_count() > 0);
        self.cancel(id);
        let wanted = Rc::new(Cell::new(true));
        self.0.insert(id.to_string(), Rc::downgrade(&wanted));
        wanted
    }

    fn cancel(&mut self, id: &str) {
        if let Some(flag) = self.0.remove(id).and_then(|flag| flag.upgrade()) {
            flag.set(false);
        }
    }
}

fn withdraw(center: &UNUserNotificationCenter, id: &str) {
    let ids = NSArray::from_retained_slice(&[NSString::from_str(id)]);
    center.removePendingNotificationRequestsWithIdentifiers(&ids);
    center.removeDeliveredNotificationsWithIdentifiers(&ids);
}

async fn authorized(center: &UNUserNotificationCenter) -> bool {
    let (done, answer) = async_channel::bounded(1);
    let completion = RcBlock::new(move |granted: Bool, error: *mut NSError| {
        report(error);
        let _ = done.try_send(granted.as_bool());
    });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert,
        &completion,
    );
    answer.recv().await.unwrap_or(false)
}

fn report(error: *mut NSError) {
    // SAFETY: Completion handlers supply a live NSError or nil.
    if let Some(error) = unsafe { error.as_ref() } {
        tracing::warn!(error = %error, "macOS notification failed");
    }
}

fn options(action: Action) -> UNNotificationActionOptions {
    match action {
        Action::Mail(notify::Button::Reply) | Action::OpenMail | Action::OpenEvent => {
            UNNotificationActionOptions::Foreground
        }
        _ => UNNotificationActionOptions::empty(),
    }
}

// Register every preference combination at startup, so notifications
// already in the notification center keep their menus after a restart.
fn categories() -> Vec<Retained<UNNotificationCategory>> {
    let mut layouts: Vec<Vec<Action>> = (0..1 << notify::Button::ALL.len())
        .map(|mask| {
            notify::Button::ALL
                .into_iter()
                .enumerate()
                .filter(|(index, _)| mask & (1 << index) != 0)
                .map(|(_, button)| Action::Mail(button))
                .collect()
        })
        .collect();
    layouts.extend([vec![Action::Join, Action::Snooze], vec![Action::Snooze]]);
    layouts
        .iter()
        .map(|buttons| {
            category(
                &serde_json::to_string(buttons).expect("notification buttons serialize"),
                buttons,
            )
        })
        .collect()
}

fn category(id: &str, buttons: &[Action]) -> Retained<UNNotificationCategory> {
    let buttons: Vec<_> = buttons
        .iter()
        .enumerate()
        .map(|(index, &button)| {
            UNNotificationAction::actionWithIdentifier_title_options(
                &NSString::from_str(&index.to_string()),
                &NSString::from_str(&button.label()),
                options(button),
            )
        })
        .collect();
    UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
        &NSString::from_str(id),
        &NSArray::from_retained_slice(&buttons),
        &NSArray::new(),
        UNNotificationCategoryOptions::empty(),
    )
}

impl Actions {
    fn chosen(&self, key: &str) -> Option<Option<Action>> {
        if key == "default" {
            Some(self.open)
        } else {
            key.parse::<usize>()
                .ok()
                .and_then(|index| self.buttons.get(index).copied())
                .map(Some)
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "PenguinMailNotificationDelegate"]
    #[ivars = async_channel::Sender<(Actions, String)>]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn received(&self, _center: &UNUserNotificationCenter, response: &UNNotificationResponse, done: &DynBlock<dyn Fn()>) {
            let key = response.actionIdentifier();
            if *key != *unsafe { UNNotificationDismissActionIdentifier } {
                let info = response.notification().request().content().userInfo();
                if let Some(value) = info.objectForKey(ns_string!("actions"))
                    .and_then(|value| value.downcast::<NSString>().ok())
                    .and_then(|value| serde_json::from_str::<Actions>(&value.to_string()).ok()) {
                    let key = if *key == *unsafe { UNNotificationDefaultActionIdentifier } {
                        "default".to_string()
                    } else { key.to_string() };
                    let _ = self.ivars().try_send((value, key));
                }
            }
            done.call(());
        }

        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn present(&self, _center: &UNUserNotificationCenter, _notice: &UNNotification, done: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>) {
            // The app already suppresses mail while its window is active;
            // calendar reminders still need to appear in the foreground.
            done.call((UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List,));
        }
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn withdrawal_cancels_delivery_and_replacement_does_not_reuse_its_flag() {
        let mut pending = Pending::default();
        let old = pending.add("reminder");
        let other = pending.add("mail");
        let replacement = pending.add("reminder");
        assert!(!old.get());
        assert!(replacement.get());
        assert!(other.get());
        pending.cancel("reminder");
        assert!(!replacement.get());
        assert!(other.get());
        drop(other);
        let _next = pending.add("next");
        assert_eq!(pending.0.len(), 1);
    }

    #[test]
    fn every_layout_registers_before_notifications_from_a_previous_run_are_clicked() {
        let all = categories();
        let ids: std::collections::HashSet<_> = all
            .iter()
            .map(|category| category.identifier().to_string())
            .collect();
        assert_eq!(ids.len(), 18);
        for category in all {
            let buttons: Vec<Action> =
                serde_json::from_str(&category.identifier().to_string()).unwrap();
            assert_eq!(buttons.len(), category.actions().len());
        }
    }

    #[test]
    fn every_mail_button_is_in_the_native_menu_and_only_reply_takes_focus() {
        let buttons: Vec<_> = notify::Button::ALL.into_iter().map(Action::Mail).collect();
        let native = category("test", &buttons).actions();
        assert_eq!(native.len(), 4);
        for (index, button) in native.iter().enumerate() {
            assert_eq!(button.identifier().to_string(), index.to_string());
            assert!(!button.title().is_empty());
            assert_eq!(
                button
                    .options()
                    .contains(UNNotificationActionOptions::Foreground),
                buttons[index] == Action::Mail(notify::Button::Reply)
            );
        }
        assert!(!options(Action::Snooze).contains(UNNotificationActionOptions::Foreground));
        assert!(!options(Action::Join).contains(UNNotificationActionOptions::Foreground));
    }

    #[test]
    fn clicks_select_only_buttons_carried_by_the_notification() {
        let actions = Actions {
            open: Some(Action::OpenMail),
            target: "issued-token".into(),
            buttons: vec![
                Action::Mail(notify::Button::Delete),
                Action::Mail(notify::Button::Reply),
            ],
        };
        let read: Actions =
            serde_json::from_str(&serde_json::to_string(&actions).unwrap()).unwrap();
        assert_eq!(
            read.chosen("0"),
            Some(Some(Action::Mail(notify::Button::Delete)))
        );
        assert_eq!(
            read.chosen("1"),
            Some(Some(Action::Mail(notify::Button::Reply)))
        );
        assert_eq!(read.chosen("default"), Some(Some(Action::OpenMail)));
        for bad in ["2", "-1", "dismiss", "quit", "9999999999999999999999"] {
            assert_eq!(read.chosen(bad), None);
        }
        let summary = Actions {
            open: None,
            target: "summary".into(),
            buttons: vec![],
        };
        assert_eq!(summary.chosen("default"), Some(None));
        assert_eq!(summary.chosen("0"), None);
    }
}
