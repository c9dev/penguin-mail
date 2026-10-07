//! Keeps the page over its anchor, and the anchor's focus and the page's
//! first-responder status in step.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use block2::RcBlock;
use gtk::glib;
use gtk::glib::translate::ToGlibPtr;
use objc2::rc::Retained;
use objc2_app_kit::{NSImage, NSWindow};
use objc2_core_graphics::CGMutablePath;
use objc2_foundation::{NSError, NSPoint, NSRect, NSSize};
use objc2_quartz_core::{CAShapeLayer, kCAFillRuleEvenOdd};

use super::Inner;

unsafe extern "C" {
    fn gdk_macos_surface_get_native_window(surface: *mut gtk::gdk::ffi::GdkSurface) -> *mut c_void;
}

/// A rectangle as x, y, width and height, measured from the top left.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    fn of(bounds: &gtk::graphene::Rect) -> Rect {
        Rect {
            x: f64::from(bounds.x()),
            y: f64::from(bounds.y()),
            w: f64::from(bounds.width()),
            h: f64::from(bounds.height()),
        }
    }

    fn meet(self, other: Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.w).min(other.x + other.w);
        let bottom = (self.y + self.h).min(other.y + other.h);
        Rect {
            x,
            y,
            w: (right - x).max(0.0),
            h: (bottom - y).max(0.0),
        }
    }

    fn is_empty(self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    pub fn holds(self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[derive(Default)]
pub(super) struct Float {
    /// The window the page is a subview of, once the anchor is in one.
    pub window: RefCell<Option<Retained<NSWindow>>>,
    /// The clip's frame and the page's inside it, as last set.
    last: Cell<Option<(Rect, Rect)>>,
    /// While a dialog is up, the picture stands in for the page.
    pub frozen: Cell<bool>,
    /// Widgets GTK draws over the page, which it lets through.
    pub covers: RefCell<Vec<glib::WeakRef<gtk::Widget>>>,
    /// Where the page is cut away for them, from its top left.
    pub holes: RefCell<Vec<Rect>>,
    /// Whether the anchor holds GTK's focus.
    pub focused: Cell<bool>,
    /// The toast overlays around the anchor, whose toasts are covers too.
    toasts: RefCell<Vec<glib::WeakRef<adw::ToastOverlay>>>,
    /// The window whose dialogs freeze the page, and the handler watching.
    dialogs: RefCell<Option<(glib::WeakRef<gtk::Widget>, glib::SignalHandlerId)>>,
}

/// Starts following the anchor: placing the page on every frame, hiding
/// it when the anchor leaves the screen, and taking the keyboard with it.
pub(super) fn follow(inner: &Rc<Inner>) {
    let weak = Rc::downgrade(inner);
    inner.anchor.add_tick_callback(move |_, _| match weak.upgrade() {
        Some(inner) => {
            place(&inner);
            glib::ControlFlow::Continue
        }
        None => glib::ControlFlow::Break,
    });
    let weak = Rc::downgrade(inner);
    inner.anchor.connect_unmap(move |_| {
        if let Some(inner) = weak.upgrade() {
            hide(&inner);
        }
    });

    // A click on the page reaches it unclaimed, since nothing in GTK
    // claims a click on an empty box, and GDK then passes it to AppKit.
    // On its way past, the anchor takes GTK's focus.
    let press = gtk::EventControllerLegacy::new();
    let anchor = inner.anchor.downgrade();
    press.connect_event(move |_, event| {
        if event.event_type() == gtk::gdk::EventType::ButtonPress
            && let Some(anchor) = anchor.upgrade()
        {
            anchor.grab_focus();
        }
        glib::Propagation::Proceed
    });
    inner.anchor.add_controller(press);

    let focus = gtk::EventControllerFocus::new();
    let weak = Rc::downgrade(inner);
    focus.connect_enter(move |_| {
        if let Some(inner) = weak.upgrade() {
            inner.float.focused.set(true);
            give_keyboard(&inner, true);
        }
    });
    let weak: Weak<Inner> = Rc::downgrade(inner);
    focus.connect_leave(move |_| {
        if let Some(inner) = weak.upgrade() {
            inner.float.focused.set(false);
            give_keyboard(&inner, false);
        }
    });
    inner.anchor.add_controller(focus);
}

/// Makes the page AppKit's first responder, so the keys GTK leaves
/// unclaimed reach it, or hands that back to GDK's own view.
fn give_keyboard(inner: &Inner, to_page: bool) {
    let Some(window) = inner.float.window.borrow().clone() else {
        return;
    };
    if to_page {
        window.makeFirstResponder(Some(&inner.view));
    } else if let Some(content) = window.contentView() {
        window.makeFirstResponder(Some(&content));
    }
}

fn hide(inner: &Inner) {
    inner.clip.setHidden(true);
    inner.float.last.set(None);
}

/// The window behind a GDK surface on macOS.
fn ns_window(surface: &gtk::gdk::Surface) -> Option<Retained<NSWindow>> {
    if !surface.type_().name().starts_with("GdkMacos") {
        return None;
    }
    let raw = unsafe { gdk_macos_surface_get_native_window(surface.to_glib_none().0) };
    unsafe { Retained::retain(raw.cast::<NSWindow>()) }
}

/// Moves the page into `window`, the first time and whenever the anchor
/// lands in another one, and watches that window's dialogs.
fn attach(inner: &Rc<Inner>, window: Retained<NSWindow>) {
    let Some(content) = window.contentView() else {
        return;
    };
    inner.clip.removeFromSuperview();
    content.addSubview(&inner.clip);
    *inner.float.window.borrow_mut() = Some(window);
    if inner.float.focused.get() {
        give_keyboard(inner, true);
    }

    let mut toasts = Vec::new();
    let mut at = inner.anchor.parent();
    while let Some(widget) = at {
        if let Some(overlay) = widget.downcast_ref::<adw::ToastOverlay>() {
            toasts.push(overlay.downgrade());
        }
        at = widget.parent();
    }
    *inner.float.toasts.borrow_mut() = toasts;

    if let Some((root, handler)) = inner.float.dialogs.take()
        && let Some(root) = root.upgrade()
    {
        root.disconnect(handler);
    }
    let Some(root) = inner.anchor.root().map(|root| root.upcast::<gtk::Widget>()) else {
        return;
    };
    if root.find_property("visible-dialog").is_none() {
        return;
    }
    let weak = Rc::downgrade(inner);
    let handler = root.connect_notify_local(Some("visible-dialog"), move |root, _| {
        let Some(inner) = weak.upgrade() else { return };
        match root.property::<Option<adw::Dialog>>("visible-dialog") {
            Some(_) => freeze(&inner),
            None => thaw(&inner),
        }
    });
    *inner.float.dialogs.borrow_mut() = Some((root.downgrade(), handler));
}

/// Puts the page over the anchor, clipped to what of it is on screen.
/// Runs every frame: a pane drag, a window resize or a scroll moves the
/// anchor without a signal on it.
fn place(inner: &Rc<Inner>) {
    let anchor = &inner.anchor;
    if !anchor.is_mapped() {
        return;
    }
    let Some(native) = anchor.native() else { return };
    let Some(window) = native.surface().as_ref().and_then(ns_window) else {
        return;
    };
    let moved = inner
        .float
        .window
        .borrow()
        .as_ref()
        .is_none_or(|before| *before != window);
    if moved {
        attach(inner, window.clone());
    }
    let Some(content) = window.contentView() else {
        return;
    };
    let top = native.upcast_ref::<gtk::Widget>();
    let Some(bounds) = anchor.compute_bounds(top) else {
        return;
    };
    let page = Rect::of(&bounds);
    // A scrolled window, a stack in transition and the like show only
    // part of what they hold.
    let mut seen = page;
    let mut at = anchor.parent();
    while let Some(widget) = at {
        if widget == *top {
            break;
        }
        if widget.overflow() == gtk::Overflow::Hidden
            && let Some(bounds) = widget.compute_bounds(top)
        {
            seen = seen.meet(Rect::of(&bounds));
        }
        at = widget.parent();
    }
    if seen.is_empty() {
        hide(inner);
        return;
    }
    // The native's widget coordinates start inside its client-side
    // shadow; the surface's start at its edge.
    let (dx, dy) = native.surface_transform();
    let clip = Rect {
        x: seen.x + dx,
        y: seen.y + dy,
        ..seen
    };
    let height = content.bounds().size.height;
    let clip_frame = NSRect::new(
        NSPoint::new(
            clip.x,
            match content.isFlipped() {
                true => clip.y,
                false => height - clip.y - clip.h,
            },
        ),
        NSSize::new(clip.w, clip.h),
    );
    // The clip is a plain view, measured from its bottom left.
    let inside = Rect {
        x: page.x - seen.x,
        y: seen.h - (page.y - seen.y) - page.h,
        ..page
    };
    if inner.float.last.get() != Some((clip, inside)) {
        inner.clip.setFrame(clip_frame);
        inner.view.setFrame(NSRect::new(
            NSPoint::new(inside.x, inside.y),
            NSSize::new(inside.w, inside.h),
        ));
        inner.float.last.set(Some((clip, inside)));
    }
    let_through(inner, page);
    if !inner.float.frozen.get() && inner.clip.isHidden() {
        inner.clip.setHidden(false);
    }
}

/// Cuts a hole in the page, in its own shape, for each widget GTK draws
/// over it: the covers, and the toasts of the overlays around it.
fn let_through(inner: &Inner, page: Rect) {
    let whole = Rect {
        x: 0.0,
        y: 0.0,
        w: page.w,
        h: page.h,
    };
    let mut holes = Vec::new();
    let mut add = |widget: &gtk::Widget, round: bool| {
        if !widget.is_mapped() {
            return;
        }
        if let Some(bounds) = widget.compute_bounds(&inner.anchor) {
            // The shape stays whole so a toast reaching past the page's
            // edge keeps its round ends; the layer's bounds cut the rest.
            let shape = Rect::of(&bounds);
            if !shape.meet(whole).is_empty() {
                holes.push((shape, round));
            }
        }
    };
    for cover in inner.float.covers.borrow().iter() {
        if let Some(cover) = cover.upgrade() {
            add(&cover, false);
        }
    }
    for overlay in inner.float.toasts.borrow().iter() {
        let Some(overlay) = overlay.upgrade() else {
            continue;
        };
        let mut child = overlay.first_child();
        while let Some(widget) = child {
            if widget.css_name() == "toast" {
                add(&widget, true);
            }
            child = widget.next_sibling();
        }
    }
    let rects: Vec<Rect> = holes.iter().map(|(hole, _)| hole.meet(whole)).collect();
    if *inner.float.holes.borrow() == rects {
        return;
    }
    let Some(layer) = inner.view.layer() else {
        return;
    };
    if holes.is_empty() {
        unsafe { layer.setMask(None) };
    } else {
        let flipped = layer.contentsAreFlipped();
        let path = CGMutablePath::new();
        unsafe {
            CGMutablePath::add_rect(
                Some(&path),
                std::ptr::null(),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(page.w, page.h)),
            );
            for (hole, round) in &holes {
                let y = match flipped {
                    true => hole.y,
                    false => page.h - hole.y - hole.h,
                };
                // A toast is a pill; the card is square to its place.
                let radius = match round {
                    true => (hole.h / 2.0).min(hole.w / 2.0),
                    false => 0.0,
                };
                CGMutablePath::add_rounded_rect(
                    Some(&path),
                    std::ptr::null(),
                    NSRect::new(NSPoint::new(hole.x, y), NSSize::new(hole.w, hole.h)),
                    radius,
                    radius,
                );
            }
        }
        let mask = CAShapeLayer::new();
        mask.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(page.w, page.h),
        ));
        mask.setPath(Some(&path));
        mask.setFillRule(unsafe { kCAFillRuleEvenOdd });
        unsafe { layer.setMask(Some(&mask)) };
    }
    *inner.float.holes.borrow_mut() = rects;
}

/// Swaps the live page for a picture of it, so GTK can draw a dialog and
/// its dimmed backdrop on top. The page stays up until the picture is
/// ready; the dialog fades in over that time anyway.
fn freeze(inner: &Rc<Inner>) {
    if inner.float.frozen.replace(true) {
        return;
    }
    let weak = Rc::downgrade(inner);
    let done = RcBlock::new(move |image: *mut NSImage, _error: *mut NSError| {
        let Some(inner) = weak.upgrade() else { return };
        if !inner.float.frozen.get() {
            return;
        }
        let texture = unsafe { image.as_ref() }
            .and_then(|image| image.TIFFRepresentation())
            .and_then(|tiff| crate::ui::texture::here(&glib::Bytes::from(&tiff.to_vec()[..])));
        if let Some(texture) = texture {
            inner.picture.set_paintable(Some(&texture));
            inner.picture.set_visible(true);
        }
        inner.clip.setHidden(true);
    });
    unsafe {
        inner
            .view
            .takeSnapshotWithConfiguration_completionHandler(None, &done)
    };
}

fn thaw(inner: &Inner) {
    if !inner.float.frozen.replace(false) {
        return;
    }
    inner.picture.set_visible(false);
    inner.picture.set_paintable(None::<&gtk::gdk::Paintable>);
    // The next frame puts the page back.
    inner.float.last.set(None);
}
