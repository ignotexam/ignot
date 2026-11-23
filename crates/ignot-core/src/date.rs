//! Calendar arithmetic in UTC: just enough to print a round's time and to put dates in a question.

use crate::{Result, invalid};

/// Milliseconds in a day.
pub const DAY_MS: u64 = 86_400_000;

/// Days since 1 January 1970 for a calendar date (Howard Hinnant's `days_from_civil`).
#[must_use]
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y.rem_euclid(400);
    let month_from_march = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The calendar date of a day count since 1 January 1970, as (year, month, day).
#[must_use]
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 { month_from_march + 3 } else { month_from_march - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, small(month), small(day))
}

fn small(n: i64) -> u32 {
    u32::try_from(n).unwrap_or(0)
}

/// The day of the week of a day count: 0 is Sunday, 6 is Saturday.
#[must_use]
pub fn weekday(days: i64) -> u32 {
    small((days + 4).rem_euclid(7))
}

/// `2027-03-09` for a day count since 1 January 1970.
#[must_use]
pub fn iso_date(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `2026-11-24T14:00:00.000Z` for milliseconds since 1 January 1970.
#[must_use]
pub fn rfc3339(ms: u64) -> String {
    let days = i64::try_from(ms / DAY_MS).unwrap_or(i64::MAX);
    let rest = ms % DAY_MS;
    let (h, m, s, milli) = (rest / 3_600_000, rest / 60_000 % 60, rest / 1_000 % 60, rest % 1_000);
    format!("{}T{h:02}:{m:02}:{s:02}.{milli:03}Z", iso_date(days))
}

/// Milliseconds since 1 January 1970 for a time such as `2026-12-03T14:00:00Z`.
///
/// Accepts a date alone (midnight UTC), seconds and a fraction when given, and `Z` or an offset like `+07:00`.
pub fn parse_rfc3339(text: &str) -> Result<u64> {
    let bad = || invalid::<u64>(format!("\"{text}\" is not a time like 2026-12-03T14:00:00Z"));
    let bytes = text.as_bytes();
    let number = |from: usize, len: usize| -> Option<u32> {
        let part = bytes.get(from..from + len)?;
        part.iter().try_fold(0u32, |n, c| c.is_ascii_digit().then(|| n * 10 + u32::from(c - b'0')))
    };
    let (Some(year), Some(month), Some(day)) = (number(0, 4), number(5, 2), number(8, 2)) else {
        return bad();
    };
    if bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return bad();
    }
    let month_length = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => return bad(),
    };
    if day == 0 || day > month_length {
        return bad();
    }
    let mut ms = days_from_civil(i64::from(year), month, day) * 86_400_000;
    if bytes.len() > 10 {
        if !matches!(bytes[10], b'T' | b't' | b' ') {
            return bad();
        }
        let (Some(hour), Some(minute)) = (number(11, 2), number(14, 2)) else {
            return bad();
        };
        if bytes.get(13) != Some(&b':') || hour > 23 || minute > 59 {
            return bad();
        }
        ms += i64::from(hour) * 3_600_000 + i64::from(minute) * 60_000;
        let mut at = 16;
        if bytes.get(at) == Some(&b':') {
            let Some(second) = number(at + 1, 2) else { return bad() };
            if second > 59 {
                return bad();
            }
            ms += i64::from(second) * 1_000;
            at += 3;
            if bytes.get(at) == Some(&b'.') {
                let digits = bytes[at + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
                if digits == 0 {
                    return bad();
                }
                // Only the first three digits are milliseconds; anything finer is dropped.
                let mut milli = 0i64;
                for i in 0..3 {
                    milli =
                        milli * 10 + bytes.get(at + 1 + i).filter(|_| i < digits).map_or(0, |c| i64::from(c - b'0'));
                }
                ms += milli;
                at += 1 + digits;
            }
        }
        match bytes.get(at) {
            Some(b'Z' | b'z') if at + 1 == bytes.len() => {}
            Some(sign @ (b'+' | b'-')) if at + 6 == bytes.len() && bytes.get(at + 3) == Some(&b':') => {
                let (Some(hours), Some(minutes)) = (number(at + 1, 2), number(at + 4, 2)) else {
                    return bad();
                };
                if hours > 23 || minutes > 59 {
                    return bad();
                }
                let offset = i64::from(hours) * 3_600_000 + i64::from(minutes) * 60_000;
                ms += if *sign == b'+' { -offset } else { offset };
            }
            _ => return bad(),
        }
    }
    u64::try_from(ms).or_else(|_| bad())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2027, 3, 1), 20_878);
        for days in (-800_000..800_000).step_by(37) {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(civil_from_days(days_from_civil(2028, 2, 29)), (2028, 2, 29));
    }

    #[test]
    fn weekdays() {
        assert_eq!(weekday(0), 4); // 1 January 1970 was a Thursday
        assert_eq!(weekday(days_from_civil(2026, 11, 24)), 2); // a Tuesday
        assert_eq!(weekday(days_from_civil(2027, 3, 7)), 0); // a Sunday
    }

    #[test]
    fn prints_and_parses_times() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(1_795_528_800_000), "2026-11-24T14:00:00.000Z");
        assert_eq!(parse_rfc3339("2026-11-24T14:00:00Z").ok(), Some(1_795_528_800_000));
        assert_eq!(parse_rfc3339("2026-11-24T14:00:00.000Z").ok(), Some(1_795_528_800_000));
        assert_eq!(parse_rfc3339("2026-11-24T21:00:00+07:00").ok(), Some(1_795_528_800_000));
        assert_eq!(parse_rfc3339("2026-11-24T14:00Z").ok(), Some(1_795_528_800_000));
        assert_eq!(parse_rfc3339("2026-11-24T14:00:00.1239Z").ok(), Some(1_795_528_800_123));
        assert_eq!(parse_rfc3339("2026-11-24").ok(), Some(1_795_478_400_000));
        for ms in [0, 1, 999, 86_399_999, 1_795_528_800_123, 4_102_444_800_000] {
            assert_eq!(parse_rfc3339(&rfc3339(ms)).ok(), Some(ms));
        }
    }

    #[test]
    fn rejects_times_that_do_not_exist() {
        for text in [
            "",
            "tomorrow",
            "2026-13-01",
            "2026-02-30",
            "2027-02-29",
            "2026-11-24T24:00:00Z",
            "2026-11-24T14:00:00",
            "2026-11-24T14:00:00+0100",
            "2026-11-24T14:00:00.Z",
            "1969-12-31T23:59:59Z",
            "2026-11-24X14:00:00Z",
        ] {
            assert!(parse_rfc3339(text).is_err(), "{text}");
        }
    }
}
