//! Penguin Mail: mail and calendar for Linux.

#[cfg(test)]
mod accent_contrast;
mod add_account;
mod app;
#[cfg(any(feature = "packaging-appimage", test))]
mod appimage;
mod assistant;
mod attachcheck;
mod autostart;
#[cfg(target_os = "macos")]
mod bundle;
mod clock_format;
mod compose;
mod contacts;
mod core;
mod demo;
mod diff;
#[cfg(all(target_os = "macos", debug_assertions))]
mod drive;
mod event_reminders;
mod exe;
mod format;
mod goa;
mod icon_theme;
mod images;
mod keyring_plug;
mod language;
mod language_names;
mod link_safety;
mod locale_time;
mod logging;
mod notify;
mod offered;
mod old_id;
mod open_thread;
mod packaging;
mod permission;
mod pgp;
mod photos;
mod protection;
mod quoted;
mod recent;
mod render;
mod richtext;
mod rules;
mod sanitize;
mod search;
mod servers;
mod settings;
mod smime;
mod starting;
mod stray_markdown;
mod templates;
#[cfg(test)]
mod text_contrast;
mod translation;
mod tray;
mod ui;
mod unread;
mod unsubscribe;
mod unsubscribe_page;
mod update;
mod wanted;
mod web;

use std::cell::RefCell;
use std::ffi::OsString;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use mailrs_domain::translate::{fill, gettext};

use settings::Settings;

/// The ID this copy registers on the bus and names its desktop file and
/// icon by. Flathub asks for one ending in the repository's name, and
/// a Flatpak may own only names under its own ID, so the Flatpak is
/// `io.github.c9dev.penguin-mail` while every other package keeps the ID
/// people's launchers, login items and mail handlers already name.
/// `scripts/stage.sh` installs the files under the same ID.
pub const APP_ID: &str = if cfg!(feature = "packaging-flatpak") {
    "io.github.c9dev.penguin-mail"
} else {
    "io.github.c9dev.PenguinMail"
};

/// The demo's own ID, so `--demo` runs beside a real copy. It sits under
/// `APP_ID`, as a Flatpak requires of every name it owns.
const DEMO_ID: &str = if cfg!(feature = "packaging-flatpak") {
    "io.github.c9dev.penguin-mail.Demo"
} else {
    "io.github.c9dev.PenguinMail.Demo"
};

fn usage() -> String {
    gettext(
        "Usage: penguin-mail [--background] [--demo] [--compose [mailto:ADDRESS]] [FILE.ics]

  --version      print the version and quit
  --background   start in the tray without opening a window
  --demo         open sample mail in a throwaway store; nothing syncs
  --compose      open a new message, addressed to ADDRESS when given
  mailto:...     the same as --compose mailto:...
  FILE.ics       open a calendar file, to add its events to a calendar",
    )
}

fn main() -> glib::ExitCode {
    #[cfg(feature = "packaging-appimage")]
    appimage::prepare().expect("could not prepare the AppImage");
    #[cfg(target_os = "macos")]
    bundle::prepare().expect("could not prepare the application bundle");
    // On macOS this has to come before GTK starts; see `web`.
    web::prepare();
    #[cfg(all(target_os = "macos", debug_assertions))]
    drive::from_env();
    // async-imap logs passwords and mail at trace level; `quiet` drops
    // those lines whatever RUST_LOG says.
    tracing_subscriber::util::SubscriberInitExt::init(mailrs_imap::quiet(
        tracing_subscriber::fmt()
            .with_writer(logging::Writer {
                details: logging::details(),
            })
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new("warn,penguin_mail=info,mailrs_sync=info")
                }),
            )
            .finish(),
    ));
    // A repeating event with no zone repeats in the computer's. Reading
    // that zone once here keeps the file reads off the calendar's first
    // expansion.
    mailrs_domain::calendar::local::zone();
    // `std::env::args` panics on an argument that is not UTF-8, such as a
    // calendar file's name in an old encoding. The flags read the lossy
    // copy; the file keeps its bytes.
    let raw_args: Vec<OsString> = std::env::args_os().collect();
    let args: Vec<String> = raw_args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
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
    // The language is the one preference read before the window exists,
    // because every word after this point has to come out in it. A demo
    // keeps its own throwaway preferences, but not this one: it is the
    // person's own copy of the app they are looking at. The bridge above
    // runs on every one of Claude Code's turns and says nothing a person
    // reads, so it starts without reading the preferences at all.
    language::bind(&Settings::load(&Settings::default_path()).language);
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", usage());
        return glib::ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--version") {
        println!("penguin-mail {}", env!("CARGO_PKG_VERSION"));
        return glib::ExitCode::SUCCESS;
    }
    if let Some(unknown) = args.iter().skip(1).find(|a| {
        a.starts_with('-') && !matches!(a.as_str(), "--background" | "--demo" | "--compose")
    }) {
        let line = fill(
            &gettext("penguin-mail: unknown option {option}"),
            &[("option", unknown)],
        );
        eprintln!("{line}\n\n{}", usage());
        return glib::ExitCode::FAILURE;
    }
    // `Some("")` opens a blank message; `Some(address)` addresses it.
    let compose = args
        .iter()
        .find_map(|a| a.strip_prefix("mailto:").map(mailto_recipient))
        .or_else(|| args.iter().any(|a| a == "--compose").then(String::new));
    let file = std::env::current_dir()
        .ok()
        .and_then(|here| named_file(&raw_args, &here));
    let demo = args.iter().any(|a| a == "--demo");
    let background = args.iter().any(|a| a == "--background");
    if !demo {
        old_id::carry_over_on_start();
    }

    // Wayland and X11 name the window after the program; matching the
    // desktop entry lets the dock show the right icon.
    glib::set_prgname(Some(if demo { DEMO_ID } else { APP_ID }));
    #[cfg(target_os = "macos")]
    glib::set_application_name(&gettext("Penguin Mail"));
    // Linux starts GTK only when a window is needed. macOS needs
    // GtkApplication to own the native menu and route its actions.
    #[cfg(target_os = "macos")]
    let gio_app: gio::Application = gtk::Application::builder()
        .application_id(if demo { DEMO_ID } else { APP_ID })
        .build()
        .upcast();
    #[cfg(not(target_os = "macos"))]
    let gio_app = gio::Application::builder()
        .application_id(if demo { DEMO_ID } else { APP_ID })
        .build();
    // A second launch with --compose or a calendar file hands the request
    // to the running copy; the check waits until the handlers below are
    // connected, since registering emits `startup` for a copy that turns
    // out to be the first.
    let handoff = compose
        .as_ref()
        .map(|to| ("compose-to", to.to_variant()))
        .or_else(|| {
            file.as_ref().map(|file| match file {
                NamedFile::Calendar(path) => ("open-calendar-file", path.to_variant()),
                NamedFile::Mail(path) => ("open-mail-file", path.to_variant()),
            })
        });
    let state: Rc<RefCell<Option<Rc<app::App>>>> = Rc::new(RefCell::new(None));
    let started = Rc::clone(&state);
    let finished = Rc::clone(&state);
    gio_app.connect_startup(move |gio_app| {
        gio::resources_register_include!("penguin-mail.gresource")
            .expect("the resources are built into the binary");
        #[cfg(feature = "packaging-appimage")]
        if let sandbox @ (appimage::Sandbox::Missing | appimage::Sandbox::Blocked { .. }) =
            appimage::sandbox()
        {
            ui::sandbox_setup::present(gio_app, &sandbox);
            return;
        }
        match core::Core::open(demo) {
            Ok(core) => {
                *started.borrow_mut() = Some(app::App::new(
                    gio_app,
                    core,
                    background,
                    compose.clone(),
                    file.clone(),
                ))
            }
            Err(err) => {
                let report = err.downcast_ref::<core::StoreNotUpdated>().is_some();
                show_fatal(gio_app, &format!("{err:#}"), report)
            }
        }
    });
    gio_app.connect_activate(move |_| {
        if let Some(app) = state.borrow().as_ref() {
            app.activate();
        }
    });
    if let Some((action, parameter)) = &handoff
        && gio_app.register(gio::Cancellable::NONE).is_ok()
        && gio_app.is_remote()
    {
        gio_app.activate_action(action, Some(parameter));
        return glib::ExitCode::SUCCESS;
    }
    let code = gio_app.run_with_args(&args[..1]);
    // The MCP servers live in a static, which nothing drops, and a stdio
    // server would outlive the app without this.
    match finished.borrow().as_ref() {
        Some(app) => {
            app.before_leaving();
            // Only the copy that ran the app; one that handed its launch to
            // it leaves the files that copy runs from.
            #[cfg(feature = "packaging-appimage")]
            appimage::leave();
        }
        None => assistant::sources::mcp::registry().stop_all(),
    }
    code
}

/// The file the desktop asked this launch to open, and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamedFile {
    Calendar(std::path::PathBuf),
    Mail(std::path::PathBuf),
}

/// What a file handed to the app holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Calendar,
    Mail,
}

/// The file the desktop asked this launch to open: the first argument
/// that is neither a flag nor a `mailto:` link, as a path or as the
/// `file://` URI Files passes for `%u`. A relative path is taken from
/// `here`, the folder the launch started in, since a copy already running
/// would read it from its own. The argument's bytes are kept as they are,
/// so a name that is not UTF-8 still opens.
fn named_file(args: &[OsString], here: &std::path::Path) -> Option<NamedFile> {
    let named = args.iter().skip(1).find(|a| {
        let a = a.to_string_lossy();
        !a.starts_with('-') && !a.starts_with("mailto:")
    })?;
    let path = match named.to_str().filter(|n| n.starts_with("file://")) {
        Some(uri) => gio::File::for_uri(uri).path()?,
        None => std::path::PathBuf::from(named),
    };
    let path = here.join(path);
    Some(match file_kind(&path) {
        FileKind::Calendar => NamedFile::Calendar(path),
        FileKind::Mail => NamedFile::Mail(path),
    })
}

/// What `path` holds: `.ics` and `.eml` say so by name; otherwise a file
/// whose first bytes, after a byte-order mark and any white space, are
/// `BEGIN:VCALENDAR` in any case is a calendar, and anything else is
/// mail, since the desktop hands over only the types the entry names.
pub fn file_kind(path: &std::path::Path) -> FileKind {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match extension.as_deref() {
        Some("ics") => return FileKind::Calendar,
        Some("eml") => return FileKind::Mail,
        _ => {}
    }
    let mut head = [0u8; 512];
    let read = std::fs::File::open(path)
        .and_then(|mut file| std::io::Read::read(&mut file, &mut head))
        .unwrap_or(0);
    let head = &head[..read];
    let head = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(head);
    let start = head
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(head.len());
    let calendar = head[start..]
        .get(..b"BEGIN:VCALENDAR".len())
        .is_some_and(|first| first.eq_ignore_ascii_case(b"BEGIN:VCALENDAR"));
    match calendar {
        true => FileKind::Calendar,
        false => FileKind::Mail,
    }
}

/// The file in the `open-calendar-file` or `open-mail-file` action's
/// parameter, a byte string so a path that is not UTF-8 arrives whole.
fn path_from_variant(parameter: &glib::Variant) -> Option<std::path::PathBuf> {
    parameter.get::<std::path::PathBuf>()
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
    // GtkApplication may have started GTK before our resources were
    // registered, so GTK being initialized does not mean our style is.
    thread_local! {
        static STYLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if STYLED.replace(true) {
        return;
    }
    gtk::init().expect("GTK starts on this display");
    adw::init().expect("libadwaita starts");
    #[cfg(feature = "packaging-appimage")]
    if let Some(context) = webkit::WebContext::default() {
        context.add_path_to_sandbox(appimage::root().expect("AppImage root is configured"), true);
    }
    if let Some(display) = gdk::Display::default() {
        let icons = gtk::IconTheme::for_display(&display);
        icons.add_resource_path("/io/github/c9dev/PenguinMail/icons");
        let theme = icons.theme_name();
        if icon_theme::needs_fallback(&theme, |name| icons.has_icon(name)) {
            tracing::info!(%theme, "the icon theme lacks the app's icons; using Adwaita");
            gtk::Settings::for_display(&display)
                .set_gtk_icon_theme_name(Some(icon_theme::FALLBACK));
        }
        let css = gtk::CssProvider::new();
        css.load_from_resource("/io/github/c9dev/PenguinMail/style.css");
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        #[cfg(target_os = "macos")]
        {
            let css = gtk::CssProvider::new();
            css.load_from_string(include_str!("../data/macos.css"));
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }
    }
    gtk::Window::set_default_icon_name(APP_ID);
}

/// A window that explains why Penguin Mail could not start. With `report`,
/// it links to the page where a problem is reported.
fn show_fatal(gio_app: &gio::Application, message: &str, report: bool) {
    ensure_gtk();
    let hold = gio_app.hold();
    let page = adw::StatusPage::builder()
        .icon_name("dialog-warning-symbolic")
        .title(gettext("Penguin Mail Could Not Start"))
        .description(glib::markup_escape_text(message).as_str())
        .build();
    if report {
        let link = gtk::LinkButton::builder()
            .label(gettext("Report a Problem"))
            .uri(format!("{}/issues/new/choose", ui::about::REPOSITORY))
            .halign(gtk::Align::Center)
            .build();
        page.set_child(Some(&link));
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    let window = adw::Window::builder()
        .default_width(560)
        .default_height(420)
        .content(&toolbar)
        .build();
    ui::window::track_dark_class(&window);
    let quit = gio_app.clone();
    window.connect_destroy(move |_| {
        let _ = &hold;
        quit.quit();
    });
    window.present();
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::{FileKind, NamedFile, file_kind, mailto_recipient, named_file, path_from_variant};

    fn args(list: &[&str]) -> Vec<OsString> {
        std::iter::once("penguin-mail")
            .chain(list.iter().copied())
            .map(OsString::from)
            .collect()
    }

    /// The folder the launch was typed in.
    const HERE: &str = "/home/ann/Downloads";

    fn file(list: &[&str]) -> Option<NamedFile> {
        named_file(&args(list), Path::new(HERE))
    }

    #[test]
    fn files_hands_over_a_calendar_file_as_a_path_or_a_uri() {
        let path = Some(NamedFile::Calendar(PathBuf::from("/tmp/my ticket.ics")));
        assert_eq!(file(&["/tmp/my ticket.ics"]), path);
        assert_eq!(file(&["file:///tmp/my%20ticket.ics"]), path);
    }

    #[test]
    fn files_hands_over_a_mail_file_as_a_path_or_a_uri() {
        let path = Some(NamedFile::Mail(PathBuf::from("/tmp/fwd.eml")));
        assert_eq!(file(&["/tmp/fwd.eml"]), path);
        assert_eq!(file(&["file:///tmp/fwd.eml"]), path);
    }

    #[test]
    fn a_file_is_told_by_its_extension_then_by_its_first_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let ics = dir.path().join("a.ics");
        std::fs::write(&ics, "x").unwrap();
        let eml = dir.path().join("a.eml");
        std::fs::write(&eml, "x").unwrap();
        let cal = dir.path().join("ticket");
        std::fs::write(&cal, "\n\nBEGIN:VCALENDAR\n").unwrap();
        let mail = dir.path().join("message");
        std::fs::write(&mail, "From: a@example.com\n").unwrap();
        assert_eq!(file_kind(&ics), FileKind::Calendar);
        assert_eq!(file_kind(&eml), FileKind::Mail);
        assert_eq!(file_kind(&cal), FileKind::Calendar);
        assert_eq!(file_kind(&mail), FileKind::Mail);
        assert_eq!(file_kind(&dir.path().join("missing")), FileKind::Mail);
        // Outlook writes a byte-order mark first, and RFC 5545 names are
        // case-insensitive.
        let bom = dir.path().join("invite.ical");
        std::fs::write(&bom, b"\xef\xbb\xbfBEGIN:VCALENDAR\r\n").unwrap();
        assert_eq!(file_kind(&bom), FileKind::Calendar);
        let lower = dir.path().join("booking");
        std::fs::write(&lower, "begin:vcalendar\n").unwrap();
        assert_eq!(file_kind(&lower), FileKind::Calendar);
    }

    #[test]
    fn a_relative_calendar_file_is_found_from_where_the_launch_was_typed() {
        // The running copy opens the path from its own folder, so a bare
        // name would point at a file that is not there.
        assert_eq!(
            file(&["invite.ics"]),
            Some(NamedFile::Calendar(PathBuf::from(
                "/home/ann/Downloads/invite.ics"
            )))
        );
        assert_eq!(
            file(&["./trips/invite.ics"]),
            Some(NamedFile::Calendar(PathBuf::from(
                "/home/ann/Downloads/trips/invite.ics"
            )))
        );
    }

    #[test]
    fn a_calendar_file_whose_name_is_not_utf8_reaches_the_running_copy_unchanged() {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::OsStr::from_bytes(b"/tmp/f\xe9rias.ics");
        let mut list = args(&[]);
        list.push(name.to_os_string());
        let Some(NamedFile::Calendar(path)) = named_file(&list, Path::new(HERE)) else {
            panic!("a calendar file");
        };
        assert_eq!(path.as_os_str(), name);
        let handed = gtk::glib::prelude::ToVariant::to_variant(&path);
        assert_eq!(path_from_variant(&handed), Some(path));
    }

    #[test]
    fn flags_and_mail_links_are_no_file() {
        assert_eq!(file(&[]), None);
        assert_eq!(file(&["--background", "--demo"]), None);
        assert_eq!(file(&["mailto:ann@example.com"]), None);
        assert_eq!(file(&["--compose", "mailto:ann@example.com"]), None);
    }

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
