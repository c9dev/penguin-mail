//! WinRT toasts on Windows: they draw the buttons, keep a click
//! working after the toast moves to Action Center, and withdraw a
//! delivered notification by tag. Toasts need an AppUserModelID, which
//! an unpackaged program gets through a shortcut in the Start Menu
//! Programs folder; `ensure_shortcut` keeps that file there.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gtk::prelude::*;
use gtk::{gio, glib};
use serde::{Deserialize, Serialize};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{
    ToastActivatedEventArgs, ToastDismissedEventArgs, ToastNotification, ToastNotificationManager,
    ToastNotifier,
};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    IPersistFile,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{
    FOLDERID_Programs, IShellLinkW, KF_FLAG_CREATE, SHGetKnownFolderPath,
    SetCurrentProcessExplicitAppUserModelID, ShellLink,
};
use windows::core::{HSTRING, Interface, PCWSTR, Ref};

use super::{Action, Actions};

pub(super) struct Desktop {
    native: Option<Native>,
}

struct Native {
    notifier: ToastNotifier,
    /// The toasts shown, by their tag, kept both for `withdraw` and so
    /// their click handlers stay alive. The event handlers must be
    /// `Send`, so the map is `Arc` even though all posting is local.
    shown: Arc<Mutex<HashMap<String, ToastNotification>>>,
}

/// A click's payload, packed into the toast's launch arguments.
#[derive(Serialize, Deserialize)]
struct Click {
    /// The app action's name, empty for "just open the app".
    a: String,
    /// The action's parameter, the same string the D-Bus path sends.
    t: String,
}

impl Desktop {
    pub fn new(app: &gio::Application, demo: bool) -> Self {
        if demo {
            return Self { native: None };
        }
        let (clicked, clicks) = async_channel::unbounded::<Click>();
        let _ = clicks_channel().set(clicked);
        let weak = app.downgrade();
        glib::spawn_future_local(async move {
            while let Ok(click) = clicks.recv().await {
                let Some(app) = weak.upgrade() else { break };
                if click.a.is_empty() {
                    app.activate();
                } else {
                    app.activate_action(&click.a, Some(&click.t.to_variant()));
                }
            }
        });
        let shown = Arc::new(Mutex::new(HashMap::new()));
        let notifier = unsafe { Self::start() }
            .inspect_err(|err| tracing::warn!(error = %err, "Windows toasts are not available"))
            .ok();
        Self {
            native: notifier.map(|notifier| Native { notifier, shown }),
        }
    }

    /// The process's id and its Start Menu shortcut, then the notifier.
    /// Any of these failing means Windows will not show toasts at all.
    unsafe fn start() -> windows::core::Result<ToastNotifier> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            SetCurrentProcessExplicitAppUserModelID(PCWSTR(HSTRING::from(crate::APP_ID).as_ptr()))?;
            ensure_shortcut();
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(crate::APP_ID))
        }
    }

    pub fn post(&self, id: &str, title: &str, body: &str, actions: Actions) {
        let Some(native) = &self.native else { return };
        let Ok(xml) = XmlDocument::new().and_then(|doc| {
            doc.LoadXml(&HSTRING::from(toast_xml(title, body, &actions)))?;
            Ok(doc)
        }) else {
            return;
        };
        let toast = match ToastNotification::CreateToastNotification(&xml).and_then(|toast| {
            toast.SetTag(&HSTRING::from(id))?;
            Ok(toast)
        }) {
            Ok(toast) => toast,
            Err(err) => {
                tracing::warn!(error = %err, "a toast did not build");
                return;
            }
        };
        let shown = native.shown.clone();
        let id_owned = id.to_string();
        let _ = toast.Dismissed(&TypedEventHandler::new(
            move |_toast: Ref<ToastNotification>, _args: Ref<ToastDismissedEventArgs>| {
                shown
                    .lock()
                    .expect("the toast map is poisoned")
                    .remove(&id_owned);
                Ok(())
            },
        ));
        let Some(sender) = clicks_channel().get().cloned() else {
            return;
        };
        let _ = toast.Activated(&TypedEventHandler::new(
            move |_toast: Ref<ToastNotification>, args: Ref<windows::core::IInspectable>| {
                let Ok(click) = serde_json::from_str::<Click>(
                    &args
                        .ok()?
                        .cast::<ToastActivatedEventArgs>()?
                        .Arguments()?
                        .to_string(),
                ) else {
                    return Ok(());
                };
                let _ = sender.try_send(click);
                Ok(())
            },
        ));
        native
            .shown
            .lock()
            .expect("the toast map is poisoned")
            .insert(id.to_string(), toast.clone());
        if let Err(err) = native.notifier.Show(&toast) {
            tracing::warn!(error = %err, "a toast did not show");
        }
    }

    pub fn withdraw(&self, id: &str) {
        let Some(native) = &self.native else { return };
        let toast = native
            .shown
            .lock()
            .expect("the toast map is poisoned")
            .remove(id);
        if let Some(toast) = toast
            && let Err(err) = native.notifier.Hide(&toast)
        {
            tracing::warn!(error = %err, "a toast would not hide");
        }
    }
}

/// The channel `Activated` handlers post to, set once at `Desktop::new`.
fn clicks_channel() -> &'static std::sync::OnceLock<async_channel::Sender<Click>> {
    static SENDER: std::sync::OnceLock<async_channel::Sender<Click>> = std::sync::OnceLock::new();
    &SENDER
}

/// The toast document: title and body, the click payload on `launch`,
/// a button per action, and the reminder scenario for events.
fn toast_xml(title: &str, body: &str, actions: &Actions) -> String {
    let click = |action: Option<&Action>| {
        action
            .map(|action| {
                let (name, target) = action.destination(&actions.target);
                Click {
                    a: name.to_string(),
                    t: target,
                }
            })
            .unwrap_or(Click {
                a: String::new(),
                t: String::new(),
            })
    };
    let launch = serde_json::to_string(&click(actions.open.as_ref()))
        .expect("a notification's click payload serializes");
    let mut xml = format!("<toast launch=\"{}\"", escaped(&launch));
    if actions.open == Some(Action::OpenEvent) {
        xml.push_str(" scenario=\"reminder\"");
    }
    xml.push_str("><visual><binding template=\"ToastGeneric\"><text>");
    xml.push_str(&escaped(title));
    xml.push_str("</text>");
    if !body.is_empty() {
        xml.push_str("<text>");
        xml.push_str(&escaped(body));
        xml.push_str("</text>");
    }
    xml.push_str("</binding></visual>");
    if !actions.buttons.is_empty() {
        xml.push_str("<actions>");
        for button in &actions.buttons {
            let args = serde_json::to_string(&click(Some(button)))
                .expect("a notification's click payload serializes");
            xml.push_str(&format!(
                "<action content=\"{}\" arguments=\"{}\"/>",
                escaped(&button.label()),
                escaped(&args),
            ));
        }
        xml.push_str("</actions>");
    }
    xml.push_str("</toast>");
    xml
}

/// XML text or an attribute value, with its five escapes.
fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Writes the Start Menu shortcut that carries the AppUserModelID to
///asts need, or updates it for the exe this run started from. A toast
/// with no AUMI shows under nothing and draws no buttons.
fn ensure_shortcut() {
    unsafe {
        let folder = SHGetKnownFolderPath(&FOLDERID_Programs, KF_FLAG_CREATE, None);
        let Ok(folder) = folder else {
            return;
        };
        let programs = folder.to_string().unwrap_or_default();
        CoTaskMemFree(Some(folder.as_ptr() as *const core::ffi::c_void));
        if programs.is_empty() {
            return;
        }
        let lnk = std::path::Path::new(&programs).join("Penguin Mail.lnk");
        let Ok(exe) = crate::exe::path() else {
            return;
        };
        let link: IShellLinkW = match CoCreateInstance(&ShellLink, None, CLSCTX_ALL) {
            Ok(link) => link,
            Err(_) => return,
        };
        let target = HSTRING::from(exe.as_os_str());
        if link.SetPath(PCWSTR(target.as_ptr())).is_err()
            || link.SetIconLocation(PCWSTR(target.as_ptr()), 0).is_err()
        {
            return;
        }
        let Ok(store) = link.cast::<IPropertyStore>() else {
            return;
        };
        let id = PROPVARIANT::from(crate::APP_ID);
        if store
            .SetValue(&PKEY_AppUserModel_ID, &id)
            .and_then(|_| store.Commit())
            .is_err()
        {
            return;
        }
        let Ok(file) = link.cast::<IPersistFile>() else {
            return;
        };
        let _ = file.Save(PCWSTR(HSTRING::from(lnk.as_os_str()).as_ptr()), true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify;

    fn actions(open: Option<Action>, target: &str, buttons: Vec<Action>) -> Actions {
        Actions {
            open,
            target: target.into(),
            buttons,
        }
    }

    #[test]
    fn a_mail_toast_carries_its_click_payload_and_escaped_text() {
        let xml = toast_xml(
            "A & B <tag>",
            "he said \"hi\"",
            &actions(
                Some(Action::OpenMail),
                "token/1",
                vec![Action::Mail(notify::Button::Reply)],
            ),
        );
        let (name, target) = Action::OpenMail.destination("token/1");
        let launch = serde_json::to_string(&Click {
            a: name.into(),
            t: target,
        })
        .unwrap();
        assert!(
            xml.contains(&format!("launch=\"{}\"", escaped(&launch))),
            "{xml}"
        );
        assert!(xml.contains("<text>A &amp; B &lt;tag&gt;</text>"), "{xml}");
        assert!(xml.contains("he said &quot;hi&quot;"), "{xml}");
        assert!(xml.contains("<action"), "{xml}");
        // Foreground is the default activation, so nothing names it.
        assert!(!xml.contains("activationType"), "{xml}");
        assert!(xml.contains("ToastGeneric"), "{xml}");
        assert!(!xml.contains("scenario"), "{xml}");
    }

    #[test]
    fn an_event_toast_is_a_reminder_and_a_click_without_open_just_activates() {
        let xml = toast_xml(
            "Standup",
            "",
            &actions(
                Some(Action::OpenEvent),
                "event",
                vec![Action::Join, Action::Snooze],
            ),
        );
        assert!(xml.contains("scenario=\"reminder\""), "{xml}");
        assert_eq!(xml.matches("<action ").count(), 2, "{xml}");
        // No empty <text></text> for an empty body.
        assert!(!xml.contains("<text></text>"), "{xml}");

        let xml = toast_xml("hi", "", &actions(None, "x", vec![]));
        let launch: Click = serde_json::from_str(
            &xml.split("launch=\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap()
                .replace("&quot;", "\""),
        )
        .unwrap();
        assert!(launch.a.is_empty(), "a body click only activates the app");
    }
}
