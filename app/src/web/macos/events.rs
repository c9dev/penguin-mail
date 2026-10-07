//! The events a page needs that GTK would take on their way to it.
//!
//! GDK pulls every event through `nextEventMatchingMask:`, turns it into a
//! GTK event, and passes the native event on to AppKit, and so to the page,
//! only when no GTK controller claimed it. Clicks arrive that way: nothing
//! in GTK claims a click on the empty anchor. Two kinds would not. Some
//! ancestor of the anchor claims every scroll, and the window binds Tab,
//! the arrows, Space and Return to moving and activating its own focus.
//!
//! NSApp is an instance of [`App`], a subclass made before GTK starts
//! (GDK takes whatever `sharedApplication` returns), whose override of
//! that method sees each event first. A scroll over a page, and one of
//! those keys while a page holds the focus, goes straight to the page's
//! window, where AppKit hands it to the page, and GDK never sees it.
//! AppKit's local event monitors cannot do this: they run later, from
//! `sendEvent:`, and see nothing GDK took.
//!
//! Every other key reaches GTK first, so the window's shortcuts work while
//! reading, and goes on to the page when GTK leaves it, as it does around
//! WebKitGTK on Linux.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use objc2::rc::Retained;
use gtk::gio::prelude::ApplicationExt;
use gtk::{gio, glib};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSDate, NSString};

use super::Inner;

/// The keys a page with the focus takes before GTK, by AppKit key code:
/// Tab, Return, Space, the keypad's Enter, Page Up, Page Down, Home, End
/// and the four arrows.
const PAGE_KEYS: [u16; 12] = [48, 36, 49, 76, 116, 121, 115, 119, 123, 124, 125, 126];

thread_local! {
    /// Every page there is, for the filter to ask.
    static PAGES: RefCell<Vec<Weak<Inner>>> = const { RefCell::new(Vec::new()) };
}

/// Makes NSApp an [`App`]. It runs before GTK starts, so GDK finds it.
pub fn prepare() {
    let Some(_mtm) = MainThreadMarker::new() else {
        return;
    };
    let _: Retained<App> = unsafe { msg_send![App::class(), sharedApplication] };
}

/// Has a click on the Dock icon, or opening the app again from the
/// Finder, bring the window back. Closing the last window leaves Penguin
/// Mail running, and AppKit asks a running app to reopen through an Apple
/// event. It goes in once the first window is up: AppKit puts in handlers
/// of its own as it finishes launching, which GDK has it do then.
pub(super) fn answer_reopen() {
    thread_local! {
        static ANSWERING: Cell<bool> = const { Cell::new(false) };
    }
    if ANSWERING.replace(true) {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    // 'aevt' and 'rapp', the core suite's Reopen Application.
    const CORE: u32 = u32::from_be_bytes(*b"aevt");
    const REOPEN: u32 = u32::from_be_bytes(*b"rapp");
    let manager = NSAppleEventManager::sharedAppleEventManager();
    let _: () = unsafe {
        msg_send![
            &manager,
            setEventHandler: &*app,
            andSelector: sel!(handleReopen:withReplyEvent:),
            forEventClass: CORE,
            andEventID: REOPEN
        ]
    };
}

pub(super) fn register(inner: &Rc<Inner>) {
    PAGES.with(|pages| {
        let mut pages = pages.borrow_mut();
        pages.retain(|page| page.strong_count() > 0);
        pages.push(Rc::downgrade(inner));
    });
}

/// The page `event` belongs to, if one should have it before GTK.
fn owner(event: &NSEvent) -> Option<Rc<Inner>> {
    let wanted: fn(&Inner, &NSEvent) -> bool = match event.r#type() {
        NSEventType::ScrollWheel => takes_scroll,
        NSEventType::KeyDown | NSEventType::KeyUp => takes_key,
        _ => return None,
    };
    PAGES.with(|pages| {
        pages
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .find(|page| on_page_window(page, event) && wanted(page, event))
    })
}

fn on_page_window(inner: &Inner, event: &NSEvent) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let window = inner.float.window.borrow();
    window.is_some() && event.window(mtm).as_ref() == window.as_ref()
}

/// Whether a scroll lands on the live page rather than on a hole in it.
fn takes_scroll(inner: &Inner, event: &NSEvent) -> bool {
    if inner.float.frozen.get() || inner.clip.isHidden() {
        return false;
    }
    let mut at = inner
        .view
        .convertPoint_fromView(event.locationInWindow(), None);
    let bounds = inner.view.bounds();
    // The holes run from the page's top left.
    if !inner.view.isFlipped() {
        at.y = bounds.size.height - at.y;
    }
    let inside = at.x >= 0.0
        && at.y >= 0.0
        && at.x < bounds.size.width
        && at.y < bounds.size.height;
    inside && !inner.float.holes.borrow().iter().any(|hole| hole.holds(at.x, at.y))
}

/// Whether a key is one the page takes first: it has the focus, and the
/// key moves or presses something without a modifier that makes it a
/// shortcut.
fn takes_key(inner: &Inner, event: &NSEvent) -> bool {
    let shortcut = NSEventModifierFlags::Command
        | NSEventModifierFlags::Control
        | NSEventModifierFlags::Option;
    inner.float.focused.get()
        && !inner.float.frozen.get()
        && !event.modifierFlags().intersects(shortcut)
        && PAGE_KEYS.contains(&event.keyCode())
}

define_class!(
    #[unsafe(super(NSApplication))]
    #[thread_kind = MainThreadOnly]
    #[name = "PenguinMailApplication"]
    pub(super) struct App;

    impl App {
        #[unsafe(method(handleReopen:withReplyEvent:))]
        fn reopen(&self, _event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            glib::idle_add_local_once(|| {
                if let Some(app) = gio::Application::default() {
                    app.activate();
                }
            });
        }

        #[unsafe(method(nextEventMatchingMask:untilDate:inMode:dequeue:))]
        fn next_event(
            &self,
            mask: NSEventMask,
            until: Option<&NSDate>,
            mode: &NSString,
            dequeue: bool,
        ) -> *mut NSEvent {
            let event: Option<Retained<NSEvent>> = unsafe {
                msg_send![super(self), nextEventMatchingMask: mask, untilDate: until, inMode: mode, dequeue: dequeue]
            };
            let Some(event) = event else {
                return std::ptr::null_mut();
            };
            if dequeue && let Some(page) = owner(&event) {
                if let Some(window) = page.float.window.borrow().as_ref() {
                    window.sendEvent(&event);
                }
                return std::ptr::null_mut();
            }
            Retained::autorelease_return(event)
        }
    }
);
