//! Persistent configuration: OAuth client credentials, poll cadence, and
//! per-calendar visibility / notification preferences.
//!
//! Stored as TOML at `~/.config/calendar-notification/config.toml`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// Preferences for a single Google calendar (keyed by calendar id in [`Config::calendars`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarPrefs {
    /// Show this calendar's events in the agenda widget.
    #[serde(default = "default_true")]
    pub visible: bool,
    /// Fire desktop reminders for this calendar's events.
    #[serde(default = "default_true")]
    pub notify: bool,
    /// Hex color (e.g. `#4285F4`) used for the agenda dot. Empty = use Google's color.
    #[serde(default)]
    pub color: String,
}

impl Default for CalendarPrefs {
    fn default() -> Self {
        Self {
            visible: true,
            notify: true,
            color: String::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_poll_interval() -> u64 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// OAuth 2.0 Desktop-app client id from Google Cloud Console.
    #[serde(default)]
    pub client_id: String,
    /// OAuth 2.0 Desktop-app client secret.
    #[serde(default)]
    pub client_secret: String,
    /// Minutes between calendar syncs.
    #[serde(default = "default_poll_interval")]
    pub poll_interval_minutes: u64,
    /// Per-calendar preferences, keyed by calendar id.
    #[serde(default)]
    pub calendars: BTreeMap<String, CalendarPrefs>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret: String::new(),
            poll_interval_minutes: default_poll_interval(),
            calendars: BTreeMap::new(),
        }
    }
}

impl Config {
    /// True once the user has pasted their OAuth client credentials.
    pub fn has_credentials(&self) -> bool {
        !self.client_id.trim().is_empty() && !self.client_secret.trim().is_empty()
    }

    /// Merge in a freshly discovered calendar without clobbering existing prefs.
    pub fn ensure_calendar(&mut self, id: &str, color: &str) {
        self.calendars
            .entry(id.to_string())
            .or_insert_with(|| CalendarPrefs {
                color: color.to_string(),
                ..Default::default()
            });
    }

    /// Load config from disk, creating a default file on first run.
    pub fn load_or_create() -> Result<Self> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating config dir {}", parent.display()))?;
        }
        if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let cfg: Config =
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
            Ok(cfg)
        } else {
            let cfg = Config::default();
            cfg.save()?;
            Ok(cfg)
        }
    }

    /// Persist current config back to disk.
    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self).context("serializing config")?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// `~/.config/calendar-notification/config.toml`
pub fn config_path() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join("config.toml"))
}

/// `~/.local/share/calendar-notification/token.json`
pub fn token_path() -> Result<PathBuf> {
    Ok(project_dirs()?.data_dir().join("token.json"))
}

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("com", "calendar-notification", "calendar-notification")
        .context("could not determine XDG project directories")
}
