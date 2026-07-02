//! Persistent configuration: OAuth client credentials, poll cadence, and
//! per-calendar visibility / notification preferences.
//!
//! Stored as TOML at `~/.config/calendar-notification/config.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
        Self::load_or_create_at(&config_path()?)
    }

    /// Persist current config back to disk.
    pub fn save(&self) -> Result<()> {
        self.save_to(&config_path()?)
    }

    /// Load from (or create a default at) a specific path — the testable core
    /// of [`Config::load_or_create`].
    pub fn load_or_create_at(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating config dir {}", parent.display()))?;
        }
        if path.exists() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            let cfg: Config =
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
            Ok(cfg)
        } else {
            let cfg = Config::default();
            cfg.save_to(path)?;
            Ok(cfg)
        }
    }

    /// Persist to a specific path — the testable core of [`Config::save`].
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self).context("serializing config")?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
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

#[cfg(test)]
mod tests {
    #![allow(clippy::field_reassign_with_default)]
    use super::*;

    #[test]
    fn default_has_no_credentials() {
        let cfg = Config::default();
        assert!(!cfg.has_credentials());
        assert_eq!(cfg.poll_interval_minutes, 5);
        assert!(cfg.calendars.is_empty());
    }

    #[test]
    fn has_credentials_requires_both_nonblank() {
        let mut cfg = Config::default();
        cfg.client_id = "id".into();
        assert!(!cfg.has_credentials(), "secret still missing");
        cfg.client_secret = "  ".into();
        assert!(!cfg.has_credentials(), "blank secret does not count");
        cfg.client_secret = "secret".into();
        assert!(cfg.has_credentials());
    }

    #[test]
    fn ensure_calendar_inserts_once_and_preserves_prefs() {
        let mut cfg = Config::default();
        cfg.ensure_calendar("cal-1", "#ff0000");
        cfg.calendars.get_mut("cal-1").unwrap().visible = false;
        // Re-ensuring must not clobber the user's changed pref.
        cfg.ensure_calendar("cal-1", "#00ff00");
        let prefs = &cfg.calendars["cal-1"];
        assert!(!prefs.visible);
        assert_eq!(prefs.color, "#ff0000");
        assert_eq!(cfg.calendars.len(), 1);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        // Only credentials present; everything else should default.
        let toml = r#"client_id = "abc"
client_secret = "xyz""#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(cfg.has_credentials());
        assert_eq!(cfg.poll_interval_minutes, 5);
        assert!(cfg.calendars.is_empty());
    }

    #[test]
    fn calendar_prefs_default_true() {
        let toml = r##"[calendars."c1"]
color = "#123456""##;
        let cfg: Config = toml::from_str(toml).unwrap();
        let p = &cfg.calendars["c1"];
        assert!(p.visible);
        assert!(p.notify);
        assert_eq!(p.color, "#123456");
    }

    #[test]
    fn load_or_create_creates_then_reads_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/config.toml");

        // First run: file doesn't exist -> defaults written.
        let created = Config::load_or_create_at(&path).unwrap();
        assert!(path.exists(), "default file should be created");
        assert!(!created.has_credentials());

        // Mutate + save, then reload and confirm it round-trips.
        let mut cfg = created;
        cfg.client_id = "id".into();
        cfg.client_secret = "sec".into();
        cfg.poll_interval_minutes = 15;
        cfg.ensure_calendar("cal", "#abcdef");
        cfg.save_to(&path).unwrap();

        let reloaded = Config::load_or_create_at(&path).unwrap();
        assert_eq!(reloaded.client_id, "id");
        assert_eq!(reloaded.poll_interval_minutes, 15);
        assert_eq!(reloaded.calendars["cal"].color, "#abcdef");
    }

    #[test]
    fn load_or_create_rejects_malformed_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is = not valid = toml =").unwrap();
        assert!(Config::load_or_create_at(&path).is_err());
    }

    #[test]
    fn xdg_paths_end_with_expected_names() {
        assert!(config_path()
            .unwrap()
            .ends_with("calendar-notification/config.toml"));
        assert!(token_path()
            .unwrap()
            .ends_with("calendar-notification/token.json"));
    }
}
