//! A keyboard-accessible editor for the saved conversation toolbar.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, glib};
use mailrs_domain::translate::{fill, gettext};

use super::Slot;
use crate::app::App;
use crate::settings::Change;

fn label(slot: Slot) -> String {
    match slot {
        Slot::Reply => gettext("Reply"),
        Slot::ReplyAll => gettext("Reply All"),
        Slot::Forward => gettext("Forward"),
        Slot::Edit => gettext("Edit Draft"),
        Slot::Archive => gettext("Archive"),
        Slot::Trash => gettext("Move to Trash"),
        Slot::Junk => gettext("Junk"),
        Slot::Read => gettext("Mark Read or Unread"),
        Slot::Flag => gettext("Flag"),
        Slot::Labels => gettext("Labels or Move to Folder"),
        Slot::Tags => gettext("Tags"),
        Slot::Print => gettext("Print…"),
        Slot::Source => gettext("View Source"),
    }
}

struct Editor {
    app: Weak<App>,
    list: gtk::ListBox,
    rows: Vec<(
        Slot,
        adw::ActionRow,
        gtk::CheckButton,
        gtk::Button,
        gtk::Button,
    )>,
    slots: RefCell<Vec<Slot>>,
    updating: Cell<bool>,
    dragging: Cell<Option<Slot>>,
}

impl Editor {
    fn save(&self) {
        if let Some(app) = self.app.upgrade() {
            app.change_settings(Change::Toolbar(Some(self.slots.borrow().clone())));
        }
        self.refresh();
    }

    fn refresh(&self) {
        self.updating.set(true);
        let slots = self.slots.borrow();
        let mut order = slots.clone();
        order.extend(Slot::ALL.into_iter().filter(|s| !slots.contains(s)));
        for (index, slot) in order.iter().enumerate() {
            let (_, row, check, up, down) = self.rows.iter().find(|r| r.0 == *slot).unwrap();
            if row.index() != index as i32 {
                self.list.remove(row);
                self.list.insert(row, index as i32);
            }
            check.set_active(slots.contains(slot));
            up.set_sensitive(index > 0 && index < slots.len());
            down.set_sensitive(index + 1 < slots.len());
        }
        self.updating.set(false);
    }

    fn move_to(&self, slot: Slot, target: Slot, after: bool) {
        {
            let mut slots = self.slots.borrow_mut();
            if slot == target || !slots.contains(&slot) || !slots.contains(&target) {
                return;
            }
            slots.retain(|s| *s != slot);
            let index = slots.iter().position(|s| *s == target).unwrap();
            slots.insert(index + usize::from(after), slot);
        }
        self.save();
    }
}

pub fn present(app: &Rc<App>, parent: &impl IsA<gtk::Widget>) {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let rows = Slot::ALL
        .into_iter()
        .map(|slot| {
            let title = label(slot);
            let check = gtk::CheckButton::new();
            crate::ui::name(&check, &title);
            let row = adw::ActionRow::builder()
                .title(&title)
                .activatable_widget(&check)
                .build();
            row.add_prefix(&check);
            let button = |icon: &str, text: String| {
                let button = gtk::Button::builder()
                    .icon_name(icon)
                    .tooltip_text(&text)
                    .valign(gtk::Align::Center)
                    .css_classes(["flat"])
                    .build();
                crate::ui::name(&button, &text);
                row.add_suffix(&button);
                button
            };
            let up = button(
                "penguin-mail-move-up-symbolic",
                fill(&gettext("Move {button} Up"), &[("button", &title)]),
            );
            let down = button(
                "penguin-mail-move-down-symbolic",
                fill(&gettext("Move {button} Down"), &[("button", &title)]),
            );
            list.append(&row);
            (slot, row, check, up, down)
        })
        .collect();
    let editor = Rc::new(Editor {
        app: Rc::downgrade(app),
        list: list.clone(),
        rows,
        slots: RefCell::new(super::layout(app.settings().toolbar.as_deref())),
        updating: Cell::new(false),
        dragging: Cell::new(None),
    });
    for (slot, row, check, up, down) in &editor.rows {
        let slot = *slot;
        let weak = Rc::downgrade(&editor);
        check.connect_toggled(move |check| {
            let Some(editor) = weak.upgrade() else { return };
            if editor.updating.get() {
                return;
            }
            editor.slots.borrow_mut().retain(|s| *s != slot);
            if check.is_active() {
                editor.slots.borrow_mut().push(slot);
            }
            editor.save();
            check.grab_focus();
        });
        for (button, step) in [(up, -1isize), (down, 1)] {
            let weak = Rc::downgrade(&editor);
            button.connect_clicked(move |_| {
                let Some(editor) = weak.upgrade() else { return };
                let target = {
                    let slots = editor.slots.borrow();
                    slots
                        .iter()
                        .position(|s| *s == slot)
                        .and_then(|i| i.checked_add_signed(step))
                        .and_then(|i| slots.get(i))
                        .copied()
                };
                if let Some(target) = target {
                    editor.move_to(slot, target, step > 0);
                    if let Some((_, _, check, up, down)) = editor.rows.iter().find(|r| r.0 == slot)
                    {
                        if up.is_sensitive() {
                            up.grab_focus();
                        } else if down.is_sensitive() {
                            down.grab_focus();
                        } else {
                            check.grab_focus();
                        }
                    }
                }
            });
        }
        let handle = gtk::Image::from_icon_name("list-drag-handle-symbolic");
        handle.set_tooltip_text(Some(&gettext("Drag to Reorder Buttons")));
        row.add_prefix(&handle);
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::MOVE);
        source.connect_prepare(|_, _, _| {
            Some(gdk::ContentProvider::for_value(
                &"penguin-toolbar".to_value(),
            ))
        });
        let weak = Rc::downgrade(&editor);
        source.connect_drag_begin(move |_, _| {
            if let Some(e) = weak.upgrade() {
                e.dragging.set(Some(slot));
            }
        });
        let weak = Rc::downgrade(&editor);
        source.connect_drag_end(move |_, _, _| {
            if let Some(e) = weak.upgrade() {
                e.dragging.set(None);
            }
        });
        handle.add_controller(source);
        let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
        let (weak, row) = (Rc::downgrade(&editor), row.downgrade());
        target.connect_drop(move |_, value, _, y| {
            let (Some(e), Some(row)) = (weak.upgrade(), row.upgrade()) else {
                return false;
            };
            if value.get::<String>().ok().as_deref() != Some("penguin-toolbar") {
                return false;
            }
            let Some(moved) = e.dragging.get() else {
                return false;
            };
            e.move_to(moved, slot, y > f64::from(row.height()) / 2.0);
            true
        });
        editor
            .rows
            .iter()
            .find(|r| r.0 == slot)
            .unwrap()
            .1
            .add_controller(target);
    }
    editor.refresh();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.set_margin_top(12);
    content.set_margin_bottom(18);
    let hint = gtk::Label::builder()
        .label(gettext(
            "Choose buttons, then drag them or use the arrows to change their order.",
        ))
        .wrap(true)
        .xalign(0.0)
        .build();
    content.append(&hint);
    content.append(&list);
    let reset = gtk::Button::with_label(&gettext("Restore Defaults"));
    let weak = Rc::downgrade(&editor);
    reset.connect_clicked(move |_| {
        if let Some(e) = weak.upgrade() {
            if let Some(app) = e.app.upgrade() {
                app.change_settings(Change::Toolbar(None));
            }
            *e.slots.borrow_mut() = super::layout(None);
            e.refresh();
        }
    });
    content.append(&reset);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&content)
            .build(),
    ));
    let dialog = adw::Dialog::builder()
        .title(gettext("Customize Toolbar"))
        .content_width(480)
        .content_height(620)
        .child(&toolbar)
        .build();
    dialog.connect_closed(move |_| {
        let _ = &editor;
    });
    dialog.present(Some(parent));
}
