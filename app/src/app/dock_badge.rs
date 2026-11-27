//! The badge's platform boundary: the macOS Dock, the Windows taskbar
//! button, or a Linux dock that draws badges.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(super) use macos::Badge;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(super) use windows::Badge;

#[cfg(not(any(target_os = "macos", windows)))]
mod linux;
#[cfg(not(any(target_os = "macos", windows)))]
pub(super) use linux::Badge;
