//! A 5-field cron expression (minute hour day-of-month month day-of-week), evaluated in local time.
//! Used by the cron trigger nodes of a workflow (`triggers.rs`).

use std::collections::BTreeSet;

use chrono::{Datelike, NaiveDateTime, Timelike};

/// A parsed 5-field cron expression: minute, hour, day of month, month, day of week.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minutes: BTreeSet<u32>,
    hours: BTreeSet<u32>,
    days: BTreeSet<u32>,
    months: BTreeSet<u32>,
    weekdays: BTreeSet<u32>,
    any_day: bool,
    any_weekday: bool,
}

fn field(text: &str, min: u32, max: u32) -> Result<(BTreeSet<u32>, bool), String> {
    let mut out = BTreeSet::new();
    let number = |s: &str| s.parse::<u32>().ok().filter(|n| (min..=max).contains(n)).ok_or(format!("{s:?} is not between {min} and {max}"));
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().ok().filter(|n| *n > 0).ok_or(format!("bad step {s:?}"))?),
            None => (part, 1),
        };
        let (lo, hi) = match range {
            "*" => (min, max),
            r => match r.split_once('-') {
                Some((a, b)) => (number(a)?, number(b)?),
                // `5/10` means from 5 to the end, every 10
                None if part.contains('/') => (number(r)?, max),
                None => (number(r)?, number(r)?),
            },
        };
        if lo > hi {
            return Err(format!("the range {part:?} runs backwards"));
        }
        out.extend((lo..=hi).step_by(step as usize));
    }
    Ok((out, text == "*" || text.starts_with("*/")))
}

impl Cron {
    pub fn parse(expr: &str) -> Result<Cron, String> {
        let expr = match expr.trim() {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            "@yearly" | "@annually" => "0 0 1 1 *",
            e => e,
        };
        let f: Vec<&str> = expr.split_whitespace().collect();
        if f.len() != 5 {
            return Err("a schedule has 5 fields: minute hour day-of-month month day-of-week".into());
        }
        let (minutes, _) = field(f[0], 0, 59).map_err(|e| format!("minute: {e}"))?;
        let (hours, _) = field(f[1], 0, 23).map_err(|e| format!("hour: {e}"))?;
        let (days, any_day) = field(f[2], 1, 31).map_err(|e| format!("day of month: {e}"))?;
        let (months, _) = field(f[3], 1, 12).map_err(|e| format!("month: {e}"))?;
        let (mut weekdays, any_weekday) = field(f[4], 0, 7).map_err(|e| format!("day of week: {e}"))?;
        if weekdays.remove(&7) {
            weekdays.insert(0); // 7 is Sunday too
        }
        Ok(Cron { minutes, hours, days, months, weekdays, any_day, any_weekday })
    }

    pub fn matches(&self, t: &NaiveDateTime) -> bool {
        if !self.minutes.contains(&t.minute()) || !self.hours.contains(&t.hour()) || !self.months.contains(&t.month()) {
            return false;
        }
        let day = self.days.contains(&t.day());
        let weekday = self.weekdays.contains(&t.weekday().num_days_from_sunday());
        // like classic cron: when both day fields are restricted, either one is enough
        if !self.any_day && !self.any_weekday { day || weekday } else { day && weekday }
    }

    /// The next `n` minutes (after `from`) this matches, within a year.
    pub fn next(&self, from: NaiveDateTime, n: usize) -> Vec<NaiveDateTime> {
        let mut t = from.with_second(0).and_then(|t| t.with_nanosecond(0)).unwrap_or(from);
        let mut out = vec![];
        for _ in 0..(366 * 24 * 60) {
            t += chrono::Duration::minutes(1);
            if self.matches(&t) {
                out.push(t);
                if out.len() == n {
                    break;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    #[test]
    fn parses_and_matches() {
        let c = Cron::parse("*/15 9-17 * * 1-5").unwrap();
        assert!(c.matches(&at("2026-11-02 09:30"))); // a Monday
        assert!(!c.matches(&at("2026-11-02 09:31")));
        assert!(!c.matches(&at("2026-11-01 09:30"))); // a Sunday
        assert!(!c.matches(&at("2026-11-02 18:00")));
        assert!(Cron::parse("0 0 * * 7").unwrap().matches(&at("2026-11-01 00:00")), "7 is Sunday");
        assert!(Cron::parse("@daily").unwrap().matches(&at("2026-11-05 00:00")));
        assert!(Cron::parse("5/20 * * * *").unwrap().matches(&at("2026-11-05 03:45")));
    }

    #[test]
    fn day_of_month_or_weekday_when_both_are_set() {
        let c = Cron::parse("0 8 13 * 5").unwrap(); // the 13th, or any Friday
        assert!(c.matches(&at("2026-11-13 08:00")));
        assert!(c.matches(&at("2026-11-06 08:00")));
        assert!(!c.matches(&at("2026-11-07 08:00")));
    }

    #[test]
    fn rejects_bad_expressions() {
        for bad in ["", "* * * *", "60 * * * *", "* 24 * * *", "* * 0 * *", "* * * 13 *", "*/0 * * * *", "5-1 * * * *", "a * * * *", "* * * * * *"] {
            assert!(Cron::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn finds_the_next_times() {
        let c = Cron::parse("30 2 * * *").unwrap();
        let n = c.next(at("2026-11-05 02:30"), 2);
        assert_eq!(n, [at("2026-11-06 02:30"), at("2026-11-07 02:30")]);
        assert_eq!(Cron::parse("0 0 29 2 *").unwrap().next(at("2026-11-05 00:00"), 1).len(), 0, "no 29 February within a year");
    }
}
