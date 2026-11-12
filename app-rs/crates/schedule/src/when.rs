//! When a scheduled job runs (#77): local wall-clock times ("09:00" on the chosen days), worked
//! out with the machine's calendar so a daylight-saving change keeps 09:00 at 09:00, plus the
//! words the Scheduled page uses for them. Port of the time half of `src/shared/schedule.ts`.
//! Every function takes the time zone, so the app passes `chrono::Local` and tests a fixed
//! zone; local times resolve the way JavaScript's `Date` does (see [`local_ms`]).

use chrono::{
    DateTime, Datelike, MappedLocalTime, NaiveDate, NaiveDateTime, Offset, TimeDelta, TimeZone,
    Timelike,
};
use wraithgrid_core::config::ScheduleWhen;

const DAY: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTH: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `TIME_PATTERN`: `HH:mm`, 00:00–23:59.
pub fn is_time(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 5
        && b[2] == b':'
        && matches!(
            (b[0], b[1]),
            (b'0'..=b'1', b'0'..=b'9') | (b'2', b'0'..=b'3')
        )
        && (b'0'..=b'5').contains(&b[3])
        && b[4].is_ascii_digit()
}

/// `LOCAL_DATETIME_PATTERN`: `YYYY-MM-DDTHH:mm` (digits only; the date is not range-checked,
/// as in the TS, and rolls over like `new Date(y, m, d)`).
pub fn is_local_datetime(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 16
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[10] == b'T'
        && is_time(&s[11..])
}

/// A local wall time as ms, resolved as JavaScript's `Date` does: in the hour the clocks go back
/// the earlier one; in the hour they skip, read with the offset from before (02:30 is 03:30).
pub fn local_ms<Tz: TimeZone>(tz: &Tz, local: NaiveDateTime) -> i64 {
    match tz.from_local_datetime(&local) {
        MappedLocalTime::Single(t) => t.timestamp_millis(),
        MappedLocalTime::Ambiguous(a, b) => a.timestamp_millis().min(b.timestamp_millis()),
        MappedLocalTime::None => {
            let before = tz
                .offset_from_utc_datetime(&(local - TimeDelta::days(1)))
                .fix();
            (local - TimeDelta::seconds(i64::from(before.local_minus_utc())))
                .and_utc()
                .timestamp_millis()
        }
    }
}

/// ms as local wall time.
pub fn local_of<Tz: TimeZone>(tz: &Tz, ms: i64) -> NaiveDateTime {
    let utc = DateTime::from_timestamp_millis(ms).unwrap_or_default();
    tz.from_utc_datetime(&utc.naive_utc()).naive_local()
}

/// `HH:mm` as (hour, minute); 09:00 when malformed, as the TS `atTime` falls back.
fn hm(time: &str) -> (u32, u32) {
    if !is_time(time) {
        return (9, 0);
    }
    let n = |s: &str| s.parse::<u32>().unwrap_or(0);
    (n(&time[..2]), n(&time[3..]))
}

/// That local day at `time` (`atTime`).
fn at_time<Tz: TimeZone>(tz: &Tz, day: NaiveDate, time: &str) -> i64 {
    let (h, m) = hm(time);
    local_ms(tz, day.and_hms_opt(h, m, 0).unwrap_or_default())
}

/// `new Date(y, m, d)`'s date: a year 0–99 is 19xx, and months and days past the end roll over.
fn js_date(y: i32, m0: i32, d: i32) -> Option<NaiveDate> {
    let y = if (0..=99).contains(&y) { 1900 + y } else { y };
    let first = NaiveDate::from_ymd_opt(y + m0.div_euclid(12), (m0.rem_euclid(12) + 1) as u32, 1)?;
    first.checked_add_signed(TimeDelta::days(i64::from(d) - 1))
}

/// A local `YYYY-MM-DDTHH:mm` as ms; None when malformed (`NaN` in the TS).
pub fn parse_local<Tz: TimeZone>(tz: &Tz, at: &str) -> Option<i64> {
    if !is_local_datetime(at) {
        return None;
    }
    let n = |r: std::ops::Range<usize>| at[r].parse::<i32>().ok();
    let day = js_date(n(0..4)?, n(5..7)? - 1, n(8..10)?)?;
    Some(at_time(tz, day, &at[11..]))
}

/// ms as a local `YYYY-MM-DDTHH:mm`, for the date field.
pub fn to_local<Tz: TimeZone>(tz: &Tz, ms: i64) -> String {
    let d = local_of(tz, ms);
    format!(
        "{}-{:02}-{:02}T{:02}:{:02}",
        d.year(),
        d.month(),
        d.day(),
        d.hour(),
        d.minute()
    )
}

/// The first time strictly after `after` that the job runs; None for a past one-off or a reset
/// not known yet. `reset_at`: the account window's reset time, for `reset` jobs.
pub fn next_run<Tz: TimeZone>(
    tz: &Tz,
    when: &ScheduleWhen,
    after: i64,
    reset_at: Option<i64>,
) -> Option<i64> {
    match when {
        ScheduleWhen::Once { at } => parse_local(tz, at).filter(|t| *t > after),
        ScheduleWhen::Weekly { days, time } => {
            let start = local_of(tz, after).date();
            // Day by day (not +24h), so a daylight-saving day still lands on the wall time.
            (0..=7)
                .filter_map(|i| start.checked_add_signed(TimeDelta::days(i)))
                .filter(|day| days.contains(&i64::from(day.weekday().num_days_from_sunday())))
                .map(|day| at_time(tz, day, time))
                .find(|t| *t > after)
        }
        ScheduleWhen::Hours { every, from } => {
            let every = (*every).clamp(1, 24) as usize;
            let start = local_of(tz, after).date();
            for i in -1..=1 {
                let Some(day) = start.checked_add_signed(TimeDelta::days(i)) else {
                    continue;
                };
                let first = local_of(tz, at_time(tz, day, from));
                for h in (0..24).step_by(every) {
                    let t = local_ms(tz, first + TimeDelta::hours(h as i64));
                    // Only that day's slots; the next day starts again at `from`.
                    if local_of(tz, t).day() != first.day() && h > 0 {
                        break;
                    }
                    if t > after {
                        return Some(t);
                    }
                }
            }
            None
        }
        ScheduleWhen::Reset { .. } => reset_at.filter(|r| *r > after),
    }
}

/// The job form's check on the time: a one-off in the future, a weekly job with a day.
pub fn when_ok<Tz: TimeZone>(tz: &Tz, when: &ScheduleWhen, now: i64) -> bool {
    match when {
        ScheduleWhen::Once { at } => parse_local(tz, at).is_some_and(|t| t > now),
        ScheduleWhen::Weekly { days, .. } => !days.is_empty(),
        _ => true,
    }
}

/// The next `n` runs after `now`, for the job form's preview; none for a reset job.
pub fn upcoming<Tz: TimeZone>(tz: &Tz, when: &ScheduleWhen, now: i64, n: usize) -> Vec<i64> {
    let mut out = Vec::new();
    if matches!(when, ScheduleWhen::Reset { .. }) {
        return out;
    }
    let mut t = now;
    while out.len() < n {
        match next_run(tz, when, t, None) {
            Some(next) => {
                out.push(next);
                t = next;
            }
            None => break,
        }
    }
    out
}

/// A new one-off's default: an hour from now, on the hour (09:05 → 10:00).
pub fn default_once_at<Tz: TimeZone>(tz: &Tz, now: i64) -> String {
    let s = to_local(tz, now + 3_600_000);
    format!("{}00", &s[..s.len().min(14)])
}

fn day_name(d: i64) -> &'static str {
    usize::try_from(d)
        .ok()
        .and_then(|d| DAY.get(d))
        .copied()
        .unwrap_or("undefined")
}

/// "weekdays 09:00", "daily 18:30", "Mon, Thu 09:00", "every 6 hours", "work limit resets".
/// `account_name`: None says "the account".
pub fn describe_when<Tz: TimeZone>(
    tz: &Tz,
    when: &ScheduleWhen,
    account_name: Option<&str>,
) -> String {
    match when {
        ScheduleWhen::Once { at } => match parse_local(tz, at) {
            Some(t) => format!("once {} {}", fmt_day(tz, t), &at[11..]),
            None => "once".into(),
        },
        ScheduleWhen::Weekly { days, time } => {
            // JavaScript's default sort compares as strings.
            let mut days = days.clone();
            days.sort_by_key(|d| d.to_string());
            let joined: Vec<String> = days.iter().map(i64::to_string).collect();
            if days.len() == 7 {
                format!("daily {time}")
            } else if joined.join(",") == "1,2,3,4,5" {
                format!("weekdays {time}")
            } else {
                let names: Vec<&str> = days.iter().map(|d| day_name(*d)).collect();
                format!("{} {time}", names.join(", "))
            }
        }
        ScheduleWhen::Hours { every, .. } => {
            if *every == 1 {
                "every hour".into()
            } else {
                format!("every {every} hours")
            }
        }
        ScheduleWhen::Reset { window } => format!(
            "{} {} limit resets",
            account_name.unwrap_or("the account"),
            if window == "fiveHour" {
                "5-hour"
            } else {
                "weekly"
            }
        ),
    }
}

/// "Mon 27 Oct".
pub fn fmt_day<Tz: TimeZone>(tz: &Tz, ms: i64) -> String {
    let d = local_of(tz, ms);
    format!(
        "{} {} {}",
        DAY[d.weekday().num_days_from_sunday() as usize],
        d.day(),
        MONTH[d.month0() as usize]
    )
}

/// "Mon 09:00", or "09:00" when it is today.
pub fn fmt_when<Tz: TimeZone>(tz: &Tz, ms: i64, now: i64) -> String {
    let d = local_of(tz, ms);
    let hm = format!("{:02}:{:02}", d.hour(), d.minute());
    if local_of(tz, now).date() == d.date() {
        hm
    } else {
        format!("{} {hm}", DAY[d.weekday().num_days_from_sunday() as usize])
    }
}
