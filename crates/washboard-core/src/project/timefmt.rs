//! UTC timestamps as text.
//!
//! Database columns use a fixed-width RFC 3339 form, `YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ`: always
//! nine fraction digits, always `Z`. Fixed width makes the text sort like the instant, so SQL
//! `ORDER BY sent_at` is correct, and nanoseconds make `SystemTime` round-trip exactly. File
//! names use the compact `YYYYMMDDTHHMMSSZ` form from PLAN §3. Calendar math is `jiff`'s.

use std::time::SystemTime;

use jiff::Timestamp;

/// `SystemTime` outside jiff's range (years -9999..=9999) saturates; no real clock gets there.
fn ts(t: SystemTime) -> Timestamp {
    Timestamp::try_from(t).unwrap_or(if t < SystemTime::UNIX_EPOCH {
        Timestamp::MIN
    } else {
        Timestamp::MAX
    })
}

/// `2026-10-06T14:03:12.123456789Z`.
pub(crate) fn format(t: SystemTime) -> String {
    ts(t).strftime("%Y-%m-%dT%H:%M:%S.%9fZ").to_string()
}

/// `20261006T140312Z`, for history file stems and `wsdl/.previous/<timestamp>`.
pub(crate) fn compact(t: SystemTime) -> String {
    ts(t).strftime("%Y%m%dT%H%M%SZ").to_string()
}

/// Parses RFC 3339 (`YYYY-MM-DDTHH:MM:SS[.fraction](Z|±HH:MM)`). `None` for anything else.
///
/// Accepts more than [`format`] produces so hand-edited databases still load.
pub(crate) fn parse(s: &str) -> Option<SystemTime> {
    s.parse::<Timestamp>().ok().map(SystemTime::from)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    #[test]
    fn known_instant() {
        let t = UNIX_EPOCH + Duration::new(1_791_295_392, 5);
        assert_eq!(format(t), "2026-10-06T14:03:12.000000005Z");
        assert_eq!(compact(t), "20261006T140312Z");
        assert_eq!(parse("2026-10-06T14:03:12.000000005Z"), Some(t));
    }

    #[test]
    fn round_trips_including_pre_epoch_and_leap_days() {
        for (secs, nanos, neg) in [
            (0u64, 0u32, false),
            (951_782_400, 999_999_999, false), // 2000-02-29
            (4_107_542_399, 1, false),         // 2100-02-28T23:59:59
            (1, 500, true),
            (86_400 * 365 * 300, 0, true),
        ] {
            let d = Duration::new(secs, nanos);
            let t = if neg { UNIX_EPOCH - d } else { UNIX_EPOCH + d };
            assert_eq!(parse(&format(t)), Some(t), "{}", format(t));
        }
        assert_eq!(
            format(UNIX_EPOCH - Duration::new(1, 0)),
            "1969-12-31T23:59:59.000000000Z"
        );
    }

    #[test]
    fn text_order_matches_time_order() {
        let a = format(UNIX_EPOCH + Duration::new(100, 500_000_000));
        let b = format(UNIX_EPOCH + Duration::new(101, 0));
        assert!(a < b);
    }

    #[test]
    fn parses_offsets_and_short_fractions() {
        let utc = parse("2026-10-06T14:03:12.5Z").expect("parses");
        assert_eq!(parse("2026-10-06T16:03:12.5+02:00"), Some(utc));
        assert_eq!(
            parse("2026-10-06T14:03:12Z"),
            parse("2026-10-06T14:03:12.000Z")
        );
    }

    #[test]
    fn rejects_garbage() {
        for s in [
            "",
            "2026-10-06",
            "2026-13-06T14:03:12Z",
            "2026-02-30T14:03:12Z",
            "2026-10-06T24:03:12Z",
            "2026-10-06T14:03:12",
            "2026-10-06T14:03:12.Z",
            "2026-10-06T14:03:12Zjunk",
            "2026-1x-06T14:03:12Z",
            "2026-10-06T14:03:12+2:00",
        ] {
            assert_eq!(parse(s), None, "{s}");
        }
    }
}
