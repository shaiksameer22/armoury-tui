//! Module 2 — the state controller (safe writes), Phase B.
//!
//! Replaces the Python `asusctl` shell-out + RON regex with native D-Bus calls
//! to **asusd 6.3.1** (`xyz.ljones.Asusd`, see `dbus.rs`). Every method returns a
//! `ControlResult` and swallows errors — a failed action must surface as a toast,
//! never a panic that tears down the render loop.
//!
//! Profile is an integer enum on this asusd: Balanced=0, Performance=1, Quiet=2
//! (Balanced=0 confirmed live against the ACPI sysfs name). Charge limit and
//! keyboard brightness are writable properties; fan curves are 8-point byte
//! arrays keyed by the profile int.

use crate::dbus::{Bus, RawCurve};
use crate::scanner::HardwareMap;

#[derive(Debug, Clone)]
pub struct ControlResult {
    pub ok: bool,
    pub message: String,
}

impl ControlResult {
    fn ok(msg: impl Into<String>) -> Self {
        ControlResult { ok: true, message: msg.into() }
    }
    fn err(msg: impl Into<String>) -> Self {
        ControlResult { ok: false, message: msg.into() }
    }
}

/// One fan's temperature→PWM curve for a given performance profile.
#[derive(Debug, Clone)]
pub struct FanCurve {
    #[allow(dead_code)] // which profile the curve belongs to; kept for the data model
    pub profile: String,
    pub fan: String,              // "CPU" | "GPU" | …
    pub points: Vec<(u8, u8)>,    // (temp °C, pwm 0-255), low→high
    pub enabled: bool,
}

impl FanCurve {
    /// PWM duty as 0-100% per point.
    pub fn pwm_pcts(&self) -> Vec<i32> {
        self.points.iter().map(|&(_, p)| (p as f64 / 255.0 * 100.0).round() as i32).collect()
    }
}

// -- profile int <-> name (asusd 6.x PlatformProfile enum) ------------------

fn profile_int(name: &str) -> Option<u32> {
    match name {
        "Balanced" => Some(0),
        "Performance" => Some(1),
        "Quiet" => Some(2),
        "LowPower" => Some(3),
        _ => None,
    }
}

pub fn profile_name(i: u32) -> Option<&'static str> {
    match i {
        0 => Some("Balanced"),
        1 => Some("Performance"),
        2 => Some("Quiet"),
        3 => Some("LowPower"),
        _ => None,
    }
}

pub struct Controller {
    bus: Option<Bus>,
}

impl Controller {
    pub fn new(_hw: &HardwareMap) -> Self {
        Controller { bus: Bus::connect().ok() }
    }

    pub fn available(&self) -> bool {
        self.bus.is_some()
    }

    fn bus(&self) -> Result<&Bus, ControlResult> {
        self.bus.as_ref().ok_or_else(|| ControlResult::err("asusd not reachable (xyz.ljones.Asusd)"))
    }

    // -- power profiles ---------------------------------------------------

    /// Profiles offered by asusd, in the canonical Quiet/Balanced/Performance
    /// order, filtered to what this machine actually supports.
    pub fn list_profiles(&self) -> Vec<String> {
        let supported: Vec<u32> = self
            .bus
            .as_ref()
            .and_then(|b| b.platform().ok())
            .and_then(|p| p.get_property::<Vec<u32>>("PlatformProfileChoices").ok())
            .unwrap_or_default();
        let order = ["Quiet", "Balanced", "Performance"];
        let avail: Vec<String> = order
            .iter()
            .filter(|n| profile_int(n).map(|i| supported.contains(&i)).unwrap_or(false))
            .map(|n| n.to_string())
            .collect();
        if avail.is_empty() {
            order.iter().map(|n| n.to_string()).collect()
        } else {
            avail
        }
    }

    pub fn set_profile(&self, name: &str) -> ControlResult {
        let bus = match self.bus() {
            Ok(b) => b,
            Err(e) => return e,
        };
        let Some(i) = profile_int(name) else {
            return ControlResult::err(format!("unknown profile '{name}'"));
        };
        match bus.platform().and_then(|p| p.set_property("PlatformProfile", i).map_err(Into::into)) {
            Ok(()) => ControlResult::ok(format!("profile → {name}")),
            Err(e) => ControlResult::err(first_line(&e.to_string())),
        }
    }

    // -- battery charge limit ---------------------------------------------

    pub fn set_charge_limit(&self, percent: i64) -> ControlResult {
        let bus = match self.bus() {
            Ok(b) => b,
            Err(e) => return e,
        };
        let pct = percent.clamp(20, 100) as u8;
        match bus.platform().and_then(|p| p.set_property("ChargeControlEndThreshold", pct).map_err(Into::into)) {
            Ok(()) => ControlResult::ok(format!("charge limit → {pct}%")),
            Err(e) => ControlResult::err(first_line(&e.to_string())),
        }
    }

    /// Charge to 100% once (for travel), then revert to the standing limit.
    pub fn one_shot_full_charge(&self) -> ControlResult {
        let bus = match self.bus() {
            Ok(b) => b,
            Err(e) => return e,
        };
        match bus.platform().and_then(|p| p.call_method("OneShotFullCharge", &()).map(|_| ()).map_err(Into::into)) {
            Ok(()) => ControlResult::ok("one-shot full charge armed"),
            Err(e) => ControlResult::err(first_line(&e.to_string())),
        }
    }

    // -- auto profile (on AC / on battery) --------------------------------

    /// (profile_on_ac, profile_on_battery, auto_switch_ac, auto_switch_battery).
    pub fn auto_profiles(&self) -> Option<(u32, u32, bool, bool)> {
        let p = self.bus.as_ref()?.platform().ok()?;
        Some((
            p.get_property("PlatformProfileOnAc").ok()?,
            p.get_property("PlatformProfileOnBattery").ok()?,
            p.get_property("ChangePlatformProfileOnAc").ok()?,
            p.get_property("ChangePlatformProfileOnBattery").ok()?,
        ))
    }

    /// Toggle automatic profile switching when (un)plugging.
    pub fn set_auto_switch(&self, on_ac: bool, enabled: bool) -> ControlResult {
        let bus = match self.bus() {
            Ok(b) => b,
            Err(e) => return e,
        };
        let prop = if on_ac { "ChangePlatformProfileOnAc" } else { "ChangePlatformProfileOnBattery" };
        match bus.platform().and_then(|p| p.set_property(prop, enabled).map_err(Into::into)) {
            Ok(()) => ControlResult::ok(format!("auto profile on {} → {}", if on_ac { "AC" } else { "battery" }, if enabled { "on" } else { "off" })),
            Err(e) => ControlResult::err(first_line(&e.to_string())),
        }
    }

    // -- EPP (energy-performance preference, per profile) -----------------

    /// (balanced_epp, performance_epp, quiet_epp) raw ints.
    pub fn epps(&self) -> Option<(u32, u32, u32)> {
        let p = self.bus.as_ref()?.platform().ok()?;
        Some((
            p.get_property("ProfileBalancedEpp").ok()?,
            p.get_property("ProfilePerformanceEpp").ok()?,
            p.get_property("ProfileQuietEpp").ok()?,
        ))
    }

    /// Cycle the EPP (0–4) of one profile. asusd's EPP enum mirrors the kernel
    /// scale (0 performance … 4 power-save); we rotate through it.
    pub fn cycle_epp(&self, profile: &str) -> ControlResult {
        let bus = match self.bus() {
            Ok(b) => b,
            Err(e) => return e,
        };
        let prop = match profile {
            "Balanced" => "ProfileBalancedEpp",
            "Performance" => "ProfilePerformanceEpp",
            "Quiet" => "ProfileQuietEpp",
            _ => return ControlResult::err(format!("no EPP for profile '{profile}'")),
        };
        let Ok(p) = bus.platform() else {
            return ControlResult::err("asusd platform unavailable");
        };
        let cur: u32 = p.get_property(prop).unwrap_or(0);
        let next = (cur + 1) % 5;
        match p.set_property(prop, next) {
            Ok(()) => ControlResult::ok(format!("{profile} EPP → {} ({next})", epp_name(next))),
            Err(e) => ControlResult::err(first_line(&e.to_string())),
        }
    }

    // -- keyboard brightness ----------------------------------------------

    pub fn set_brightness(&self, level: i64) -> ControlResult {
        let bus = match self.bus() {
            Ok(b) => b,
            Err(e) => return e,
        };
        let lvl = level.clamp(0, 3) as u32;
        match bus.aura().and_then(|p| p.set_property("Brightness", lvl).map_err(Into::into)) {
            Ok(()) => ControlResult::ok(format!("brightness → {lvl}")),
            Err(e) => ControlResult::err(first_line(&e.to_string())),
        }
    }

    // -- aura (LED mode) --------------------------------------------------

    /// LED modes asusd reports as supported on this keyboard (raw ints).
    pub fn supported_aura_modes(&self) -> Vec<u32> {
        self.bus
            .as_ref()
            .and_then(|b| b.aura().ok())
            .and_then(|p| p.get_property::<Vec<u32>>("SupportedBasicModes").ok())
            .unwrap_or_default()
    }

    pub fn current_aura_mode(&self) -> Option<u32> {
        self.bus.as_ref()?.aura().ok()?.get_property::<u32>("LedMode").ok()
    }

    /// Set the LED effect mode (uses the colours already stored in asusd).
    /// Colour/speed editing via LedModeData is a follow-up — see dbus.rs.
    pub fn set_aura_mode(&self, mode: u32) -> ControlResult {
        let bus = match self.bus() {
            Ok(b) => b,
            Err(e) => return e,
        };
        match bus.aura().and_then(|p| p.set_property("LedMode", mode).map_err(Into::into)) {
            Ok(()) => ControlResult::ok(format!("aura mode → {}", aura_mode_name(mode))),
            Err(e) => ControlResult::err(first_line(&e.to_string())),
        }
    }

    /// Advance to the next asusd-supported LED mode (read current, set next).
    pub fn cycle_aura_mode(&self) -> ControlResult {
        if self.bus().is_err() {
            return ControlResult::err("asusd not reachable (xyz.ljones.Asusd)");
        }
        let modes = self.supported_aura_modes();
        if modes.is_empty() {
            return ControlResult::err("no aura modes on this keyboard");
        }
        let cur = self.current_aura_mode().unwrap_or(modes[0]);
        let next = modes.iter().position(|&m| m == cur).map(|i| (i + 1) % modes.len()).unwrap_or(0);
        self.set_aura_mode(modes[next])
    }

    // -- fan curves -------------------------------------------------------

    pub fn get_fan_curves(&self, profile: &str) -> Vec<FanCurve> {
        let Ok(bus) = self.bus() else { return Vec::new() };
        let Some(i) = profile_int(profile) else { return Vec::new() };
        let raw: Vec<RawCurve> = match bus.fancurves().and_then(|p| p.call("FanCurveData", &(i,)).map_err(Into::into)) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        raw.into_iter()
            .map(|(fan, temps, pwms, enabled)| FanCurve {
                profile: profile.to_string(),
                fan: fan.to_uppercase(),
                points: zip8(temps).into_iter().zip(zip8(pwms)).collect(),
                enabled,
            })
            .collect()
    }

    pub fn set_fan_curve(&self, profile: &str, curve: &FanCurve) -> ControlResult {
        let bus = match self.bus() {
            Ok(b) => b,
            Err(e) => return e,
        };
        let Some(i) = profile_int(profile) else {
            return ControlResult::err(format!("unknown profile '{profile}'"));
        };
        let temps = unzip8(curve.points.iter().map(|&(t, _)| t));
        let pwms = unzip8(curve.points.iter().map(|&(_, p)| p));
        let raw: RawCurve = (curve.fan.to_lowercase(), temps, pwms, true);
        match bus.fancurves().and_then(|p| p.call_method("SetFanCurve", &(i, raw)).map(|_| ()).map_err(Into::into)) {
            Ok(()) => ControlResult::ok(format!("{} curve set on {profile}", curve.fan)),
            Err(e) => ControlResult::err(first_line(&e.to_string())),
        }
    }

    pub fn set_curve_enabled(&self, profile: &str, enabled: bool) -> ControlResult {
        let bus = match self.bus() {
            Ok(b) => b,
            Err(e) => return e,
        };
        let Some(i) = profile_int(profile) else {
            return ControlResult::err(format!("unknown profile '{profile}'"));
        };
        match bus.fancurves().and_then(|p| p.call_method("SetFanCurvesEnabled", &(i, enabled)).map(|_| ()).map_err(Into::into)) {
            Ok(()) => ControlResult::ok(format!("{profile} curves {}", if enabled { "enabled" } else { "disabled" })),
            Err(e) => ControlResult::err(first_line(&e.to_string())),
        }
    }

    pub fn reset_fan_curve(&self, profile: &str) -> ControlResult {
        let bus = match self.bus() {
            Ok(b) => b,
            Err(e) => return e,
        };
        let Some(i) = profile_int(profile) else {
            return ControlResult::err(format!("unknown profile '{profile}'"));
        };
        match bus.fancurves().and_then(|p| p.call_method("SetCurvesToDefaults", &(i,)).map(|_| ()).map_err(Into::into)) {
            Ok(()) => ControlResult::ok(format!("{profile} curves reset to default")),
            Err(e) => ControlResult::err(first_line(&e.to_string())),
        }
    }
}

/// Best-effort label for an EPP int (kernel energy-performance-preference scale).
pub fn epp_name(epp: u32) -> &'static str {
    match epp {
        0 => "performance",
        1 => "balance-perf",
        2 => "default",
        3 => "balance-power",
        4 => "power-save",
        _ => "epp",
    }
}

/// Best-effort name for an asusd LED mode int (device-dependent).
pub fn aura_mode_name(mode: u32) -> &'static str {
    match mode {
        0 => "static",
        1 => "breathe",
        2 => "pulse",
        3 => "rainbow",
        6 => "highlight",
        _ => "mode",
    }
}

fn zip8(c: crate::dbus::Curve8) -> Vec<u8> {
    vec![c.0, c.1, c.2, c.3, c.4, c.5, c.6, c.7]
}

fn unzip8(it: impl Iterator<Item = u8>) -> crate::dbus::Curve8 {
    let mut a = [0u8; 8];
    for (slot, v) in a.iter_mut().zip(it) {
        *slot = v;
    }
    (a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7])
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or(s).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_roundtrip() {
        for n in ["Quiet", "Balanced", "Performance"] {
            assert_eq!(profile_name(profile_int(n).unwrap()), Some(n));
        }
        assert_eq!(profile_int("Balanced"), Some(0)); // confirmed live
    }

    // Read-only smoke test against a live asusd (no-op if the daemon is absent).
    #[test]
    fn reads_from_asusd() {
        let hw = crate::scanner::scan();
        let c = Controller::new(&hw);
        if !c.available() {
            eprintln!("asusd not reachable; skipping");
            return;
        }
        let profs = c.list_profiles();
        eprintln!("profiles = {profs:?}");
        assert!(profs.contains(&"Balanced".to_string()));
        for p in &profs {
            let curves = c.get_fan_curves(p);
            eprintln!("{p}: {} fan curve(s): {:?}", curves.len(),
                      curves.iter().map(|c| (c.fan.clone(), c.enabled, c.pwm_pcts())).collect::<Vec<_>>());
        }
        eprintln!("aura modes = {:?}, current = {:?}", c.supported_aura_modes(), c.current_aura_mode());
    }

    #[test]
    fn curve_pcts() {
        let c = FanCurve { profile: "Performance".into(), fan: "CPU".into(), points: vec![(30, 0), (60, 128), (90, 255)], enabled: true };
        assert_eq!(c.pwm_pcts(), vec![0, 50, 100]);
    }
}
