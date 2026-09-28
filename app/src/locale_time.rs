//! The parts of the system locale glibc keeps that the `libc` crate does
//! not wrap for Linux: which weekday a week starts on, and whether the
//! locale's own clock spells the hour with a period such as "AM" or
//! "PM". Both come from `nl_langinfo`; `mailrs_domain::calendar::{week,
//! clock}` turn what it answers into a weekday or a yes/no, so only the
//! reading itself, which needs a display to see change, lives here.
//!
//! Cached per thread once read, the way
//! [`mailrs_domain::translate::date_locale`] caches the date's own
//! locale: the interface's language is read once at startup and never
//! again, and the system locale that `LC_TIME` names does not change
//! under a running process either.

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_int};

use chrono::Weekday;
use mailrs_domain::calendar::clock::is_12_hour_pattern;
use mailrs_domain::calendar::week::{self, WeekStart, weekday_from_first_weekday_byte};

unsafe extern "C" {
    fn nl_langinfo(item: c_int) -> *mut c_char;
}

/// glibc's `_NL_TIME_FIRST_WEEKDAY`. `<langinfo.h>` builds every item as
/// `_NL_ITEM(category, index) = (category << 16) | index`, with `LC_TIME`
/// category 2 and this item 44 places into it; compiling a one-line
/// program against the headers on Ubuntu 26.04, this build's own target,
/// printed 131176. A libc with no such item, such as musl, answers
/// nothing useful for it either way, and [`read_first_weekday_byte`]'s
/// range check keeps that safe: an unexpected byte falls back to Monday.
const NL_TIME_FIRST_WEEKDAY: c_int = 131_176;

/// glibc's `T_FMT`, computed the same way (131114): the locale's own
/// `strftime` pattern for a bare time of day, such as `%H:%M:%S` or `%r`.
const T_FMT: c_int = 131_114;

thread_local! {
    static FIRST_WEEKDAY: Cell<Option<Weekday>> = const { Cell::new(None) };
    static PREFERS_12_HOUR: Cell<Option<bool>> = const { Cell::new(None) };
    static WEEK_START_SETTING: Cell<WeekStart> = const { Cell::new(WeekStart::Automatic) };
}

fn read_first_weekday_byte() -> Option<u8> {
    // SAFETY: `nl_langinfo` returns a pointer glibc owns, valid until the
    // next locale-reading call on this thread; `_NL_TIME_FIRST_WEEKDAY`
    // answers with one byte, not a string, so this reads that byte alone
    // and copies it out before any such call could reuse the buffer.
    let ptr = unsafe { nl_langinfo(NL_TIME_FIRST_WEEKDAY) };
    (!ptr.is_null()).then(|| unsafe { *ptr.cast::<u8>() })
}

fn read_t_fmt() -> Option<String> {
    // SAFETY: as above; `T_FMT` answers a NUL-terminated C string, copied
    // to an owned `String` before any later call could reuse it.
    let ptr = unsafe { nl_langinfo(T_FMT) };
    (!ptr.is_null()).then(|| unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned())
}

/// The weekday the system locale starts a week on, Monday when it names
/// none glibc understands.
pub fn first_weekday() -> Weekday {
    FIRST_WEEKDAY.with(|cell| {
        cell.get().unwrap_or_else(|| {
            let day = read_first_weekday_byte()
                .map(weekday_from_first_weekday_byte)
                .unwrap_or(Weekday::Mon);
            cell.set(Some(day));
            day
        })
    })
}

/// Fixes the weekday [`first_weekday`] gives on this thread, as a test
/// does to see a range built on a known week start rather than whatever
/// this machine's own locale happens to name.
#[cfg(test)]
pub fn set_first_weekday_for_test(day: Weekday) {
    FIRST_WEEKDAY.with(|cell| cell.set(Some(day)));
}

/// Sets the "Week Starts On" choice [`week_start_weekday`] resolves
/// against: called once at startup with the saved setting, and again
/// whenever Preferences changes it, so every reader sees the new choice
/// without a restart.
pub fn set_week_start_setting(setting: WeekStart) {
    WEEK_START_SETTING.with(|cell| cell.set(setting));
}

/// The weekday every calendar grid starts its week on:
/// [`mailrs_domain::calendar::week::week_start`] applied to the
/// person's own "Week Starts On" choice and [`first_weekday`]. The one
/// function the Week grid, the Month grid and the mini month all call,
/// so a Preferences change and the locale's own answer read the same
/// way everywhere.
pub fn week_start_weekday() -> Weekday {
    week::week_start(WEEK_START_SETTING.with(Cell::get), first_weekday())
}

/// Whether the system locale's own clock spells the hour in twelve-hour
/// form, for `mailrs::clock_format` to fall back to where GNOME's
/// `clock-format` schema is missing.
pub fn prefers_12_hour() -> bool {
    PREFERS_12_HOUR.with(|cell| {
        cell.get().unwrap_or_else(|| {
            let prefers = read_t_fmt().is_some_and(|pattern| is_12_hour_pattern(&pattern));
            cell.set(Some(prefers));
            prefers
        })
    })
}
