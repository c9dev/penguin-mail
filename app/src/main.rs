//! Penguin Mail: a Gmail client for the GNOME desktop.

mod app;
mod assistant;
mod autostart;
mod compose;
mod contacts;
mod core;
mod demo;
mod diff;
mod format;
mod hide_my_email;
mod notify;
mod render;
mod rules;
mod sanitize;
mod search;
mod settings;
mod tray;
mod ui;
mod unsubscribe;

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

pub const APP_ID: &str = "dev.penguinmail.PenguinMail";

const USAGE: &str = "Usage: penguin-mail [--background] [--demo] [--compose [mailto:ADDRESS]]

  --background   start in the tray without opening a window
  --demo         open sample mail in a throwaway store; nothing syncs
  --compose      open a new message, addressed to ADDRESS when given
  mailto:...     the same as --compose mailto:...";

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("warn,penguin_mail=info,mailrs_sync=info")
            }),
        )
        .init();
    let args: Vec<String> = std::env::args().collect();
    // Claude Code starts this binary as the assistant's MCP server. It only
    // relays tool calls to the running window, so it needs no GTK.
    if args.get(1).map(String::as_str) == Some("--mcp-bridge") {
        let Some(socket) = args.get(2) else {
            eprintln!("--mcp-bridge needs a socket path");
            return glib::ExitCode::FAILURE;
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime starts");
        return match runtime.block_on(mailrs_ai::bridge::run_mcp_stdio(std::path::Path::new(
            socket,
        ))) {
            Ok(()) => glib::ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("mcp bridge: {err}");
                glib::ExitCode::FAILURE
            }
        };
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return glib::ExitCode::SUCCESS;
    }
    if let Some(unknown) = args.iter().skip(1).find(|a| {
        !matches!(a.as_str(), "--background" | "--demo" | "--compose") && !a.starts_with("mailto:")
    }) {
        eprintln!("penguin-mail: unknown option {unknown}\n\n{USAGE}");
        return glib::ExitCode::FAILURE;
    }
    // `Some("")` opens a blank message; `Some(address)` addresses it.
    let compose = args
        .iter()
        .find_map(|a| a.strip_prefix("mailto:").map(mailto_recipient))
        .or_else(|| args.iter().any(|a| a == "--compose").then(String::new));
    let demo = args.iter().any(|a| a == "--demo");
    let background = args.iter().any(|a| a == "--background");

    // Wayland and X11 name the window after the program; matching the
    // desktop entry lets the dock show the right icon.
    glib::set_prgname(Some(if demo {
        "dev.penguinmail.PenguinMail.Demo"
    } else {
        APP_ID
    }));
    // A plain GApplication: GTK starts only when a window is first needed,
    // so a process running in the tray never loads the graphics stack.
    let gio_app = gio::Application::builder()
        .application_id(if demo {
            "dev.penguinmail.PenguinMail.Demo"
        } else {
            APP_ID
        })
        .build();
    // A second launch with --compose hands the request to the running copy.
    if let Some(to) = &compose
        && gio_app.register(gio::Cancellable::NONE).is_ok()
        && gio_app.is_remote()
    {
        gio_app.activate_action("compose-to", Some(&to.to_variant()));
        return glib::ExitCode::SUCCESS;
    }
    let state: Rc<RefCell<Option<Rc<app::App>>>> = Rc::new(RefCell::new(None));
    let started = Rc::clone(&state);
    gio_app.connect_startup(move |gio_app| {
        gio::resources_register_include!("penguin-mail.gresource")
            .expect("the resources are built into the binary");
        match core::Core::open(demo) {
            Ok(core) => {
                *started.borrow_mut() =
                    Some(app::App::new(gio_app, core, background, compose.clone()))
            }
            Err(err) => show_fatal(gio_app, &format!("{err:#}")),
        }
    });
    gio_app.connect_activate(move |_| {
        if let Some(app) = state.borrow().as_ref() {
            app.activate();
        }
    });
    gio_app.run_with_args(&args[..1])
}

/// The address part of a `mailto:` URI, percent-decoded.
fn mailto_recipient(rest: &str) -> String {
    let address = rest.split('?').next().unwrap_or(rest);
    let bytes = address.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = address.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Starts GTK and libadwaita and loads the app's icons and stylesheet.
/// Safe to call more than once.
pub fn ensure_gtk() {
    if gtk::is_initialized_main_thread() {
        return;
    }
    gtk::init().expect("GTK starts on this display");
    adw::init().expect("libadwaita starts");
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display)
            .add_resource_path("/dev/penguinmail/PenguinMail/icons");
        let css = gtk::CssProvider::new();
        css.load_from_resource("/dev/penguinmail/PenguinMail/style.css");
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    gtk::Window::set_default_icon_name(APP_ID);
}

/// A window that explains why Penguin Mail could not start.
fn show_fatal(gio_app: &gio::Application, message: &str) {
    ensure_gtk();
    let hold = gio_app.hold();
    let page = adw::StatusPage::builder()
        .icon_name("dialog-warning-symbolic")
        .title("Penguin Mail Could Not Start")
        .description(glib::markup_escape_text(message).as_str())
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    let window = adw::Window::builder()
        .default_width(560)
        .default_height(420)
        .content(&toolbar)
        .build();
    let quit = gio_app.clone();
    window.connect_destroy(move |_| {
        let _ = &hold;
        quit.quit();
    });
    window.present();
}

#[cfg(test)]
mod tests {
    use super::mailto_recipient;

    #[test]
    fn mailto_uris_yield_their_address() {
        assert_eq!(mailto_recipient("ann@example.com"), "ann@example.com");
        assert_eq!(
            mailto_recipient("ann%40example.com?subject=Hi"),
            "ann@example.com"
        );
        assert_eq!(
            mailto_recipient("Ann%20Lee%20%3Cann@example.com%3E"),
            "Ann Lee <ann@example.com>"
        );
    }
}
