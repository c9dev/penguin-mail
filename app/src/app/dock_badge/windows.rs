//! The taskbar badge on Windows: `ITaskbarList3::SetOverlayIcon` draws
//! a small overlay at the corner of every window's taskbar button. The
//! unread count is drawn into the overlay icon itself, since the shell
//! takes no number.

use std::cell::{OnceCell, RefCell};

use gtk::prelude::*;
use mailrs_domain::translate::fill_plural;
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON,
};
use windows::core::{HSTRING, PCWSTR, w};

/// The beak's orange, as Windows colors want it: blue, green, red.
const ORANGE: COLORREF = COLORREF(0x000C_66E8);
const WHITE: COLORREF = COLORREF(0x00FF_FFFF);

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

/// An icon-sized orange square with `unread` in white, drawn small like
/// the unread dot on the app's own icon. Counts past 99 say 99+.
unsafe fn count_icon(unread: i64) -> HICON {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    let size_y = size as usize;
    let text = if unread > 99 {
        "99+".to_string()
    } else {
        unread.to_string()
    };
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let color = CreateCompatibleBitmap(screen, size, size);
        SelectObject(dc, color.into());
        let brush = CreateSolidBrush(ORANGE);
        let square = RECT {
            left: 0,
            top: 0,
            right: size,
            bottom: size,
        };
        FillRect(dc, &square, brush);
        let height = match text.len() {
            1 => size * 70 / 100,
            2 => size * 55 / 100,
            _ => size * 42 / 100,
        };
        let font = CreateFontW(
            -height,
            0,
            0,
            0,
            FW_BOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            DEFAULT_QUALITY,
            DEFAULT_PITCH.0 as u32 | FF_DONTCARE.0 as u32,
            w!("Segoe UI"),
        );
        let old_font = SelectObject(dc, font.into());
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, WHITE);
        let mut square_text = square;
        let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        DrawTextW(
            dc,
            &mut wide,
            &mut square_text,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        SelectObject(dc, old_font);
        // The AND mask is all zeros: the square draws opaque everywhere.
        let row = ((size + 15) / 16) as usize * 2;
        let zeros = vec![0u8; row * size_y];
        let mask = CreateBitmap(
            size,
            size,
            1,
            1,
            Some(zeros.as_ptr() as *const core::ffi::c_void),
        );
        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(mask.into());
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(font.into());
        let _ = DeleteObject(brush.into());
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        icon
    }
}
