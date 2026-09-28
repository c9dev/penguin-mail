//! Which clock the calendar prints times with: the grid's hour labels,
//! cards, popovers, the editor, search results, agenda rows,
//! notifications and the mail card all print a time of day through
//! [`time_text`], the one function, so GNOME's own `clock-format`
//! setting reaches every one of them alike. Falls back to the system
//! locale's own convention where the schema is missing, as in the
//! Flatpak or on a desktop that is not GNOME.

use std::cell::Cell;

use chrono::NaiveTime;
use gtk::gio;
use gtk::gio::prelude::SettingsExt;
use mailrs_domain::calendar::clock::ClockFormat;
use mailrs_domain::translate::date_locale;

const SCHEMA_ID: &str = "org.gnome.desktop.interface";
const KEY: &str = "clock-format";

thread_local! {
    static CACHED: Cell<Option<ClockFormat>> = const { Cell::new(None) };
}

/// The clock to print times in, read once and cached the way
/// [`mailrs_domain::translate::date_locale`] caches the date's own
/// locale. [`watch`] clears the cache when GNOME's own setting changes.
pub fn current() -> ClockFormat {
    CACHED.with(|cell| {
        cell.get().unwrap_or_else(|| {
            let format = gnome_clock_format().unwrap_or_else(locale_clock_format);
            cell.set(Some(format));
            format
        })
    })
}

/// GNOME's own setting, when its schema is installed. A desktop that is
/// not GNOME, or a Flatpak sandbox without the portal, has no such
/// schema; asking `gio::Settings` for a key it does not have prints a
/// warning and answers every key with its type's default, which would
/// misread an empty string as 24-hour, so this asks the schema source
/// first and only opens the settings once it knows the schema is there.
fn gnome_clock_format() -> Option<ClockFormat> {
    gio::SettingsSchemaSource::default()?.lookup(SCHEMA_ID, true)?;
    match gio::Settings::new(SCHEMA_ID).string(KEY).as_str() {
        "12h" => Some(ClockFormat::Hour12),
        "24h" => Some(ClockFormat::Hour24),
        _ => None,
    }
}

fn locale_clock_format() -> ClockFormat {
    if crate::locale_time::prefers_12_hour() {
        ClockFormat::Hour12
    } else {
        ClockFormat::Hour24
    }
}

/// Watches GNOME's `clock-format` for changes while the app runs,
/// clearing the cache and calling `f` on each one so an open calendar
/// redraws in the new clock. The caller keeps the returned
/// `gio::Settings` alive for as long as it wants the watch to last;
/// `None` where the schema is not installed, which needs no watch since
/// [`current`] can then only ever answer with the locale's own.
pub fn watch(f: impl Fn() + 'static) -> Option<gio::Settings> {
    gio::SettingsSchemaSource::default()?.lookup(SCHEMA_ID, true)?;
    let settings = gio::Settings::new(SCHEMA_ID);
    settings.connect_changed(Some(KEY), move |_, _| {
        CACHED.with(|cell| cell.set(None));
        f();
    });
    Some(settings)
}

/// "15:05" or "3:05 PM": `at`'s time of day in the clock [`current`]
/// names.
pub fn time_text(at: NaiveTime) -> String {
    mailrs_domain::calendar::clock::format_time(at, current(), date_locale())
}

/// Reads `text` back as a time of day, in either clock: a dropdown a
/// person may have typed into as well as picked from.
pub fn parse_time_text(text: &str) -> Option<NaiveTime> {
    mailrs_domain::calendar::clock::parse_time(text)
}
