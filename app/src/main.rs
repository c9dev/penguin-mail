//! mailrs: a Gmail client for the GNOME desktop.

mod app;
mod compose;
mod core;
mod demo;
mod diff;
mod format;
mod notify;
mod render;
mod sanitize;
mod tray;
mod ui;

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

pub const APP_ID: &str = "dev.mailrs.Mailrs";

const USAGE: &str = "Usage: mailrs [--background] [--demo]

  --background   start in the tray without opening a window
  --demo         open sample mail in a throwaway store; nothing syncs";

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("warn,mailrs=info,mailrs_sync=info")
            }),
        )
        .init();
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return glib::ExitCode::SUCCESS;
    }
    if let Some(unknown) = args
        .iter()
        .skip(1)
        .find(|a| !matches!(a.as_str(), "--background" | "--demo"))
    {
        eprintln!("mailrs: unknown option {unknown}\n\n{USAGE}");
        return glib::ExitCode::FAILURE;
    }
    let demo = args.iter().any(|a| a == "--demo");
    let background = args.iter().any(|a| a == "--background");

    // Wayland and X11 name the window after the program; matching the
    // desktop entry lets the dock show the right icon.
    glib::set_prgname(Some(if demo {
        "dev.mailrs.Mailrs.Demo"
    } else {
        APP_ID
    }));
    // A plain GApplication: GTK starts only when a window is first needed,
    // so a process running in the tray never loads the graphics stack.
    let gio_app = gio::Application::builder()
        .application_id(if demo {
            "dev.mailrs.Mailrs.Demo"
        } else {
            APP_ID
        })
        .build();
    let state: Rc<RefCell<Option<Rc<app::App>>>> = Rc::new(RefCell::new(None));
    let started = Rc::clone(&state);
    gio_app.connect_startup(move |gio_app| {
        gio::resources_register_include!("mailrs.gresource")
            .expect("the resources are built into the binary");
        match core::Core::open(demo) {
            Ok(core) => *started.borrow_mut() = Some(app::App::new(gio_app, core, background)),
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

/// Starts GTK and libadwaita and loads the app's icons and stylesheet.
/// Safe to call more than once.
pub fn ensure_gtk() {
    if gtk::is_initialized_main_thread() {
        return;
    }
    gtk::init().expect("GTK starts on this display");
    adw::init().expect("libadwaita starts");
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_resource_path("/dev/mailrs/Mailrs/icons");
        let css = gtk::CssProvider::new();
        css.load_from_resource("/dev/mailrs/Mailrs/style.css");
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    gtk::Window::set_default_icon_name(APP_ID);
}

/// A window that explains why mailrs could not start.
fn show_fatal(gio_app: &gio::Application, message: &str) {
    ensure_gtk();
    let hold = gio_app.hold();
    let page = adw::StatusPage::builder()
        .icon_name("dialog-warning-symbolic")
        .title("mailrs Could Not Start")
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
