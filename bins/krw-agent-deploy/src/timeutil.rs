//! Pure-std UTC timestamp formatting.
//!
//! The deployment controller stamps receipts with UTC timestamps and never
//! depends on local time. `std` has no calendar formatting, so the small
//! civil-from-days conversion (Howard Hinnant's algorithm) is implemented
//! here and unit-tested against fixed epochs.

/// Compact UTC run timestamp, e.g. `20260816T012000Z`.
pub fn format_utc_compact(unix_seconds: u64) -> String {
    let (year, month, day) = civil_from_days(days_from_seconds(unix_seconds));
    let (hour, minute, second) = hms_from_seconds(unix_seconds);
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

/// Inverse of [`format_utc_compact`]: parse a compact UTC timestamp back to
/// unix seconds. Used to recover a run's wall clock from its stamped run
/// directory name (kept deterministic in tests).
pub fn unix_seconds_from_compact(compact: &str) -> Option<u64> {
    let year: i64 = compact.get(0..4)?.parse().ok()?;
    let month: i64 = compact.get(4..6)?.parse().ok()?;
    let day: i64 = compact.get(6..8)?.parse().ok()?;
    if !compact.get(8..9)?.eq_ignore_ascii_case("t") {
        return None;
    }
    let hour: u64 = compact.get(9..11)?.parse().ok()?;
    let minute: u64 = compact.get(11..13)?.parse().ok()?;
    let second: u64 = compact.get(13..15)?.parse().ok()?;
    if !compact.get(15..16)?.eq_ignore_ascii_case("z") {
        return None;
    }
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400
        + i64::try_from(hour * 3_600 + minute * 60 + second).expect("time of day fits i64");
    u64::try_from(seconds).ok()
}

/// ISO-8601 UTC timestamp, e.g. `2026-08-16T01:20:00Z`.
pub fn format_utc_iso(unix_seconds: u64) -> String {
    let (year, month, day) = civil_from_days(days_from_seconds(unix_seconds));
    let (hour, minute, second) = hms_from_seconds(unix_seconds);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn days_from_seconds(unix_seconds: u64) -> i64 {
    // Any u64 second count divided by 86_400 stays far below i64::MAX; the
    // saturating fallback only guards the theoretical overflow.
    i64::try_from(unix_seconds / 86_400).unwrap_or(i64::MAX)
}

fn hms_from_seconds(unix_seconds: u64) -> (u64, u64, u64) {
    let day_seconds = unix_seconds % 86_400;
    (
        day_seconds / 3_600,
        (day_seconds % 3_600) / 60,
        day_seconds % 60,
    )
}

/// Convert days since 1970-01-01 to a proleptic Gregorian date.
///
/// Howard Hinnant's `civil_from_days` algorithm; valid for the full u64
/// second range the controller will ever see.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    // Algorithm guarantees day_index in [1, 31] and month in [1, 12].
    let day_index = doy - (153 * mp + 2) / 5 + 1;
    let d = u32::try_from(day_index).expect("day index within [1, 31]");
    let month_index = if mp < 10 { mp + 3 } else { mp - 9 };
    let m = u32::try_from(month_index).expect("month index within [1, 12]");
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Inverse of `civil_from_days` (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_shifted = (month + 9) % 12;
    let day_of_year = (153 * month_shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_formats_as_1970_utc() {
        assert_eq!(format_utc_compact(0), "19700101T000000Z");
        assert_eq!(format_utc_iso(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn mid_december_2025_matches_known_utc() {
        assert_eq!(format_utc_compact(1_765_000_000), "20251206T054640Z");
        assert_eq!(format_utc_iso(1_765_000_000), "2025-12-06T05:46:40Z");
    }

    #[test]
    fn leap_day_handles_feb_29_2024() {
        // 2024-02-29T23:59:59Z == 1709251199.
        assert_eq!(format_utc_iso(1_709_251_199), "2024-02-29T23:59:59Z");
        assert_eq!(format_utc_iso(1_709_251_200), "2024-03-01T00:00:00Z");
    }

    #[test]
    fn year_boundary_rolls_to_next_day() {
        assert_eq!(format_utc_compact(1_786_843_200), "20260816T012000Z");
    }

    #[test]
    fn compact_timestamp_round_trips_through_unix_seconds() {
        for seconds in [
            0u64,
            1_765_000_000,
            1_786_843_200,
            1_709_251_199,
            4_102_444_800,
        ] {
            let compact = format_utc_compact(seconds);
            assert_eq!(
                unix_seconds_from_compact(&compact),
                Some(seconds),
                "round trip {compact}"
            );
        }
    }

    #[test]
    fn malformed_compact_timestamps_are_rejected() {
        assert_eq!(unix_seconds_from_compact("20260816T012000"), None);
        assert_eq!(unix_seconds_from_compact("20261316T012000Z"), None);
        assert_eq!(unix_seconds_from_compact("not-a-timestamp"), None);
    }
}
