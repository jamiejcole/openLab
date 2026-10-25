// SPDX-License-Identifier: GPL-3.0-or-later

//! Rolling backups (NFR-06): file naming and which old backups to delete. Times are passed in
//! as milliseconds since the Unix epoch (UTC); listing and deleting files is the engine's job.

/// Default number of backups kept per workspace.
pub const DEFAULT_BACKUP_KEEP: u32 = 20;
/// Largest `keep` accepted from settings.
pub const MAX_BACKUP_KEEP: u32 = 1000;

const EXTENSION: &str = ".openlab";
const MS_PER_DAY: i64 = 86_400_000;
/// 0000-01-01T00:00:00.000Z and 9999-12-31T23:59:59.999Z: the range four-digit years can name.
const MIN_MS: i64 = -62_167_219_200_000;
const MAX_MS: i64 = 253_402_300_799_999;

/// File name of a backup made at `ms`: `YYYYMMDD-HHMMSS-mmm.openlab`, UTC. Names sort in time
/// order. Times outside years 0000 to 9999 are clamped.
pub fn backup_file_name(ms: i64) -> String {
    let ms = ms.clamp(MIN_MS, MAX_MS);
    let days = ms.div_euclid(MS_PER_DAY);
    let in_day = ms.rem_euclid(MS_PER_DAY);
    let (y, m, d) = civil_from_days(days);
    let (h, min, s, milli) = (
        in_day / 3_600_000,
        in_day / 60_000 % 60,
        in_day / 1000 % 60,
        in_day % 1000,
    );
    format!("{y:04}{m:02}{d:02}-{h:02}{min:02}{s:02}-{milli:03}{EXTENSION}")
}

/// The time encoded in a name made by [`backup_file_name`]; `None` for any other name.
pub fn parse_backup_name(name: &str) -> Option<i64> {
    let stem = name.strip_suffix(EXTENSION)?;
    let digits = |s: &str| -> Option<i64> {
        (s.bytes().all(|b| b.is_ascii_digit()) && !s.is_empty()).then(|| s.parse().ok())?
    };
    let (date, rest) = stem.split_once('-')?;
    let (time, milli) = rest.split_once('-')?;
    if date.len() != 8 || time.len() != 6 || milli.len() != 3 {
        return None;
    }
    let (y, m, d) = (
        digits(date.get(..4)?)?,
        digits(date.get(4..6)?)?,
        digits(date.get(6..8)?)?,
    );
    let (h, min, s) = (
        digits(time.get(..2)?)?,
        digits(time.get(2..4)?)?,
        digits(time.get(4..6)?)?,
    );
    let ms = days_from_civil(y, m, d)
        .checked_mul(MS_PER_DAY)?
        .checked_add(h * 3_600_000 + min * 60_000 + s * 1000 + digits(milli)?)?;
    // Rejects impossible dates and times (Feb 30, 25:00) by requiring an exact round trip.
    (backup_file_name(ms) == name).then_some(ms)
}

/// Which of `names` to delete so that only the `keep` newest backups remain. Names that are not
/// backups (anything [`parse_backup_name`] rejects) are never selected, so a user's own files
/// in the folder are safe. The result is oldest first, without duplicates.
pub fn select_backups_to_delete(names: &[String], keep: usize) -> Vec<String> {
    let mut backups: Vec<(i64, &String)> = names
        .iter()
        .filter_map(|name| parse_backup_name(name).map(|ms| (ms, name)))
        .collect();
    // Names are unique per time, so sorting by time orders them; dedup handles a repeated name.
    backups.sort();
    backups.dedup();
    let excess = backups.len().saturating_sub(keep);
    backups
        .into_iter()
        .take(excess)
        .map(|(_, name)| name.clone())
        .collect()
}

// Howard Hinnant's civil-date algorithms (public domain), on days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn names_encode_utc_time() {
        assert_eq!(backup_file_name(0), "19700101-000000-000.openlab");
        assert_eq!(
            backup_file_name(1_700_000_000_123),
            "20231114-221320-123.openlab"
        );
        // Leap day and a pre-epoch time.
        assert_eq!(
            backup_file_name(951_782_400_000),
            "20000229-000000-000.openlab"
        );
        assert_eq!(backup_file_name(-1), "19691231-235959-999.openlab");
    }

    #[test]
    fn out_of_range_times_clamp() {
        assert_eq!(backup_file_name(i64::MAX), "99991231-235959-999.openlab");
        assert_eq!(backup_file_name(i64::MIN), "00000101-000000-000.openlab");
    }

    #[test]
    fn name_round_trip() {
        for ms in [
            0,
            1,
            999,
            86_399_999,
            1_700_000_000_123,
            -1,
            -86_400_000,
            MAX_MS,
            MIN_MS,
        ] {
            assert_eq!(parse_backup_name(&backup_file_name(ms)), Some(ms), "{ms}");
        }
    }

    #[test]
    fn foreign_names_do_not_parse() {
        for name in [
            "",
            ".openlab",
            "notes.txt",
            "20231114-221320-123.OPENLAB",
            "20231114-221320-123.openlab.tmp",
            "20231114-221320-12.openlab",
            "20231114-221320.openlab",
            "2023111-4221320-123.openlab",
            "20230230-000000-000.openlab",
            "20231314-000000-000.openlab",
            "20231114-250000-000.openlab",
            "20231114-006000-000.openlab",
            "+0231114-221320-123.openlab",
            "２０２３１１１４-221320-123.openlab",
            "Backup of Show.openlab",
            "20231114-221320-123 copy.openlab",
        ] {
            assert_eq!(parse_backup_name(name), None, "{name:?}");
        }
    }

    #[test]
    fn keeps_exactly_the_newest() {
        let all: Vec<String> = (0..25).map(|i| backup_file_name(i * 1000)).collect();
        let doomed = select_backups_to_delete(&all, 20);
        assert_eq!(doomed, all[..5].to_vec());
        // Input order does not matter.
        let mut shuffled = all.clone();
        shuffled.reverse();
        assert_eq!(select_backups_to_delete(&shuffled, 20), all[..5].to_vec());
        assert!(select_backups_to_delete(&all, 25).is_empty());
        assert!(select_backups_to_delete(&all, 100).is_empty());
        assert_eq!(select_backups_to_delete(&all, 0).len(), 25);
    }

    #[test]
    fn odd_names_are_left_alone() {
        let mut list = names(&["readme.txt", ".DS_Store", "ünï.openlab", "x.openlab.tmp"]);
        list.extend((0..3).map(|i| backup_file_name(i * 1000)));
        list.push(backup_file_name(1000)); // repeated
        let doomed = select_backups_to_delete(&list, 2);
        assert_eq!(doomed, vec![backup_file_name(0)]);
        assert!(select_backups_to_delete(&[], 5).is_empty());
    }
}
