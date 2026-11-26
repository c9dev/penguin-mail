//! The notification-area icon on Windows. tray-icon owns a hidden
//! window whose messages the GTK main loop pumps, so the icon, its
//! tooltip and its muda context menu all live on the GTK thread. A
//! `watch` flag marks the state dirty; the loop redraws once no matter
//! how many updates came between wakes. Clicks arrive by event handler,
//! so nothing polls.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gtk::glib;
use mailrs_domain::translate::{fill, gettext};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use super::{MailTray, MenuRow, TrayCommand};

/// The icons the resource script embeds in the exe: 1 is the plain
/// envelope, 2 the envelope with the unread dot.
const PLAIN_ICON: u16 = 1;
const UNREAD_ICON: u16 = 2;

/// What the tray's event handlers pass to the GTK loop.
enum Heard {
    Menu(MenuId),
    Toggle,
}

/// The state the shell reads; `changed` wakes the GTK-side drawer.
struct Shared {
    tray: Mutex<MailTray>,
    changed: tokio::sync::watch::Sender<()>,
}

/// What `App::tray` holds: `update` applies a change to the state and
/// wakes the drawer, the same shape `ksni::Handle` gives Linux.
#[derive(Clone)]
pub struct Handle {
    shared: Arc<Shared>,
}

impl Handle {
    pub async fn update(&self, f: impl FnOnce(&mut MailTray) + Send + 'static) {
        f(&mut self.shared.tray.lock().expect("the tray state is poisoned"));
        let _ = self.shared.changed.send(());
    }
}

/// The pieces redraw needs, held on the GTK thread: the icon itself is
/// `!Send`, so it stays here while `update` runs on worker threads.
struct OnScreen {
    /// `None` once [`remove`] took it away.
    icon: RefCell<Option<TrayIcon>>,
    plain: Icon,
    unread: Icon,
    /// Each visible menu row's id to the command it sends.
    rows: RefCell<HashMap<MenuId, TrayCommand>>,
}

thread_local! {
    /// The icon's owner on the GTK thread, so [`remove`] can reach it.
    static SCREEN: RefCell<Option<Rc<OnScreen>>> = const { RefCell::new(None) };
}

/// Takes the icon out of the notification area. A process that ends
/// without running destructors (`process::exit`, or the main loop's static
/// state at exit) would otherwise leave a dead icon there until the mouse
/// passes over it. Does nothing off the GTK thread or with no icon.
pub(crate) fn remove() {
    SCREEN.with(|held| {
        if let Some(screen) = held.borrow().as_ref() {
            drop(screen.icon.borrow_mut().take());
        }
    });
}

/// Puts the icon in the notification area and starts its redraw and
/// event loops, or nothing when Windows will not take it. Runs on the
/// GTK thread.
pub fn spawn(tray: MailTray) -> Option<Handle> {
    let plain = Icon::from_resource(PLAIN_ICON, None)
        .inspect_err(|err| tracing::warn!(error = %err, "the tray icon is not in the exe"))
        .ok()?;
    let unread = Icon::from_resource(UNREAD_ICON, None)
        .inspect_err(|err| tracing::warn!(error = %err, "the unread tray icon is not in the exe"))
        .ok()?;
    let shared = Arc::new(Shared {
        tray: Mutex::new(tray),
        changed: tokio::sync::watch::channel(()).0,
    });
    let icon = TrayIconBuilder::new()
        .with_tooltip("Penguin Mail")
        .with_icon(plain.clone())
        .with_menu_on_left_click(false)
        .build()
        .inspect_err(|err| tracing::warn!(error = %err, "the tray icon did not start"))
        .ok()?;
    let screen = Rc::new(OnScreen {
        icon: RefCell::new(Some(icon)),
        plain,
        unread,
        rows: RefCell::new(HashMap::new()),
    });
    redraw(
        &screen,
        &shared.tray.lock().expect("the tray state is poisoned"),
    );
    SCREEN.with(|held| *held.borrow_mut() = Some(screen.clone()));
    glib::spawn_future_local({
        let shared = shared.clone();
        let screen = screen.clone();
        async move {
            let mut changed = shared.changed.subscribe();
            while changed.changed().await.is_ok() {
                redraw(
                    &screen,
                    &shared.tray.lock().expect("the tray state is poisoned"),
                );
            }
        }
    });
    // The handlers run on the GTK thread, inside the window's message
    // pump, but they must be `Send`; they hand the events to the loop.
    let (heard, events) = async_channel::unbounded::<Heard>();
    MenuEvent::set_event_handler(Some({
        let heard = heard.clone();
        move |event: MenuEvent| {
            let _ = heard.try_send(Heard::Menu(event.id));
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let _ = heard.try_send(Heard::Toggle);
        }
    }));
    glib::spawn_future_local({
        let shared = shared.clone();
        async move {
            while let Ok(heard) = events.recv().await {
                let command = match heard {
                    Heard::Toggle => Some(TrayCommand::Toggle),
                    Heard::Menu(id) => screen.rows.borrow().get(&id).cloned(),
                };
                if let Some(command) = command {
                    shared
                        .tray
                        .lock()
                        .expect("the tray state is poisoned")
                        .send(command);
                }
            }
        }
    });
    Some(Handle { shared })
}

/// Draws the current state: the glyph, the tooltip sentence, and the
/// menu built from the same rows the AppIndicator shows on Linux.
fn redraw(screen: &OnScreen, tray: &MailTray) {
    let icon = screen.icon.borrow();
    let Some(icon) = icon.as_ref() else { return };
    let glyph = if tray.unread > 0 {
        &screen.unread
    } else {
        &screen.plain
    };
    if let Err(err) = icon.set_icon(Some(glyph.clone())) {
        tracing::warn!(error = %err, "the tray glyph did not change");
    }
    let tip = fill(
        &gettext("Penguin Mail: {summary}"),
        &[("summary", &tray.summary())],
    );
    // A NOTIFYICONDATA tip holds 127 characters.
    let tip: String = tip.chars().take(124).collect();
    if let Err(err) = icon.set_tooltip(Some(tip)) {
        tracing::warn!(error = %err, "the tray tooltip did not change");
    }
    let mut rows = screen.rows.borrow_mut();
    rows.clear();
    icon.set_menu(Some(Box::new(menu(&tray.menu_model(), &mut rows))));
}

/// The context menu for the rows `menu_model` gives, recording which id
/// sends which command for the event loop. Ids come from the command,
/// not the row's place, so a click on a menu drawn before the last
/// redraw still sends the command it displayed.
fn menu(rows: &[MenuRow], commands: &mut HashMap<MenuId, TrayCommand>) -> Menu {
    let menu = Menu::new();
    for row in rows {
        let appended = match row {
            MenuRow::Item {
                label,
                enabled,
                command,
            } => {
                let item = MenuItem::with_id(MenuId(command_key(command)), label, *enabled, None);
                commands.insert(item.id().clone(), command.clone());
                menu.append(&item)
            }
            MenuRow::Disabled { label } => menu.append(&MenuItem::new(label, false, None)),
            MenuRow::Separator => menu.append(&PredefinedMenuItem::separator()),
        };
        if let Err(err) = appended {
            tracing::warn!(error = %err, "a tray menu row did not fit");
        }
    }
    menu
}

/// A stable id for a command, so menus rebuilt between redraws keep
/// the same id on the same entry.
fn command_key(command: &TrayCommand) -> String {
    match command {
        TrayCommand::ReadAll => "read-all".into(),
        TrayCommand::Toggle => "toggle".into(),
        TrayCommand::Open => "open".into(),
        TrayCommand::OpenInbox(id) => format!("inbox-{id}"),
        TrayCommand::Compose => "compose".into(),
        TrayCommand::Check => "check".into(),
        TrayCommand::CheckForUpdates => "check-updates".into(),
        TrayCommand::InstallUpdate => "install-update".into(),
        TrayCommand::WhatsNew => "whats-new".into(),
        TrayCommand::Quit => "quit".into(),
    }
}
