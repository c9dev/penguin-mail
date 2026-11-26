//! The window a due reminder opens when the person chose it over a
//! notification: one row per reminder, each row naming the event and
//! when it runs, with Dismiss, Snooze and Join, and a click that opens
//! the event. The window stays above everything until every row in it
//! is answered, which is the point of choosing it: a notification is
//! easy to miss, a window that waits is not.
//!
//! Everything a row shows and offers comes from
//! [`crate::event_reminders::notice`], and every button activates the
//! app action a notification's own button would, so the window is
//! another skin over the same reminder, not a second path that can
//! drift from it.

use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use mailrs_domain::translate::gettext;

use crate::event_reminders::notice;
use crate::ui;

/// The reminder window: one per process, hidden until a reminder opens
/// it. Closing the window answers everything still in it.
pub struct Reminders {
    app: gio::Application,
    window: adw::Window,
    rows: gtk::Box,
}

impl Reminders {
    /// Builds the window around the app's actions, which each row
    /// activates just as a notification's buttons do.
    pub fn new(app: &gio::Application) -> Rc<Self> {
        let rows = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .build();
        let window = adw::Window::builder()
            .title(gettext("Reminders"))
            .default_width(360)
            .resizable(false)
            .hide_on_close(true)
            .content(&rows)
            .build();
        ui::name(&window, &gettext("Reminders"));
        anchor_above(&window);
        // Closing the window answers everything still in it: the rows
        // go, and each reminder stays shown, off until its snooze or
        // its event brings it again.
        window.connect_hide({
            let rows = rows.clone();
            move |_| {
                while let Some(child) = rows.first_child() {
                    rows.remove(&child);
                }
            }
        });
        Rc::new(Self {
            app: app.clone(),
            window,
            rows,
        })
    }

    /// Puts `due` up as a row and brings the window onto the screen.
    pub fn show(&self, due: &notice::Notice) {
        self.rows.append(&self.row_of(due));
        self.window.present();
    }

    /// One reminder: the event's title and when, a click that opens
    /// the event, and the buttons a notification offers, with Dismiss.
    fn row_of(&self, due: &notice::Notice) -> gtk::Widget {
        let said = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(3)
            .hexpand(true)
            .build();
        said.append(
            &gtk::Label::builder()
                .label(&due.title)
                .wrap(true)
                .xalign(0.0)
                .css_classes(["heading"])
                .build(),
        );
        said.append(
            &gtk::Label::builder()
                .label(&due.body)
                .wrap(true)
                .xalign(0.0)
                .css_classes(["dim-label"])
                .build(),
        );
        // The row's words open the event, the way a notification's
        // own body does, and the row goes with them: reading what the
        // reminder names answers it.
        let event = gtk::Button::builder()
            .child(&said)
            .css_classes(["flat"])
            .build();
        ui::name(&event, &due.title);
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .css_classes(["card"])
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        row.append(&event);

        // Taking the row off, and the window with it once no row is
        // left. A reminder the shown log already holds never comes
        // back on its own, so this is the reminder's end.
        let vanish = {
            let (rows, window, row) = (self.rows.clone(), self.window.clone(), row.clone());
            move || {
                rows.remove(&row);
                if rows.first_child().is_none() {
                    window.set_visible(false);
                }
            }
        };
        event.connect_clicked({
            let app = self.app.clone();
            let target = due.target.clone();
            let vanish = vanish.clone();
            move |_| {
                app.activate_action(notice::SHOW_EVENT, Some(&target.to_variant()));
                vanish();
            }
        });

        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::End)
            .build();
        for button in &due.buttons {
            let made = gtk::Button::builder()
                .label(button.label())
                .css_classes(["flat"])
                .build();
            ui::name(&made, &button.label());
            made.connect_clicked({
                let app = self.app.clone();
                let target = due.target.clone();
                let pressed = button.action();
                // Join opens the call and leaves the row: the meeting
                // link going out is not the reminder answered. Snooze
                // brings the reminder back in five minutes, so the row
                // goes and the window waits for that.
                let vanish = matches!(button, notice::Button::Snooze).then(|| vanish.clone());
                move |_| {
                    app.activate_action(pressed, Some(&target.to_variant()));
                    if let Some(vanish) = &vanish {
                        vanish();
                    }
                }
            });
            buttons.append(&made);
        }
        let dismiss = gtk::Button::builder().label(gettext("Dismiss")).build();
        ui::name(&dismiss, &gettext("Dismiss"));
        dismiss.connect_clicked({
            let vanish = vanish.clone();
            move |_| vanish()
        });
        buttons.append(&dismiss);
        row.append(&buttons);
        row.upcast()
    }
}

/// Holds the window above everything, where the compositor offers the
/// layer shell. Where it does not (GNOME Shell, a plain X11 session),
/// the window comes up presented and the compositor stacks it as it
/// stacks any window.
#[cfg(feature = "layer-shell")]
fn anchor_above(window: &adw::Window) {
    use gtk4_layer_shell::{Edge, Layer, LayerShell};
    if gtk4_layer_shell::is_supported() {
        window.init_layer_shell();
        window.set_layer(Layer::Top);
        window.set_namespace(Some("penguin-mail-reminder"));
        window.set_anchor(Edge::Bottom, true);
        window.set_anchor(Edge::Right, true);
        window.set_anchor(Edge::Left, false);
        window.set_anchor(Edge::Top, false);
        window.set_margin(Edge::Bottom, 12);
        window.set_margin(Edge::Right, 12);
    }
}

/// Without the layer shell there is no way to hold a window above the
/// rest; it comes up presented and the compositor stacks it as any
/// other window.
#[cfg(not(feature = "layer-shell"))]
fn anchor_above(_window: &adw::Window) {}
