use std::collections::VecDeque;

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Away,
    Idle,
    Gaming,
    Watching,
    Focus,
    Chatting,
    Browsing,
    Drifting,
    Active,
}

impl State {
    fn is_leisure(self) -> bool {
        matches!(self, State::Browsing | State::Chatting | State::Active)
    }
}

/// A rule as written in config.toml. `class` entries are case-insensitive
/// globs where `*` matches any run of characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSpec {
    pub name: String,
    #[serde(default)]
    pub class: Vec<String>,
    pub caption: Option<String>,
    pub fullscreen: Option<bool>,
    pub media_playing: Option<bool>,
    pub state: State,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClassifierConfig {
    pub idle_after_secs: u32,
    pub away_after_secs: u32,
    /// This many window switches between leisure apps inside the window means drifting.
    pub drift_switches: u32,
    pub drift_window_secs: u32,
    pub rules: Vec<RuleSpec>,
}

impl Default for ClassifierConfig {
    fn default() -> Self {
        let rule = |name: &str, class: &[&str], caption: Option<&str>, fullscreen, media, state| {
            RuleSpec {
                name: name.into(),
                class: class.iter().map(|c| (*c).into()).collect(),
                caption: caption.map(Into::into),
                fullscreen,
                media_playing: media,
                state,
            }
        };
        let terminals = [
            "konsole",
            "org.kde.konsole",
            "kitty",
            "alacritty",
            "foot",
            "org.wezfurlong.wezterm",
        ];
        let browsers = ["firefox", "org.mozilla.firefox"];
        Self {
            idle_after_secs: 180,
            away_after_secs: 900,
            drift_switches: 12,
            drift_window_secs: 600,
            rules: vec![
                rule(
                    "steam game",
                    &["steam_app_*"],
                    None,
                    None,
                    None,
                    State::Gaming,
                ),
                rule(
                    "mahjong",
                    &["kmahjongg", "org.kde.kmahjongg"],
                    None,
                    None,
                    None,
                    State::Gaming,
                ),
                rule(
                    "fullscreen video",
                    &browsers,
                    None,
                    Some(true),
                    Some(true),
                    State::Watching,
                ),
                rule(
                    "claude web",
                    &browsers,
                    Some("(?i)claude"),
                    None,
                    None,
                    State::Focus,
                ),
                rule(
                    "anime tab",
                    &browsers,
                    Some("(?i)(crunchyroll|hidive|anime|netflix|youtube|episode)"),
                    None,
                    Some(true),
                    State::Watching,
                ),
                rule(
                    "discord",
                    &["discord", "vesktop", "equibop", "webcord"],
                    None,
                    None,
                    None,
                    State::Chatting,
                ),
                rule(
                    "ssh or claude in a terminal",
                    &terminals,
                    Some("(?i)(ssh|claude)"),
                    None,
                    None,
                    State::Focus,
                ),
                rule(
                    "editor",
                    &[
                        "code",
                        "codium",
                        "org.kde.kate",
                        "dev.zed.zed",
                        "jetbrains-*",
                    ],
                    None,
                    None,
                    None,
                    State::Focus,
                ),
                rule("browser", &browsers, None, None, None, State::Browsing),
            ],
        }
    }
}

#[derive(Debug)]
struct Rule {
    name: String,
    class: Vec<Regex>,
    caption: Option<Regex>,
    fullscreen: Option<bool>,
    media_playing: Option<bool>,
    state: State,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("rule {rule:?}: {source}")]
    Pattern { rule: String, source: regex::Error },
}

/// What the sensors report right now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub class: String,
    pub caption: String,
    pub fullscreen: bool,
    pub media_playing: bool,
    pub input_idle_secs: u64,
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verdict {
    pub state: State,
    pub rule: Option<String>,
}

#[derive(Debug)]
pub struct Classifier {
    rules: Vec<Rule>,
    idle_after: u64,
    away_after: u64,
    drift_switches: usize,
    drift_window: i64,
    switches: VecDeque<i64>,
    last_class: String,
}

impl Classifier {
    pub fn new(cfg: &ClassifierConfig) -> Result<Self, Error> {
        let pattern = |rule: &str, p: &str| {
            Regex::new(p).map_err(|source| Error::Pattern {
                rule: rule.into(),
                source,
            })
        };
        let rules = cfg
            .rules
            .iter()
            .map(|r| {
                Ok(Rule {
                    name: r.name.clone(),
                    class: r
                        .class
                        .iter()
                        .map(|g| pattern(&r.name, &glob_to_regex(g)))
                        .collect::<Result<_, _>>()?,
                    caption: r
                        .caption
                        .as_deref()
                        .map(|c| pattern(&r.name, c))
                        .transpose()?,
                    fullscreen: r.fullscreen,
                    media_playing: r.media_playing,
                    state: r.state,
                })
            })
            .collect::<Result<_, Error>>()?;
        Ok(Self {
            rules,
            idle_after: u64::from(cfg.idle_after_secs),
            away_after: u64::from(cfg.away_after_secs),
            drift_switches: cfg.drift_switches as usize,
            drift_window: i64::from(cfg.drift_window_secs),
            switches: VecDeque::new(),
            last_class: String::new(),
        })
    }

    pub fn classify(&mut self, s: &Snapshot, now: i64) -> Verdict {
        let matched = self.rules.iter().find(|r| {
            (r.class.is_empty() || r.class.iter().any(|c| c.is_match(&s.class)))
                && r.caption.as_ref().is_none_or(|c| c.is_match(&s.caption))
                && r.fullscreen.is_none_or(|f| f == s.fullscreen)
                && r.media_playing.is_none_or(|m| m == s.media_playing)
        });
        let window_state = matched.map_or(State::Active, |r| r.state);

        if window_state.is_leisure() && s.class != self.last_class {
            self.switches.push_back(now);
        }
        self.last_class.clone_from(&s.class);
        while self
            .switches
            .front()
            .is_some_and(|t| now - t > self.drift_window)
        {
            self.switches.pop_front();
        }

        let (state, rule) = if s.locked {
            (State::Away, Some("screen locked"))
        } else if window_state == State::Watching {
            // Watching needs no input, so idleness doesn't override it.
            (State::Watching, matched.map(|r| r.name.as_str()))
        } else if s.input_idle_secs >= self.away_after {
            (State::Away, Some("idle long"))
        } else if s.input_idle_secs >= self.idle_after {
            (State::Idle, Some("idle"))
        } else if window_state.is_leisure()
            && self.drift_switches > 0
            && self.switches.len() >= self.drift_switches
        {
            (State::Drifting, Some("many app switches"))
        } else {
            (window_state, matched.map(|r| r.name.as_str()))
        };
        Verdict {
            state,
            rule: rule.map(Into::into),
        }
    }
}

pub(crate) fn glob_to_regex(glob: &str) -> String {
    let parts: Vec<String> = glob.split('*').map(regex::escape).collect();
    format!("(?i)^{}$", parts.join(".*"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(class: &str, caption: &str, fullscreen: bool, media: bool, idle: u64) -> Snapshot {
        Snapshot {
            class: class.into(),
            caption: caption.into(),
            fullscreen,
            media_playing: media,
            input_idle_secs: idle,
            locked: false,
        }
    }

    fn classifier() -> Classifier {
        Classifier::new(&ClassifierConfig::default()).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn default_rules() {
        let cases = [
            (
                "steam game",
                snap("steam_app_1172470", "Apex", true, false, 0),
                State::Gaming,
            ),
            (
                "kmahjongg",
                snap("org.kde.kmahjongg", "KMahjongg", false, false, 0),
                State::Gaming,
            ),
            (
                "fullscreen anime",
                snap("firefox", "Frieren - Crunchyroll", true, true, 400),
                State::Watching,
            ),
            (
                "windowed anime tab",
                snap("firefox", "Episode 3 — Mozilla Firefox", false, true, 0),
                State::Watching,
            ),
            (
                "paused anime is browsing",
                snap("firefox", "Episode 3 — Mozilla Firefox", false, false, 0),
                State::Browsing,
            ),
            (
                "claude web",
                snap("firefox", "Claude — Mozilla Firefox", false, false, 0),
                State::Focus,
            ),
            (
                "discord",
                snap("discord", "#general", false, true, 0),
                State::Chatting,
            ),
            (
                "ssh in konsole",
                snap("org.kde.konsole", "req@box: ssh dev", false, false, 0),
                State::Focus,
            ),
            (
                "plain konsole",
                snap("org.kde.konsole", "~ : bash", false, false, 0),
                State::Active,
            ),
            (
                "music while browsing",
                snap("firefox", "Reddit", false, true, 0),
                State::Browsing,
            ),
            (
                "idle with music",
                snap("firefox", "Reddit", false, true, 200),
                State::Idle,
            ),
            (
                "idle long",
                snap("discord", "#general", false, false, 1000),
                State::Away,
            ),
            (
                "class match is case-insensitive",
                snap("Steam_App_42", "", false, false, 0),
                State::Gaming,
            ),
        ];
        for (name, s, want) in cases {
            assert_eq!(classifier().classify(&s, 0).state, want, "case {name}");
        }
    }

    #[test]
    fn locked_is_away() {
        let s = Snapshot {
            locked: true,
            ..snap("steam_app_1", "", true, false, 0)
        };
        assert_eq!(classifier().classify(&s, 0).state, State::Away);
    }

    #[test]
    fn rapid_leisure_switching_is_drifting() {
        let mut c = classifier();
        let apps = ["firefox", "discord"];
        let mut last = State::Active;
        for i in 0..12 {
            last = c
                .classify(
                    &snap(apps[i % 2], "x", false, false, 0),
                    i64::try_from(i).unwrap_or(0) * 10,
                )
                .state;
        }
        assert_eq!(last, State::Drifting);
        // Ten minutes later the switches have aged out.
        assert_eq!(
            c.classify(&snap("firefox", "x", false, false, 0), 2000)
                .state,
            State::Browsing
        );
    }

    #[test]
    fn bad_pattern_is_reported() {
        let cfg = ClassifierConfig {
            rules: vec![RuleSpec {
                name: "broken".into(),
                class: vec![],
                caption: Some("(".into()),
                fullscreen: None,
                media_playing: None,
                state: State::Focus,
            }],
            ..ClassifierConfig::default()
        };
        assert!(
            matches!(Classifier::new(&cfg), Err(Error::Pattern { rule, .. }) if rule == "broken")
        );
    }
}
