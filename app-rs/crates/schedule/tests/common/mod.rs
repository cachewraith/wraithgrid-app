//! A time zone with daylight saving for the tests, so they run the same on any machine:
//! Europe/Berlin's rules (CET +1, CEST +2 from the last Sunday of March to the last Sunday of
//! October, switching at 01:00 UTC), as the TS tests set `TZ=Europe/Berlin`.
#![allow(dead_code)]

use chrono::{
    Datelike, FixedOffset, MappedLocalTime, NaiveDate, NaiveDateTime, TimeDelta, TimeZone,
};
use wraithgrid_core::config::{ScheduleTarget, ScheduleWhen, ScheduledJob};

#[derive(Clone, Copy, Debug)]
pub struct Berlin;

fn last_sunday_1utc(year: i32, month: u32) -> NaiveDateTime {
    let mut d = NaiveDate::from_ymd_opt(year, month + 1, 1).unwrap() - TimeDelta::days(1);
    while d.weekday().num_days_from_sunday() != 0 {
        d -= TimeDelta::days(1);
    }
    d.and_hms_opt(1, 0, 0).unwrap()
}

fn offset_at(utc: &NaiveDateTime) -> FixedOffset {
    let y = utc.year();
    let summer = *utc >= last_sunday_1utc(y, 3) && *utc < last_sunday_1utc(y, 10);
    FixedOffset::east_opt(if summer { 7200 } else { 3600 }).unwrap()
}

impl TimeZone for Berlin {
    type Offset = FixedOffset;

    fn from_offset(_: &FixedOffset) -> Self {
        Berlin
    }

    fn offset_from_local_date(&self, local: &NaiveDate) -> MappedLocalTime<FixedOffset> {
        self.offset_from_local_datetime(&local.and_hms_opt(0, 0, 0).unwrap())
    }

    fn offset_from_local_datetime(&self, local: &NaiveDateTime) -> MappedLocalTime<FixedOffset> {
        let fits: Vec<FixedOffset> = [7200, 3600]
            .into_iter()
            .map(|s| FixedOffset::east_opt(s).unwrap())
            .filter(|o| {
                let utc = *local - TimeDelta::seconds(i64::from(o.local_minus_utc()));
                offset_at(&utc) == *o
            })
            .collect();
        match fits[..] {
            [] => MappedLocalTime::None,
            [o] => MappedLocalTime::Single(o),
            [a, b, ..] => MappedLocalTime::Ambiguous(a, b),
        }
    }

    fn offset_from_utc_date(&self, utc: &NaiveDate) -> FixedOffset {
        offset_at(&utc.and_hms_opt(0, 0, 0).unwrap())
    }

    fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> FixedOffset {
        offset_at(utc)
    }
}

pub const TZ: Berlin = Berlin;

/// A local `YYYY-MM-DDTHH:mm` in Berlin as ms.
pub fn at(s: &str) -> i64 {
    wraithgrid_schedule::parse_local(&TZ, s).expect("a local date-time")
}

/// A plain enabled weekly job, as the job form makes one.
pub fn job(id: &str) -> ScheduledJob {
    ScheduledJob {
        id: id.into(),
        ws_id: "w".into(),
        account_id: "a".into(),
        prompt: "Review PRs".into(),
        model: None,
        effort: None,
        chat_mode: None,
        when: ScheduleWhen::Weekly {
            days: vec![1],
            time: "09:00".into(),
        },
        target: ScheduleTarget::New,
        at_limit: "wait".into(),
        enabled: true,
        one_shot: false,
        next_at: None,
        runs: vec![],
    }
}
