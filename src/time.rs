//! Times as instants, shown in local time. Plain local date-times break where local time does:
//! a span across a clock change comes out an hour off, and after a flight to another time zone
//! every stored time would quietly move by the difference.

use chrono::{DateTime, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};

pub type Time = DateTime<Local>;

/// The instant a local wall-clock reading names: the first of the two in the hour the clocks go
/// back, and an hour on in the hour they skip.
pub fn local(t: NaiveDateTime) -> Time {
    Local.from_local_datetime(&t).earliest()
        .or_else(|| Local.from_local_datetime(&(t + Duration::hours(1))).earliest())
        .unwrap_or_else(|| Local.from_utc_datetime(&t))
}

/// Local midnight at the start of `day`.
pub fn midnight(day: NaiveDate) -> Time { local(day.and_time(NaiveTime::MIN)) }

/// A local wall-clock time, for the mock, scenes and tests.
pub fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> Time {
    local(NaiveDate::from_ymd_opt(year, month, day).and_then(|d| d.and_hms_opt(hour, minute, 0)).expect("a valid date and time"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_reading_maps_to_one_instant_even_around_clock_changes() {
        // Wherever the tests run, a plain reading round-trips.
        let t = at(2026, 11, 10, 14, 5);
        assert_eq!(t.naive_local(), NaiveDate::from_ymd_opt(2026, 11, 10).unwrap().and_hms_opt(14, 5, 0).unwrap());
        // A day is from one local midnight to the next, however long that is.
        let d = NaiveDate::from_ymd_opt(2026, 3, 29).unwrap();
        assert!(midnight(d.succ_opt().unwrap()) - midnight(d) >= Duration::hours(23));
    }

    #[test]
    fn a_span_across_a_clock_change_counts_real_time() {
        // In Europe this weekend has 49 hours; `TZ=Europe/Copenhagen cargo test` checks that.
        let (from, to) = (at(2026, 10, 24, 9, 0), at(2026, 10, 26, 9, 0));
        let change = from.offset().local_minus_utc() - to.offset().local_minus_utc();
        assert_eq!(to - from, Duration::hours(48) + Duration::seconds(change.into()));
    }
}
