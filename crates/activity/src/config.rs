use std::path::PathBuf;

use activity_core::classify::ClassifierConfig;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// KDE Connect device id; unset means the first reachable paired device.
    pub kdeconnect_device: Option<String>,
    /// A reminder missed by up to this much (suspend, daemon down) still fires.
    pub grace_secs: u32,
    pub classifier: ClassifierConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            kdeconnect_device: None,
            grace_secs: 1800,
            classifier: ClassifierConfig::default(),
        }
    }
}

fn xdg(var: &str, fallback: &str) -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(var).filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(fallback))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(xdg("XDG_CONFIG_HOME", ".config")?.join("activity/config.toml"))
}

pub fn data_dir() -> Result<PathBuf> {
    Ok(xdg("XDG_DATA_HOME", ".local/share")?.join("activity"))
}

pub fn load() -> Result<Config> {
    let path = config_path()?;
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}
