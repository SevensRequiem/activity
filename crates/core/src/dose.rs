use serde::Serialize;

pub const DAY: i64 = 86_400;

/// Spacing rules for a medicine reminder, in minutes. `max_per_day` counts a
/// rolling 24 hours, not a calendar day, so a shifting sleep schedule can't
/// squeeze two days' doses together. 0 means no cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    pub every_min: u32,
    pub min_gap_min: u32,
    pub max_per_day: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Warning {
    TooSoon { gap_min: i64, min_gap_min: u32 },
    OverMax { count: u32, max: u32 },
}

fn mins(m: u32) -> i64 {
    i64::from(m) * 60
}

/// When the cap stops blocking a dose, given doses sorted ascending: the
/// `max`-th most recent one leaving the 24h window.
fn cap_release(doses: &[i64], max: u32) -> Option<i64> {
    let max = usize::try_from(max).ok().filter(|m| *m > 0)?;
    let i = doses.len().checked_sub(max)?;
    Some(doses[i] + DAY)
}

/// When the next dose is due. `doses` are taken times sorted ascending and
/// must include at least the last 24 hours. `skipped` is the due time of a
/// skipped dose after the last one taken, which restarts spacing from there.
/// With no history the first dose is due `now`.
pub fn next_due(rules: Rules, doses: &[i64], skipped: Option<i64>, now: i64) -> i64 {
    let base = doses.last().copied().max(skipped);
    let Some(base) = base else { return now };
    let spaced = base + mins(rules.every_min);
    cap_release(doses, rules.max_per_day).map_or(spaced, |r| spaced.max(r))
}

/// The earliest time another dose keeps both the minimum gap and the cap.
pub fn earliest_safe(rules: Rules, doses: &[i64]) -> Option<i64> {
    let gap = doses.last().map(|l| l + mins(rules.min_gap_min));
    gap.max(cap_release(doses, rules.max_per_day))
}

/// Warnings for a dose taken at `at`, checked against the other doses
/// (sorted ascending, not including this one). Looks both ways so a
/// backdated dose is checked against the one logged after it too.
pub fn warnings(rules: Rules, others: &[i64], at: i64) -> Vec<Warning> {
    let mut out = Vec::new();
    let gap = others
        .iter()
        .map(|t| (at - t).abs())
        .min()
        .filter(|g| *g < mins(rules.min_gap_min));
    if let Some(g) = gap {
        out.push(Warning::TooSoon {
            gap_min: g / 60,
            min_gap_min: rules.min_gap_min,
        });
    }
    if rules.max_per_day > 0 {
        let window = others
            .iter()
            .filter(|t| **t > at - DAY && **t <= at)
            .count();
        let count = u32::try_from(window).unwrap_or(u32::MAX).saturating_add(1);
        if count > rules.max_per_day {
            out.push(Warning::OverMax {
                count,
                max: rules.max_per_day,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3600;
    const RULES: Rules = Rules {
        every_min: 360,
        min_gap_min: 240,
        max_per_day: 4,
    };

    /// name, rules, doses taken, missed due time, expected next due.
    type Case<'a> = (&'a str, Rules, &'a [i64], Option<i64>, i64);

    #[test]
    fn next_due_cases() {
        #[rustfmt::skip]
        let cases: &[Case] = &[
            ("first dose is due now", RULES, &[], None, 100),
            ("spaced from the last dose", RULES, &[8 * H], None, 14 * H),
            ("a late dose shifts the next one", RULES, &[8 * H, 15 * H + 1800], None, 21 * H + 1800),
            ("skip restarts spacing from its due time", RULES, &[8 * H], Some(14 * H), 20 * H),
            ("cap waits for the oldest to leave the window",
                RULES, &[0, 2 * H, 4 * H, 6 * H], None, DAY),
            ("no cap", Rules { max_per_day: 0, ..RULES }, &[0, 2 * H, 4 * H, 6 * H], None, 12 * H),
        ];
        for (name, rules, doses, skipped, want) in cases {
            assert_eq!(next_due(*rules, doses, *skipped, 100), *want, "case {name}");
        }
    }

    #[test]
    fn earliest_safe_cases() {
        assert_eq!(earliest_safe(RULES, &[]), None);
        assert_eq!(earliest_safe(RULES, &[10 * H]), Some(14 * H));
        assert_eq!(earliest_safe(RULES, &[0, 5 * H, 10 * H, 15 * H]), Some(DAY));
    }

    #[test]
    fn warning_cases() {
        let too_soon = |gap_min| Warning::TooSoon {
            gap_min,
            min_gap_min: 240,
        };
        #[rustfmt::skip]
        let cases: &[(&str, &[i64], i64, Vec<Warning>)] = &[
            ("fine", &[0], 6 * H, vec![]),
            ("too soon after the last", &[0], 2 * H, vec![too_soon(120)]),
            ("backdated too close to a later one", &[0, 10 * H], 9 * H, vec![too_soon(60)]),
            ("fifth in 24h", &[0, 5 * H, 10 * H, 15 * H], 20 * H, vec![Warning::OverMax { count: 5, max: 4 }]),
            ("oldest has left the window", &[0, 5 * H, 10 * H, 15 * H], DAY, vec![]),
        ];
        for (name, others, at, want) in cases {
            assert_eq!(&warnings(RULES, others, *at), want, "case {name}");
        }
    }
}
