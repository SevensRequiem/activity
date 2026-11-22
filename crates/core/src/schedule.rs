use jiff::Timestamp;
use jiff::civil::{Date, DateTime, Time, Weekday};
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Day {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Day {
    fn matches(self, weekday: Weekday) -> bool {
        let w = match self {
            Day::Mon => Weekday::Monday,
            Day::Tue => Weekday::Tuesday,
            Day::Wed => Weekday::Wednesday,
            Day::Thu => Weekday::Thursday,
            Day::Fri => Weekday::Friday,
            Day::Sat => Weekday::Saturday,
            Day::Sun => Weekday::Sunday,
        };
        w == weekday
    }
}

/// Wall-clock times in the local zone, so "09:00 daily" stays 09:00 across DST.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Schedule {
    Once {
        at: DateTime,
    },
    Daily {
        times: Vec<Time>,
        #[serde(default)]
        days: Vec<Day>,
    },
    Every {
        minutes: u32,
        start: Time,
        end: Time,
        #[serde(default)]
        days: Vec<Day>,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("daily schedule needs 1 to {MAX_TIMES} times")]
    TimeCount,
    #[error("interval must be 1 to 1440 minutes")]
    Interval,
    #[error("window start must not be after its end")]
    Window,
    #[error("a weekday is listed twice")]
    DuplicateDay,
}

const MAX_TIMES: usize = 48;

impl Schedule {
    pub fn validate(&self) -> Result<(), Error> {
        let days = match self {
            Schedule::Once { .. } => return Ok(()),
            Schedule::Daily { times, days } => {
                if times.is_empty() || times.len() > MAX_TIMES {
                    return Err(Error::TimeCount);
                }
                days
            }
            Schedule::Every {
                minutes,
                start,
                end,
                days,
            } => {
                if !(1..=1440).contains(minutes) {
                    return Err(Error::Interval);
                }
                if start > end {
                    return Err(Error::Window);
                }
                days
            }
        };
        for (i, d) in days.iter().enumerate() {
            if days[..i].contains(d) {
                return Err(Error::DuplicateDay);
            }
        }
        Ok(())
    }

    /// First fire time strictly after `after`, or `None` when the schedule is exhausted.
    pub fn next_after(&self, after: Timestamp, tz: &TimeZone) -> Option<Timestamp> {
        let (times, days) = match self {
            Schedule::Once { at } => {
                let ts = at.to_zoned(tz.clone()).ok()?.timestamp();
                return (ts > after).then_some(ts);
            }
            Schedule::Daily { times, days } => (times.clone(), days),
            Schedule::Every {
                minutes,
                start,
                end,
                days,
            } => (every_times(*minutes, *start, *end), days),
        };
        // A DST shift can push a time from the start date's evening into the next
        // day, so look back one day; 8 days ahead always covers every weekday.
        let mut date: Date = after.to_zoned(tz.clone()).date().yesterday().ok()?;
        for _ in 0..9 {
            if days.is_empty() || days.iter().any(|d| d.matches(date.weekday())) {
                let best = times
                    .iter()
                    .filter_map(|t| date.to_datetime(*t).to_zoned(tz.clone()).ok())
                    .map(|z| z.timestamp())
                    .filter(|ts| *ts > after)
                    .min();
                if best.is_some() {
                    return best;
                }
            }
            date = date.tomorrow().ok()?;
        }
        None
    }
}

fn every_times(minutes: u32, start: Time, end: Time) -> Vec<Time> {
    let first = minute_of_day(start);
    let last = minute_of_day(end);
    (first..=last)
        .step_by(minutes as usize)
        .filter_map(|m| {
            Time::new(i8::try_from(m / 60).ok()?, i8::try_from(m % 60).ok()?, 0, 0).ok()
        })
        .collect()
}

fn minute_of_day(t: Time) -> u32 {
    u32::from(t.hour().unsigned_abs()) * 60 + u32::from(t.minute().unsigned_abs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ts(s: &str, tz: &TimeZone) -> Timestamp {
        s.parse::<DateTime>()
            .and_then(|d| d.to_zoned(tz.clone()))
            .map_or_else(|e| panic!("bad test time {s}: {e}"), |z| z.timestamp())
    }

    fn sched(json: &str) -> Schedule {
        serde_json::from_str(json).unwrap_or_else(|e| panic!("bad schedule {json}: {e}"))
    }

    #[test]
    fn next_after_cases() {
        let ny = TimeZone::get("America/New_York").unwrap_or_else(|e| panic!("{e}"));
        let cases: &[(&str, &str, &str, Option<&str>)] = &[
            (
                "daily later today",
                r#"{"kind":"daily","times":["09:00","14:30"]}"#,
                "2026-11-23T10:00",
                Some("2026-11-23T14:30"),
            ),
            (
                "daily rolls to tomorrow",
                r#"{"kind":"daily","times":["09:00"]}"#,
                "2026-11-23T09:00",
                Some("2026-11-24T09:00"),
            ),
            (
                "daily weekday filter skips weekend",
                r#"{"kind":"daily","times":["09:00"],"days":["mon"]}"#,
                "2026-11-24T08:00",
                Some("2026-11-30T09:00"),
            ),
            (
                "every within window",
                r#"{"kind":"every","minutes":45,"start":"09:00","end":"12:00"}"#,
                "2026-11-23T09:10",
                Some("2026-11-23T09:45"),
            ),
            (
                "every past window end",
                r#"{"kind":"every","minutes":60,"start":"09:00","end":"12:00"}"#,
                "2026-11-23T12:00",
                Some("2026-11-24T09:00"),
            ),
            (
                "once future",
                r#"{"kind":"once","at":"2026-12-01T18:00"}"#,
                "2026-11-23T10:00",
                Some("2026-12-01T18:00"),
            ),
            (
                "once past is exhausted",
                r#"{"kind":"once","at":"2026-11-01T18:00"}"#,
                "2026-11-23T10:00",
                None,
            ),
            (
                "time in spring-forward gap lands after the gap",
                r#"{"kind":"daily","times":["02:30"]}"#,
                "2026-03-08T00:00",
                Some("2026-03-08T03:30"),
            ),
        ];
        for (name, json, after, want) in cases {
            let got = sched(json).next_after(ts(after, &ny), &ny);
            assert_eq!(got, want.map(|w| ts(w, &ny)), "case {name}");
        }
    }

    #[test]
    fn validate_cases() {
        let cases: &[(&str, &str, Result<(), Error>)] = &[
            (
                "no times",
                r#"{"kind":"daily","times":[]}"#,
                Err(Error::TimeCount),
            ),
            (
                "zero interval",
                r#"{"kind":"every","minutes":0,"start":"09:00","end":"10:00"}"#,
                Err(Error::Interval),
            ),
            (
                "interval over a day",
                r#"{"kind":"every","minutes":1441,"start":"09:00","end":"10:00"}"#,
                Err(Error::Interval),
            ),
            (
                "interval of exactly a day",
                r#"{"kind":"every","minutes":1440,"start":"09:00","end":"10:00"}"#,
                Ok(()),
            ),
            (
                "inverted window",
                r#"{"kind":"every","minutes":5,"start":"10:00","end":"09:00"}"#,
                Err(Error::Window),
            ),
            (
                "duplicate day",
                r#"{"kind":"daily","times":["09:00"],"days":["mon","mon"]}"#,
                Err(Error::DuplicateDay),
            ),
        ];
        for (name, json, want) in cases {
            assert_eq!(&sched(json).validate(), want, "case {name}");
        }
    }

    proptest! {
        #[test]
        fn next_is_strictly_later_and_minimal(
            secs in 1_700_000_000i64..1_900_000_000,
            minutes in 1u32..=180,
            start in 0i8..12,
            hour_times in proptest::collection::vec(0i8..24, 1..4),
        ) {
            let tz = TimeZone::get("Europe/Berlin").unwrap_or_else(|e| panic!("{e}"));
            let after = Timestamp::from_second(secs).unwrap_or_else(|e| panic!("{e}"));
            let times = hour_times.iter().filter_map(|h| Time::new(*h, 15, 0, 0).ok()).collect();
            let schedules = [
                Schedule::Daily { times, days: vec![] },
                Schedule::Every {
                    minutes,
                    start: Time::new(start, 0, 0, 0).unwrap_or_else(|e| panic!("{e}")),
                    end: Time::new(22, 0, 0, 0).unwrap_or_else(|e| panic!("{e}")),
                    days: vec![Day::Tue, Day::Sat],
                },
            ];
            for s in &schedules {
                let next = s.next_after(after, &tz);
                prop_assert!(next.is_some());
                let next = next.unwrap_or_else(|| unreachable!());
                prop_assert!(next > after);
                prop_assert!(next.as_second() - after.as_second() <= 8 * 86_400);
                // Asking from one second earlier must give the same answer: nothing was skipped.
                let just_before = Timestamp::from_second(next.as_second() - 1).unwrap_or_else(|e| panic!("{e}"));
                if just_before >= after {
                    prop_assert_eq!(s.next_after(just_before, &tz), Some(next));
                }
            }
        }
    }
}
