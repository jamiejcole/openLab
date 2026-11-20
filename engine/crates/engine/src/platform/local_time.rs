// SPDX-License-Identifier: GPL-3.0-or-later

//! The computer's time zone offset at an instant (wall-clock triggers, TRG-05): `localtime_r`'s
//! `tm_gmtoff` on macOS and Linux, `SystemTimeToTzSpecificLocalTime` on Windows. std has no such
//! call and a date/time crate would be a new dependency, so the foreign calls are declared here.
//! Each may read the zone database, so it never runs on the control thread (`LocalZone`).

/// Seconds to add to UTC second `utc` (since 1970-01-01) for the local time then, daylight
/// saving included; `None` where the platform gives none or the call failed.
pub fn utc_offset_at(utc: i64) -> Option<i64> {
    imp::utc_offset_at(utc).filter(|o| o.abs() <= MAX_OFFSET_SECS)
}

/// No zone is further than this from UTC (the real extremes are -12 h and +14 h); anything
/// beyond is a failed call.
const MAX_OFFSET_SECS: i64 = 18 * 3600;

#[cfg(unix)]
mod imp {
    use std::os::raw::{c_char, c_int, c_long};

    /// `struct tm` on macOS and Linux (64-bit): nine ints, then `tm_gmtoff` (a `long`, 64 bits
    /// there) and `tm_zone`. The
    /// tail is padded well past the real size so a layout mistake could not write out of bounds.
    #[repr(C)]
    struct Tm {
        ints: [c_int; 9],
        gmtoff: i64,
        zone: *const c_char,
        spare: [c_long; 8],
    }

    unsafe extern "C" {
        fn localtime_r(time: *const i64, out: *mut Tm) -> *mut Tm;
        fn tzset();
    }

    pub fn utc_offset_at(utc: i64) -> Option<i64> {
        let mut tm = Tm {
            ints: [0; 9],
            gmtoff: 0,
            zone: std::ptr::null(),
            spare: [0; 8],
        };
        // SAFETY: `tzset` takes no arguments and only refreshes the C library's zone (a zone
        // the operator changed is followed); `utc` is a valid `time_t` (64-bit on every target
        // this builds for) and `tm` a valid, writable, larger-than-needed `struct tm`;
        // `localtime_r` writes only into it and returns null on failure.
        let ok = unsafe {
            tzset();
            !localtime_r(&utc, &mut tm).is_null()
        };
        ok.then_some(tm.gmtoff)
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    unsafe extern "system" {
        fn FileTimeToSystemTime(file: *const FileTime, system: *mut SystemTime) -> i32;
        fn SystemTimeToFileTime(system: *const SystemTime, file: *mut FileTime) -> i32;
        fn SystemTimeToTzSpecificLocalTime(
            zone: *const c_void,
            utc: *const SystemTime,
            local: *mut SystemTime,
        ) -> i32;
    }

    /// Seconds from 1601-01-01 (FILETIME's epoch) to 1970-01-01.
    const EPOCH_DIFF: i64 = 11_644_473_600;

    pub fn utc_offset_at(utc: i64) -> Option<i64> {
        let ticks = u64::try_from(utc.checked_add(EPOCH_DIFF)?.checked_mul(10_000_000)?).ok()?;
        let file = FileTime {
            low: ticks as u32,
            high: (ticks >> 32) as u32,
        };
        let (mut utc_st, mut local_st, mut local_ft) = (
            SystemTime::default(),
            SystemTime::default(),
            FileTime::default(),
        );
        // SAFETY: every pointer is to a valid local of the type the call takes; a null zone
        // means the computer's current zone; each call only writes its out-parameter.
        let ok = unsafe {
            FileTimeToSystemTime(&file, &mut utc_st) != 0
                && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc_st, &mut local_st) != 0
                && SystemTimeToFileTime(&local_st, &mut local_ft) != 0
        };
        if !ok {
            return None;
        }
        let local = (u64::from(local_ft.high) << 32) | u64::from(local_ft.low);
        let local_secs = i64::try_from(local / 10_000_000).ok()? - EPOCH_DIFF;
        Some(local_secs - utc)
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    pub fn utc_offset_at(_utc: i64) -> Option<i64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offset_is_a_whole_quarter_hour_within_a_day_of_utc() {
        if !cfg!(any(unix, windows)) {
            assert_eq!(utc_offset_at(0), None);
            return;
        }
        // 2026-01-01 and 2026-07-01: both halves of the year, so daylight saving is met in
        // whichever hemisphere the machine is set to.
        for utc in [1_767_225_600, 1_782_864_000] {
            let offset = utc_offset_at(utc).expect("this platform reports its zone");
            assert!(offset.abs() <= MAX_OFFSET_SECS, "{offset}");
            assert_eq!(offset % 900, 0, "{offset}");
        }
    }
}
