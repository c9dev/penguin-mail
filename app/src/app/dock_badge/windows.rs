//! The taskbar badge on Windows: `ITaskbarList3::SetOverlayIcon` draws
//! a small overlay at the corner of every window's taskbar button. The
//! unread count is drawn into the overlay icon itself, since the shell
//! takes no number.

use std::cell::{OnceCell, RefCell};

use gtk::prelude::*;
use mailrs_domain::translate::fill_plural;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON,
};
use windows::core::{HSTRING, PCWSTR};

pub(in crate::app) struct Badge {
    /// The taskbar list, made at the first count: COM and the shell
    /// extension take a moment, and a headless run never needs either.
    /// `None` once that failed, so it is not retried per count.
    taskbar: OnceCell<Option<ITaskbarList3>>,
    /// The icon for the count last drawn; one icon per count rather
    /// than one per call.
    icon: RefCell<Option<(i64, HICON)>>,
}

impl Badge {
    pub(in crate::app) fn new(_app_id: &str) -> Badge {
        Badge {
            taskbar: OnceCell::new(),
            icon: RefCell::new(None),
        }
    }

    /// Draws `unread` over every window's taskbar button, clearing the
    /// overlay at zero. Unlike the Linux signal this is reapplied on
    /// every call: the shell takes the overlay per window, so windows
    /// that opened since the last count need it too.
    pub(in crate::app) fn show(&self, unread: i64) {
        let Some(taskbar) = self.taskbar() else {
            return;
        };
        let unread = unread.max(0);
        // The description is the overlay's alt text, read aloud.
        let description = HSTRING::from(&fill_plural(
            "{count} unread message",
            "{count} unread messages",
            unread.max(1) as usize,
            &[("count", &unread.to_string())],
        ));
        let (icon, alt) = if unread == 0 {
            (HICON::default(), PCWSTR::null())
        } else {
            (self.icon_for(unread), PCWSTR(description.as_ptr()))
        };
        for hwnd in toplevels() {
            unsafe {
                let _ = taskbar.SetOverlayIcon(hwnd, icon, alt);
            }
        }
    }

    /// Clears the overlay before the process ends.
    pub(in crate::app) fn hide_now(&self) {
        self.show(0);
    }

    fn taskbar(&self) -> Option<&ITaskbarList3> {
        self.taskbar
            .get_or_init(|| unsafe {
                // COM on this thread may already be up for WebView2;
                // S_FALSE still means usable.
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                let taskbar: windows::core::Result<ITaskbarList3> =
                    CoCreateInstance(&TaskbarList, None, CLSCTX_ALL);
                taskbar
                    .inspect_err(|err| tracing::debug!(error = %err, "no taskbar for the badge"))
                    .ok()
            })
            .as_ref()
    }

    /// The overlay icon for `unread`, rebuilt when the count changes.
    fn icon_for(&self, unread: i64) -> HICON {
        let mut slot = self.icon.borrow_mut();
        if let Some((count, icon)) = *slot
            && count == unread
        {
            return icon;
        }
        if let Some((_, old)) = slot.take() {
            unsafe {
                let _ = DestroyIcon(old);
            }
        }
        let icon = unsafe { count_icon(unread) };
        *slot = Some((unread, icon));
        icon
    }
}

impl Drop for Badge {
    fn drop(&mut self) {
        if let Some((_, icon)) = self.icon.get_mut() {
            unsafe {
                let _ = DestroyIcon(*icon);
            }
        }
    }
}

/// Every top-level window's HWND, the buttons the overlay belongs on.
fn toplevels() -> Vec<HWND> {
    // A run started with --background never initialized GTK; it has no
    // taskbar buttons to badge either, and asking panics.
    if !gtk::is_initialized_main_thread() {
        return Vec::new();
    }
    gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|widget| {
            widget
                .downcast::<gtk::Window>()
                .ok()?
                .surface()?
                .downcast::<gdk4_win32::Win32Surface>()
                .ok()
                .map(|surface| HWND(surface.handle().0))
        })
        .collect()
}

/// The overlay icon: [`badge_pixels`] in a 32-bit icon, whose alpha
/// leaves the corners of the round badge transparent.
unsafe fn count_icon(unread: i64) -> HICON {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    let Some(pixels) = badge_pixels(unread, size) else {
        return HICON::default();
    };
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                // Negative: the rows run top to bottom, as cairo's do.
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
            return HICON::default();
        };
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u8>(), pixels.len());
        // With a 32-bit color bitmap Windows draws by its alpha; the AND
        // mask must still exist, so it is all zeros.
        let row = ((size + 15) / 16) as usize * 2;
        let zeros = vec![0u8; row * size as usize];
        let mask = CreateBitmap(
            size,
            size,
            1,
            1,
            Some(zeros.as_ptr() as *const core::ffi::c_void),
        );
        let icon_info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&icon_info).unwrap_or_default();
        let _ = DeleteObject(mask.into());
        let _ = DeleteObject(color.into());
        icon
    }
}

/// A round orange badge with a white rim and `unread` in white, `size`
/// pixels square, as straight-alpha BGRA rows for a Windows icon. Counts
/// past 99 say 99+.
fn badge_pixels(unread: i64, size: i32) -> Option<Vec<u8>> {
    use gtk::cairo;
    let text = if unread > 99 {
        "99+".to_string()
    } else {
        unread.to_string()
    };
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, size, size).ok()?;
    {
        let cr = cairo::Context::new(&surface).ok()?;
        let side = f64::from(size);
        let middle = side / 2.0;
        // The white rim keeps the badge apart from the envelope under it.
        cr.arc(middle, middle, middle, 0.0, std::f64::consts::TAU);
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.fill().ok()?;
        cr.arc(middle, middle, middle - (side / 16.0).max(1.0), 0.0, std::f64::consts::TAU);
        // The beak's orange.
        cr.set_source_rgb(0.91, 0.40, 0.05);
        cr.fill().ok()?;
        cr.select_font_face("Segoe UI", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        let mut options = cairo::FontOptions::new().ok()?;
        // Grey antialiasing: ClearType's colored fringes show on a badge.
        options.set_antialias(cairo::Antialias::Gray);
        cr.set_font_options(&options);
        cr.set_font_size(side * if text.len() == 1 { 0.72 } else { 0.56 });
        let mut extents = cr.text_extents(&text).ok()?;
        let room = side * 0.78;
        if extents.width() > room {
            cr.set_font_size(side * 0.56 * room / extents.width());
            extents = cr.text_extents(&text).ok()?;
        }
        cr.move_to(
            middle - extents.width() / 2.0 - extents.x_bearing(),
            middle - extents.height() / 2.0 - extents.y_bearing(),
        );
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.show_text(&text).ok()?;
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let width = size as usize * 4;
    let data = surface.data().ok()?;
    let mut pixels = Vec::with_capacity(width * size as usize);
    for row in data.chunks(stride).take(size as usize) {
        // cairo's pixels are premultiplied; an icon's are not.
        for pixel in row[..width].chunks_exact(4) {
            let alpha = pixel[3];
            let straight = |c: u8| match alpha {
                0 => 0,
                a => ((u16::from(c) * 255 + u16::from(a) / 2) / u16::from(a)).min(255) as u8,
            };
            pixels.extend([straight(pixel[0]), straight(pixel[1]), straight(pixel[2]), alpha]);
        }
    }
    Some(pixels)
}

#[cfg(test)]
mod tests {
    use super::badge_pixels;

    #[test]
    fn the_badge_is_round_and_carries_the_count() {
        let size = 32;
        let pixels = badge_pixels(7, size).expect("cairo draws");
        assert_eq!(pixels.len(), (size * size * 4) as usize);
        let alpha = |x: i32, y: i32| pixels[((y * size + x) * 4 + 3) as usize];
        // The corners stay clear and the middle is filled.
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(size - 1, size - 1), 0);
        assert_eq!(alpha(size / 2, size / 2), 255);
        // Some pixels are white text on the orange.
        let white = pixels
            .chunks_exact(4)
            .filter(|p| p[0] > 230 && p[1] > 230 && p[2] > 230 && p[3] == 255)
            .count();
        assert!(white > 10, "{white}");
        assert!(badge_pixels(1234, 16).is_some());
    }
}
