//! PowerCheck's timestamps: `dd/mm/yyyy HH:MM`, Irish wall-clock, no offset.
//!
//! Ireland keeps UTC in winter and UTC+1 between the last Sundays of March and
//! October, changing at 01:00 UTC. That rule is all a time-zone database would
//! be consulted for here, so it is written out rather than depended on.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};

const FORMAT: &str = "%d/%m/%Y %H:%M";

/// Reads an Irish wall-clock time as the instant it names.
///
/// [`None`] for the empty string PowerCheck sends in place of a missing value,
/// and for anything else that is not a time. The hour that happens twice in
/// October is read as its first occurrence.
#[must_use]
pub fn parse_dublin(value: &str) -> Option<DateTime<Utc>> {
    let local = NaiveDateTime::parse_from_str(value.trim(), FORMAT).ok()?;
    let as_summer = Utc.from_utc_datetime(&(local - Duration::hours(1)));

    if is_summer_time(as_summer) {
        Some(as_summer)
    } else {
        Some(Utc.from_utc_datetime(&local))
    }
}

/// How an instant is written for an operator: Zulu, to the minute.
#[must_use]
pub fn zulu(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%MZ").to_string()
}

fn is_summer_time(at: DateTime<Utc>) -> bool {
    match (changeover(at.year(), 3), changeover(at.year(), 10)) {
        (Some(start), Some(end)) => at >= start && at < end,
        _ => false,
    }
}

/// 01:00 UTC on the last Sunday of a 31-day month.
fn changeover(year: i32, month: u32) -> Option<DateTime<Utc>> {
    let last = NaiveDate::from_ymd_opt(year, month, 31)?;
    let sunday = last - Duration::days(i64::from(last.weekday().num_days_from_sunday()));

    Some(Utc.from_utc_datetime(&sunday.and_hms_opt(1, 0, 0)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::summer("31/07/2026 17:34", "2026-07-31T16:34:00Z")]
    #[case::winter("15/01/2026 09:00", "2026-01-15T09:00:00Z")]
    #[case::before_spring_forward("29/03/2026 00:30", "2026-03-29T00:30:00Z")]
    #[case::after_spring_forward("29/03/2026 02:30", "2026-03-29T01:30:00Z")]
    #[case::before_fall_back("25/10/2026 00:30", "2026-10-24T23:30:00Z")]
    #[case::after_fall_back("25/10/2026 02:30", "2026-10-25T02:30:00Z")]
    #[case::padded(" 15/01/2026 09:00 ", "2026-01-15T09:00:00Z")]
    fn an_irish_wall_clock_time_is_the_instant_it_names(#[case] written: &str, #[case] utc: &str) {
        let expected: DateTime<Utc> = utc.parse().expect("an instant");

        assert_eq!(parse_dublin(written), Some(expected));
    }

    #[rstest]
    #[case::empty("")]
    #[case::iso("2026-07-31T17:34:00Z")]
    #[case::words("tomorrow")]
    fn anything_else_is_no_time_at_all(#[case] written: &str) {
        assert_eq!(parse_dublin(written), None);
    }
}
