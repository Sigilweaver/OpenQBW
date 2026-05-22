//! SA17 date and counter epoch utilities (Phase 6, WP-6C).
//!
//! SA17 stores calendar dates as `u32` "SA-days" where SA-day 0 maps to
//! the Unix date 1980-12-31 (i.e. SA-day 1 = 1981-01-01). The offset to
//! the Unix epoch is therefore:
//!
//! ```text
//!     unix_day = sa_day - DATE_EPOCH_DAYS_BEFORE_UNIX_NEG
//!              = sa_day + DATE_EPOCH_OFFSET_TO_UNIX_DAYS
//! ```
//!
//! Empirically verified on Rock Castle Construction: with offset `4017`
//! the recovered `txn_date_raw` values fall between 1994-08-06 and
//! 2013-03-09, which matches the file's known business-data window.
//!
//! Times of day and timezones are NOT encoded in the date field that
//! line items use; dates are intentionally TZ-naive in the source data.
//! Counter fields (`counter`) are monotone integers within a (type, year)
//! tuple and are unaffected by the epoch.

/// SA-day -> Unix-day offset: `unix_day = sa_day - DATE_EPOCH_DAYS_BEFORE_UNIX`.
/// Equivalently, SA-day 0 = Unix-day -4017 = 1956-12-?? -- but we use the
/// "SA-day 4017 = Unix-day 0" framing throughout the codebase.
pub const DATE_EPOCH_DAYS_BEFORE_UNIX: i64 = 4017;

/// Lower plausibility bound for SA-day values: any `sa_day < 1` is
/// rejected as a missing/zero placeholder.
pub const SA_DAY_MIN_PLAUSIBLE: u32 = 1;

/// Upper plausibility bound (~year 2200) for SA-day values. SA-day
/// 80000 is roughly 2199-12-26 in the Unix calendar.
pub const SA_DAY_MAX_PLAUSIBLE: u32 = 80_000;

/// Convert an SA-day to days since the Unix epoch (1970-01-01).
///
/// Returns `None` for SA-days outside the plausible window
/// `[SA_DAY_MIN_PLAUSIBLE, SA_DAY_MAX_PLAUSIBLE]`.
pub fn sa_day_to_unix_day(sa_day: u32) -> Option<i64> {
    if !(SA_DAY_MIN_PLAUSIBLE..=SA_DAY_MAX_PLAUSIBLE).contains(&sa_day) {
        return None;
    }
    Some(sa_day as i64 - DATE_EPOCH_DAYS_BEFORE_UNIX)
}

/// Inverse of [`sa_day_to_unix_day`].
///
/// Returns `None` if the result would be outside the plausible SA-day
/// window.
pub fn unix_day_to_sa_day(unix_day: i64) -> Option<u32> {
    let v = unix_day + DATE_EPOCH_DAYS_BEFORE_UNIX;
    if v < 0 || v > SA_DAY_MAX_PLAUSIBLE as i64 {
        return None;
    }
    Some(v as u32)
}

/// SA-day -> Unix seconds at midnight UTC. Convenience wrapper for
/// downstream code that uses `chrono` or `time` for formatting.
pub fn sa_day_to_unix_seconds(sa_day: u32) -> Option<i64> {
    sa_day_to_unix_day(sa_day).map(|d| d * 86_400)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1990-01-01 is unix-day 7305. With offset 4017, sa_day is 11322.
    #[test]
    fn sa_day_round_trips_through_1990() {
        let sa_day = unix_day_to_sa_day(7305).unwrap();
        assert_eq!(sa_day, 7305 + 4017);
        assert_eq!(sa_day_to_unix_day(sa_day), Some(7305));
    }

    #[test]
    fn day_zero_and_max_are_rejected_in_sa() {
        assert_eq!(sa_day_to_unix_day(0), None);
        assert_eq!(sa_day_to_unix_day(u32::MAX), None);
    }

    #[test]
    fn upper_plausible_boundary_is_inclusive() {
        assert!(sa_day_to_unix_day(SA_DAY_MAX_PLAUSIBLE).is_some());
        assert_eq!(sa_day_to_unix_day(SA_DAY_MAX_PLAUSIBLE + 1), None);
    }

    /// Rock Castle empirical range: min = 13000 (1994-08-06),
    /// max = 19790 (2013-03-09).
    #[test]
    fn rock_castle_min_max_decode_to_expected_unix_days() {
        // 1994-08-06 = unix day 8983.
        assert_eq!(sa_day_to_unix_day(13_000), Some(8_983));
        // 2013-03-09 = unix day 15773.
        assert_eq!(sa_day_to_unix_day(19_790), Some(15_773));
    }

    #[test]
    fn unix_seconds_aligns_with_unix_day() {
        let secs = sa_day_to_unix_seconds(13_000).unwrap();
        assert_eq!(secs % 86_400, 0);
        assert_eq!(secs / 86_400, 8_983);
    }

    #[test]
    fn negative_unix_day_rejected_for_sa_conversion() {
        assert_eq!(unix_day_to_sa_day(-5000), None);
    }
}
