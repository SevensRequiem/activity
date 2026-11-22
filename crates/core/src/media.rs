use std::collections::{BTreeMap, HashMap};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::classify::{Error, glob_to_regex};

/// An open span is re-written at most this often, so a crash loses little.
pub const CHECKPOINT_SECS: i64 = 60;
/// The main loop ticks at least every 30 s; a longer silence means the machine
/// slept, and nothing was playing while it did.
pub const GAP_SECS: i64 = 90;
const MAX_HOST: usize = 253;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Music,
    Video,
    Podcast,
    Call,
    Other,
}

/// Maps a playing MPRIS player to a site or app name. `player` entries are
/// case-insensitive globs on the player's bus name; `url` is a regex on the
/// page or track URL. Every given field has to match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRule {
    pub name: String,
    pub kind: Kind,
    #[serde(default)]
    pub player: Vec<String>,
    pub url: Option<String>,
}

/// A voice app: in a call while one of `apps` (a `PipeWire` client's binary or
/// application name, case-insensitive) holds a capture stream open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceRule {
    pub name: String,
    pub apps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MediaConfig {
    /// Shorter spans are never written: stray sounds, mic tests, skipped tracks.
    pub min_secs: u32,
    pub sources: Vec<SourceRule>,
    pub voice: Vec<VoiceRule>,
}

impl Default for MediaConfig {
    fn default() -> Self {
        let url = |name: &str, kind, host: &str| SourceRule {
            name: name.into(),
            kind,
            player: Vec::new(),
            url: Some(format!(r"(?i)^https?://([a-z0-9-]+\.)*{host}")),
        };
        let player = |name: &str, kind, glob: &str| SourceRule {
            name: name.into(),
            kind,
            player: vec![glob.into()],
            url: None,
        };
        Self {
            min_secs: 30,
            sources: vec![
                url("YouTube Music", Kind::Music, r"music\.youtube\.com/"),
                url("YouTube", Kind::Video, r"(youtube\.com|youtu\.be)/"),
                url("Netflix", Kind::Video, r"netflix\.com/"),
                url("Crunchyroll", Kind::Video, r"crunchyroll\.com/"),
                url("HIDIVE", Kind::Video, r"hidive\.com/"),
                url("Twitch", Kind::Video, r"twitch\.tv/"),
                url(
                    "Spotify",
                    Kind::Podcast,
                    r"open\.spotify\.com/(episode|show)/",
                ),
                url("Spotify", Kind::Music, r"open\.spotify\.com/"),
                player("Spotify", Kind::Music, "spotify"),
                url("SoundCloud", Kind::Music, r"soundcloud\.com/"),
                url("Bandcamp", Kind::Music, r"bandcamp\.com/"),
                player("mpv", Kind::Video, "mpv"),
                player("VLC", Kind::Video, "vlc"),
                player("Haruna", Kind::Video, "*haruna*"),
                player("Celluloid", Kind::Video, "*celluloid*"),
                player("Elisa", Kind::Music, "*elisa*"),
                player("Strawberry", Kind::Music, "strawberry"),
                player("Rhythmbox", Kind::Music, "rhythmbox"),
            ],
            voice: vec![VoiceRule {
                name: "Discord".into(),
                apps: ["vesktop.bin", "vesktop", "discord", "equibop", "webcord"]
                    .map(Into::into)
                    .into(),
            }],
        }
    }
}

#[derive(Debug)]
struct Source {
    name: String,
    kind: Kind,
    player: Vec<Regex>,
    url: Option<Regex>,
}

#[derive(Debug)]
pub struct Sources(Vec<Source>);

impl Sources {
    pub fn new(cfg: &MediaConfig) -> Result<Self, Error> {
        let pattern = |rule: &str, p: &str| {
            Regex::new(p).map_err(|source| Error::Pattern {
                rule: rule.into(),
                source,
            })
        };
        cfg.sources
            .iter()
            .map(|r| {
                Ok(Source {
                    name: r.name.clone(),
                    kind: r.kind,
                    player: r
                        .player
                        .iter()
                        .map(|g| pattern(&r.name, &glob_to_regex(g)))
                        .collect::<Result<_, _>>()?,
                    url: r.url.as_deref().map(|u| pattern(&r.name, u)).transpose()?,
                })
            })
            .collect::<Result<_, _>>()
            .map(Self)
    }

    /// The URL only picks the source here; it's never stored.
    pub fn classify(&self, player: &str, url: Option<&str>) -> (String, Kind) {
        let url = url.unwrap_or("");
        let rule = self.0.iter().find(|r| {
            (r.player.is_empty() || r.player.iter().any(|p| p.is_match(player)))
                && r.url.as_ref().is_none_or(|u| u.is_match(url))
        });
        match rule {
            Some(r) => (r.name.clone(), r.kind),
            None => (host(url).unwrap_or_else(|| player.to_owned()), Kind::Other),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Voice(Vec<(String, Vec<String>)>);

impl Voice {
    pub fn new(cfg: &MediaConfig) -> Self {
        Self(
            cfg.voice
                .iter()
                .map(|r| {
                    let apps = r
                        .apps
                        .iter()
                        .filter(|a| !a.is_empty())
                        .map(|a| a.to_lowercase())
                        .collect();
                    (r.name.clone(), apps)
                })
                .collect(),
        )
    }

    pub fn app(&self, binary: &str, name: &str) -> Option<&str> {
        let (binary, name) = (binary.to_lowercase(), name.to_lowercase());
        self.0
            .iter()
            .find(|(_, apps)| apps.iter().any(|a| *a == binary || *a == name))
            .map(|(rule, _)| rule.as_str())
    }
}

/// `PipeWire` props are set by any client, including other users' apps, so
/// only this much of each is kept.
const MAX_PROP: usize = 128;

#[derive(Debug, Default)]
struct Node {
    class: String,
    binary: String,
    name: String,
}

/// The `PipeWire` nodes `pw-dump --monitor` has reported, reduced to what call
/// detection needs.
#[derive(Debug, Default)]
pub struct Streams(HashMap<u64, Node>);

impl Streams {
    /// Applies one object from a `pw-dump` batch: a new object, an update
    /// (which may leave out the props) or a removal (`"info": null`).
    pub fn apply(&mut self, obj: &Value) {
        let Some(id) = obj.get("id").and_then(Value::as_u64) else {
            return;
        };
        let Some(info) = obj.get("info") else {
            return;
        };
        if info.is_null() {
            self.0.remove(&id);
            return;
        }
        let is_node = obj.get("type").and_then(Value::as_str) == Some("PipeWire:Interface:Node");
        let Some(props) = info.get("props").and_then(Value::as_object) else {
            return;
        };
        if !is_node && !self.0.contains_key(&id) {
            return;
        }
        let node = self.0.entry(id).or_default();
        for (field, key) in [
            (&mut node.class, "media.class"),
            (&mut node.binary, "application.process.binary"),
            (&mut node.name, "application.name"),
        ] {
            if let Some(v) = props.get(key).and_then(Value::as_str) {
                *field = v.chars().take(MAX_PROP).collect();
            }
        }
    }

    /// The voice app holding a microphone stream open, if any.
    pub fn call<'v>(&self, voice: &'v Voice) -> Option<&'v str> {
        self.0
            .values()
            .filter(|n| n.class == "Stream/Input/Audio")
            .find_map(|n| voice.app(&n.binary, &n.name))
    }
}

/// The host of an http(s) URL without `www.`, or None for anything that isn't
/// a plain DNS name.
pub fn host(url: &str) -> Option<String> {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    let skip = if lower.starts_with("https://") {
        8
    } else if lower.starts_with("http://") {
        7
    } else {
        return None;
    };
    let authority = url[skip..].split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let valid = !host.is_empty()
        && host.len() <= MAX_HOST
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-');
    valid.then(|| host.to_owned())
}

pub type Key = (String, Kind);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Write {
    /// Store the span and hand its row back through `Tracker::set_row`.
    Insert {
        key: Key,
        start: i64,
        end: i64,
    },
    Extend {
        row: i64,
        end: i64,
    },
}

#[derive(Debug)]
struct Open {
    start: i64,
    row: Option<i64>,
    written: i64,
}

/// Turns "what is playing right now" samples into stored spans.
#[derive(Debug)]
pub struct Tracker {
    min_secs: i64,
    open: BTreeMap<Key, Open>,
    last_tick: Option<i64>,
}

impl Tracker {
    pub fn new(min_secs: u32) -> Self {
        Self {
            min_secs: i64::from(min_secs),
            open: BTreeMap::new(),
            last_tick: None,
        }
    }

    pub fn tick(&mut self, now: i64, active: &[Key]) -> Vec<Write> {
        let mut out = Vec::new();
        if let Some(last) = self.last_tick
            && now - last > GAP_SECS
        {
            self.close(|_| true, last, &mut out);
        }
        self.close(|k| !active.contains(k), now, &mut out);
        for key in active {
            let o = self.open.entry(key.clone()).or_insert(Open {
                start: now,
                row: None,
                written: now,
            });
            match o.row {
                None if now - o.start >= self.min_secs => {
                    out.push(Write::Insert {
                        key: key.clone(),
                        start: o.start,
                        end: now,
                    });
                    o.written = now;
                }
                Some(row) if now - o.written >= CHECKPOINT_SECS => {
                    out.push(Write::Extend { row, end: now });
                    o.written = now;
                }
                _ => {}
            }
        }
        self.last_tick = Some(now);
        out
    }

    /// A quit right after resume ends spans before the sleep, as `tick` would.
    pub fn close_all(&mut self, now: i64) -> Vec<Write> {
        let mut out = Vec::new();
        let end = match self.last_tick {
            Some(last) if now - last > GAP_SECS => last,
            _ => now,
        };
        self.close(|_| true, end, &mut out);
        out
    }

    pub fn set_row(&mut self, key: &Key, row: i64) {
        if let Some(o) = self.open.get_mut(key) {
            o.row = Some(row);
        }
    }

    /// Everything playing now with its start, including spans still under `min_secs`.
    pub fn open(&self) -> impl Iterator<Item = (&Key, i64, Option<i64>)> {
        self.open.iter().map(|(k, o)| (k, o.start, o.row))
    }

    fn close(&mut self, which: impl Fn(&Key) -> bool, end: i64, out: &mut Vec<Write>) {
        let keys: Vec<Key> = self.open.keys().filter(|k| which(k)).cloned().collect();
        for key in keys {
            let Some(o) = self.open.remove(&key) else {
                continue;
            };
            match o.row {
                Some(row) => out.push(Write::Extend { row, end }),
                None if end - o.start >= self.min_secs => out.push(Write::Insert {
                    key,
                    start: o.start,
                    end,
                }),
                None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_cases() -> Result<(), Error> {
        let sources = Sources::new(&MediaConfig::default())?;
        let cases = [
            (
                "firefox",
                Some("https://www.youtube.com/watch?v=x"),
                "YouTube",
                Kind::Video,
            ),
            (
                "firefox",
                Some("https://m.youtube.com/watch?v=x"),
                "YouTube",
                Kind::Video,
            ),
            (
                "firefox",
                Some("https://youtu.be/x"),
                "YouTube",
                Kind::Video,
            ),
            (
                "firefox",
                Some("https://music.youtube.com/watch?v=x"),
                "YouTube Music",
                Kind::Music,
            ),
            (
                "firefox",
                Some("HTTPS://WWW.NETFLIX.COM/watch/1"),
                "Netflix",
                Kind::Video,
            ),
            (
                "firefox",
                Some("https://www.crunchyroll.com/watch/G3/x"),
                "Crunchyroll",
                Kind::Video,
            ),
            (
                "spotify",
                Some("https://open.spotify.com/track/1"),
                "Spotify",
                Kind::Music,
            ),
            (
                "spotify",
                Some("https://open.spotify.com/episode/1"),
                "Spotify",
                Kind::Podcast,
            ),
            ("spotify", None, "Spotify", Kind::Music),
            (
                "firefox",
                Some("https://open.spotify.com/episode/1"),
                "Spotify",
                Kind::Podcast,
            ),
            (
                "firefox",
                Some("https://www.nicovideo.jp/watch/sm9"),
                "nicovideo.jp",
                Kind::Other,
            ),
            (
                "firefox",
                Some("https://evil.example/?u=youtube.com/"),
                "evil.example",
                Kind::Other,
            ),
            (
                "firefox",
                Some("https://youtube.com.evil.example/"),
                "youtube.com.evil.example",
                Kind::Other,
            ),
            (
                "firefox",
                Some("file:///home/me/clip.mp4"),
                "firefox",
                Kind::Other,
            ),
            ("firefox", None, "firefox", Kind::Other),
            ("mpv", Some("file:///x.mkv"), "mpv", Kind::Video),
            ("org.kde.elisa", None, "Elisa", Kind::Music),
        ];
        for (player, url, source, kind) in cases {
            assert_eq!(
                sources.classify(player, url),
                (source.to_owned(), kind),
                "{player} {url:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn host_cases() {
        let long = format!("https://{}.com/", "a".repeat(260));
        let cases = [
            (
                "https://User:pw@WWW.Example.COM:8443/p",
                Some("example.com"),
            ),
            ("http://example.com", Some("example.com")),
            ("https://example.com?q=1", Some("example.com")),
            ("https://example.com#frag", Some("example.com")),
            ("https://sub.example.co.uk/x", Some("sub.example.co.uk")),
            ("https://[::1]/", None),
            ("https://ex ample.com/", None),
            ("https:///path", None),
            ("ftp://example.com/", None),
            ("javascript:alert(1)", None),
            ("", None),
            (long.as_str(), None),
        ];
        for (url, want) in cases {
            assert_eq!(host(url).as_deref(), want, "{url}");
        }
    }

    #[test]
    fn voice_apps() {
        let voice = Voice::new(&MediaConfig::default());
        let cases = [
            ("vesktop.bin", "", Some("Discord")),
            ("", "Vesktop", Some("Discord")),
            ("Discord", "WEBRTC VoiceEngine", Some("Discord")),
            ("firefox", "Firefox", None),
            ("", "", None),
        ];
        for (binary, name, want) in cases {
            assert_eq!(voice.app(binary, name), want, "{binary:?} {name:?}");
        }
    }

    #[test]
    fn streams_follow_pw_dump() -> Result<(), serde_json::Error> {
        let voice = Voice::new(&MediaConfig::default());
        let node = r#"{"id":50,"type":"PipeWire:Interface:Node","info":{"props":
            {"media.class":"Stream/Input/Audio","application.process.binary":"vesktop.bin",
             "application.name":"WEBRTC VoiceEngine"}}}"#;
        let cases = [
            ("vesktop mic opens", node, Some("Discord")),
            (
                "update without props keeps it",
                r#"{"id":50,"type":"PipeWire:Interface:Node","info":{"state":"running"}}"#,
                Some("Discord"),
            ),
            ("removal", r#"{"id":50,"info":null}"#, None),
            (
                "vesktop playing sound is not a call",
                r#"{"id":51,"type":"PipeWire:Interface:Node","info":{"props":
                    {"media.class":"Stream/Output/Audio","application.process.binary":"vesktop.bin"}}}"#,
                None,
            ),
            (
                "a client object with the same props is ignored",
                r#"{"id":52,"type":"PipeWire:Interface:Client","info":{"props":
                    {"media.class":"Stream/Input/Audio","application.process.binary":"vesktop.bin"}}}"#,
                None,
            ),
            (
                "another app recording",
                r#"{"id":53,"type":"PipeWire:Interface:Node","info":{"props":
                    {"media.class":"Stream/Input/Audio","application.process.binary":"obs"}}}"#,
                None,
            ),
            ("vesktop mic opens again", node, Some("Discord")),
        ];
        let mut streams = Streams::default();
        for (name, obj, want) in cases {
            streams.apply(&serde_json::from_str(obj)?);
            assert_eq!(streams.call(&voice), want, "{name}");
        }
        Ok(())
    }

    fn key(name: &str) -> Key {
        (name.into(), Kind::Music)
    }

    type Steps = Vec<(i64, &'static [&'static str])>;

    /// Runs ticks of (time, active names), feeding each insert a row id from 1
    /// up, then closes everything at `quit` if given.
    fn run(steps: &[(i64, &[&str])], quit: Option<i64>) -> Vec<Write> {
        let mut t = Tracker::new(30);
        let mut writes = Vec::new();
        let mut next_row = 1;
        for (now, names) in steps {
            let active: Vec<Key> = names.iter().map(|n| key(n)).collect();
            for w in t.tick(*now, &active) {
                if let Write::Insert { key, .. } = &w {
                    t.set_row(key, next_row);
                    next_row += 1;
                }
                writes.push(w);
            }
        }
        if let Some(now) = quit {
            writes.extend(t.close_all(now));
        }
        writes
    }

    #[test]
    fn tracker_cases() {
        let ins = |name: &str, start, end| Write::Insert {
            key: key(name),
            start,
            end,
        };
        let ext = |row, end| Write::Extend { row, end };
        let cases: [(&str, Steps, Option<i64>, Vec<Write>); 7] = [
            (
                "too short to keep",
                vec![(0, &["a"]), (20, &["a"]), (25, &[])],
                None,
                vec![],
            ),
            (
                "inserted at min, checkpointed once a minute, closed",
                vec![
                    (0, &["a"]),
                    (30, &["a"]),
                    (60, &["a"]),
                    (90, &["a"]),
                    (95, &[]),
                ],
                None,
                vec![ins("a", 0, 30), ext(1, 90), ext(1, 95)],
            ),
            (
                "stopped between ticks after reaching min",
                vec![(0, &["a"]), (25, &["a"]), (40, &[])],
                None,
                vec![ins("a", 0, 40)],
            ),
            (
                "switching source closes the old span",
                vec![(0, &["a"]), (30, &["a"]), (35, &["b"]), (65, &["b"])],
                None,
                vec![ins("a", 0, 30), ext(1, 35), ins("b", 35, 65)],
            ),
            (
                "sleep ends spans at the last tick and restarts them",
                vec![(0, &["a"]), (30, &["a"]), (500, &["a"]), (530, &["a"])],
                None,
                vec![ins("a", 0, 30), ext(1, 30), ins("a", 500, 530)],
            ),
            (
                "two at once",
                vec![(0, &["a", "b"]), (30, &["a", "b"]), (40, &["b"])],
                None,
                vec![ins("a", 0, 30), ins("b", 0, 30), ext(1, 40)],
            ),
            (
                "quit closes what's open",
                vec![(0, &["a"]), (30, &["a"]), (45, &["a"])],
                Some(50),
                vec![ins("a", 0, 30), ext(1, 50)],
            ),
        ];
        for (name, steps, quit, want) in cases {
            assert_eq!(run(&steps, quit), want, "{name}");
        }
    }

    #[test]
    fn close_all_after_sleep_ends_at_last_tick() {
        let key: Key = ("YouTube".into(), Kind::Video);
        let cases = [(100, 100), (100 + GAP_SECS + 1, 40)];
        for (quit, end) in cases {
            let mut t = Tracker::new(5);
            t.tick(0, std::slice::from_ref(&key));
            t.tick(40, std::slice::from_ref(&key));
            t.set_row(&key, 7);
            assert_eq!(
                t.close_all(quit),
                [Write::Extend { row: 7, end }],
                "quit at {quit}"
            );
        }
    }
}
