//! The parts of the system locale glibc keeps that the `libc` crate does
//! not wrap for Linux: which weekday a week starts on, and whether the
//! locale's own clock spells the hour with a period such as "AM" or
//! "PM". Unix reads them through `nl_langinfo`, Windows through
//! `GetLocaleInfoEx`; `mailrs_domain::calendar::{week, clock}` turn what
//! they answer into a weekday or a yes/no, so only the reading itself,
//! which needs a display to see change, lives here.
//!
//! Cached per thread once read, the way
//! [`mailrs_domain::translate::date_locale`] caches the date's own
//! locale: the interface's language is read once at startup and never
//! again, and the system locale that `LC_TIME` names does not change
//! under a running process either.

// windows-gnu's clippy misses that the initializer is already const.
#![allow(clippy::missing_const_for_thread_local)]

use std::cell::Cell;
use std::ffi::c_int;
#[cfg(unix)]
use std::ffi::{CStr, c_char};

use chrono::Weekday;
#[cfg(unix)]
use mailrs_domain::calendar::clock::is_12_hour_pattern;
#[cfg(unix)]
use mailrs_domain::calendar::week::weekday_from_first_weekday_byte;
use mailrs_domain::calendar::week::{self, WeekStart};

#[cfg(unix)]
unsafe extern "C" {
    fn nl_langinfo(item: c_int) -> *mut c_char;
}

/// glibc's `_NL_TIME_FIRST_WEEKDAY`. `<langinfo.h>` builds every item as
/// `_NL_ITEM(category, index) = (category << 16) | index`, with `LC_TIME`
/// category 2 and this item 44 places into it; compiling a one-line
/// program against the headers on Ubuntu 26.04, this build's own target,
/// printed 131176. A libc with no such item, such as musl, answers
/// nothing useful for it either way, and the byte's range check in
/// [`locale_first_weekday`] keeps that safe: an unexpected byte falls
/// back to Monday.
#[cfg(unix)]
const NL_TIME_FIRST_WEEKDAY: c_int = 131_176;

/// glibc's `T_FMT`, computed the same way (131114): the locale's own
/// `strftime` pattern for a bare time of day, such as `%H:%M:%S` or `%r`.
#[cfg(all(unix, not(target_os = "macos")))]
const T_FMT: c_int = 131_114;

// Darwin uses its own langinfo item numbers, from <langinfo.h>.
#[cfg(target_os = "macos")]
const T_FMT: c_int = 3;

/// `LOCALE_IFIRSTDAYOFWEEK` and `LOCALE_STIMEFORMAT`, the same two facts
/// the Unix branch asks `nl_langinfo` for, as Windows names them.
#[cfg(windows)]
const LOCALE_IFIRSTDAYOFWEEK: u32 = 0x000C;
#[cfg(windows)]
const LOCALE_STIMEFORMAT: u32 = 0x1003;

#[cfg(windows)]
unsafe extern "system" {
    fn GetLocaleInfoEx(locale: *const u16, kind: u32, data: *mut u16, len: c_int) -> c_int;
}

thread_local! {
    static FIRST_WEEKDAY: Cell<Option<Weekday>> = const { Cell::new(None) };
    static PREFERS_12_HOUR: Cell<Option<bool>> = const { Cell::new(None) };
    static WEEK_START_SETTING: Cell<WeekStart> = const { Cell::new(WeekStart::Automatic) };
}

#[cfg(unix)]
fn locale_first_weekday() -> Option<Weekday> {
    // SAFETY: `nl_langinfo` returns a pointer glibc owns, valid until the
    // next locale-reading call on this thread; `_NL_TIME_FIRST_WEEKDAY`
    // answers with one byte, not a string, so this reads that byte alone
    // and copies it out before any such call could reuse the buffer.
    let ptr = unsafe { nl_langinfo(NL_TIME_FIRST_WEEKDAY) };
    (!ptr.is_null())
        .then(|| unsafe { *ptr.cast::<u8>() })
        .map(weekday_from_first_weekday_byte)
}

/// The user's first day of week as Windows numbers it: 0 Monday to 6
/// Sunday.
#[cfg(windows)]
fn locale_first_weekday() -> Option<Weekday> {
    let day = match locale_info(LOCALE_IFIRSTDAYOFWEEK)?.as_str() {
        "0" => Weekday::Mon,
        "1" => Weekday::Tue,
        "2" => Weekday::Wed,
        "3" => Weekday::Thu,
        "4" => Weekday::Fri,
        "5" => Weekday::Sat,
        "6" => Weekday::Sun,
        _ => return None,
    };
    Some(day)
}

#[cfg(unix)]
fn locale_prefers_12_hour() -> Option<bool> {
    // SAFETY: as above; `T_FMT` answers a NUL-terminated C string, copied
    // to an owned `String` before any later call could reuse it.
    let ptr = unsafe { nl_langinfo(T_FMT) };
    (!ptr.is_null())
        .then(|| {
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned()
        })
        .map(|pattern| is_12_hour_pattern(&pattern))
}

#[cfg(windows)]
fn locale_prefers_12_hour() -> Option<bool> {
    // A twelve-hour clock spells the hour with a lowercase h ("h:mm:ss
    // tt"); twenty-four-hour locales use HH and no designator.
    locale_info(LOCALE_STIMEFORMAT).map(|pattern| pattern.contains('h'))
}

/// `GetLocaleInfoEx` for the user's default locale, as UTF-16.
#[cfg(windows)]
fn locale_info(kind: u32) -> Option<String> {
    let mut buf = [0u16; 128];
    // SAFETY: buf is writable for its whole length; a null locale name
    // reads the user's default locale.
    let len =
        unsafe { GetLocaleInfoEx(std::ptr::null(), kind, buf.as_mut_ptr(), buf.len() as c_int) };
    (len > 0).then(|| String::from_utf16_lossy(&buf[..len as usize - 1]))
}

/// The weekday the system locale starts a week on, Monday when it names
/// none glibc understands.
pub fn first_weekday() -> Weekday {
    FIRST_WEEKDAY.with(|cell| {
        cell.get().unwrap_or_else(|| {
            let day = locale_first_weekday().unwrap_or(Weekday::Mon);
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
            let prefers = locale_prefers_12_hour().unwrap_or(false);
            cell.set(Some(prefers));
            prefers
        })
    })
}

/// Fixes what [`prefers_12_hour`] answers on this thread, as a test does
/// to check one clock's words rather than this machine's own.
#[cfg(test)]
pub fn set_prefers_12_hour_for_test(prefers: bool) {
    PREFERS_12_HOUR.with(|cell| cell.set(Some(prefers)));
}
