// SPDX-License-Identifier: GPL-3.0-or-later

//! CPU time this process has used (DX-03), from the operating system's own counters: `/proc`
//! on Linux, `getrusage` on macOS, `GetProcessTimes` on Windows. std has no such call and a
//! crate for it would be a new dependency, so the two foreign calls are declared here.

/// User plus system CPU seconds the process has used so far; `None` where the platform gives
/// no figure or the call failed. Monotonic.
pub fn process_cpu_seconds() -> Option<f64> {
    imp::process_cpu_seconds()
}

#[cfg(target_os = "linux")]
mod imp {
    /// Clock ticks per second in `/proc/<pid>/stat` (`USER_HZ`): 100 on every Linux ABI that
    /// matters, and fixed for userspace by the kernel.
    const USER_HZ: f64 = 100.0;

    pub fn process_cpu_seconds() -> Option<f64> {
        let text = std::fs::read_to_string("/proc/self/stat").ok()?;
        parse_stat(&text)
    }

    /// Fields 14 and 15 (utime, stime), counted after the command name, which is in
    /// parentheses and may itself contain spaces and parentheses.
    pub(super) fn parse_stat(text: &str) -> Option<f64> {
        let after = text.get(text.rfind(')')? + 1..)?;
        let mut fields = after.split_whitespace();
        // `after` starts at field 3 (state); utime is field 14.
        let utime: u64 = fields.nth(11)?.parse().ok()?;
        let stime: u64 = fields.next()?.parse().ok()?;
        Some((utime + stime) as f64 / USER_HZ)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::os::raw::{c_int, c_long};

    /// `struct timeval` on macOS (both architectures): `time_t` and `suseconds_t` (an `i32`,
    /// padded).
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Timeval {
        sec: i64,
        usec: i32,
        pad: i32,
    }

    /// `struct rusage`: two timevals then fourteen longs. The tail is padded well past the
    /// real size so a layout mistake could not write out of bounds.
    #[repr(C)]
    #[derive(Default)]
    struct Rusage {
        utime: Timeval,
        stime: Timeval,
        rest: [c_long; 14],
        spare: [c_long; 16],
    }

    unsafe extern "C" {
        fn getrusage(who: c_int, usage: *mut Rusage) -> c_int;
    }

    const RUSAGE_SELF: c_int = 0;

    pub fn process_cpu_seconds() -> Option<f64> {
        let mut usage = Rusage::default();
        // SAFETY: `usage` is a valid, writable, larger-than-needed `rusage`; `getrusage` only
        // writes into it and has no other preconditions.
        let rc = unsafe { getrusage(RUSAGE_SELF, &mut usage) };
        (rc == 0).then(|| {
            let secs = |t: Timeval| t.sec as f64 + f64::from(t.usec) * 1e-6;
            secs(usage.utime) + secs(usage.stime)
        })
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

    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn GetProcessTimes(
            process: *mut c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }

    pub fn process_cpu_seconds() -> Option<f64> {
        let (mut c, mut e, mut k, mut u) = (
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
        );
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no closing; the four
        // out-pointers are valid, writable `FILETIME`s.
        let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
        // FILETIME counts 100 ns ticks.
        let secs = |t: FileTime| ((u64::from(t.high) << 32) | u64::from(t.low)) as f64 * 1e-7;
        (ok != 0).then(|| secs(k) + secs(u))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    pub fn process_cpu_seconds() -> Option<f64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_time_is_reported_and_never_goes_back() {
        let a = process_cpu_seconds();
        if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
            let a = a.expect("this platform reports CPU time");
            // Burn a little CPU (no clock involved: a fixed amount of work).
            let mut x = 0u64;
            for i in 0..50_000_000u64 {
                x = x.wrapping_add(std::hint::black_box(i) ^ (x >> 3));
            }
            std::hint::black_box(x);
            let b = process_cpu_seconds().expect("still reported");
            assert!(a >= 0.0 && b >= a, "{a} then {b}");
        } else {
            assert!(a.is_none());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_stat_line_parses_even_with_awkward_command_names() {
        // pid (comm) state ppid pgrp session tty tpgid flags minflt cminflt majflt cmajflt
        // utime stime ...
        let line = "42 (a) b) c) S 1 2 3 4 5 6 7 8 9 10 250 150 0 0 20 0 1 0 0";
        assert_eq!(imp::parse_stat(line), Some(4.0));
        assert_eq!(imp::parse_stat("garbage"), None);
        assert_eq!(imp::parse_stat("1 (x) S 1 2"), None);
        assert_eq!(imp::parse_stat("1 (x) S 1 2 3 4 5 6 7 8 9 10 n 5"), None);
    }
}
