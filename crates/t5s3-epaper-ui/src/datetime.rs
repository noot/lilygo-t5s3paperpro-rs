use t5s3_epaper_core::Clock;

pub(crate) const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
pub(crate) const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

// read the RTC and return (hours, minutes) of local time, or None if it has
// not been set to a real wall-clock time yet (i.e. no successful NTP sync).
pub(crate) fn status_time(clock: &mut Clock) -> Option<(u32, u32)> {
    let secs = clock.now_us() / 1_000_000;
    // ~year 2020; below this the RTC is just counting up from boot, unsynced.
    if secs > 1_600_000_000 {
        let sod = (secs % 86_400) as u32;
        Some((sod / 3600, (sod % 3600) / 60))
    } else {
        None
    }
}

// read the RTC and return (day-of-week, year, month, day) of local time, or
// None if it has not been synced to a real wall-clock time yet.
pub(crate) fn status_date(clock: &mut Clock) -> Option<(usize, i64, u32, u32)> {
    let secs = clock.now_us() / 1_000_000;
    if secs > 1_600_000_000 {
        let days = (secs / 86_400) as i64;
        let (year, month, day) = civil_from_days(days);
        let dow = ((days + 4) % 7) as usize; // 1970-01-01 was a Thursday; 0 = Sunday
        Some((dow, year, month, day))
    } else {
        None
    }
}

// gregorian (year, month, day) from days since the unix epoch.
// see http://howardhinnant.github.io/date_algorithms.html#civil_from_days
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (year + i64::from(month <= 2), month, day)
}
