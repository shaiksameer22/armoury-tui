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
                Preset {
                    name: "Travel".into(),
                    profile: Some("Quiet".into()),
                    charge_limit: Some(60),
                    brightness: Some(1),
                },
                Preset {
                    name: "Balanced".into(),
                    profile: Some("Balanced".into()),
                    charge_limit: Some(80),
                    brightness: Some(2),
                },
                Preset {
                    name: "Gaming".into(),
                    profile: Some("Performance".into()),
                    charge_limit: Some(100),
                    brightness: Some(3),
                },
            ],
            rules: vec![Rule {
                battery_below: 20,
                preset: "Travel".into(),
            }],
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
        let Some(p) = path() else {
            return Config::default();
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_default_theme() {
        let c = Config::default();
        assert_eq!(c.theme, "Cyberpunk");
    }

    #[test]
    fn test_default_startup_tab() {
        let c = Config::default();
        assert_eq!(c.startup_tab, 0);
    }

    #[test]
    fn test_default_presets_count() {
        let c = Config::default();
        assert_eq!(c.presets.len(), 3);
    }

    #[test]
    fn test_default_preset_names() {
        let c = Config::default();
        let names: Vec<&str> = c.presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Travel", "Balanced", "Gaming"]);
    }

    #[test]
    fn test_default_rules_count() {
        let c = Config::default();
        assert_eq!(c.rules.len(), 1);
    }

    #[test]
    fn test_default_rule_values() {
        let c = Config::default();
        assert_eq!(c.rules[0].battery_below, 20);
        assert_eq!(c.rules[0].preset, "Travel");
    }

    #[test]
    fn test_default_alert_thresholds() {
        let c = Config::default();
        assert_eq!(c.cpu_temp_alert, 90.0);
        assert_eq!(c.gpu_temp_alert, 87.0);
        assert_eq!(c.batt_low_pct, 15.0);
        assert_eq!(c.fan_stall_temp, 75.0);
    }

    #[test]
    fn test_preset_travel() {
        let c = Config::default();
        let p = c.preset("Travel");
        assert!(p.is_some());
        let p = p.unwrap();
        assert_eq!(p.name, "Travel");
        assert_eq!(p.profile, Some("Quiet".into()));
        assert_eq!(p.charge_limit, Some(60));
        assert_eq!(p.brightness, Some(1));
    }

    #[test]
    fn test_preset_nonexistent() {
        let c = Config::default();
        assert!(c.preset("Nonexistent").is_none());
    }

    #[test]
    fn test_save_load_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("armoury_tui_test_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        // Override XDG_CONFIG_HOME so path() points into our temp dir.
        std::env::set_var("XDG_CONFIG_HOME", &tmp);

        let original = Config::default();
        original.save().expect("save should succeed");

        let loaded = Config::load();
        assert_eq!(loaded.theme, original.theme);
        assert_eq!(loaded.startup_tab, original.startup_tab);
        assert_eq!(loaded.presets.len(), original.presets.len());
        assert_eq!(loaded.rules.len(), original.rules.len());
        assert_eq!(loaded.cpu_temp_alert, original.cpu_temp_alert);

        // Clean up
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_malformed_toml_falls_back_to_defaults() {
        let tmp = std::env::temp_dir().join(format!("armoury_tui_bad_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &tmp);

        // Write bad TOML to the config path
        let config_dir = tmp.join("armoury-tui");
        std::fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("config.toml");
        let mut f = std::fs::File::create(&config_path).unwrap();
        writeln!(f, "{{{{ this is not valid TOML !!! }}}}").unwrap();

        let loaded = Config::load();
        let defaults = Config::default();
        // Malformed TOML should fall back to defaults via unwrap_or_default()
        assert_eq!(loaded.theme, defaults.theme);
        assert_eq!(loaded.startup_tab, defaults.startup_tab);
        assert_eq!(loaded.presets.len(), defaults.presets.len());

        // Clean up
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
