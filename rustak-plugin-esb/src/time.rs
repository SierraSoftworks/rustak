//! PowerCheck's timestamps: `dd/mm/yyyy HH:MM`, Irish wall-clock, no offset.
//!
//! Ireland keeps UTC in winter and UTC+1 between the last Sundays of March and
//! October, changing at 01:00 UTC. That rule is all a time-zone database would
//! be consulted for here, so it is written out rather than depended on.

use rustak_core::time::{SignedDuration, Timestamp, civil};

const FORMAT: &str = "%d/%m/%Y %H:%M";

/// Reads an Irish wall-clock time as the instant it names.
///
/// [`None`] for the empty string PowerCheck sends in place of a missing value,
/// and for anything else that is not a time. The hour that happens twice in
/// October is read as its first occurrence.
#[must_use]
pub fn parse_dublin(value: &str) -> Option<Timestamp> {
    let local = civil::DateTime::strptime(FORMAT, value.trim()).ok()?;
    let as_utc = Timestamp::from_civil(local)?;
    let as_summer = as_utc - SignedDuration::from_hours(1);

    if is_summer_time(as_summer) {
        Some(as_summer)
    } else {
        Some(as_utc)
    }
}

/// How an instant is written for an operator: Zulu, to the minute.
#[must_use]
pub fn zulu(at: Timestamp) -> String {
    at.strftime("%Y-%m-%d %H:%MZ").to_string()
}

fn is_summer_time(at: Timestamp) -> bool {
    let year = at.civil().year();
    match (changeover(year, 3), changeover(year, 10)) {
        (Some(start), Some(end)) => at >= start && at < end,
        _ => false,
    }
}

/// 01:00 UTC on the last Sunday of a month.
fn changeover(year: i16, month: i8) -> Option<Timestamp> {
    let sunday = civil::Date::new(year, month, 1)
        .ok()?
        .nth_weekday_of_month(-1, civil::Weekday::Sunday)
        .ok()?;

    Timestamp::from_civil(sunday.at(1, 0, 0, 0))
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
        let expected: Timestamp = utc.parse().expect("an instant");

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
