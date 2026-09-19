//! Asking for a date and time, for Send Later and Remind Me.

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::EpochMillis;

/// Shows a calendar and a time, an hour from now to start. Returns the
/// chosen moment, or `None` when cancelled or when the local clock skips
/// that time.
pub async fn pick_time(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    confirm: &str,
) -> Option<EpochMillis> {
    let now = glib::DateTime::now_local().expect("the clock reads");
    let start = now.add_hours(1).unwrap_or_else(|_| now.clone());
    let calendar = gtk::Calendar::new();
    calendar.set_date(&start);
    let hour = gtk::SpinButton::with_range(0.0, 23.0, 1.0);
    hour.set_value(start.hour() as f64);
    let minute = gtk::SpinButton::with_range(0.0, 55.0, 5.0);
    minute.set_value(0.0);
    for spin in [&hour, &minute] {
        spin.set_numeric(true);
        spin.set_wrap(true);
        spin.set_orientation(gtk::Orientation::Vertical);
        spin.connect_output(|spin| {
            spin.set_text(&format!("{:02}", spin.value() as i32));
            glib::Propagation::Stop
        });
    }
    let time = gtk::Box::builder()
        .spacing(6)
        .halign(gtk::Align::Center)
        .build();
    time.append(&hour);
    time.append(&gtk::Label::new(Some(":")));
    time.append(&minute);
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    content.append(&calendar);
    content.append(&time);
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .extra_child(&content)
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("pick", confirm)]);
    dialog.set_response_appearance("pick", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("pick"));
    dialog.set_close_response("cancel");
    if dialog.choose_future(Some(parent)).await != "pick" {
        return None;
    }
    let day = calendar.date();
    glib::DateTime::from_local(
        day.year(),
        day.month(),
        day.day_of_month(),
        hour.value() as i32,
        minute.value() as i32,
        0.0,
    )
    .ok()
    .map(|at| at.to_unix() * 1000)
}
