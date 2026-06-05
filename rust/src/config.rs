//! User config at `~/.config/armoury-tui/config.toml`.
//!
//! Persists the colour theme, startup tab and alert thresholds, and holds the
//! power **presets** (named profile/charge/brightness bundles) and **rules**
//! (apply a preset when the battery drops below a level). A starter file is
//! written on first run so users have something to edit.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: String,
    pub startup_tab: usize,
    pub cpu_temp_alert: f64,
    pub gpu_temp_alert: f64,
    pub batt_low_pct: f64,
    pub fan_stall_temp: f64,
    pub presets: Vec<Preset>,
    pub rules: Vec<Rule>,
}

/// A named bundle of control settings, applied in one shot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub charge_limit: Option<i64>,
    #[serde(default)]
    pub brightness: Option<i64>,
}

/// Apply `preset` when the (discharging) battery falls below `battery_below` %.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub battery_below: u8,
    pub preset: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: "Cyberpunk".into(),
            startup_tab: 0,
            cpu_temp_alert: 90.0,
            gpu_temp_alert: 87.0,
            batt_low_pct: 15.0,
            fan_stall_temp: 75.0,
            presets: vec![
                Preset { name: "Travel".into(), profile: Some("Quiet".into()), charge_limit: Some(60), brightness: Some(1) },
                Preset { name: "Balanced".into(), profile: Some("Balanced".into()), charge_limit: Some(80), brightness: Some(2) },
                Preset { name: "Gaming".into(), profile: Some("Performance".into()), charge_limit: Some(100), brightness: Some(3) },
            ],
            rules: vec![Rule { battery_below: 20, preset: "Travel".into() }],
        }
    }
}

pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("armoury-tui").join("config.toml"))
}

impl Config {
    /// Load config, writing a default starter file if none exists.
    pub fn load() -> Config {
        let Some(p) = path() else { return Config::default() };
        match std::fs::read_to_string(&p) {
            Ok(s) => toml::from_str(&s).unwrap_or_default(),
            Err(_) => {
                let c = Config::default();
                let _ = c.save();
                c
            }
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(p) = path() else { return Ok(()) };
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(p, toml::to_string_pretty(self).unwrap_or_default())
    }

    pub fn preset(&self, name: &str) -> Option<&Preset> {
        self.presets.iter().find(|p| p.name == name)
    }
}
