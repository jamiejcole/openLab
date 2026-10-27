// SPDX-License-Identifier: GPL-3.0-or-later

//! UTC wall-clock text from epoch milliseconds, without a date/time dependency (the log and
//! crash files need only ISO-8601 stamps).

/// `2026-10-27T12:00:00.123Z`. Negative times (a clock before 1970) are shown as such.
pub fn iso(ms: i64) -> String {
    let (y, mo, d, h, mi, s, milli) = parts(ms);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z")
}

/// `20261027T120000Z`, safe in a file name.
pub fn compact(ms: i64) -> String {
    let (y, mo, d, h, mi, s, _) = parts(ms);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// (year, month, day, hour, minute, second, millisecond) in UTC.
fn parts(ms: i64) -> (i64, i64, i64, i64, i64, i64, i64) {
    let secs = ms.div_euclid(1000);
    let milli = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, mo, d) = civil_from_days(days);
    (y, mo, d, rem / 3600, rem % 3600 / 60, rem % 60, milli)
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_instants() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
        // A leap day, and the day after.
        assert_eq!(iso(1_709_164_800_000), "2024-02-29T00:00:00.000Z");
        assert_eq!(iso(1_709_251_199_999), "2024-02-29T23:59:59.999Z");
        assert_eq!(iso(1_709_251_200_000), "2024-03-01T00:00:00.000Z");
        // 2100 is not a leap year.
        assert_eq!(iso(4_107_542_400_000), "2100-03-01T00:00:00.000Z");
        assert_eq!(compact(1_700_000_000_123), "20231114T221320Z");
    }

    #[test]
    fn before_the_epoch_is_total() {
        assert_eq!(iso(-1), "1969-12-31T23:59:59.999Z");
        assert!(iso(i64::MIN).len() > 10);
        assert!(iso(i64::MAX).len() > 10);
    }
}
