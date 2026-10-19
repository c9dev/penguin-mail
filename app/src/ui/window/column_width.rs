//! Resizes the existing split views without replacing their narrow layouts.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};
use mailrs_domain::translate::gettext;

use crate::settings::MailColumn;

/// The separator owns keyboard focus; its split view measures drags,
/// because a right-to-left pane moves its origin while being resized.
pub(super) fn wrap(
    child: &impl IsA<gtk::Widget>,
    split: &impl IsA<gtk::Widget>,
    column: MailColumn,
    changed: impl Fn(i32, bool) -> i32 + 'static,
) -> (gtk::Box, gtk::Separator) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    child.set_hexpand(true);
    row.append(child);
    let divider = gtk::Separator::builder()
        .orientation(gtk::Orientation::Vertical)
        .width_request(8)
        .focusable(true)
        .css_classes(["column-resizer"])
        .build();
    divider.set_cursor_from_name(Some("col-resize"));
    let label = match column {
        MailColumn::Mailboxes => gettext("Mailbox sidebar width"),
        MailColumn::Messages => gettext("Message list width"),
    };
    crate::ui::describe(
        &divider,
        &label,
        &gettext(
            "Drag to resize, or use Left and Right. Home and End choose the smallest and largest widths.",
        ),
    );
    let (least, most) = column.limits();
    divider.update_property(&[
        gtk::accessible::Property::ValueMin(f64::from(least)),
        gtk::accessible::Property::ValueMax(f64::from(most)),
    ]);
    row.append(&divider);
    let weak = divider.downgrade();
    let changed: Rc<dyn Fn(i32, bool) -> i32> = Rc::new(move |width, save| {
        let width = changed(column.clamp(width), save);
        tracing::debug!(?column, width, save, "mail column resized");
        if let Some(divider) = weak.upgrade() {
            divider.update_property(&[gtk::accessible::Property::ValueNow(f64::from(width))]);
        }
        width
    });
    divider.connect_state_flags_changed(|divider, _| {
        if let Some(parent) = divider.parent() {
            divider.update_property(&[gtk::accessible::Property::ValueNow(f64::from(
                parent.width(),
            ))]);
        }
    });
    let start = Rc::new(Cell::new(None));
    let width = Rc::new(Cell::new(0));
    let drag = gtk::GestureDrag::builder()
        .button(1)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    let (pane, handle, began, current) = (
        row.downgrade(),
        divider.downgrade(),
        start.clone(),
        width.clone(),
    );
    drag.connect_drag_begin(move |gesture, x, y| {
        let (Some(pane), Some(handle)) = (pane.upgrade(), handle.upgrade()) else {
            return;
        };
        let on_handle = gesture
            .widget()
            .and_then(|split| {
                split.compute_point(&handle, &gtk::graphene::Point::new(x as f32, y as f32))
            })
            .is_some_and(|point| {
                point.x() >= 0.0
                    && point.x() < handle.width() as f32
                    && point.y() >= 0.0
                    && point.y() < handle.height() as f32
            });
        if !handle.is_visible() || !on_handle {
            began.set(None);
            gesture.set_state(gtk::EventSequenceState::Denied);
            return;
        }
        tracing::debug!(?column, direction = ?pane.direction(), "mail column drag began");
        handle.grab_focus();
        began.set(Some(pane.width()));
        current.set(pane.width());
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    let (pane, began, current, change) = (
        row.downgrade(),
        start.clone(),
        width.clone(),
        changed.clone(),
    );
    drag.connect_drag_update(move |_, dx, _| {
        let (Some(pane), Some(start)) = (pane.upgrade(), began.get()) else {
            return;
        };
        let direction = if pane.direction() == gtk::TextDirection::Rtl {
            -1.0
        } else {
            1.0
        };
        let next = column.clamp(start.saturating_add((dx * direction).round() as i32));
        current.set(change(next, false));
    });
    let change = changed.clone();
    drag.connect_drag_end(move |_, _, _| {
        if start.take().is_some() {
            change(width.get(), true);
        }
    });
    split.add_controller(drag);
    let keys = gtk::EventControllerKey::new();
    let pane = row.downgrade();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        let Some(pane) = pane.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if modifiers.intersects(
            gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::META_MASK,
        ) {
            return glib::Propagation::Proceed;
        }
        let Some(width) = key_width(
            column,
            pane.width(),
            key,
            pane.direction() == gtk::TextDirection::Rtl,
        ) else {
            return glib::Propagation::Proceed;
        };
        changed(width, true);
        glib::Propagation::Stop
    });
    divider.add_controller(keys);
    (row, divider)
}

fn key_width(column: MailColumn, width: i32, key: gdk::Key, rtl: bool) -> Option<i32> {
    let step = if rtl { -12 } else { 12 };
    let (least, most) = column.limits();
    Some(column.clamp(match key {
        gdk::Key::Left => width.saturating_sub(step),
        gdk::Key::Right => width.saturating_add(step),
        gdk::Key::Home => least,
        gdk::Key::End => most,
        _ => return None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_resizing_respects_direction_and_bounds() {
        for column in [MailColumn::Mailboxes, MailColumn::Messages] {
            let (least, most) = column.limits();
            assert_eq!(key_width(column, least, gdk::Key::Left, false), Some(least));
            assert_eq!(key_width(column, most, gdk::Key::Right, false), Some(most));
            assert_eq!(
                key_width(column, least, gdk::Key::Left, true),
                Some(least + 12)
            );
            assert_eq!(
                key_width(column, most, gdk::Key::Right, true),
                Some(most - 12)
            );
            assert_eq!(key_width(column, most, gdk::Key::Home, false), Some(least));
            assert_eq!(key_width(column, least, gdk::Key::End, true), Some(most));
            assert_eq!(key_width(column, least, gdk::Key::Tab, false), None);
        }
    }
}
