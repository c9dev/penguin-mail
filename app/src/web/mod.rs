//! The one way to a web engine, for the conversation and for the
//! composer's Preview and forwarded message.
//!
//! [`WebView`] is a handle on one page drawn from HTML the app wrote
//! itself. Two adapters stand behind it, chosen when the app is built:
//! WebKitGTK on Linux, where the page is a GTK widget, and WKWebView on
//! macOS, where WebKitGTK does not exist and the page is a native view laid
//! over an empty GTK widget that holds its place.
//!
//! Each adapter offers the same names: [`WebView`] with its two kinds,
//! `reading` for the conversation and `sealed` for a page that runs no
//! script; [`WeakWebView`]; [`Request`], a picture the page asks for at an
//! address of the app's own scheme; [`Filter`] and [`compile_filter`], the
//! rules that block remote content; [`Finder`], which finds text; and
//! [`holds_focus`]. A caller never names the engine beneath.

#[cfg(target_os = "linux")]
mod webkitgtk;
#[cfg(target_os = "linux")]
pub use webkitgtk::*;
