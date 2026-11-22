use std::collections::{BTreeMap, HashMap};

use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use serde::{Deserialize, Serialize};

use crate::classify::State;
use crate::media::Kind;
use crate::schedule::Day;
use crate::store::{MediaSpan, Span};

/// A pause shorter than this joins two spans of one source into one session,
/// so pausing a video or a call dropping for a moment isn't a new session.
pub const SESSION_GAP: i64 = 120;
const TOP: usize = 10;

/// Whole local days, plus as many days before them for comparison.
#[derive(Debug)]
pub struct Window {
    pub dates: Vec<Date>,
    pub prev_from: i64,
    pub from: i64,
    pub to: i64,
}

impl Window {
    /// The `days` local days ending with `last`.
    pub fn ending(last: Date, days: u32, tz: &TimeZone) -> Result<Self, jiff::Error> {
        let n = i64::from(days.max(1));
        let first = last.checked_sub((n - 1).days())?;
        let start = |d: Date| d.to_zoned(tz.clone()).map(|z| z.timestamp().as_second());
        Ok(Self {
            dates: first.series(1.day()).take_while(|d| *d <= last).collect(),
            prev_from: start(first.checked_sub(n.days())?)?,
            from: start(first)?,
            to: start(last.tomorrow()?)?,
        })
    }

    fn clip(&self, start: i64, end: i64) -> i64 {
        (end.min(self.to) - start.max(self.from)).max(0)
    }
}

#[derive(Debug, Serialize)]
pub struct Series<K: Ord> {
    /// Seconds per local day, in the order of `Summary::dates`.
    pub days: Vec<BTreeMap<K, i64>>,
    /// Seconds per local hour of the day, summed over the window.
    pub hours: Vec<BTreeMap<K, i64>>,
    pub total: BTreeMap<K, i64>,
    /// Totals for the window before this one.
    pub prev: BTreeMap<K, i64>,
}

impl<K: Ord + Copy> Series<K> {
    fn new(days: usize) -> Self {
        Self {
            days: vec![BTreeMap::new(); days],
            hours: vec![BTreeMap::new(); 24],
            total: BTreeMap::new(),
            prev: BTreeMap::new(),
        }
    }

    /// Splits `[start, end)` at local hour boundaries, which are also day
    /// boundaries, so 23- and 25-hour DST days come out right.
    fn add(&mut self, key: K, start: i64, end: i64, w: &Window, tz: &TimeZone) {
        let prev = end.min(w.from) - start.max(w.prev_from);
        if prev > 0 {
            *self.prev.entry(key).or_default() += prev;
        }
        let end = end.min(w.to);
        let mut t = start.max(w.from);
        while t < end {
            let Ok(ts) = Timestamp::from_second(t) else {
                return;
            };
            let local = tz.to_datetime(ts);
            let hour_start = t - i64::from(local.minute()) * 60 - i64::from(local.second());
            let next = (hour_start + 3600).min(end);
            let secs = next - t;
            if let Ok(day) = w.dates.binary_search(&local.date()) {
                *self.days[day].entry(key).or_default() += secs;
            }
            *self.hours[usize::from(local.hour().unsigned_abs())]
                .entry(key)
                .or_default() += secs;
            *self.total.entry(key).or_default() += secs;
            t = next;
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Top {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    pub secs: i64,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Calls {
    pub count: i64,
    pub total: i64,
    pub longest: i64,
    pub average: i64,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub dates: Vec<Date>,
    pub media: Series<Kind>,
    pub states: Series<State>,
    /// Sites and apps by time, calls left out.
    pub sources: Vec<Top>,
    /// Gaming time by window title.
    pub games: Vec<Top>,
    pub calls: Calls,
}

/// `media` and `activity` should cover `w.prev_from..w.to`. Open activity
/// spans run until `now`.
pub fn summary(
    media: &[MediaSpan],
    activity: &[Span],
    w: &Window,
    now: i64,
    tz: &TimeZone,
) -> Summary {
    let mut media_series = Series::new(w.dates.len());
    let mut sources: HashMap<(&str, Kind), i64> = HashMap::new();
    for m in media {
        media_series.add(m.kind, m.start, m.end, w, tz);
        if m.kind != Kind::Call {
            *sources.entry((m.source.as_str(), m.kind)).or_default() += w.clip(m.start, m.end);
        }
    }

    let mut states = Series::new(w.dates.len());
    let mut games: HashMap<&str, i64> = HashMap::new();
    for a in activity {
        let end = a.end.unwrap_or(now);
        states.add(a.state, a.start, end, w, tz);
        if a.state == State::Gaming {
            let title = if a.caption.trim().is_empty() {
                &a.class
            } else {
                &a.caption
            };
            *games.entry(title).or_default() += w.clip(a.start, end);
        }
    }

    let (count, total, longest) = sessions(media.iter().filter(|m| m.kind == Kind::Call))
        .iter()
        .map(|c| w.clip(c.start, c.end))
        .filter(|d| *d > 0)
        .fold((0, 0, 0), |(n, t, l), d| (n + 1, t + d, l.max(d)));

    Summary {
        dates: w.dates.clone(),
        media: media_series,
        states,
        sources: top(sources.into_iter().map(|((name, kind), secs)| Top {
            name: name.into(),
            kind: Some(kind),
            secs,
        })),
        games: top(games.into_iter().map(|(name, secs)| Top {
            name: name.into(),
            kind: None,
            secs,
        })),
        calls: Calls {
            count,
            total,
            longest,
            average: if count > 0 { total / count } else { 0 },
        },
    }
}

fn top(items: impl Iterator<Item = Top>) -> Vec<Top> {
    let mut v: Vec<Top> = items.filter(|t| t.secs > 0).collect();
    v.sort_by(|a, b| b.secs.cmp(&a.secs).then_with(|| a.name.cmp(&b.name)));
    v.truncate(TOP);
    v
}

/// Joins spans of the same source and kind separated by at most
/// `SESSION_GAP`. Expects spans ordered by start.
pub fn sessions<'a>(spans: impl IntoIterator<Item = &'a MediaSpan>) -> Vec<MediaSpan> {
    let mut out: Vec<MediaSpan> = Vec::new();
    let mut last: HashMap<(&str, Kind), usize> = HashMap::new();
    for m in spans {
        match last.get(&(m.source.as_str(), m.kind)) {
            Some(&i) if m.start - out[i].end <= SESSION_GAP => {
                out[i].end = out[i].end.max(m.end);
            }
            _ => {
                last.insert((m.source.as_str(), m.kind), out.len());
                out.push(m.clone());
            }
        }
    }
    out
}

/// When the weekly summary notification goes out, in local time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SummaryConfig {
    pub enabled: bool,
    pub weekday: Day,
    pub at: Time,
}

impl Default for SummaryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            weekday: Day::Mon,
            at: Time::constant(9, 0, 0, 0),
        }
    }
}

impl SummaryConfig {
    /// The latest slot at or before `now`.
    pub fn last_slot(&self, now: i64, tz: &TimeZone) -> Option<i64> {
        let today = tz.to_datetime(Timestamp::from_second(now).ok()?).date();
        (0..8).find_map(|back| {
            let date = today.checked_sub(back.days()).ok()?;
            if !self.weekday.matches(date.weekday()) {
                return None;
            }
            let slot = date.to_datetime(self.at).to_zoned(tz.clone()).ok()?;
            let slot = slot.timestamp().as_second();
            (slot <= now).then_some(slot)
        })
    }
}

fn dur(secs: i64) -> String {
    let min = (secs + 30) / 60;
    match (min / 60, min % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m:02}m"),
    }
}

/// The weekly notification's text.
pub fn digest(s: &Summary) -> String {
    fn get<K: Ord>(m: &BTreeMap<K, i64>, k: &K) -> i64 {
        m.get(k).copied().unwrap_or(0)
    }
    let mut parts = Vec::new();
    let mut add = |label: &str, cur: i64, prev: i64| {
        if cur < 60 {
            return;
        }
        let d = cur - prev;
        let change = match d {
            ..=-60 => format!(" (−{})", dur(-d)),
            60.. => format!(" (+{})", dur(d)),
            _ => String::new(),
        };
        parts.push(format!("{label} {}{change}", dur(cur)));
    };
    for (kind, label) in [
        (Kind::Music, "Music"),
        (Kind::Video, "Video"),
        (Kind::Podcast, "Podcasts"),
    ] {
        add(label, get(&s.media.total, &kind), get(&s.media.prev, &kind));
    }
    for (state, label) in [(State::Gaming, "Gaming"), (State::Focus, "Focus")] {
        add(
            label,
            get(&s.states.total, &state),
            get(&s.states.prev, &state),
        );
    }
    match s.calls.count {
        0 => {}
        1 => parts.push(format!("Calls {} in 1 call", dur(s.calls.total))),
        n => parts.push(format!("Calls {} over {n} calls", dur(s.calls.total))),
    }
    let mut names: Vec<&str> = Vec::new();
    for t in &s.sources {
        if names.len() < 3 && !names.contains(&t.name.as_str()) {
            names.push(&t.name);
        }
    }
    if !names.is_empty() {
        parts.push(format!("Top: {}", names.join(", ")));
    }
    if parts.is_empty() {
        "Nothing tracked this week".into()
    } else {
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::DateTime;

    fn ny() -> TimeZone {
        TimeZone::get("America/New_York").unwrap_or_else(|e| panic!("{e}"))
    }

    fn ts(s: &str) -> i64 {
        s.parse::<DateTime>()
            .and_then(|d| d.to_zoned(ny()))
            .map_or_else(
                |e| panic!("bad test time {s}: {e}"),
                |z| z.timestamp().as_second(),
            )
    }

    fn date(s: &str) -> Date {
        s.parse()
            .unwrap_or_else(|e| panic!("bad test date {s}: {e}"))
    }

    fn window(last: &str, days: u32) -> Window {
        Window::ending(date(last), days, &ny()).unwrap_or_else(|e| panic!("{e}"))
    }

    fn media(start: &str, end: &str, kind: Kind, source: &str) -> MediaSpan {
        MediaSpan {
            start: ts(start),
            end: ts(end),
            kind,
            source: source.into(),
        }
    }

    fn activity(start: &str, end: Option<&str>, state: State, caption: &str) -> Span {
        Span {
            start: ts(start),
            end: end.map(ts),
            state,
            class: "steam_app_1".into(),
            caption: caption.into(),
        }
    }

    #[test]
    fn window_lengths() {
        let cases = [
            ("2026-11-21", 1, 24),
            ("2026-03-08", 1, 23),
            ("2026-11-01", 1, 25),
            ("2026-11-21", 7, 7 * 24),
        ];
        for (last, days, hours) in cases {
            let w = window(last, days);
            assert_eq!(w.to - w.from, hours * 3600, "{last} x{days}");
            assert_eq!(
                w.from - w.prev_from,
                i64::from(days) * 86400,
                "{last} x{days} prev"
            );
            assert_eq!(w.dates.len(), days as usize, "{last} x{days} dates");
            assert_eq!(w.dates.last(), Some(&date(last)), "{last} x{days} last");
        }
    }

    struct Bucket {
        name: &'static str,
        last: &'static str,
        days: u32,
        span: (&'static str, &'static str),
        per_day: &'static [i64],
        per_hour: &'static [(usize, i64)],
        prev: i64,
    }

    #[test]
    fn buckets() {
        let cases = [
            Bucket {
                name: "across midnight",
                last: "2026-11-21",
                days: 2,
                span: ("2026-11-20T23:30", "2026-11-21T00:45"),
                per_day: &[1800, 2700],
                per_hour: &[(23, 1800), (0, 2700)],
                prev: 0,
            },
            Bucket {
                name: "spring forward skips 02:00",
                last: "2026-03-08",
                days: 1,
                span: ("2026-03-08T00:00", "2026-03-08T04:00"),
                per_day: &[3 * 3600],
                per_hour: &[(0, 3600), (1, 3600), (2, 0), (3, 3600)],
                prev: 0,
            },
            Bucket {
                name: "fall back repeats 01:00",
                last: "2026-11-01",
                days: 1,
                span: ("2026-11-01T00:00", "2026-11-01T03:00"),
                per_day: &[4 * 3600],
                per_hour: &[(0, 3600), (1, 7200), (2, 3600)],
                prev: 0,
            },
            Bucket {
                name: "clipped, the rest counts for the previous window",
                last: "2026-11-21",
                days: 1,
                span: ("2026-11-20T23:00", "2026-11-22T01:30"),
                per_day: &[86400],
                per_hour: &[(23, 3600), (1, 3600), (12, 3600)],
                prev: 3600,
            },
        ];
        for c in cases {
            let w = window(c.last, c.days);
            let m = [media(c.span.0, c.span.1, Kind::Music, "Spotify")];
            let s = summary(&m, &[], &w, 0, &ny());
            let got = |m: &BTreeMap<Kind, i64>| m.get(&Kind::Music).copied().unwrap_or(0);
            let name = c.name;
            let days: Vec<i64> = s.media.days.iter().map(got).collect();
            assert_eq!(days, c.per_day, "{name}: days");
            for (hour, secs) in c.per_hour {
                assert_eq!(got(&s.media.hours[*hour]), *secs, "{name}: hour {hour}");
            }
            let total: i64 = c.per_day.iter().sum();
            assert_eq!(got(&s.media.total), total, "{name}: total");
            assert_eq!(got(&s.media.prev), c.prev, "{name}: prev");
        }
    }

    #[test]
    fn summary_tops_games_and_calls() {
        let w = window("2026-11-21", 1);
        let m = [
            media(
                "2026-11-21T10:00",
                "2026-11-21T11:00",
                Kind::Video,
                "YouTube",
            ),
            media(
                "2026-11-21T12:00",
                "2026-11-21T12:30",
                Kind::Music,
                "Spotify",
            ),
            media(
                "2026-11-21T13:00",
                "2026-11-21T13:20",
                Kind::Podcast,
                "Spotify",
            ),
            media(
                "2026-11-21T20:00",
                "2026-11-21T21:00",
                Kind::Call,
                "Discord",
            ),
            media(
                "2026-11-21T21:01",
                "2026-11-21T21:30",
                Kind::Call,
                "Discord",
            ),
            media(
                "2026-11-21T22:00",
                "2026-11-21T22:15",
                Kind::Call,
                "Discord",
            ),
        ];
        let a = [
            activity(
                "2026-11-21T14:00",
                Some("2026-11-21T16:00"),
                State::Gaming,
                "Hades",
            ),
            activity(
                "2026-11-21T16:00",
                Some("2026-11-21T17:00"),
                State::Focus,
                "code",
            ),
            activity(
                "2026-11-21T17:00",
                Some("2026-11-21T17:30"),
                State::Gaming,
                "",
            ),
            activity("2026-11-21T18:00", None, State::Gaming, "Hades"),
        ];
        let s = summary(&m, &a, &w, ts("2026-11-21T18:30"), &ny());
        let names: Vec<(&str, Option<Kind>, i64)> = s
            .sources
            .iter()
            .map(|t| (t.name.as_str(), t.kind, t.secs))
            .collect();
        assert_eq!(
            names,
            [
                ("YouTube", Some(Kind::Video), 3600),
                ("Spotify", Some(Kind::Music), 1800),
                ("Spotify", Some(Kind::Podcast), 1200),
            ]
        );
        let games: Vec<(&str, i64)> = s.games.iter().map(|t| (t.name.as_str(), t.secs)).collect();
        assert_eq!(games, [("Hades", 9000), ("steam_app_1", 1800)]);
        assert_eq!(s.states.total.get(&State::Focus), Some(&3600));
        assert_eq!(
            s.calls,
            Calls {
                count: 2,
                total: 6300,
                longest: 5400,
                average: 3150
            }
        );
    }

    #[test]
    fn sessions_merge_short_gaps() {
        let m = |start: i64, end: i64, source: &str| MediaSpan {
            start,
            end,
            kind: Kind::Video,
            source: source.into(),
        };
        let cases = [
            (
                "gap within limit",
                vec![m(0, 100, "a"), m(220, 300, "a")],
                vec![m(0, 300, "a")],
            ),
            (
                "gap too long",
                vec![m(0, 100, "a"), m(221, 300, "a")],
                vec![m(0, 100, "a"), m(221, 300, "a")],
            ),
            (
                "another source in between",
                vec![m(0, 100, "a"), m(110, 150, "b"), m(160, 300, "a")],
                vec![m(0, 300, "a"), m(110, 150, "b")],
            ),
        ];
        for (name, spans, want) in cases {
            assert_eq!(sessions(&spans), want, "{name}");
        }
    }

    #[test]
    fn last_slot_cases() {
        let cfg = SummaryConfig::default();
        let cases = [
            ("2026-11-23T08:59", Some("2026-11-16T09:00")),
            ("2026-11-23T09:00", Some("2026-11-23T09:00")),
            ("2026-11-25T20:00", Some("2026-11-23T09:00")),
            ("2026-11-29T23:59", Some("2026-11-23T09:00")),
        ];
        for (now, want) in cases {
            assert_eq!(cfg.last_slot(ts(now), &ny()), want.map(ts), "{now}");
        }
    }

    #[test]
    fn digest_text() {
        let w = window("2026-11-21", 7);
        let m = [
            media(
                "2026-11-13T10:00",
                "2026-11-13T12:00",
                Kind::Music,
                "Spotify",
            ),
            media(
                "2026-11-20T10:00",
                "2026-11-20T14:30",
                Kind::Music,
                "Spotify",
            ),
            media(
                "2026-11-20T15:00",
                "2026-11-20T16:00",
                Kind::Video,
                "YouTube",
            ),
            media(
                "2026-11-21T20:00",
                "2026-11-21T21:00",
                Kind::Call,
                "Discord",
            ),
        ];
        let s = summary(&m, &[], &w, ts("2026-11-22T00:00"), &ny());
        assert_eq!(
            digest(&s),
            "Music 4h 30m (+2h 30m) · Video 1h (+1h) · Calls 1h in 1 call · Top: Spotify, YouTube"
        );
        let empty = summary(&[], &[], &w, 0, &ny());
        assert_eq!(digest(&empty), "Nothing tracked this week");
    }
}
