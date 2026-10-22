//! The small window a file opened from Files or the command line gets:
//! a header bar over a toast overlay, and the status page shown when the
//! file will not open. The calendar file window and the mail file window
//! both start from here, so a change to the shell is made once.

use adw::prelude::*;
use gtk::glib;

/// A window named `title`, with its toast overlay for the content.
pub fn shell(title: &str) -> (adw::Window, adw::ToastOverlay) {
    let window = adw::Window::builder()
        .title(title)
        .default_width(520)
        .default_height(-1)
        .build();
    // The colors follow the window's light or dark class, which the main
    // window sets for itself.
    crate::ui::window::track_dark_class(&window);
    let toasts = adw::ToastOverlay::new();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&toasts));
    window.set_content(Some(&toolbar));
    (window, toasts)
}

/// What the window says when the file gives it nothing to show.
pub fn status(icon: &str, title: &str, description: &str) -> adw::StatusPage {
    adw::StatusPage::builder()
        .icon_name(icon)
        .title(glib::markup_escape_text(title).as_str())
        .description(glib::markup_escape_text(description).as_str())
        .build()
}
