//! How usage reads on screen (`src/renderer/lib/usage-format.ts`): compact costs, token
//! counts, time until a reset, and how full a limit is.

/// `$0`, `<$0.01`, `$3.18`, `$1.2K`.
pub fn fmt_cost(usd: f64) -> String {
    if usd <= 0.0 {
        "$0".into()
    } else if usd < 0.01 {
        "<$0.01".into()
    } else if usd < 1000.0 {
        format!("${usd:.2}")
    } else if usd < 10_000.0 {
        format!("${:.1}K", usd / 1000.0)
    } else {
        format!("${:.0}K", usd / 1000.0)
    }
}

/// `940`, `12.9K`, `1.2M`.
pub fn fmt_tokens(n: u64) -> String {
    let f = n as f64;
    if n < 1000 {
        n.to_string()
    } else if n < 1_000_000 {
        if n < 10_000 {
            format!("{:.1}K", f / 1000.0)
        } else {
            format!("{:.0}K", f / 1000.0)
        }
    } else if n < 10_000_000 {
        format!("{:.1}M", f / 1_000_000.0)
    } else {
        format!("{:.0}M", f / 1_000_000.0)
    }
}

/// Time until `at`: `2h 14m`, `42m 10s`, `3d 4h`; `now` once it has passed.
pub fn fmt_until(at: i64, now: i64) -> String {
    let s = ((at - now) as f64 / 1000.0).round().max(0.0) as i64;
    if s == 0 {
        return "now".into();
    }
    let (d, h, m) = (s / 86_400, s % 86_400 / 3600, s % 3600 / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m {}s", s % 60)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Ok,
    Warn,
    Err,
}

/// How full a limit is: under 70% fine, under 90% a warning, then serious.
pub fn severity(utilization: f64) -> Severity {
    if utilization >= 0.9 {
        Severity::Err
    } else if utilization >= 0.7 {
        Severity::Warn
    } else {
        Severity::Ok
    }
}

/// "resets in 2h 13m" within a day (minutes only under an hour); further off, the weekday
/// and time of the reset in local time.
pub fn reset_label(at: i64, now: i64) -> String {
    let ms = at - now;
    if ms < 3_600_000 {
        return format!(
            "resets in {}m",
            ((ms as f64 / 60_000.0).ceil() as i64).max(1)
        );
    }
    if ms < 86_400_000 {
        return format!("resets in {}", fmt_until(at, now));
    }
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(at).single() {
        Some(t) => format!("resets {}", t.format("%a %H:%M")),
        None => format!("resets in {}", fmt_until(at, now)),
    }
}

/// `14:32` in local time.
pub fn local_hhmm(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map_or_else(String::new, |t| t.format("%H:%M").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: i64 = 1_800_000_000_000;

    #[test]
    fn reads_compactly() {
        assert_eq!(fmt_cost(0.0), "$0");
        assert_eq!(fmt_cost(0.004), "<$0.01");
        assert_eq!(fmt_cost(3.184), "$3.18");
        assert_eq!(fmt_cost(1234.0), "$1.2K");
        assert_eq!(fmt_tokens(940), "940");
        assert_eq!(fmt_tokens(12_900), "13K");
        assert_eq!(fmt_tokens(1_234_567), "1.2M");
        assert_eq!(fmt_until(NOW + (2 * 3600 + 14 * 60) * 1000, NOW), "2h 14m");
        assert_eq!(fmt_until(NOW + (42 * 60 + 10) * 1000, NOW), "42m 10s");
        assert_eq!(fmt_until(NOW + (3 * 86400 + 4 * 3600) * 1000, NOW), "3d 4h");
        assert_eq!(fmt_until(NOW - 5, NOW), "now");
        assert_eq!(
            [0.3, 0.75, 0.95].map(severity),
            [Severity::Ok, Severity::Warn, Severity::Err]
        );
        assert_eq!(reset_label(NOW + 90_000, NOW), "resets in 2m");
        assert_eq!(reset_label(NOW + 2 * 3_600_000, NOW), "resets in 2h 0m");
        assert!(reset_label(NOW + 3 * 86_400_000, NOW).starts_with("resets "));
    }
}
