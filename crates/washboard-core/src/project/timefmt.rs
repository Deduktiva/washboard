//! UTC timestamps as text, hand-rolled.
//!
//! Database columns use a fixed-width RFC 3339 form, `YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ`: always
//! nine fraction digits, always `Z`. Fixed width makes the text sort like the instant, so SQL
//! `ORDER BY sent_at` is correct, and nanoseconds make `SystemTime` round-trip exactly. A date
//! library (`time`, `jiff`) would not buy anything here: there are no time zones, no calendars
//! beyond proleptic Gregorian, and no user-facing formatting (the UI formats dates with
//! Foundation). File names use the compact `YYYYMMDDTHHMMSSZ` form from PLAN §3.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

const NANOS_PER_SEC: i128 = 1_000_000_000;
const SECS_PER_DAY: i64 = 86_400;

struct Parts {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    nanos: u32,
}

fn split(t: SystemTime) -> Parts {
    let nanos: i128 = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i128::try_from(d.as_nanos()).unwrap_or(i128::MAX),
        Err(e) => -i128::try_from(e.duration().as_nanos()).unwrap_or(i128::MAX),
    };
    let secs = i64::try_from(nanos.div_euclid(NANOS_PER_SEC)).unwrap_or(i64::MAX);
    // rem_euclid of a positive divisor is in 0..1e9, fits u32.
    let sub = u32::try_from(nanos.rem_euclid(NANOS_PER_SEC)).unwrap_or(0);
    let days = secs.div_euclid(SECS_PER_DAY);
    let sod = secs.rem_euclid(SECS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    Parts {
        year,
        month,
        day,
        hour: (sod / 3600) as u32,
        minute: (sod % 3600 / 60) as u32,
        second: (sod % 60) as u32,
        nanos: sub,
    }
}

/// `2026-10-06T14:03:12.123456789Z`.
pub(crate) fn format(t: SystemTime) -> String {
    let p = split(t);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        p.year, p.month, p.day, p.hour, p.minute, p.second, p.nanos
    )
}

/// `20261006T140312Z`, for history file stems and `wsdl/.previous/<timestamp>`.
pub(crate) fn compact(t: SystemTime) -> String {
    let p = split(t);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        p.year, p.month, p.day, p.hour, p.minute, p.second
    )
}

/// Parses RFC 3339 (`YYYY-MM-DDTHH:MM:SS[.fraction](Z|±HH:MM)`). `None` for anything else.
///
/// Accepts more than [`format`] produces so hand-edited databases still load.
pub(crate) fn parse(s: &str) -> Option<SystemTime> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    if !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let year = i64::from(digits(&b[0..4])?);
    let month = digits(&b[5..7])?;
    let day = digits(&b[8..10])?;
    let hour = digits(&b[11..13])?;
    let minute = digits(&b[14..16])?;
    let second = digits(&b[17..19])?;
    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let mut rest = &b[19..];
    let mut nanos: u32 = 0;
    if let Some(frac) = rest.strip_prefix(b".") {
        let n = frac.iter().take_while(|c| c.is_ascii_digit()).count();
        if n == 0 {
            return None;
        }
        for (i, c) in frac[..n].iter().enumerate() {
            if i < 9 {
                nanos = nanos * 10 + u32::from(c - b'0');
            }
        }
        for _ in n..9 {
            nanos *= 10;
        }
        rest = &frac[n..];
    }
    let offset_secs: i64 = match rest {
        [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] => {
            let h = i64::from(digits(&[*h1, *h2])?);
            let m = i64::from(digits(&[*m1, *m2])?);
            if h > 23 || m > 59 {
                return None;
            }
            let off = h * 3600 + m * 60;
            if *sign == b'+' { off } else { -off }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    let secs =
        days * SECS_PER_DAY + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second)
            - offset_secs;
    let base = if secs >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_secs(secs.unsigned_abs()))?
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(secs.unsigned_abs()))?
    };
    base.checked_add(Duration::from_nanos(u64::from(nanos)))
}

fn digits(b: &[u8]) -> Option<u32> {
    b.iter().try_fold(0u32, |acc, c| {
        c.is_ascii_digit().then(|| acc * 10 + u32::from(c - b'0'))
    })
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 → (year, month, day).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Inverse of [`civil_from_days`].
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
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
