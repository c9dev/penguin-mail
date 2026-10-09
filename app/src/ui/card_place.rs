//! The event card's place in the page.
//!
//! The card is a GTK widget, and the message it belongs to is drawn by
//! WebKit. The page keeps an empty place in that message, between its
//! header and its body, and a script in the page says where the place is
//! whenever it moves. The view lays the card over it and tells the page
//! how tall the card is, so the body starts below it.
//!
//! The keyboard follows the page's order. The place takes the focus when
//! Tab reaches it and hands it to the card; Tab past the card's last
//! button goes back into the page after the place, and Shift+Tab the
//! other way. The card does not take the focus in GTK's own order, which
//! would put it after the whole page.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

use crate::web::WebView;

/// Where the page's place for the card is, in CSS pixels from the top
/// left of the web view's visible area.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Place {
    pub x: f64,
    pub y: f64,
    pub width: f64,
}

/// What the page's script says.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Said {
    /// The place is on the page at this spot.
    At(Place),
    /// The page has no place for the card showing: no invitation, or its
    /// message is closed or opening.
    Gone,
    /// The place took the keyboard focus, which it hands to the card.
    Enter,
}

impl Said {
    pub fn parse(message: &str) -> Option<Said> {
        match message {
            "none" => return Some(Said::Gone),
            "enter" => return Some(Said::Enter),
            _ => {}
        }
        let numbers: Vec<f64> = message
            .split(' ')
            .map(|part| part.parse::<f64>().ok().filter(|n| n.is_finite()))
            .collect::<Option<_>>()?;
        let [x, y, width] = numbers[..] else {
            return None;
        };
        Some(Said::At(Place { x, y, width }))
    }
}

/// The card's frame in the web view's own pixels: `x`, `y` and `width`.
/// The page measures in CSS pixels and the widget in its own, so the zoom
/// level stands between the two. The card never gets less than the
/// narrowest it can draw in, `least`; the web view clips what is left.
pub fn frame(place: Place, zoom: f64, least: i32) -> (i32, i32, i32) {
    let pixels = |css: f64| (css * zoom).round() as i32;
    (
        pixels(place.x),
        pixels(place.y),
        pixels(place.width).max(least),
    )
}

/// The script in the page. It says where the place is after anything
/// that can move it: a scroll, a resize, a patch, a message opening or
/// closing, a picture arriving. A message still opening or closing says
/// `none`, since the card cannot fold with the body; the card comes up
/// once the fold has finished moving. It also says `enter` when the place
/// takes the focus, and holds two calls for the view: one sets the place's
/// height, the other moves the focus on from the place when the card lets
/// go of it.
pub const SCRIPT: &str = r#"(function () {
  var post = function (said) {
    window.webkit.messageHandlers.mailrsCard.postMessage(said);
  };
  var said = '', queued = false;
  var measure = function () {
    queued = false;
    var place = document.querySelector('.event-slot'), now = 'none';
    if (place) {
      var message = place.closest('.message'), fold = place.parentElement;
      if (message && message.classList.contains('expanded') &&
          fold.clientHeight + 1 >= fold.scrollHeight) {
        var box = place.getBoundingClientRect();
        if (box.width > 0) now = box.left + ' ' + box.top + ' ' + box.width;
      }
    }
    if (now !== said) { said = now; post(now); }
  };
  var soon = function () {
    if (!queued) { queued = true; requestAnimationFrame(measure); }
  };
  window.addEventListener('scroll', measure, { passive: true });
  window.addEventListener('resize', soon);
  document.addEventListener('transitionrun', soon, true);
  document.addEventListener('transitionend', soon, true);
  new ResizeObserver(soon).observe(document.documentElement);
  new MutationObserver(soon).observe(document.body, {
    childList: true, subtree: true, attributes: true, attributeFilter: ['class']
  });
  document.addEventListener('focusin', function (event) {
    var target = event.target;
    if (target.classList && target.classList.contains('event-slot')) post('enter');
  }, true);
  window.mailrsCardHeight = function (height) {
    document.documentElement.style.setProperty('--event-card', height + 'px');
    soon();
  };
  // Every element Tab reaches, in the page's order, the bodies' shadow
  // roots included, with the place among them.
  var reachable = function (place) {
    var found = [];
    var walk = function (node) {
      for (var child = node.firstElementChild; child; child = child.nextElementSibling) {
        if (child === place || (child.tabIndex >= 0 && !child.disabled &&
            (child.tagName !== 'A' || child.hasAttribute('href')) &&
            child.getClientRects().length > 0)) found.push(child);
        if (child.shadowRoot) walk(child.shadowRoot);
        walk(child);
      }
    };
    walk(document.body);
    return found;
  };
  window.mailrsLeaveCard = function (back) {
    var place = document.querySelector('.event-slot');
    if (!place) return 'none';
    var all = reachable(place), at = all.indexOf(place);
    for (var i = back ? at - 1 : at + 1; i >= 0 && i < all.length; i += back ? -1 : 1) {
      var next = all[i];
      next.focus({ focusVisible: true });
      if (next.getRootNode().activeElement === next) return 'moved';
    }
    return 'none';
  };
  measure();
})();"#;

/// The name the script's messages arrive under.
pub const HANDLER: &str = "mailrsCard";

/// How far one notch of a mouse wheel over the card scrolls the page, in
/// CSS pixels. WebKit moves about this far for one notch over the page.
const WHEEL_STEP: f64 = 60.0;

/// Lays the card over its place in the page, and keeps it there.
///
/// The card sits in a `gtk::Overlay` over the web view. The overlay asks
/// where the card goes each time it lays out, and the answer comes from
/// the place the page last reported. The page scrolls in its own process,
/// so the card follows a scroll a frame or so behind.
///
/// Keyboard: GTK's own order would visit the card after the whole page,
/// so the card takes no focus from GTK's order (`can_focus` is off on its
/// holder). The place in the page takes the focus in the page's order and
/// hands it to the card; Tab past either end of the card hands it back to
/// the page, on the element after or before the place.
pub struct Host {
    pub overlay: gtk::Overlay,
    holder: gtk::Box,
    webview: WebView,
    place: Cell<Option<Place>>,
    /// The card's height the page was last told, in CSS pixels.
    told: Cell<Option<i32>>,
    /// Whether the last Tab pressed in the conversation went backwards,
    /// which says which end of the card the place hands the focus to.
    backward: Cell<bool>,
}

impl Host {
    pub fn new(webview: &WebView, card: &impl IsA<gtk::Widget>) -> Rc<Host> {
        webview.add_script(SCRIPT);
        let holder = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .visible(false)
            .can_focus(false)
            .build();
        holder.append(card);
        let overlay = gtk::Overlay::builder().child(&webview.widget()).build();
        overlay.add_overlay(&holder);
        overlay.set_clip_overlay(&holder, true);
        webview.cover(&holder);
        let host = Rc::new(Host {
            overlay,
            holder,
            webview: webview.clone(),
            place: Cell::new(None),
            told: Cell::new(None),
            backward: Cell::new(false),
        });

        let weak = Rc::downgrade(&host);
        host.overlay.connect_get_child_position(move |_, child| {
            let host = weak.upgrade()?;
            (child == host.holder.upcast_ref::<gtk::Widget>())
                .then(|| host.position())
                .flatten()
        });
        let weak = Rc::downgrade(&host);
        webview.on_message(HANDLER, move |value| {
            let said = Said::parse(value);
            if let (Some(host), Some(said)) = (weak.upgrade(), said) {
                host.heard(said);
            }
        });

        // The holder takes the focus only while the place hands it over,
        // so GTK's own Tab order never lands on the card after the page.
        let focus = gtk::EventControllerFocus::new();
        let holder = host.holder.downgrade();
        focus.connect_leave(move |_| {
            if let Some(holder) = holder.upgrade() {
                holder.set_can_focus(false);
            }
        });
        host.holder.add_controller(focus);
        // A click on the card lets its buttons take the focus, as anywhere
        // else in the window, and a popover opened from it keeps the keys.
        let press = gtk::GestureClick::new();
        press.set_propagation_phase(gtk::PropagationPhase::Capture);
        let holder = host.holder.downgrade();
        press.connect_pressed(move |_, _, _, _| {
            if let Some(holder) = holder.upgrade() {
                holder.set_can_focus(true);
            }
        });
        // A click on the title or the strip takes no focus, so no focus
        // leave comes to set the flag back, and the next Tab from the end
        // of the page would visit the card again. Once the click is over,
        // and a button in it has had its chance to take the focus, the
        // flag goes unless the focus sits inside the card. A click a
        // button claims cancels this gesture rather than releasing it,
        // and `end` comes either way.
        let holder = host.holder.downgrade();
        press.connect_end(move |_, _| {
            let holder = holder.clone();
            glib::idle_add_local_once(move || {
                if let Some(holder) = holder.upgrade()
                    && !keeps_focus_after_click(holder.state_flags())
                {
                    holder.set_can_focus(false);
                }
            });
        });
        host.holder.add_controller(press);

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&host);
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(host) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            host.tab(key, state)
        });
        host.overlay.add_controller(keys);

        // The page under the card does not see the wheel while the
        // pointer is over the card, so the card passes it on.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let weak = Rc::downgrade(&host);
        scroll.connect_scroll(move |scroll, dx, dy| {
            let Some(host) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let step = match scroll.unit() {
                gdk::ScrollUnit::Wheel => WHEEL_STEP,
                _ => 1.0 / host.webview.zoom(),
            };
            host.webview
                .run(&format!("window.scrollBy({},{})", dx * step, dy * step));
            glib::Propagation::Stop
        });
        host.holder.add_controller(scroll);
        host
    }

    /// What the root element of a whole page carries, so the place has the
    /// card's height from the start rather than after a round trip.
    pub fn root_style(&self) -> String {
        match self.told.get() {
            Some(height) => format!(" style=\"--event-card:{height}px\""),
            None => String::new(),
        }
    }

    /// Where the card goes, for the overlay, or `None` to leave it where
    /// GTK would put it while the page has no place showing.
    fn position(&self) -> Option<gdk::Rectangle> {
        let place = self.place.get()?;
        let zoom = self.webview.zoom();
        let (least, _, _, _) = self.holder.measure(gtk::Orientation::Horizontal, -1);
        let (x, y, width) = frame(place, zoom, least);
        let (_, height, _, _) = self.holder.measure(gtk::Orientation::Vertical, width);
        self.tell_height((f64::from(height) / zoom).round() as i32);
        Some(gdk::Rectangle::new(x, y, width, height))
    }

    /// Gives the page the card's height, when it changed. It goes after
    /// the layout under way, which is where the height was found.
    fn tell_height(&self, height: i32) {
        if self.told.replace(Some(height)) == Some(height) {
            return;
        }
        let webview = self.webview.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(webview) = webview.upgrade() {
                webview.run(&format!(
                    "window.mailrsCardHeight&&window.mailrsCardHeight({height})"
                ));
            }
        });
    }

    fn heard(&self, said: Said) {
        match said {
            Said::At(place) => {
                self.place.set(Some(place));
                self.holder.set_visible(true);
                self.overlay.queue_allocate();
            }
            Said::Gone => {
                self.place.set(None);
                self.holder.set_visible(false);
            }
            Said::Enter => {
                let direction = match self.backward.get() {
                    true => gtk::DirectionType::TabBackward,
                    false => gtk::DirectionType::TabForward,
                };
                self.holder.set_can_focus(true);
                if !self.holder.child_focus(direction) {
                    self.holder.set_can_focus(false);
                }
            }
        }
    }

    /// Notes which way Tab went, and moves the focus on from the card at
    /// either end of it.
    fn tab(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        let backward = match key {
            gdk::Key::ISO_Left_Tab => true,
            gdk::Key::Tab | gdk::Key::KP_Tab => state.contains(gdk::ModifierType::SHIFT_MASK),
            _ => return glib::Propagation::Proceed,
        };
        self.backward.set(backward);
        if !self
            .holder
            .state_flags()
            .contains(gtk::StateFlags::FOCUS_WITHIN)
        {
            return glib::Propagation::Proceed;
        }
        let direction = match backward {
            true => gtk::DirectionType::TabBackward,
            false => gtk::DirectionType::TabForward,
        };
        if self.holder.child_focus(direction) {
            return glib::Propagation::Stop;
        }
        let webview = self.webview.clone();
        let root = self.overlay.root();
        self.webview
            .eval(&format!("window.mailrsLeaveCard({backward})"), move |done| {
                if done.is_ok_and(|value| value == "moved") {
                    webview.grab_focus();
                } else if let Some(root) = root {
                    // Nothing in the page on that side: the focus goes on
                    // to whatever follows the conversation.
                    root.child_focus(direction);
                }
            });
        glib::Propagation::Stop
    }
}

/// Whether the card's holder should stay in GTK's Tab order once a click's
/// gesture has ended. The idle callback in [`Host::new`] reads this against
/// the holder's live state, once GTK has settled the click's own focus
/// grab: the holder keeps `can_focus` only while the focus actually landed
/// inside it (on the card or one of its buttons), never for a click that
/// lands on the title or the strip, which take no focus of their own.
fn keeps_focus_after_click(state: gtk::StateFlags) -> bool {
    state.contains(gtk::StateFlags::FOCUS_WITHIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_says_where_the_place_is_in_css_pixels() {
        assert_eq!(
            Said::parse("12.5 -40 690"),
            Some(Said::At(Place {
                x: 12.5,
                y: -40.0,
                width: 690.0
            }))
        );
    }

    #[test]
    fn the_page_says_when_the_place_goes_and_when_it_takes_the_focus() {
        assert_eq!(Said::parse("none"), Some(Said::Gone));
        assert_eq!(Said::parse("enter"), Some(Said::Enter));
    }

    #[test]
    fn anything_else_the_page_says_is_ignored() {
        assert_eq!(Said::parse("12 x 690"), None);
        assert_eq!(Said::parse("12 40"), None);
        assert_eq!(Said::parse("NaN 40 690"), None);
        assert_eq!(Said::parse(""), None);
    }

    #[test]
    fn the_frame_follows_the_zoom() {
        let place = Place {
            x: 36.0,
            y: 200.0,
            width: 700.0,
        };
        assert_eq!(frame(place, 1.25, 300), (45, 250, 875));
    }

    /// A narrow window leaves the place narrower than the card can draw,
    /// and GTK refuses a widget less than its minimum.
    #[test]
    fn the_card_keeps_its_least_width_in_a_narrow_page() {
        let place = Place {
            x: 14.0,
            y: 10.0,
            width: 200.0,
        };
        assert_eq!(frame(place, 1.0, 320), (14, 10, 320));
    }

    #[test]
    fn a_click_that_never_focused_the_card_leaves_the_tab_order() {
        assert!(!keeps_focus_after_click(gtk::StateFlags::NORMAL));
        assert!(!keeps_focus_after_click(gtk::StateFlags::PRELIGHT));
    }

    #[test]
    fn a_click_that_focused_the_card_or_a_button_in_it_keeps_the_tab_order() {
        assert!(keeps_focus_after_click(gtk::StateFlags::FOCUS_WITHIN));
        assert!(keeps_focus_after_click(
            gtk::StateFlags::FOCUS_WITHIN | gtk::StateFlags::PRELIGHT
        ));
    }
}
