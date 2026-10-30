//! SA17 date and counter utilities (Phase 6, WP-6C).
//!
//! SA17 stores calendar dates as `u32` "SA-days". This crate converts them
//! with
//!
//! ```text
//!     unix_day = sa_day - DATE_EPOCH_DAYS_BEFORE_UNIX   (4017)
//! ```
//!
//! which places SA-day 0 at 1959-01-02. The offset was fitted so that the
//! Rock Castle Construction sample's dates (SA-days 13000..19790) land
//! between 1994-08-06 and 2013-03-09. It has not been checked against a
//! date known from another source, so a converted date is only as reliable
//! as that fit. Callers that need the raw value use `txn_date_raw`.
//!
//! Line items and headers carry no date column that this crate can locate
//! by schema. The date is found by scanning a short window after the record
//! anchor for a `(date, counter)` pair of `u32`s ([`scan_date_counter`]).
//! The scan reports how it found the value ([`DateSource`]) and refuses to
//! pick when it finds more than one pair.
//!
//! Times of day and timezones are NOT encoded in the date field that
//! line items use; dates are intentionally TZ-naive in the source data.
//! Counter fields (`counter`) are monotone integers within a (type, year)
//! tuple and are unaffected by the epoch.

/// SA-day -> Unix-day offset: `unix_day = sa_day - DATE_EPOCH_DAYS_BEFORE_UNIX`.
/// SA-day 4017 is Unix-day 0 (1970-01-01); SA-day 0 is 1959-01-02.
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

/// Lowest SA-day the date scan accepts as a candidate. A locator bound only:
/// it keeps small integers in neighbouring fields from matching.
pub(crate) const DATE_SCAN_MIN: u32 = 10_000;

/// Highest SA-day the date scan accepts as a candidate. Covers dates past
/// 2013 (the end of the Rock Castle sample) into the 2060s.
pub(crate) const DATE_SCAN_MAX: u32 = 40_000;

/// How a record's date was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DateSource {
    /// Exactly one `(date, counter)` pair sat in the scan window. The
    /// window position, not a schema column, identifies the field.
    WindowScan,
    /// No pair in the scan window.
    NotFound,
    /// Two or more separate pairs sat in the scan window; none was chosen.
    Ambiguous,
}

impl DateSource {
    /// Stable lowercase name, used in exports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WindowScan => "window_scan",
            Self::NotFound => "not_found",
            Self::Ambiguous => "ambiguous",
        }
    }
}

/// Result of [`scan_date_counter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DateScan {
    /// SA-day, set only when `source` is [`DateSource::WindowScan`].
    pub date: Option<u32>,
    /// Counter that followed the date, set under the same condition.
    pub counter: Option<u32>,
    /// How the values were obtained.
    pub source: DateSource,
}

/// Look for a `(date, counter)` pair of little-endian `u32`s starting at
/// byte offsets `from..to` of `body`: a date in
/// [`DATE_SCAN_MIN`]`..=`[`DATE_SCAN_MAX`] followed by a counter in
/// `1..counter_limit`.
///
/// Matches whose offsets are less than 8 bytes apart overlap and count as
/// one pair (the first). Two or more non-overlapping pairs are ambiguous.
pub(crate) fn scan_date_counter(
    body: &[u8],
    from: usize,
    to: usize,
    counter_limit: u32,
) -> DateScan {
    let last_start_excl = to.min((body.len() + 1).saturating_sub(8));
    let mut first: Option<(usize, u32, u32)> = None;
    let mut i = from;
    while i < last_start_excl {
        let d = u32::from_le_bytes([body[i], body[i + 1], body[i + 2], body[i + 3]]);
        let c = u32::from_le_bytes([body[i + 4], body[i + 5], body[i + 6], body[i + 7]]);
        if (DATE_SCAN_MIN..=DATE_SCAN_MAX).contains(&d) && c > 0 && c < counter_limit {
            match first {
                None => first = Some((i, d, c)),
                Some((at, _, _)) if i >= at + 8 => {
                    return DateScan {
                        date: None,
                        counter: None,
                        source: DateSource::Ambiguous,
                    };
                }
                Some(_) => {}
            }
        }
        i += 1;
    }
    match first {
        Some((_, d, c)) => DateScan {
            date: Some(d),
            counter: Some(c),
            source: DateSource::WindowScan,
        },
        None => DateScan {
            date: None,
            counter: None,
            source: DateSource::NotFound,
        },
    }
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

    fn put(body: &mut [u8], at: usize, date: u32, counter: u32) {
        body[at..at + 4].copy_from_slice(&date.to_le_bytes());
        body[at + 4..at + 8].copy_from_slice(&counter.to_le_bytes());
    }

    #[test]
    fn scan_finds_dates_after_2013() {
        // 2024-ish under this crate's conversion: unix day 19800 -> SA-day 23817.
        let mut body = vec![0xAAu8; 64];
        put(&mut body, 8, 23_817, 500);
        let r = scan_date_counter(&body, 0, 64, 1_000_000);
        assert_eq!(r.date, Some(23_817));
        assert_eq!(r.counter, Some(500));
        assert_eq!(r.source, DateSource::WindowScan);
    }

    #[test]
    fn scan_reports_two_separate_pairs_as_ambiguous() {
        let mut body = vec![0xAAu8; 64];
        put(&mut body, 8, 23_817, 500);
        put(&mut body, 24, 15_000, 7);
        let r = scan_date_counter(&body, 0, 64, 1_000_000);
        assert_eq!(r.source, DateSource::Ambiguous);
        assert_eq!((r.date, r.counter), (None, None));
    }

    #[test]
    fn scan_treats_overlapping_pair_as_one() {
        // counter 15_000 is itself in the date range and is followed by 9.
        let mut body = vec![0xAAu8; 64];
        put(&mut body, 8, 15_511, 15_000);
        body[16..20].copy_from_slice(&9u32.to_le_bytes());
        let r = scan_date_counter(&body, 0, 64, 1_000_000);
        assert_eq!(r.source, DateSource::WindowScan);
        assert_eq!(r.date, Some(15_511));
    }

    #[test]
    fn scan_reports_not_found() {
        let body = vec![0xAAu8; 64];
        let r = scan_date_counter(&body, 0, 64, 1_000_000);
        assert_eq!(r.source, DateSource::NotFound);
    }

    #[test]
    fn scan_never_reads_past_the_body() {
        let mut body = vec![0u8; 10];
        put(&mut body, 2, 15_511, 5);
        let r = scan_date_counter(&body, 0, 1000, 1_000_000);
        assert_eq!(r.date, Some(15_511));
        assert_eq!(
            scan_date_counter(&body[..5], 0, 1000, 10).source,
            DateSource::NotFound
        );
    }
}
